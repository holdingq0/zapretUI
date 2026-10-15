use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rkn_core::config::Config;
use rkn_core::ipc::{
    Event, Kind, Learn, Level, LineKind, LogLine, Message, Phase, Request, SiteState, SiteStatus, State, Status, Update,
    UpdateState,
};
use rkn_core::memory::{NetMemory, now};
use rkn_core::paths;
use rkn_core::reserve::Reserve;
use rkn_core::sites::{SiteMode, Sites, normalize_host};
use rkn_engine::Winws2;
use rkn_probe::Verdict;
use rkn_search::View;
use tokio::sync::{broadcast, mpsc, watch};

use crate::netid::{self, Network};
use crate::procs;
use crate::targets;
use crate::tuner::{self, Job, Learned, Probed};
use crate::update::{self, Ask};
use crate::engine;

const EVENTS_KEPT: usize = 500;
const MAX_CRASHES: u32 = 5;
const NET_POLL: Duration = Duration::from_secs(10);
const NET_SETTLE: Duration = Duration::from_secs(15);
const OFFLINE_SETTLE: Duration = Duration::from_secs(120);
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const RELEARN_AFTER: Duration = Duration::from_secs(30 * 60);
const LEARN_LOG: usize = 16;
const DIRECT_TTL: u64 = 24 * 3600;
const RESTART_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
enum Problem {
    Missing(String),
    Failed(String),
}

pub struct LearnCtx {
    pub network: Network,
    pub engine_dir: PathBuf,
    pub extra: Vec<String>,
    /// The strategies that worked here before: a new search tries them first.
    pub seed: Reserve,
}

struct Inner {
    config: Config,
    sites: Sites,
    network: Network,
    memory: Option<NetMemory>,
    engine: Winws2,
    planned: bool,
    problem: Option<Problem>,
    crashes: u32,
    retry_at: Option<Instant>,
    last_error: Option<String>,
    hosts: BTreeSet<String>,
    learning: Option<String>,
    learn: Option<Box<Learn>>,
    learn_reason: Option<String>,
    last_learn: Option<Instant>,
    checking: bool,
    whitelist: bool,
    checks: BTreeMap<String, Verdict>,
    net_polled: Instant,
    net_pending: Option<(Network, Instant)>,
    published: Instant,
    unpublished: bool,
    verifying: BTreeSet<String>,
    restart_at: Option<Instant>,
    procs: BTreeSet<String>,
    has_probed: bool,
    has_direct: bool,
    update: Update,
    updating: bool,
}

impl Inner {
    fn reserve(&self) -> Reserve {
        self.memory.as_ref().map(|m| m.reserve.clone()).unwrap_or_default()
    }

    fn is_direct(&self, host: &str) -> bool {
        self.memory.as_ref().is_some_and(|m| fresh_direct(m).any(|d| targets::within(host, d)))
    }

    fn confirmed(&self, host: &str) -> bool {
        let hit = |entry: &String| targets::expand(entry).iter().any(|d| targets::within(host, d));
        self.memory.as_ref().is_some_and(|m| m.blocked.iter().any(hit))
            || self.sites.modes.iter().any(|(h, m)| *m == SiteMode::Always && hit(h))
    }

    fn extra_hosts(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .sites
            .modes
            .iter()
            .filter(|(_, m)| **m == SiteMode::Always)
            .map(|(h, _)| h.clone())
            .collect();
        if let Some(memory) = &self.memory {
            out.extend(memory.blocked.iter().chain(&memory.unfixed).cloned());
        }
        out.sort();
        out.dedup();
        out
    }

    fn covered(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| targets::within(host, h)) || self.confirmed(host)
    }

    /// Discord is blocked here, so its calls need the voice profile as well.
    fn voice(&self) -> bool {
        let discord = targets::by_name("Discord").map(targets::Service::id).unwrap_or_default();
        !discord.is_empty() && self.confirmed(&discord) && self.sites.mode(&discord) != SiteMode::Never
    }

    fn plan(&self) -> Option<rkn_engine::Plan> {
        engine::build_plan(&self.sites, &self.reserve(), self.has_probed, self.has_direct, self.voice())
    }

    /// The last search on this network ended long enough ago, across restarts of the service too.
    fn learn_due(&self) -> bool {
        let recent = self.last_learn.is_some_and(|at| at.elapsed() < RELEARN_AFTER);
        let saved = self.memory.as_ref().is_some_and(|m| now().saturating_sub(m.learned_at) < RELEARN_AFTER.as_secs());
        !recent && !saved
    }
}

pub struct Hub {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<Message>,
    jobs: mpsc::UnboundedSender<Job>,
    verify: mpsc::UnboundedSender<String>,
    updates: mpsc::UnboundedSender<Ask>,
    moved: watch::Sender<u64>,
    stop: watch::Sender<u64>,
    quit: watch::Sender<bool>,
}

fn events_file() -> PathBuf {
    paths::state_dir().join("events.jsonl")
}

fn read_autohostlist() -> BTreeSet<String> {
    std::fs::read_to_string(paths::autohostlist_file())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// The event log is for reading by hand: keep its last lines only.
fn trim_events() {
    let text = std::fs::read_to_string(events_file()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > 2 * EVENTS_KEPT {
        let body: String = lines[lines.len() - EVENTS_KEPT..].iter().map(|l| format!("{l}\n")).collect();
        let _ = paths::write_atomic(&events_file(), &body);
    }
}

fn touch(path: PathBuf) {
    if !path.exists() {
        let _ = paths::write_atomic(&path, "");
    }
}

fn fresh_direct(memory: &NetMemory) -> impl Iterator<Item = &String> {
    let now = now();
    memory.direct.iter().filter(move |(_, at)| now.saturating_sub(**at) < DIRECT_TTL).map(|(h, _)| h)
}

fn sync_lists(inner: &mut Inner) {
    let memory = inner.memory.as_ref();
    let mut probed: Vec<String> = memory.map(|m| m.blocked.iter().flat_map(|h| targets::expand(h)).collect()).unwrap_or_default();
    let mut direct: Vec<String> = memory.map(|m| fresh_direct(m).cloned().collect()).unwrap_or_default();
    probed.sort();
    probed.dedup();
    direct.sort();
    direct.dedup();
    inner.has_probed = !probed.is_empty();
    inner.has_direct = !direct.is_empty();
    for (path, list) in [(paths::probed_file(), probed), (paths::direct_file(), direct)] {
        let body: String = list.iter().map(|h| format!("{h}\n")).collect();
        if let Err(e) = paths::write_atomic(&path, &body) {
            tracing::warn!("{}: {e}", path.display());
        }
    }
}

fn drop_from_autohostlist(hosts: &[String]) {
    let path = paths::autohostlist_file();
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let body: String = text
        .lines()
        .filter(|l| !hosts.iter().any(|h| h == l.trim()))
        .map(|l| format!("{l}\n"))
        .collect();
    if let Err(e) = paths::write_atomic(&path, &body) {
        tracing::warn!("autohostlist: {e}");
    }
}

fn unique(names: impl IntoIterator<Item = String>) -> String {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out.join(", ")
}

fn network_label(network: &Network) -> String {
    if network.name.is_empty() { "текущая сеть".into() } else { format!("сеть «{}»", network.name) }
}

impl Hub {
    pub fn new(jobs: mpsc::UnboundedSender<Job>, verify: mpsc::UnboundedSender<String>, updates: mpsc::UnboundedSender<Ask>) -> Self {
        let _ = std::fs::create_dir_all(paths::state_dir());
        trim_events();
        let (tx, _) = broadcast::channel(256);
        let found = netid::current();
        tracing::info!(found = ?found, "network");
        let network = found.filter(|n| n.internet).unwrap_or_else(Network::unknown);
        let hub = Self {
            inner: Mutex::new(Inner {
                config: Config::load(),
                sites: Sites::load(),
                network: network.clone(),
                memory: None,
                engine: Winws2::new(paths::engine_dir()),
                planned: false,
                problem: None,
                crashes: 0,
                retry_at: None,
                last_error: None,
                hosts: read_autohostlist(),
                learning: None,
                learn: None,
                learn_reason: None,
                last_learn: None,
                checking: false,
                whitelist: false,
                checks: BTreeMap::new(),
                net_polled: Instant::now(),
                net_pending: None,
                published: Instant::now(),
                unpublished: false,
                verifying: BTreeSet::new(),
                restart_at: None,
                procs: BTreeSet::new(),
                has_probed: false,
                has_direct: false,
                // Not "off" for the first moments of a fresh start: the window would say updates are disabled.
                update: Update {
                    state: if update::enabled() { UpdateState::Idle } else { UpdateState::Off },
                    ..Update::default()
                },
                updating: false,
            }),
            tx,
            jobs,
            verify,
            updates,
            moved: watch::channel(0).0,
            stop: watch::channel(0).0,
            quit: watch::channel(false).0,
        };
        {
            let mut inner = hub.lock();
            // Only with the network known: at boot before Wi-Fi is up it is not, and home must not go.
            if !inner.network.is_unknown() {
                rkn_core::memory::prune(&inner.network.id, Duration::from_secs(365 * 24 * 3600));
            }
            hub.load_memory(&mut inner);
            sync_lists(&mut inner);
            if inner.config.enabled && !inner.network.is_unknown() {
                let job = if inner.memory.is_none() { Job::Learn } else { Job::Check };
                let _ = hub.jobs.send(job);
            }
            let pending: Vec<String> =
                inner.hosts.iter().filter(|h| !inner.confirmed(h) && !inner.is_direct(h)).cloned().collect();
            for host in pending {
                hub.request_verify(&mut inner, host);
            }
        }
        hub
    }

    fn request_verify(&self, inner: &mut Inner, host: String) {
        if inner.memory.is_some() && inner.verifying.insert(host.clone()) {
            let _ = self.verify.send(host);
        }
    }

    pub fn verified(&self, probes: Vec<Probed>) {
        let mut inner = self.lock();
        let mut dirty = false;
        let mut removed: Vec<String> = Vec::new();
        let mut events: Vec<Event> = Vec::new();
        for probe in probes {
            let host = probe.target.host.clone();
            inner.verifying.remove(&host);
            let Some(memory) = inner.memory.as_mut() else { continue };
            let verdict = probe.outcome.verdict;
            if verdict.fixable() {
                let service = targets::service_of(&host).map(|_| targets::group(&host));
                let entry = service.as_ref().map_or_else(|| host.clone(), |g| g.id.clone());
                if !memory.blocked.contains(&entry) {
                    memory.blocked.push(entry.clone());
                    memory.blocked.sort();
                    dirty = true;
                    let text = match &service {
                        Some(group) => format!("{}: провайдер режет. Весь сервис идёт через обход", group.name),
                        None => format!("{host}: не открывается напрямую. Идёт через обход"),
                    };
                    events.push(Event::new(Level::Info, text).host(entry));
                }
            } else if verdict != Verdict::Failed {
                tracing::info!(host, ?verdict, "autohostlist entry opens directly, excluded");
                memory.direct.insert(host.clone(), now());
                removed.push(host);
                dirty = true;
            }
        }
        if dirty {
            if let Some(memory) = &inner.memory {
                if let Err(e) = memory.save(&inner.network.id) {
                    tracing::warn!("network memory: {e:#}");
                }
            }
            sync_lists(&mut inner);
            if !removed.is_empty() {
                drop_from_autohostlist(&removed);
                inner.hosts.retain(|h| !removed.contains(h));
            }
            for event in events {
                self.emit(event);
            }
            inner.restart_at.get_or_insert(Instant::now() + RESTART_DELAY);
        }
        self.publish(&mut inner);
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn load_memory(&self, inner: &mut Inner) {
        inner.memory = match NetMemory::load(&inner.network.id) {
            Ok(memory) => memory,
            Err(e) => {
                self.emit(Event::new(Level::Warn, format!("Не удалось прочитать память сети: {e}")));
                None
            }
        };
    }

    pub fn subscribe(&self) -> (Status, broadcast::Receiver<Message>) {
        let mut inner = self.lock();
        let rx = self.tx.subscribe();
        (status(&mut inner), rx)
    }

    fn emit(&self, event: Event) {
        match event.level {
            Level::Info => tracing::info!("{}", event.text),
            Level::Warn => tracing::warn!("{}", event.text),
            Level::Error => tracing::error!("{}", event.text),
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(events_file()) {
            let _ = f.write_all(rkn_core::ipc::encode(&event).as_bytes());
        }
        let _ = self.tx.send(Message::Event(event));
    }

    fn publish(&self, inner: &mut Inner) {
        inner.published = Instant::now();
        inner.unpublished = false;
        let _ = self.tx.send(Message::Status(Box::new(status(inner))));
    }

    // Learning reports progress on every trial; the UI only needs a few frames per second.
    fn publish_progress(&self, inner: &mut Inner) {
        if inner.published.elapsed() < PROGRESS_EVERY {
            inner.unpublished = true;
        } else {
            self.publish(inner);
        }
    }

    pub fn network_moves(&self) -> watch::Receiver<u64> {
        self.moved.subscribe()
    }

    /// Fires when a portable copy has to exit: its window closed or a new version took over.
    pub fn quits(&self) -> watch::Receiver<bool> {
        self.quit.subscribe()
    }

    pub fn quit(&self) {
        self.quit.send_replace(true);
    }

    pub fn stops(&self) -> watch::Receiver<u64> {
        self.stop.subscribe()
    }

    fn stop_learning(&self, inner: &Inner) -> bool {
        if inner.learning.is_none() {
            return false;
        }
        self.stop.send_modify(|n| *n += 1);
        true
    }

    pub fn note(&self, level: Level, text: String) {
        self.emit(Event::new(level, text));
    }

    pub fn apply(&self) {
        let mut inner = self.lock();
        self.apply_locked(&mut inner);
        self.publish(&mut inner);
    }

    fn apply_locked(&self, inner: &mut Inner) {
        inner.engine.stop();
        inner.retry_at = None;
        inner.crashes = 0;
        inner.last_error = None;
        let was = inner.problem.take();
        if !inner.config.enabled {
            engine::unload_driver();
            return;
        }
        inner.restart_at = None;
        let Some(plan) = inner.plan() else {
            inner.planned = false;
            engine::unload_driver();
            return;
        };
        inner.planned = true;
        if !inner.engine.installed() {
            inner.problem = Some(Problem::Missing("Движок не установлен. Переустановите zapretUI".into()));
        } else if let Err(e) = engine::verify(inner.engine.dir()) {
            inner.problem = Some(Problem::Failed(format!("Файлы движка повреждены: {e:#}. Переустановите zapretUI")));
        } else {
            touch(paths::autohostlist_file());
            // Starting waits for the driver; the window shows "Запуск" meanwhile instead of the old state.
            self.publish(inner);
            if let Err(e) = inner.engine.start(&plan) {
                let text = format!("{e:#}");
                self.emit(Event::new(Level::Warn, format!("winws2 не запустился: {text}. Повтор через 2 с")));
                inner.last_error = Some(text);
                inner.crashes = 1;
                inner.retry_at = Some(Instant::now() + Duration::from_secs(2));
            }
        }
        if inner.problem != was {
            if let Some(Problem::Missing(text) | Problem::Failed(text)) = inner.problem.clone() {
                self.emit(Event::new(Level::Error, text));
            }
        }
    }

    fn poll_network(&self) {
        {
            let mut inner = self.lock();
            if inner.net_polled.elapsed() < NET_POLL {
                return;
            }
            inner.net_polled = Instant::now();
        }
        let found = netid::current();
        let mut inner = self.lock();
        // A network that drops out for a moment or loses internet is not a new network: keep the current one.
        let Some(network) = found else {
            inner.net_pending = None;
            return;
        };
        if network.id == inner.network.id {
            inner.net_pending = None;
            if (&network.name, network.link) != (&inner.network.name, inner.network.link) {
                inner.network.name = network.name;
                inner.network.link = network.link;
                self.publish(&mut inner);
            }
            return;
        }
        let since = match &inner.net_pending {
            Some((pending, since)) if pending.id == network.id => *since,
            _ => {
                inner.net_pending = Some((network.clone(), Instant::now()));
                Instant::now()
            }
        };
        let settle = if network.internet { NET_SETTLE } else { OFFLINE_SETTLE };
        let first = inner.network.is_unknown() && network.internet;
        if !first && since.elapsed() < settle {
            return;
        }
        inner.net_pending = None;
        inner.network = network;
        self.moved.send_modify(|n| *n += 1);
        inner.checks.clear();
        inner.learn = None;
        inner.whitelist = false;
        inner.last_learn = None;
        inner.verifying.clear();
        self.load_memory(&mut inner);
        sync_lists(&mut inner);
        let label = network_label(&inner.network);
        let text = match &inner.memory {
            Some(_) => format!("Подключена {label}. Способ обхода уже известен"),
            None => format!("Подключена новая {label}"),
        };
        self.emit(Event::new(Level::Info, text));
        self.apply_locked(&mut inner);
        if inner.config.enabled {
            let job = if inner.memory.is_none() { Job::Learn } else { Job::Check };
            let _ = self.jobs.send(job);
        }
        self.publish(&mut inner);
    }

    pub fn tick(&self) {
        self.poll_network();
        let all = procs::running();
        let mut inner = self.lock();
        let mut changed = false;

        let watched: BTreeSet<String> = targets::games()
            .flat_map(|s| s.processes.iter().map(|p| p.to_ascii_lowercase()))
            .filter(|p| all.contains(p))
            .collect();
        if watched != inner.procs {
            inner.procs = watched;
            changed = true;
        }

        let hosts = read_autohostlist();
        let added: Vec<String> = hosts.difference(&inner.hosts).cloned().collect();
        if hosts != inner.hosts {
            inner.hosts = hosts;
            changed = true;
        }
        for host in added {
            if !inner.confirmed(&host) && !inner.is_direct(&host) {
                self.request_verify(&mut inner, host);
            }
        }
        if inner.restart_at.is_some_and(|at| Instant::now() >= at) {
            self.apply_locked(&mut inner);
            changed = true;
        }

        let should_run = inner.config.enabled && inner.planned && inner.problem.is_none() && !inner.updating;
        if should_run && inner.retry_at.is_none() && !inner.engine.running() {
            inner.crashes += 1;
            changed = true;
            if inner.crashes > MAX_CRASHES {
                let text = match &inner.last_error {
                    Some(e) => format!("winws2 не запускается: {e}"),
                    None => "winws2 завершается сразу после запуска".into(),
                };
                inner.problem = Some(Problem::Failed(text.clone()));
                self.emit(Event::new(Level::Error, format!("{text}. Перезапуск остановлен")));
            } else {
                let delay = Duration::from_secs(2u64.pow(inner.crashes));
                inner.retry_at = Some(Instant::now() + delay);
                let text = match &inner.last_error {
                    Some(e) => format!("winws2 не запустился: {e}. Повтор через {} с", delay.as_secs()),
                    None => format!("winws2 неожиданно завершился. Перезапуск через {} с", delay.as_secs()),
                };
                self.emit(Event::new(Level::Warn, text));
            }
        }
        if inner.retry_at.is_some_and(|at| Instant::now() >= at) {
            inner.retry_at = None;
            changed = true;
            if let Err(e) = engine::verify(inner.engine.dir()) {
                inner.engine.stop();
                let text = format!("Файлы движка повреждены: {e:#}. Переустановите zapretUI");
                self.emit(Event::new(Level::Error, text.clone()));
                inner.problem = Some(Problem::Failed(text));
            } else if let Some(plan) = inner.plan() {
                match inner.engine.start(&plan) {
                    Ok(_) => inner.last_error = None,
                    Err(e) => inner.last_error = Some(format!("{e:#}")),
                }
            }
        }
        if inner.crashes > 0 && inner.retry_at.is_none() && inner.engine.running() {
            inner.crashes = inner.crashes.saturating_sub(1);
        }

        if changed || inner.unpublished {
            self.publish(&mut inner);
        }
    }

    pub fn set_update(&self, state: UpdateState, version: Option<String>, error: Option<String>) {
        let mut inner = self.lock();
        let checked = matches!(state, UpdateState::Latest | UpdateState::Available).then(now);
        inner.update = Update { state, version, error, checked_at: checked.or(inner.update.checked_at) };
        self.publish(&mut inner);
    }

    pub fn auto_update(&self) -> bool {
        self.lock().config.auto_update
    }

    /// A game from the catalog is running or a search is going on: not the time to restart.
    pub fn busy(&self) -> bool {
        let inner = self.lock();
        inner.learning.is_some() || !inner.procs.is_empty()
    }

    /// Everything that holds engine files stops: the bypass and a search that may be running.
    pub fn stop_for_update(&self) {
        let mut inner = self.lock();
        inner.updating = true;
        self.stop_learning(&inner);
        inner.engine.stop();
        self.publish(&mut inner);
    }

    pub fn resume_after_update(&self) {
        let mut inner = self.lock();
        inner.updating = false;
        self.apply_locked(&mut inner);
        self.publish(&mut inner);
    }

    pub fn shutdown(&self) {
        let mut inner = self.lock();
        inner.engine.stop();
        engine::unload_driver();
    }

    pub fn check_every(&self) -> Option<Duration> {
        let inner = self.lock();
        inner.config.enabled
            .then(|| Duration::from_secs(u64::from(inner.config.check_interval_min.max(2)) * 60))
    }

    pub fn begin_learning(&self) -> Option<LearnCtx> {
        let mut inner = self.lock();
        if !inner.config.enabled || inner.learning.is_some() || inner.updating {
            return None;
        }
        let problem = if !inner.engine.installed() {
            Some(Problem::Missing("Движок не установлен. Переустановите zapretUI".into()))
        } else {
            engine::verify(inner.engine.dir())
                .err()
                .map(|e| Problem::Failed(format!("Файлы движка повреждены: {e:#}. Переустановите zapretUI")))
        };
        if let Some(problem) = problem {
            if inner.problem.as_ref() != Some(&problem) {
                let (Problem::Missing(text) | Problem::Failed(text)) = &problem;
                self.emit(Event::new(Level::Error, text.clone()));
                inner.problem = Some(problem);
                self.publish(&mut inner);
            }
            return None;
        }
        let label = network_label(&inner.network);
        let first = inner.memory.is_none();
        let reason = inner.learn_reason.take();
        let text = match &reason {
            _ if first => format!("Подбираю способ обхода: {label}"),
            Some(reason) => format!("Переподбираю способ обхода. {reason}"),
            None => "Переподбираю способ обхода".into(),
        };
        self.emit(Event::new(Level::Info, text));
        inner.learning = Some(phase_text(Phase::Probing).into());
        inner.learn = Some(Box::new(Learn { phase: Phase::Probing, percent: 0, log: Vec::new() }));
        self.publish(&mut inner);
        Some(LearnCtx {
            network: inner.network.clone(),
            engine_dir: inner.engine.dir().to_path_buf(),
            extra: inner.extra_hosts(),
            seed: inner.reserve(),
        })
    }

    pub fn learn_probed(&self, line: Option<LogLine>, done: usize, total: usize) {
        let mut inner = self.lock();
        let Some(learn) = inner.learn.as_mut() else { return };
        learn.percent = (done * 10 / total.max(1)) as u8;
        if let Some(line) = line {
            push_line(learn, line);
        }
        self.publish_progress(&mut inner);
    }

    pub fn learn_view(&self, view: View, class_no: usize, classes: usize) {
        let mut inner = self.lock();
        let last = class_no + 1 == classes;
        let phase = match view.phase {
            Phase::Done if !last => Phase::Round2,
            other => other,
        };
        let Some(learn) = inner.learn.as_mut() else { return };
        let share = (class_no as f32 + view.percent) / classes.max(1) as f32;
        learn.percent = (10.0 + 90.0 * share).clamp(0.0, 100.0) as u8;
        learn.phase = phase;
        if let Some(line) = view.line {
            push_line(learn, line);
        }
        inner.learning = Some(phase_text(phase).into());
        self.publish_progress(&mut inner);
    }

    pub fn abandon_learning(&self) {
        let mut inner = self.lock();
        inner.learning = None;
        inner.learn = None;
        self.publish(&mut inner);
    }

    /// The user stopped learning (button or pause). The old reserve keeps working and the
    /// background check will not start a new search on its own for a while.
    pub fn learning_stopped(&self) {
        let mut inner = self.lock();
        inner.learning = None;
        inner.learn = None;
        inner.last_learn = Some(Instant::now());
        if inner.config.enabled {
            let text = if inner.memory.is_some() {
                "Подбор остановлен. Работает прежний способ обхода"
            } else {
                "Подбор остановлен. Для этой сети способ обхода не подобран"
            };
            self.emit(Event::new(Level::Info, text));
        }
        self.publish(&mut inner);
    }

    /// Trial instances of winws2 are gone; on pause the driver may unload now.
    pub fn trials_gone(&self) {
        let inner = self.lock();
        if !inner.config.enabled {
            engine::unload_driver();
        }
    }

    pub fn finish_learning(&self, ctx: &LearnCtx, learned: Learned) {
        let mut inner = self.lock();
        inner.learning = None;
        if let Some(learn) = inner.learn.as_mut() {
            learn.percent = 100;
            learn.phase = if learned.error.is_some() { Phase::Failed } else { Phase::Done };
            if learned.whitelist {
                let text = "Открываются только российские сайты. Обход здесь не поможет".into();
                push_line(learn, LogLine { kind: LineKind::Problem, key: "Белый список".into(), text });
            } else if learned.reserve.is_empty() && learned.unfixed.is_empty() && learned.error.is_none() {
                let text = "Блокировок по DPI нет, обход не нужен".into();
                push_line(learn, LogLine { kind: LineKind::Stage, key: "Итог".into(), text });
            }
        }
        inner.last_learn = Some(Instant::now());
        let current = inner.network.id == ctx.network.id;
        if current {
            inner.whitelist = learned.whitelist;
            inner.checks = learned
                .probes
                .iter()
                .filter(|p| !p.control && p.target.tls && (learned.error.is_some() || !p.outcome.verdict.fixable()))
                .map(|p| (p.target.host.clone(), p.outcome.verdict))
                .collect();
        }
        if let Some(e) = &learned.error {
            self.emit(Event::new(Level::Error, format!("Подбор способа обхода не удался: {e}")));
            self.publish(&mut inner);
            return;
        }
        if learned.whitelist {
            let text = "Похоже на белый список оператора: открываются только разрешённые сайты. Обход здесь не поможет";
            self.emit(Event::new(Level::Warn, text));
            self.publish(&mut inner);
            return;
        }

        let pick = |verdict: Verdict| -> Vec<&Probed> {
            learned.probes.iter().filter(|p| !p.control && p.outcome.verdict == verdict).collect()
        };
        let name_of = |host: &String| targets::name(host);
        let list = |hosts: &[String]| unique(hosts.iter().map(name_of));
        let secs = learned.took.as_secs().max(1);
        if !learned.reserve.is_empty() {
            let best = learned.reserve.tls.first().or(learned.reserve.http.first()).map(|r| r.label()).unwrap_or_default();
            let text = format!(
                "Способ обхода подобран за {secs} с, проверено вариантов: {}. Лучший: {best}. Через обход: {}",
                learned.tried,
                list(&learned.blocked)
            );
            self.emit(Event::new(Level::Info, text));
        } else if learned.unfixed.is_empty() {
            self.emit(Event::new(Level::Info, "Блокировок по DPI не найдено. Обход не нужен"));
        }
        if !learned.unfixed.is_empty() {
            let text = format!("Не удалось подобрать способ обхода для: {}", list(&learned.unfixed));
            self.emit(Event::new(Level::Warn, text));
        }
        let by_ip = pick(Verdict::IpBlocked);
        if !by_ip.is_empty() {
            let text = format!("{}: блокировка по IP-адресу, обход DPI здесь не поможет", tuner::names(&by_ip));
            self.emit(Event::new(Level::Warn, text));
        }
        let dns = pick(Verdict::DnsSpoofed);
        if !dns.is_empty() {
            let text = format!("{}: провайдер подменяет DNS. Поможет DoH в настройках Windows", tuner::names(&dns));
            self.emit(Event::new(Level::Warn, text));
        }

        let blocked = &learned.blocked;
        let direct: BTreeMap<String, u64> = if current {
            inner
                .memory
                .as_ref()
                .map(|m| {
                    m.direct
                        .iter()
                        .filter(|(host, _)| !blocked.iter().any(|b| targets::within(host, b)))
                        .map(|(host, at)| (host.clone(), *at))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Default::default()
        };
        let memory = NetMemory {
            name: ctx.network.name.clone(),
            learned_at: now(),
            blocked: learned.blocked,
            unfixed: learned.unfixed,
            reserve: learned.reserve,
            direct,
        };
        if let Err(e) = memory.save(&ctx.network.id) {
            self.emit(Event::new(Level::Warn, format!("Не удалось сохранить способ обхода: {e:#}")));
        }
        if current {
            inner.memory = Some(memory);
            sync_lists(&mut inner);
            self.apply_locked(&mut inner);
            let _ = self.jobs.send(Job::Check);
        }
        self.publish(&mut inner);
    }

    pub fn begin_check(&self) -> Option<Vec<String>> {
        let mut inner = self.lock();
        if inner.learning.is_some() || inner.checking {
            return None;
        }
        inner.checking = true;
        self.publish(&mut inner);
        Some(inner.extra_hosts())
    }

    /// Returns the catalog services that stopped opening through the bypass, when a new search
    /// would be due. The caller checks them again before blaming the provider.
    pub fn finish_check(&self, probes: Vec<Probed>) -> Vec<Probed> {
        let mut inner = self.lock();
        inner.checking = false;
        let was_whitelist = inner.whitelist;
        inner.whitelist = tuner::whitelist(&probes);
        if inner.whitelist && !was_whitelist {
            let text = "Белый список оператора: открываются только российские сайты. Обход здесь не поможет";
            self.emit(Event::new(Level::Warn, text));
        }
        let sites: Vec<&Probed> = probes.iter().filter(|p| !p.control && p.target.tls).collect();
        inner.checks = sites.iter().map(|p| (p.target.host.clone(), p.outcome.verdict)).collect();

        let working = sites.iter().filter(|p| matches!(p.outcome.verdict, Verdict::Ok | Verdict::DnsSpoofed)).count();
        let failing: Vec<&Probed> = sites.iter().copied().filter(|p| p.outcome.verdict.fixable()).collect();
        if !failing.is_empty() {
            let text = format!("Проверка: открываются {working} из {}. Не открываются: {}", sites.len(), tuner::names(&failing));
            self.emit(Event::new(Level::Warn, text));
        }
        let suspects = if tuner::online(&probes) && watching(&inner) {
            let unfixed: Vec<String> = inner.memory.as_ref().map(|m| m.unfixed.clone()).unwrap_or_default();
            // Sites outside the catalog never start a search: one slow site is no reason to retune the rest.
            failing
                .into_iter()
                .filter(|p| !unfixed.contains(&p.target.host) && targets::service_of(&p.target.host).is_some())
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        self.publish(&mut inner);
        suspects
    }


    /// A repeated probe of sites that failed the check: its results replace the old ones.
    pub fn rechecked(&self, probes: &[Probed]) {
        let mut inner = self.lock();
        for p in probes.iter().filter(|p| !p.control && p.target.tls) {
            inner.checks.insert(p.target.host.clone(), p.outcome.verdict);
        }
        self.publish(&mut inner);
    }

    /// The suspects failed every repeated check too. A new search starts if one is due; when the
    /// strategy was found just now and still does not open them, a search cannot help them: they
    /// are remembered as not fixable and stop starting searches.
    pub fn confirm_relearn(&self, still: &[Probed]) -> bool {
        let mut inner = self.lock();
        if still.is_empty() || !watching(&inner) {
            return false;
        }
        let refs: Vec<&Probed> = still.iter().collect();
        let fresh = inner.memory.as_ref().is_some_and(|m| now().saturating_sub(m.learned_at) < RELEARN_AFTER.as_secs());
        if fresh {
            let network = inner.network.id.clone();
            if let Some(memory) = inner.memory.as_mut() {
                for p in &refs {
                    if !memory.unfixed.contains(&p.target.host) {
                        memory.unfixed.push(p.target.host.clone());
                    }
                }
                memory.unfixed.sort();
                if let Err(e) = memory.save(&network) {
                    tracing::warn!("network memory: {e:#}");
                }
            }
            let text = format!(
                "{}: подобранный способ обхода не помогает, похоже на блокировку по адресу. Переподбор из-за этого не запускается",
                tuner::names(&refs)
            );
            self.emit(Event::new(Level::Warn, text));
            self.publish(&mut inner);
            return false;
        }
        // A search of its own never starts during a game: trial engines and bursts of probes are
        // not worth a lag spike. The first check after the game picks it up.
        if !inner.learn_due() || !inner.procs.is_empty() {
            return false;
        }
        let names: Vec<String> = refs.iter().map(|p| p.name.clone()).collect();
        let others = inner
            .checks
            .iter()
            .any(|(host, v)| matches!(v, Verdict::Ok) && !names.contains(&targets::name(host)));
        let verb = if names.iter().collect::<BTreeSet<_>>().len() == 1 { "перестал" } else { "перестали" };
        let rest = if others { ". Остальные сайты работают как обычно" } else { "" };
        inner.learn_reason = Some(format!("{} {verb} открываться{rest}", tuner::names(&refs)));
        true
    }

    pub fn handle(&self, request: Request) -> Message {
        let mut inner = self.lock();
        let reply = match request {
            Request::Status | Request::Subscribe => return Message::Status(Box::new(status(&mut inner))),
            Request::GetConfig => return Message::Config(inner.config.clone()),
            Request::SetEnabled { enabled } => {
                if inner.config.enabled != enabled {
                    inner.config.enabled = enabled;
                    let text = match enabled {
                        true => "Обход включён",
                        false if self.stop_learning(&inner) => "Обход поставлен на паузу, подбор остановлен",
                        false => "Обход поставлен на паузу. Драйвер выгружен",
                    };
                    self.apply_locked(&mut inner);
                    self.emit(Event::new(Level::Info, text));
                    if enabled {
                        let job = if inner.memory.is_none() { Job::Learn } else { Job::Check };
                        let _ = self.jobs.send(job);
                    }
                }
                inner.config.save()
            }
            Request::SetConfig { mut config } => {
                config.check_interval_min = config.check_interval_min.clamp(2, 60);
                // Pause has its own request; a settings screen opened earlier must not undo it.
                config.enabled = inner.config.enabled;
                inner.config = config;
                inner.config.save()
            }
            Request::SetSiteMode { host, mode } => {
                let Some(host) = normalize_host(&host) else {
                    return Message::Error { message: "Это не похоже на адрес сайта".into() };
                };
                let group = targets::group(&host);
                let (host, name) = (group.id, group.name);
                if inner.sites.mode(&host) != mode {
                    inner.sites.set(host.clone(), mode);
                    let text = match mode {
                        SiteMode::Auto => format!("{name}: обход включается автоматически"),
                        SiteMode::Always => format!("{name}: всегда через обход"),
                        SiteMode::Never => format!("{name}: всегда напрямую"),
                    };
                    let saved = inner.sites.save(&targets::expand);
                    self.apply_locked(&mut inner);
                    self.emit(Event::new(Level::Info, text).host(host));
                    saved
                } else {
                    Ok(())
                }
            }
            Request::Relearn => {
                if inner.learning.is_some() {
                    self.emit(Event::new(Level::Info, "Подбор способа обхода уже идёт"));
                } else {
                    if !inner.config.enabled {
                        inner.config.enabled = true;
                        self.apply_locked(&mut inner);
                        self.emit(Event::new(Level::Info, "Обход включён для подбора"));
                    }
                    let _ = self.jobs.send(Job::Learn);
                }
                inner.config.save()
            }
            Request::StopLearning => {
                self.stop_learning(&inner);
                Ok(())
            }
            Request::Shutdown if paths::portable() => {
                self.quit();
                Ok(())
            }
            Request::Shutdown => return Message::Error { message: "Служба выключается только из системы".into() },
            Request::CheckUpdate | Request::InstallUpdate if !update::enabled() => {
                return Message::Error { message: "В этой сборке обновления выключены".into() };
            }
            Request::CheckUpdate => {
                let _ = self.updates.send(Ask::Check);
                Ok(())
            }
            Request::InstallUpdate => {
                let _ = self.updates.send(Ask::Install);
                Ok(())
            }
        };
        self.publish(&mut inner);
        match reply {
            Ok(()) => Message::Ok,
            Err(e) => Message::Error { message: format!("{e:#}") },
        }
    }
}

/// The background check may act on what it finds: the bypass is on and has a strategy to judge.
fn watching(inner: &Inner) -> bool {
    inner.config.enabled && !inner.whitelist && inner.learning.is_none() && inner.memory.is_some()
}

fn status(inner: &mut Inner) -> Status {
    let running = inner.engine.running();
    let mut unfixed = inner.memory.as_ref().map(|m| m.unfixed.clone()).unwrap_or_default();
    for (host, check) in &inner.checks {
        if check.fixable() && !inner.covered(host) && !unfixed.contains(host) {
            unfixed.push(host.clone());
        }
    }
    let (state, message) = match &inner.problem {
        _ if !inner.config.enabled => (State::Paused, None),
        Some(Problem::Missing(m)) => (State::EngineMissing, Some(m.clone())),
        Some(Problem::Failed(m)) => (State::Failed, Some(m.clone())),
        None if inner.learning.is_some() => (State::Learning, inner.learning.clone()),
        None if inner.whitelist => (State::Whitelist, None),
        None if inner.memory.is_none() => (State::Tuning, Some("Способы обхода для этой сети ещё не подобраны".into())),
        None if !inner.planned && !unfixed.is_empty() => (
            State::Tuning,
            Some(format!(
                "Не удалось подобрать способ обхода для: {}",
                unique(unfixed.iter().map(|h| targets::name(h)))
            )),
        ),
        None if !inner.planned || running => (State::Working, None),
        None => (State::Starting, None),
    };

    let mut hosts: BTreeSet<String> = inner.hosts.clone();
    hosts.extend(inner.sites.modes.keys().cloned());
    if let Some(memory) = &inner.memory {
        hosts.extend(memory.blocked.iter().chain(&memory.unfixed).cloned());
    }
    hosts.extend(inner.checks.iter().filter(|(_, v)| **v != Verdict::Ok).map(|(h, _)| h.clone()));
    hosts.retain(|h| !inner.is_direct(h) || inner.sites.mode(&targets::group(h).id) != SiteMode::Auto);

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for host in hosts {
        groups.entry(targets::group(&host).id).or_default().push(host);
    }

    let mut sites: Vec<SiteStatus> = Vec::new();
    for service in &targets::catalog().service {
        let id = service.id();
        let found = groups.remove(&id);
        if service.kind == Kind::Site && service.probe.is_empty() && found.is_none() {
            continue;
        }
        let mut members = found.unwrap_or_default();
        for host in &service.probe {
            if !members.contains(host) {
                members.push(host.clone());
            }
        }
        sites.push(row(inner, state, running, id, service.name.clone(), members, service));
    }
    // Hosts outside the catalog (the engine's own finds) still go through the bypass, but the window
    // lists only the services people know: a row for every flaky site would only confuse.

    Status {
        state,
        network: (!inner.network.name.is_empty()).then(|| inner.network.name.clone()),
        link: inner.network.link,
        sites,
        message,
        version: env!("CARGO_PKG_VERSION").into(),
        update: inner.update.clone(),
        portable: paths::portable(),
        learn: inner.learn.clone(),
    }
}

fn row(
    inner: &Inner,
    state: State,
    running: bool,
    id: String,
    name: String,
    members: Vec<String>,
    service: &targets::Service,
) -> SiteStatus {
    let mode = match inner.sites.mode(&id) {
        SiteMode::Auto => members.iter().map(|h| inner.sites.mode(h)).find(|m| *m != SiteMode::Auto).unwrap_or_default(),
        other => other,
    };
    let launcher = Some(service).filter(|s| s.probe.is_empty()).and_then(|s| s.launcher.as_deref()).and_then(targets::by_name);
    let probes: &[String] = match launcher {
        Some(l) => &l.probe,
        None => &members,
    };
    let check = probes.iter().filter_map(|h| inner.checks.get(h)).copied().min_by_key(|v| *v == Verdict::Ok);
    let covered = inner.covered(&id)
        || members.iter().any(|h| inner.covered(h))
        || launcher.is_some_and(|l| inner.covered(&l.id()));
    let site_state = {
        match (mode, state, check) {
            (SiteMode::Never, _, _) | (_, State::Paused, None) => SiteState::Direct,
            // A check keeps the last result on screen; only a service never checked waits for it.
            (_, _, None) if inner.checking && !(covered && state == State::Working && running) => SiteState::Checking,
            (_, _, Some(verdict)) => match verdict {
                // A restart of the engine (new hosts, a new strategy) takes a second: the rows stay as they were.
                Verdict::Ok if (running || state == State::Starting) && (covered || mode == SiteMode::Always) => SiteState::Bypassed,
                Verdict::Ok => SiteState::Direct,
                Verdict::DnsSpoofed => SiteState::DnsSpoofed,
                Verdict::IpBlocked => SiteState::IpBlocked,
                Verdict::Geoblocked => SiteState::Geoblocked,
                Verdict::Cut => SiteState::Partial,
                _ if state == State::Learning => SiteState::Tuning,
                _ => SiteState::Blocked,
            },
            (_, State::Working, None) if covered => SiteState::Bypassed,
            (_, State::Learning | State::Tuning, None) => SiteState::Tuning,
            (_, State::Starting, None) => SiteState::Checking,
            _ => SiteState::Unknown,
        }
    };
    let launched = service.processes.iter().any(|p| inner.procs.contains(&p.to_ascii_lowercase()));
    SiteStatus {
        id,
        name,
        state: site_state,
        mode,
        kind: service.kind,
        icon: service.icon.clone(),
        running: launched,
        via: mode != SiteMode::Never && (covered || mode == SiteMode::Always),
    }
}

fn push_line(learn: &mut Learn, line: LogLine) {
    learn.log.push(line);
    if learn.log.len() > LEARN_LOG {
        learn.log.remove(0);
    }
}

fn phase_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Probing => "Проверяю, какие сайты режет провайдер",
        Phase::Round1 => "Этап 1 из 2: проверяю варианты обхода",
        Phase::Round2 => "Этап 2 из 2: проверяю лучшие варианты на всех сайтах",
        Phase::Done => "Готово",
        Phase::Failed => "Подбор не удался",
    }
}
