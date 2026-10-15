use std::sync::Arc;
use std::time::{Duration, Instant};

use rkn_core::ipc::{Level, LineKind, LogLine};
use rkn_core::reserve::Reserve;
use rkn_probe::{Outcome, Target, Verdict};
use rkn_search::{Class, Hit};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::hub::{Hub, LearnCtx};
use crate::targets;

const PARALLEL: usize = 8;
const MAX_TARGETS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Learn,
    Check,
}

#[derive(Debug, Clone)]
pub struct Probed {
    pub target: Target,
    pub name: String,
    pub control: bool,
    pub outcome: Outcome,
}

#[derive(Debug, Default)]
pub struct Learned {
    pub reserve: Reserve,
    pub blocked: Vec<String>,
    pub unfixed: Vec<String>,
    pub whitelist: bool,
    pub probes: Vec<Probed>,
    pub tried: usize,
    pub took: Duration,
    pub error: Option<String>,
}

pub async fn run(hub: Arc<Hub>, mut jobs: mpsc::UnboundedReceiver<Job>) {
    loop {
        let next = match hub.check_every() {
            Some(every) => match tokio::time::timeout(every, jobs.recv()).await {
                Ok(job) => job,
                Err(_) => Some(Job::Check),
            },
            None => jobs.recv().await,
        };
        let Some(mut job) = next else { return };
        while let Ok(more) = jobs.try_recv() {
            job = match (job, more) {
                (Job::Learn, _) | (_, Job::Learn) => Job::Learn,
                (Job::Check, Job::Check) => Job::Check,
            };
        }
        match job {
            Job::Learn => learn(&hub).await,
            Job::Check => {
                if let Some(extra) = hub.begin_check() {
                    let probes = probe_all(targets_for(&extra, false), false, &mut |_, _| {}).await;
                    if hub.finish_check(probes) {
                        learn(&hub).await;
                    }
                }
            }
        }
    }
}

fn targets_for(extra: &[String], learning: bool) -> Vec<(Target, String, bool)> {
    let catalog = targets::catalog();
    let mut out: Vec<(Target, String, bool)> = Vec::new();
    for entry in &targets::sites() {
        if learning || !entry.http {
            out.push((entry.target(), entry.name.clone(), false));
        }
    }
    if learning {
        out.extend(catalog.control.iter().map(|e| (e.target(), e.name.clone(), true)));
    }
    for host in extra {
        let group = targets::group(host).id;
        if !out.iter().any(|(t, _, _)| t.tls && targets::group(&t.host).id == group) {
            out.push((Target::https(host.as_str()), targets::name(host), false));
        }
    }
    out.truncate(MAX_TARGETS);
    out
}

pub async fn probe_all(
    targets: Vec<(Target, String, bool)>,
    reserved: bool,
    on_done: &mut (dyn FnMut(&Probed, usize) + Send),
) -> Vec<Probed> {
    let gate = Arc::new(Semaphore::new(PARALLEL));
    let mut set = JoinSet::new();
    for (i, (target, name, control)) in targets.into_iter().enumerate() {
        let gate = gate.clone();
        set.spawn(async move {
            let _permit = gate.acquire_owned().await;
            let block = reserved.then(rkn_search::port_block);
            let outcome = rkn_probe::probe(&target, block.as_ref(), rkn_search::probe_opts()).await;
            (i, Probed { target, name, control, outcome })
        });
    }
    let mut out: Vec<(usize, Probed)> = Vec::new();
    while let Some(result) = set.join_next().await {
        if let Ok(item) = result {
            on_done(&item.1, out.len() + 1);
            out.push(item);
        }
    }
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, p)| p).collect()
}

pub fn whitelist(probes: &[Probed]) -> bool {
    let controls_ok = probes.iter().any(|p| p.control && p.outcome.verdict == Verdict::Ok);
    let sites: Vec<&Probed> = probes.iter().filter(|p| !p.control && p.target.tls).collect();
    let reachable = sites.iter().any(|p| matches!(p.outcome.verdict, Verdict::Ok | Verdict::DnsSpoofed));
    let ip_level = sites.iter().filter(|p| p.outcome.verdict == Verdict::IpBlocked).count();
    controls_ok && !sites.is_empty() && !reachable && ip_level * 2 >= sites.len()
}

fn diagnosis(probe: &Probed) -> Option<LogLine> {
    let outcome = &probe.outcome;
    let text = match outcome.verdict {
        Verdict::TlsBlocked => "Блокировка по имени сайта (TLS)".to_string(),
        Verdict::Cut if outcome.bytes == 0 => "Обрыв сразу после рукопожатия".to_string(),
        Verdict::Cut => format!("Обрыв загрузки на {} КБ", outcome.bytes.div_ceil(1024)),
        Verdict::HttpBlocked if outcome.detail.contains("заглушк") => "Перенаправление на заглушку провайдера".to_string(),
        Verdict::HttpBlocked => "HTTP-запрос глушится".to_string(),
        Verdict::IpBlocked => "Заблокирован по IP-адресу, пропущен".to_string(),
        Verdict::DnsSpoofed => "Подмена DNS, лечится DoH".to_string(),
        Verdict::Geoblocked => "Сервис закрыт для России".to_string(),
        _ => return None,
    };
    let host = probe.target.host.strip_prefix("www.").unwrap_or(&probe.target.host);
    let key = if probe.target.tls { host.to_string() } else { format!("http://{host}") };
    Some(LogLine { kind: LineKind::Site, key, text })
}

pub fn names(probes: &[&Probed]) -> String {
    let mut list: Vec<&str> = Vec::new();
    for probe in probes {
        if !list.contains(&probe.name.as_str()) {
            list.push(&probe.name);
        }
    }
    list.join(", ")
}

pub async fn verifier(hub: Arc<Hub>, mut hosts: mpsc::UnboundedReceiver<String>) {
    while let Some(first) = hosts.recv().await {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut batch = vec![first];
        while let Ok(more) = hosts.try_recv() {
            batch.push(more);
        }
        batch.sort();
        batch.dedup();
        let list = batch.iter().map(|h| (Target::https(h.as_str()), targets::name(h), false)).collect();
        let probes = probe_all(list, true, &mut |_, _| {}).await;
        hub.verified(probes);
    }
}

async fn learn(hub: &Arc<Hub>) {
    let mut moved = hub.network_moves();
    let mut stop = hub.stops();
    let Some(ctx) = hub.begin_learning() else { return };
    // Dropping the search stops its winws2 trials: the new network gets its own job.
    tokio::select! {
        result = learn_with(hub, &ctx) => hub.finish_learning(&ctx, result),
        _ = moved.changed() => {
            tracing::info!("network changed, learning abandoned");
            hub.abandon_learning();
        }
        _ = stop.changed() => {
            tracing::info!("learning stopped by the user");
            hub.learning_stopped();
            rkn_search::settle().await;
            hub.trials_gone();
        }
    }
}

async fn learn_with(hub: &Hub, ctx: &LearnCtx) -> Learned {
    let started = Instant::now();
    let targets = targets_for(&ctx.extra, true);
    let total = targets.len();
    let probes = probe_all(targets, true, &mut |probe, done| {
        let line = if probe.control { None } else { diagnosis(probe) };
        hub.learn_probed(line, done, total);
    })
    .await;
    let mut learned = Learned::default();
    if whitelist(&probes) {
        learned.whitelist = true;
        learned.probes = probes;
        return learned;
    }

    let hits = |tls: bool| -> Vec<Hit> {
        probes
            .iter()
            .filter(|p| !p.control && p.target.tls == tls && p.outcome.verdict.fixable())
            .filter_map(|p| Some(Hit { target: p.target.clone(), ip: p.outcome.ip?, verdict: p.outcome.verdict }))
            .collect()
    };
    let classes = [(Class::Tls, hits(true)), (Class::Http, hits(false))];
    let blocked: Vec<&Probed> = probes.iter().filter(|p| !p.control && p.outcome.verdict.fixable()).collect();
    if !blocked.is_empty() {
        hub.note(Level::Info, format!("Провайдер режет: {}. Подбираю способ обхода", names(&blocked)));
    }

    let classes: Vec<(Class, Vec<Hit>)> = classes.into_iter().filter(|(_, hits)| !hits.is_empty()).collect();
    let count = classes.len();
    for (class_no, (class, hits)) in classes.into_iter().enumerate() {
        let job = rkn_search::Job { class, hits, engine_dir: ctx.engine_dir.clone() };
        let progress = |view| hub.learn_view(view, class_no, count);
        let found = match rkn_search::search(&job, &progress).await {
            Ok(found) => found,
            Err(e) => {
                learned.error = Some(format!("{e:#}"));
                break;
            }
        };
        learned.tried += found.tried;
        learned.blocked.extend(found.fixed);
        learned.unfixed.extend(found.unfixed);
        match class {
            Class::Tls => learned.reserve.tls = found.recipes,
            Class::Http => learned.reserve.http = found.recipes,
        }
    }
    learned.blocked.sort();
    learned.blocked.dedup();
    learned.unfixed.sort();
    learned.unfixed.dedup();
    learned.unfixed.retain(|h| !learned.blocked.contains(h));
    learned.probes = probes;
    learned.took = started.elapsed();
    learned
}

pub fn cli_probe() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let probes = runtime.block_on(probe_all(targets_for(&[], true), true, &mut |_, _| {}));
    for p in &probes {
        let o = &p.outcome;
        let scheme = if p.target.tls { "https" } else { "http" };
        let ip = o.ip.map(|ip| ip.to_string()).unwrap_or_default();
        let ms = o.latency_ms.map(|ms| format!("{ms} мс")).unwrap_or_default();
        println!("{:<34} {:<16} {:<12} {:>7}  {}", format!("{scheme}://{}", p.target.host), ip, format!("{:?}", o.verdict), ms, o.detail);
    }
    if whitelist(&probes) {
        println!("\nПохоже на режим белых списков");
    }
    Ok(())
}
