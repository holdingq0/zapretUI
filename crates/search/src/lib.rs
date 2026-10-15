mod generate;
mod priors;

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use rkn_core::ipc::{LineKind, LogLine, Phase};
use rkn_core::recipe::Recipe;
use rkn_engine::{Filter, Plan, Profile, Proto, Winws2, lua_files, port_filter};
use rkn_probe::{Opts, PortBlock, Target, Verdict};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

pub use generate::{Candidate, Class, candidates};
pub use priors::Priors;

pub const RESERVED_PORTS: (u16, u16) = (40000, 47999);
const BLOCK: u16 = 32;
const BATCH: usize = 40;
const PARALLEL: usize = 4;
const MAX_BATCHES: usize = 5;
const ENOUGH: usize = 6;
const FINALISTS: usize = 8;
const KEEP: usize = 4;
const ROUND1_HITS: usize = 3;
const ROUND2_HITS: usize = 8;
const ROUND2_REPEATS: usize = 2;
const BUDGET: Duration = Duration::from_secs(240);

static NEXT_BLOCK: AtomicU32 = AtomicU32::new(u32::MAX);
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// Counts trial engines that may still own a winws2 process, including ones whose search was dropped
/// while `start` ran on a blocking thread.
struct Live;

impl Live {
    fn new() -> Self {
        LIVE.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

// Fields drop in order: the process is killed before the count goes down.
struct TrialEngine {
    _engine: Winws2,
    _live: Live,
}

/// Waits until every trial winws2 of a dropped search has exited (at most a few seconds).
pub async fn settle() {
    let deadline = Instant::now() + Duration::from_secs(10);
    while LIVE.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub fn port_block() -> PortBlock {
    let (lo, hi) = RESERVED_PORTS;
    let blocks = u32::from((hi - lo + 1) / BLOCK);
    let _ = NEXT_BLOCK.compare_exchange(u32::MAX, rkn_core::memory::now() as u32 % blocks, Ordering::Relaxed, Ordering::Relaxed);
    let index = NEXT_BLOCK.fetch_add(1, Ordering::Relaxed) % blocks;
    let start = lo + index as u16 * BLOCK;
    PortBlock::new(start, start + BLOCK - 1)
}

pub fn probe_opts() -> Opts {
    Opts { connect: Duration::from_secs(3), idle: Duration::from_millis(2500), want: 48 * 1024 }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub target: Target,
    pub ip: IpAddr,
    pub verdict: Verdict,
    /// A catalog service, not a site the engine found by itself: the first round tests on these.
    pub core: bool,
}

pub struct Job {
    pub class: Class,
    pub hits: Vec<Hit>,
    pub engine_dir: PathBuf,
    /// Strategies that worked on this network before, tried in the first batch.
    pub seed: Vec<Recipe>,
}

#[derive(Debug, Clone, Default)]
pub struct Found {
    pub recipes: Vec<Recipe>,
    pub fixed: Vec<String>,
    pub unfixed: Vec<String>,
    pub tried: usize,
}

#[derive(Debug, Clone)]
struct Score {
    passed: Vec<usize>,
    attempts: usize,
    latency_sum: u64,
}

impl Score {
    fn total(&self) -> usize {
        self.passed.iter().sum()
    }

    fn rate(&self) -> f64 {
        if self.attempts == 0 { 0.0 } else { self.total() as f64 / self.attempts as f64 }
    }

    fn latency(&self) -> u64 {
        self.latency_sum / self.total().max(1) as u64
    }
}

#[derive(Default)]
struct Stats(HashMap<(&'static str, String), (u32, u32)>);

impl Stats {
    fn record(&mut self, candidate: &Candidate, success: bool) {
        for feature in &candidate.features {
            let entry = self.0.entry(feature.clone()).or_default();
            entry.0 += 1;
            entry.1 += u32::from(success);
        }
    }

    fn multiplier(&self, candidate: &Candidate) -> f64 {
        candidate
            .features
            .iter()
            .map(|f| match self.0.get(f) {
                Some((tried, passed)) => ((f64::from(*passed) + 1.0) / (f64::from(*tried) + 2.0) / 0.5).clamp(0.15, 3.0),
                None => 1.0,
            })
            .product()
    }
}

fn pick(pool: &mut Vec<Candidate>, stats: &Stats, n: usize) -> Vec<Candidate> {
    let mut scored: Vec<(f64, usize)> =
        pool.iter().enumerate().map(|(i, c)| (c.prior * stats.multiplier(c), i)).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.truncate(400);
    let mut groups: HashMap<&str, i32> = HashMap::new();
    let mut chosen: Vec<usize> = Vec::new();
    for _ in 0..n {
        let best = scored
            .iter()
            .filter(|(_, i)| !chosen.contains(i))
            .map(|(score, i)| (score * 0.55f64.powi(groups.get(pool[*i].group.as_str()).copied().unwrap_or(0)), *i))
            .max_by(|a, b| a.0.total_cmp(&b.0));
        let Some((_, i)) = best else { break };
        *groups.entry(pool[i].group.as_str()).or_default() += 1;
        chosen.push(i);
    }
    let picked: Vec<Candidate> = chosen.iter().map(|i| pool[*i].clone()).collect();
    chosen.sort_unstable_by(|a, b| b.cmp(a));
    for i in chosen {
        pool.swap_remove(i);
    }
    picked
}

fn test_plan(class: Class, recipe: &Recipe, block: &PortBlock) -> Plan {
    let port = class.port().to_string();
    Plan {
        wf_tcp_out: port.clone(),
        wf_udp_out: String::new(),
        raw_filter: Some(port_filter(block.lo(), block.hi())),
        lua_files: lua_files(),
        debug_log: None,
        profiles: vec![Profile {
            name: "probe".into(),
            proto: Proto::Tcp,
            filter: Filter { ports: port, l7: vec![class.l7().into()], ..Filter::default() },
            recipes: vec![recipe.clone()],
        }],
    }
}

async fn trial(dir: PathBuf, class: Class, recipe: Recipe, hits: Arc<Vec<Hit>>, repeats: usize) -> anyhow::Result<Score> {
    let block = Arc::new(port_block());
    let plan = test_plan(class, &recipe, &block);
    let live = Live::new();
    let (engine, started) = tokio::task::spawn_blocking(move || {
        let mut engine = Winws2::new(dir).background();
        let started = engine.start(&plan);
        (TrialEngine { _engine: engine, _live: live }, started)
    })
    .await?;
    started?;

    let mut set = JoinSet::new();
    for (i, hit) in hits.iter().enumerate() {
        for _ in 0..repeats {
            let hit = hit.clone();
            let block = block.clone();
            set.spawn(async move { (i, rkn_probe::fetch(&hit.target, hit.ip, Some(&block), probe_opts()).await) });
        }
    }
    let mut score = Score { passed: vec![0; hits.len()], attempts: 0, latency_sum: 0 };
    while let Some(result) = set.join_next().await {
        let Ok((i, outcome)) = result else { continue };
        score.attempts += 1;
        if outcome.verdict == Verdict::Ok {
            score.passed[i] += 1;
            score.latency_sum += u64::from(outcome.latency_ms.unwrap_or(0));
        }
    }
    tokio::task::spawn_blocking(move || drop(engine)).await?;
    tracing::debug!(recipe = %recipe.label(), passed = score.total(), attempts = score.attempts, "trial");
    Ok(score)
}


#[derive(Debug, Clone)]
pub struct View {
    pub phase: Phase,
    pub percent: f32,
    pub line: Option<LogLine>,
}

struct Board<'a> {
    class: Class,
    phase: Phase,
    percent: f32,
    progress: &'a (dyn Fn(View) + Send + Sync),
}

impl Board<'_> {
    fn emit(&self, line: Option<LogLine>) {
        (self.progress)(View { phase: self.phase, percent: self.percent, line });
    }

    fn say(&self, kind: LineKind, key: &str, text: String) {
        let key = match self.class {
            Class::Tls => key.to_string(),
            Class::Http => format!("HTTP, {}", key.to_lowercase()),
        };
        self.emit(Some(LogLine { kind, key, text }));
    }
}

/// Runs the recipes, four winws2 at a time. `enough` may end the batch early: dropping the set
/// aborts the trials still running or waiting for a slot. `done` sees every finished trial.
async fn run_batch(
    job: &Job,
    recipes: &[Recipe],
    hits: &Arc<Vec<Hit>>,
    repeats: usize,
    enough: &mut (dyn FnMut(&[Option<Score>]) -> bool + Send),
    done: &mut (dyn FnMut(Option<&Score>) + Send),
) -> anyhow::Result<Vec<Option<Score>>> {
    let gate = Arc::new(Semaphore::new(PARALLEL));
    let mut set = JoinSet::new();
    for (i, recipe) in recipes.iter().enumerate() {
        let gate = gate.clone();
        let dir = job.engine_dir.clone();
        let recipe = recipe.clone();
        let hits = hits.clone();
        let class = job.class;
        set.spawn(async move {
            let _permit = gate.acquire_owned().await;
            (i, trial(dir, class, recipe, hits, repeats).await)
        });
    }
    let mut scores: Vec<Option<Score>> = vec![None; recipes.len()];
    let mut first_error: Option<anyhow::Error> = None;
    while let Some(result) = set.join_next().await {
        let (i, outcome) = result.context("проверка варианта")?;
        match outcome {
            Ok(score) => {
                done(Some(&score));
                scores[i] = Some(score);
                if enough(&scores) {
                    break;
                }
            }
            Err(e) => {
                tracing::debug!("trial failed: {e:#}");
                first_error.get_or_insert(e);
                done(None);
            }
        }
    }
    if scores.iter().all(Option::is_none) {
        if let Some(e) = first_error {
            return Err(e);
        }
    }
    Ok(scores)
}

/// Up to three sites with different kinds of blocking, catalog services first: a flaky site the
/// engine found by itself must not fail every candidate in the first round.
fn round1_hits(hits: &[Hit]) -> Vec<Hit> {
    let mut ordered: Vec<&Hit> = hits.iter().filter(|h| h.core).collect();
    ordered.extend(hits.iter().filter(|h| !h.core));
    let mut picked: Vec<Hit> = Vec::new();
    for hit in &ordered {
        if picked.len() < ROUND1_HITS && !picked.iter().any(|p| p.verdict == hit.verdict) {
            picked.push((*hit).clone());
        }
    }
    for hit in &ordered {
        if picked.len() < ROUND1_HITS && !picked.iter().any(|p| p.target == hit.target) {
            picked.push((*hit).clone());
        }
    }
    picked
}

/// Puts the old strategies at the head of the pool, in their old order of preference.
fn seed(pool: &mut Vec<Candidate>, seeds: &[Recipe]) {
    for (i, recipe) in seeds.iter().enumerate() {
        let prior = 1000.0 - i as f64;
        match pool.iter_mut().find(|c| &c.recipe == recipe) {
            Some(known) => known.prior = prior,
            None => pool.push(Candidate {
                recipe: recipe.clone(),
                features: vec![("family", "seed".into())],
                group: format!("seed:{i}"),
                prior,
            }),
        }
    }
}

fn hosts_of(hits: &[Hit], indices: impl Iterator<Item = usize>) -> Vec<String> {
    let mut out: Vec<String> = indices.map(|i| hits[i].target.host.clone()).collect();
    out.sort();
    out.dedup();
    out
}

pub async fn search(job: &Job, progress: &(dyn Fn(View) + Send + Sync)) -> anyhow::Result<Found> {
    if job.hits.is_empty() {
        bail!("нет сайтов для проверки");
    }
    let started = Instant::now();
    let mut pool = candidates(job.class, &Priors::builtin());
    seed(&mut pool, &job.seed);
    let total = pool.len();
    let mut stats = Stats::default();
    let first = Arc::new(round1_hits(&job.hits));
    let mut tested: Vec<(Candidate, Score)> = Vec::new();
    let mut board = Board { class: job.class, phase: Phase::Round1, percent: 0.0, progress };

    for batch_no in 0..MAX_BATCHES {
        if started.elapsed() > BUDGET {
            break;
        }
        let batch = pick(&mut pool, &stats, BATCH);
        if batch.is_empty() {
            break;
        }
        let text = if batch_no == 0 {
            format!("{} {} обхода из {total}", batch.len(), variants(batch.len()))
        } else {
            format!("Ещё {} {} взамен отсеянных", batch.len(), variants(batch.len()))
        };
        board.say(LineKind::Stage, "Этап 1", text);

        let recipes: Vec<Recipe> = batch.iter().map(|c| c.recipe.clone()).collect();
        let n = recipes.len() as f32;
        let mut done = 0f32;
        let need = first.len();
        let before = tested.iter().filter(|(_, s)| s.total() == need).count();
        let mut enough = |scores: &[Option<Score>]| {
            before + scores.iter().flatten().filter(|s| s.total() == need).count() >= ENOUGH
        };
        let outcome = run_batch(job, &recipes, &first, 1, &mut enough, &mut |_| {
            done += 1.0;
            board.percent = 0.75 * (1.0 - 0.5f32.powf(batch_no as f32 + done / n));
            board.emit(None);
        })
        .await;
        let scores = match outcome {
            Ok(scores) => scores,
            Err(e) if batch_no == 0 => return Err(e),
            Err(e) => {
                tracing::warn!("batch failed: {e:#}");
                break;
            }
        };
        for (candidate, score) in batch.into_iter().zip(scores) {
            let Some(score) = score else { continue };
            stats.record(&candidate, score.total() == first.len());
            tested.push((candidate, score));
        }
        let winners = tested.iter().filter(|(_, s)| s.total() == first.len()).count();
        if winners >= ENOUGH {
            break;
        }
    }

    let tried = tested.len();
    let winners = tested.iter().filter(|(_, s)| s.total() == first.len()).count();
    let mut order: Vec<usize> = (0..tested.len()).filter(|i| tested[*i].1.total() > 0).collect();
    order.sort_by(|a, b| {
        let ((ca, sa), (cb, sb)) = (&tested[*a], &tested[*b]);
        sb.total()
            .cmp(&sa.total())
            .then(ca.recipe.cost().cmp(&cb.recipe.cost()))
            .then(cb.prior.total_cmp(&ca.prior))
    });
    order.truncate(FINALISTS);
    if order.is_empty() {
        board.phase = Phase::Done;
        board.percent = 1.0;
        board.say(LineKind::Problem, "Итог", format!("Ни один из {tried} вариантов не открыл сайты"));
        let unfixed = hosts_of(&job.hits, 0..job.hits.len());
        return Ok(Found { tried, unfixed, ..Found::default() });
    }

    board.phase = Phase::Round2;
    board.percent = 0.75;

    let all: Arc<Vec<Hit>> =
        Arc::new(job.hits.iter().filter(|h| h.core).chain(job.hits.iter().filter(|h| !h.core)).take(ROUND2_HITS).cloned().collect());
    board.say(
        LineKind::Stage,
        "Этап 2",
        format!("Прошли {winners} из {tried}. Лучшие {} — на {} {}", order.len(), all.len(), sites_word(all.len())),
    );
    let recipes: Vec<Recipe> = order.iter().map(|t| tested[*t].0.recipe.clone()).collect();
    let n = recipes.len() as f32;
    let mut done = 0f32;
    let scores = run_batch(job, &recipes, &all, ROUND2_REPEATS, &mut |_| false, &mut |_| {
        done += 1.0;
        board.percent = 0.75 + 0.25 * done / n;
        board.emit(None);
    })
    .await?;

    let mut ranked: Vec<(usize, Score)> =
        scores.into_iter().enumerate().filter_map(|(k, s)| s.map(|s| (k, s))).filter(|(_, s)| s.rate() >= 0.5).collect();
    ranked.sort_by(|(ka, sa), (kb, sb)| {
        sb.rate()
            .total_cmp(&sa.rate())
            .then(recipes[*ka].cost().cmp(&recipes[*kb].cost()))
            .then(sa.latency().cmp(&sb.latency()))
    });

    let covers = |s: &Score| -> Vec<usize> { (0..all.len()).filter(|i| s.passed[*i] == ROUND2_REPEATS).collect() };
    let mut chosen: Vec<usize> = Vec::new();
    let mut covered = vec![false; all.len()];
    if !ranked.is_empty() {
        chosen.push(0);
        for i in covers(&ranked[0].1) {
            covered[i] = true;
        }
    }
    while chosen.len() < KEEP {
        let next = ranked
            .iter()
            .enumerate()
            .filter(|(i, _)| !chosen.contains(i))
            .map(|(i, (_, s))| (i, covers(s).into_iter().filter(|c| !covered[*c]).count()))
            .filter(|(_, gain)| *gain > 0)
            .max_by_key(|(i, gain)| (*gain, std::cmp::Reverse(*i)));
        let Some((i, _)) = next else { break };
        for c in covers(&ranked[i].1) {
            covered[c] = true;
        }
        chosen.push(i);
    }
    for i in 0..ranked.len() {
        if chosen.len() >= KEEP {
            break;
        }
        if !chosen.contains(&i) {
            chosen.push(i);
        }
    }

    board.phase = Phase::Done;
    board.percent = 1.0;
    match chosen.first() {
        Some(best) => {
            let (k, score) = &ranked[*best];
            board.say(LineKind::Chosen, "Выбран", format!("{}, отклик {} мс", recipes[*k].label(), score.latency()));
            let spare = chosen.len() - 1;
            if spare > 0 {
                let text = format!("Ещё {spare} {} на случай изменений у провайдера", variants(spare));
                board.say(LineKind::Spare, "Запасные", text);
            }
        }
        None => board.say(LineKind::Problem, "Итог", "Лучшие варианты не прошли повторную проверку".into()),
    }

    let fixed = hosts_of(&all, (0..all.len()).filter(|i| covered[*i]));
    let unfixed = hosts_of(&job.hits, 0..job.hits.len()).into_iter().filter(|h| !fixed.contains(h)).collect();
    let recipes = chosen.into_iter().map(|i| recipes[ranked[i].0].clone()).collect();
    Ok(Found { recipes, fixed, unfixed, tried })
}

fn variants(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, h) if h != 11 => "вариант",
        (2..=4, h) if !(12..=14).contains(&h) => "варианта",
        _ => "вариантов",
    }
}

fn sites_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, h) if h != 11 => "сайте",
        _ => "сайтах",
    }
}
