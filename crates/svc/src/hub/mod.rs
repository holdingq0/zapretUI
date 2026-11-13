mod files;
mod learning;
mod requests;
mod runner;
mod status;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rkn_core::config::Config;
use rkn_core::ipc::{
    Event, Kind, Learn, Level, LineKind, LogLine, Message, Phase, Request, SiteState, SiteStatus, State, Status, SystemDns, Update,
    UpdateState,
};
use rkn_core::memory::{NetMemory, now};
use rkn_core::paths;
use rkn_core::reserve::Reserve;
use rkn_core::sites::{SiteMode, Sites, normalize_host};
use rkn_engine::{Plan, Winws2};
use rkn_probe::Verdict;
use rkn_search::View;
use tokio::sync::{broadcast, mpsc, watch};

use crate::netid::{self, Network};
use crate::procs;
use crate::targets;
use crate::tuner::{self, Job, Learned, Probed};
use crate::update::{self, Ask};
use crate::engine;

use files::*;
use runner::*;
use status::*;

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
/// An engine restarted after a crash counts as recovered only after this much work.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// The service runs for months: the event log is cut down now and then, not only at start.
const TRIM_EVERY: Duration = Duration::from_secs(6 * 3600);
/// How often the engine thread looks whether winws2 is still alive.
const ENGINE_POLL: Duration = Duration::from_secs(1);

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
    /// Hosts the old strategy could not fix.
    pub unfixed: Vec<String>,
}

struct Inner {
    config: Config,
    sites: Sites,
    network: Network,
    memory: Option<NetMemory>,
    /// What the engine thread has to do next.
    order: Option<Order>,
    /// An order is waiting or being carried out: no verdict on the engine until it is done.
    busy: bool,
    /// winws2 was running when the engine thread last looked.
    up: bool,
    planned: bool,
    problem: Option<Problem>,
    crashes: u32,
    retry_at: Option<Instant>,
    restarted_at: Option<Instant>,
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
    trimmed: Instant,
    net_pending: Option<(Network, Instant)>,
    published: Instant,
    unpublished: bool,
    verifying: BTreeSet<String>,
    restart_at: Option<Instant>,
    procs: BTreeSet<String>,
    has_probed: bool,
    has_direct: bool,
    has_ipset: bool,
    update: Update,
    updating: bool,
    /// How each neural network fares with the chosen resolver, by service id.
    ai: BTreeMap<String, SiteState>,
    /// The DNS Windows itself is set to, as the forwarder last read it.
    sysdns: SystemDns,
}

impl Inner {
    /// The resolver for the neural networks as the forwarder should have it now; empty while the
    /// bypass is paused or none is chosen.
    fn ai_spec(&self) -> String {
        match self.config.enabled && rkn_core::aidns::upstream(&self.config.ai_dns).is_some() {
            true => self.config.ai_dns.trim().to_owned(),
            false => String::new(),
        }
    }

    fn reserve(&self) -> Reserve {
        self.memory.as_ref().map(|m| m.reserve.clone()).unwrap_or_default()
    }

    fn is_direct(&self, host: &str) -> bool {
        self.memory.as_ref().is_some_and(|m| fresh_direct(m).any(|d| targets::within(host, d)))
    }

    fn confirmed(&self, host: &str) -> bool {
        let hit = |entry: &String| targets::expand(entry).iter().any(|d| targets::within(host, d));
        targets::forced(host)
            || self.memory.as_ref().is_some_and(|m| m.blocked.iter().any(hit))
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
        engine::build_plan(&self.sites, &self.reserve(), self.has_probed, self.has_direct, self.has_ipset, self.voice())
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
    /// winws2 itself. Only the engine thread starts it; an update and the shutdown stop it. Never
    /// taken while `inner` is held: a start waits for the driver for seconds.
    engine: Mutex<Winws2>,
    engine_dir: PathBuf,
    kick: std_mpsc::Sender<()>,
    kicks: Mutex<Option<std_mpsc::Receiver<()>>>,
    stopping: AtomicBool,
    tx: broadcast::Sender<Message>,
    jobs: mpsc::UnboundedSender<Job>,
    verify: mpsc::UnboundedSender<String>,
    updates: mpsc::UnboundedSender<Ask>,
    moved: watch::Sender<u64>,
    stop: watch::Sender<u64>,
    quit: watch::Sender<bool>,
    /// The resolver for the neural networks the forwarder should run with (`Inner::ai_spec`).
    ai_dns: watch::Sender<String>,
    /// A window has connected: the look at the neural networks should be fresh.
    ai_poke: tokio::sync::Notify,
}

impl Hub {
    pub fn new(jobs: mpsc::UnboundedSender<Job>, verify: mpsc::UnboundedSender<String>, updates: mpsc::UnboundedSender<Ask>) -> Self {
        let _ = std::fs::create_dir_all(paths::state_dir());
        trim_events();
        let (tx, _) = broadcast::channel(256);
        let found = netid::current(None);
        tracing::info!(found = ?found, "network");
        let network = found.filter(|n| n.internet).unwrap_or_else(Network::unknown);
        let (kick, kicks) = std_mpsc::channel();
        let hub = Self {
            engine: Mutex::new(Winws2::new(paths::engine_dir())),
            engine_dir: paths::engine_dir(),
            kick,
            kicks: Mutex::new(Some(kicks)),
            stopping: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                config: Config::load(),
                sites: Sites::load(),
                network: network.clone(),
                memory: None,
                order: None,
                busy: false,
                up: false,
                planned: false,
                problem: None,
                crashes: 0,
                retry_at: None,
                restarted_at: None,
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
                trimmed: Instant::now(),
                net_pending: None,
                published: Instant::now(),
                unpublished: false,
                verifying: BTreeSet::new(),
                restart_at: None,
                procs: BTreeSet::new(),
                has_probed: false,
                has_direct: false,
                has_ipset: false,
                // Not "off" for the first moments of a fresh start: the window would say updates are disabled.
                update: Update {
                    state: if update::enabled() { UpdateState::Idle } else { UpdateState::Off },
                    ..Update::default()
                },
                updating: false,
                ai: BTreeMap::new(),
                sysdns: SystemDns::default(),
            }),
            tx,
            jobs,
            verify,
            updates,
            moved: watch::channel(0).0,
            stop: watch::channel(0).0,
            quit: watch::channel(false).0,
            ai_dns: watch::channel(String::new()).0,
            ai_poke: tokio::sync::Notify::new(),
        };
        {
            let mut inner = hub.lock();
            hub.sync_ai(&mut inner);
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
        let before = inner.plan().map(|p| p.args());
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
            // winws2 rereads changed list files by itself; a restart drops every open connection,
            // so it is only for a new command line (the first list file, voice for Discord).
            if inner.plan().map(|p| p.args()) != before {
                inner.restart_at.get_or_insert(Instant::now() + RESTART_DELAY);
            }
        }
        self.publish(&mut inner);
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn load_memory(&self, inner: &mut Inner) {
        inner.memory = match NetMemory::load(&inner.network.id) {
            Ok(memory) => {
                if memory.is_some() {
                    rkn_core::memory::touch(&inner.network.id);
                }
                memory
            }
            Err(e) => {
                self.emit(Event::new(Level::Warn, format!("Не удалось прочитать память сети: {e}")));
                None
            }
        };
    }

    pub fn subscribe(&self) -> (Status, broadcast::Receiver<Message>) {
        let mut inner = self.lock();
        let rx = self.tx.subscribe();
        // A window has just opened: the neural networks get a fresh look for it.
        self.ai_poke.notify_one();
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

    pub async fn ai_poked(&self) {
        self.ai_poke.notified().await
    }

    pub fn ai_dns(&self) -> watch::Receiver<String> {
        self.ai_dns.subscribe()
    }

    /// Tells the forwarder which resolver the settings ask for now. What was known about the old
    /// one says nothing about the new.
    fn sync_ai(&self, inner: &mut Inner) {
        let spec = inner.ai_spec();
        let changed = self.ai_dns.send_if_modified(|current| {
            let differs = *current != spec;
            if differs {
                *current = spec;
            }
            differs
        });
        if changed {
            inner.ai.clear();
        }
    }

    pub fn set_system_dns(&self, dns: SystemDns) {
        let mut inner = self.lock();
        if inner.sysdns != dns {
            inner.sysdns = dns;
            self.publish(&mut inner);
        }
    }

    /// The forwarder's report: whether the neural networks let in, with the resolver `spec` in effect.
    pub fn set_ai(&self, spec: &str, states: BTreeMap<String, SiteState>) {
        let mut inner = self.lock();
        if inner.ai_spec() == spec && inner.ai != states {
            inner.ai = states;
            self.publish(&mut inner);
        }
    }

    pub fn note(&self, level: Level, text: String) {
        self.emit(Event::new(level, text));
    }

    fn poll_network(&self) {
        let current = {
            let mut inner = self.lock();
            if inner.net_polled.elapsed() < NET_POLL {
                return;
            }
            inner.net_polled = Instant::now();
            inner.network.id.clone()
        };
        let found = netid::current(Some(&current));
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
        if inner.trimmed.elapsed() >= TRIM_EVERY {
            inner.trimmed = Instant::now();
            trim_events();
        }

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
        if should_run && inner.retry_at.is_none() && !inner.busy && !inner.up {
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
            if let Err(e) = engine::verify(&self.engine_dir) {
                self.order(&mut inner, Order::Stop { unload: false });
                let text = format!("Файлы движка повреждены: {e:#}. Переустановите zapretUI");
                self.emit(Event::new(Level::Error, text.clone()));
                inner.problem = Some(Problem::Failed(text));
            } else if let Some(plan) = inner.plan() {
                self.order(&mut inner, Order::Run { plan, retry: true });
            }
        }
        // An engine that dies seconds after each restart must reach MAX_CRASHES, not restart forever.
        let stable = inner.restarted_at.is_some_and(|at| at.elapsed() >= STABLE_AFTER);
        if inner.crashes > 0 && inner.retry_at.is_none() && stable && inner.up {
            inner.crashes = 0;
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

    pub fn check_every(&self) -> Option<Duration> {
        let inner = self.lock();
        inner.config.enabled
            .then(|| Duration::from_secs(u64::from(inner.config.check_interval_min.max(2)) * 60))
    }
}
