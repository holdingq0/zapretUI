// DNS for the neural networks (see rkn_core::aidns). A small forwarder listens on a loopback
// address, NRPT rules send the names of the catalog's neural networks to it, and it asks the
// chosen resolver over DoH. Everything else on the computer keeps its DNS.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use rkn_core::aidns::{self, Upstream};
use rkn_core::ipc::{Level, SiteState, SystemDns};
use rkn_probe::{Resolver, Target, Verdict};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};

use crate::hub::Hub;
use crate::{nrpt, sysdns, targets};

/// An address of its own in the loopback network: port 53 on 127.0.0.1 is often taken by another
/// local resolver, and NRPT cannot name a port.
const LISTEN: Ipv4Addr = Ipv4Addr::new(127, 83, 0, 53);
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// The service starts with Windows, before the network has settled: the first look waits a little.
const SETTLE: Duration = Duration::from_secs(4);
/// A look that found something closed is repeated soon: one lost connection must not stay on
/// screen as "closed" until the next regular check.
const RECHECKS: [Duration; 3] = [Duration::from_secs(15), Duration::from_secs(45), Duration::from_secs(120)];
/// The window asking for a fresh look gets one no more often than this.
const POKE_GAP: Duration = Duration::from_secs(20);
const PARALLEL: usize = 64;
const TCP_IDLE: Duration = Duration::from_secs(10);
/// What fits a datagram for sure; a longer answer is asked again over TCP.
const UDP_MAX: usize = 4096;
/// Countries the neural networks of the catalog do not serve.
const CLOSED: [&str; 2] = ["RU", "BY"];
/// The service asked what country it sees: it is behind Cloudflare, which tells.
const HOME: &str = "chatgpt.com";
const AAAA: u16 = 28;
const SVCB: u16 = 64;
const HTTPS: u16 = 65;

struct Live {
    spec: String,
    name: String,
    resolver: Arc<Resolver>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Live {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

pub async fn run(hub: Arc<Hub>) {
    let mut wanted = hub.ai_dns();
    let mut moved = hub.network_moves();
    // Rules a killed service left behind point at a forwarder that is gone.
    let _ = tokio::task::spawn_blocking(nrpt::remove).await;
    let mut live: Option<Live> = None;
    let mut down = false;
    // The setting a failed start was already reported for: the retries stay quiet.
    let mut warned = String::new();
    // Looks in a row that found something closed.
    let mut doubts = 0usize;
    tokio::time::sleep(SETTLE).await;
    loop {
        let spec = wanted.borrow_and_update().clone();
        if live.as_ref().is_none_or(|l| l.spec != spec) {
            if let Some(old) = live.take() {
                drop(old);
                let _ = tokio::task::spawn_blocking(nrpt::remove).await;
                if spec.is_empty() {
                    hub.note(Level::Info, "DNS для нейросетей выключен".into());
                }
            }
            down = false;
            if let Some(upstream) = aidns::upstream(&spec) {
                match start(&spec, upstream).await {
                    Ok(started) => {
                        hub.note(Level::Info, format!("DNS для нейросетей: {}", started.name));
                        live = Some(started);
                        warned.clear();
                    }
                    Err(e) => {
                        if warned != spec {
                            hub.note(Level::Warn, format!("DNS для нейросетей не включился: {e:#}"));
                            warned = spec.clone();
                        }
                    }
                }
            }
        }
        let mut wait = CHECK_EVERY;
        match &live {
            Some(l) => {
                l.resolver.retry_doh();
                let now_down = l.resolver.resolve(HOME).await.is_none() && l.resolver.resolve(HOME).await.is_none();
                if now_down && !down {
                    hub.note(Level::Warn, format!("DNS для нейросетей: {} не отвечает", l.name));
                } else if !now_down && down {
                    hub.note(Level::Info, format!("DNS для нейросетей: {} снова отвечает", l.name));
                }
                down = now_down;
                if down {
                    wait = RETRY_AFTER;
                }
            }
            // The forwarder did not start (its port is taken): try again soon.
            None if !spec.is_empty() => wait = RETRY_AFTER,
            None => {}
        }
        // Whatever resolver is in effect, the one chosen here or the one Windows is set to, the
        // rows say what the services answer right now.
        let system = tokio::task::spawn_blocking(sysdns::read).await.unwrap_or_default();
        let (states, relay) = check().await;
        hub.set_system_dns(SystemDns {
            servers: system.servers.iter().map(ToString::to_string).collect(),
            doh: system.doh,
            // With the forwarder running the relay seen is its resolver's, not the system's.
            relay: relay && live.is_none(),
        });
        let all_open = !states.is_empty() && states.values().all(|s| *s == SiteState::Open);
        hub.set_ai(&spec, states);
        if all_open {
            doubts = 0;
        } else {
            if let Some(soon) = RECHECKS.get(doubts) {
                wait = wait.min(*soon);
            }
            doubts += 1;
        }
        let looked = Instant::now();
        tokio::select! {
            changed = wanted.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = moved.changed() => doubts = 0,
            // The window was opened: what it shows should be fresh.
            _ = hub.ai_poked() => {
                tokio::time::sleep(POKE_GAP.saturating_sub(looked.elapsed())).await;
                doubts = 0;
            }
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// On the way out: the names go back to the system DNS.
pub fn cleanup() {
    nrpt::remove();
}

async fn start(spec: &str, upstream: Upstream) -> anyhow::Result<Live> {
    let at = SocketAddr::from((LISTEN, 53));
    let udp = UdpSocket::bind(at).await.with_context(|| format!("порт {at} занят"))?;
    let tcp = TcpListener::bind(at).await.with_context(|| format!("порт {at} занят"))?;
    let doh = upstream.doh.map(|d| (d.host, d.port, d.path));
    let resolver = Arc::new(Resolver::new(doh, upstream.ips, upstream.plain));
    let tasks = vec![tokio::spawn(serve_udp(udp, resolver.clone())), tokio::spawn(serve_tcp(tcp, resolver.clone()))];
    let live = Live { spec: spec.to_owned(), name: upstream.name, resolver, tasks };
    let domains = targets::ai_domains();
    tokio::task::spawn_blocking(move || nrpt::apply(&domains, &LISTEN.to_string())).await?.context("правила DNS в Windows")?;
    Ok(live)
}

async fn answer(resolver: &Resolver, query: &[u8]) -> Option<Vec<u8>> {
    // Only questions; whatever else arrives on the port gets no reply.
    if query.len() < 12 || query[2] & 0x80 != 0 {
        return None;
    }
    // An HTTPS record carries the service's own addresses and keys, and an IPv6 address is its own
    // too: with either the browser would go to the service directly, past the relay.
    if matches!(rkn_probe::qtype(query), Some(AAAA | SVCB | HTTPS)) {
        return rkn_probe::reply(query, 0);
    }
    if let Some(answer) = resolver.exchange(query).await {
        return Some(answer);
    }
    // The resolver is out of reach: the real addresses are better than no addresses.
    if let Some(answer) = rkn_probe::public(query).await {
        return Some(answer);
    }
    rkn_probe::reply(query, 2)
}

async fn serve_udp(socket: UdpSocket, resolver: Arc<Resolver>) {
    let socket = Arc::new(socket);
    let gate = Arc::new(Semaphore::new(PARALLEL));
    let mut buf = [0u8; 4096];
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(got) => got,
            // Windows reports here that an earlier reply found its port closed.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
        };
        let Ok(permit) = gate.clone().try_acquire_owned() else { continue };
        let query = buf[..n].to_vec();
        let (socket, resolver) = (socket.clone(), resolver.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let Some(mut reply) = answer(&resolver, &query).await else { return };
            if reply.len() > UDP_MAX {
                // Marked as cut: the client asks again over TCP.
                let Some(mut cut) = rkn_probe::reply(&query, 0) else { return };
                cut[2] |= 0x02;
                reply = cut;
            }
            let _ = socket.send_to(&reply, from).await;
        });
    }
}

async fn serve_tcp(listener: TcpListener, resolver: Arc<Resolver>) {
    let gate = Arc::new(Semaphore::new(PARALLEL));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        let Ok(permit) = gate.clone().try_acquire_owned() else { continue };
        let resolver = resolver.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = talk(stream, &resolver).await;
        });
    }
}

/// DNS over TCP: every message has its length in two bytes before it.
async fn talk(mut stream: TcpStream, resolver: &Resolver) -> Option<()> {
    loop {
        let mut head = [0u8; 2];
        tokio::time::timeout(TCP_IDLE, stream.read_exact(&mut head)).await.ok()?.ok()?;
        let mut query = vec![0u8; usize::from(u16::from_be_bytes(head))];
        tokio::time::timeout(TCP_IDLE, stream.read_exact(&mut query)).await.ok()?.ok()?;
        let reply = answer(resolver, &query).await?;
        let len = u16::try_from(reply.len()).ok()?;
        stream.write_all(&len.to_be_bytes()).await.ok()?;
        stream.write_all(&reply).await.ok()?;
    }
}

/// What one neural network answers when reached the way a browser on this computer reaches it.
struct Look {
    /// The system resolver gave an address for it.
    resolved: bool,
    /// The country it sees the computer in, for a service that tells.
    country: Option<String>,
    opens: bool,
    /// None of the system's addresses is one the public resolvers give: a relay stands in.
    relayed: bool,
}

fn lets_in(country: &str) -> bool {
    !CLOSED.contains(&country)
}

async fn look(host: String) -> Look {
    let mut system = rkn_probe::system_addrs(&host).await;
    if system.is_empty() {
        // One lost answer is not a resolver that is down.
        tokio::time::sleep(Duration::from_secs(1)).await;
        system = rkn_probe::system_addrs(&host).await;
    }
    let Some(ip) = system.first().copied() else {
        return Look { resolved: false, country: None, opens: false, relayed: false };
    };
    let mut country = rkn_probe::seen_from(&host, ip).await;
    if country.is_none() {
        // A site that does tell the country may have missed once; one that does not says so at once.
        country = rkn_probe::seen_from(&host, ip).await;
    }
    // The same reach as every other check: some of these sites send headers of tens of kilobytes.
    let opts = rkn_search::probe_opts();
    let opens = country.is_some() || rkn_probe::fetch(&Target::https(host.as_str()), ip, None, opts).await.verdict == Verdict::Ok;
    let real = rkn_probe::public_addrs(&host).await;
    let relayed = !real.is_empty() && system.iter().all(|ip| !real.contains(ip));
    Look { resolved: true, country, opens, relayed }
}

/// Whether each neural network lets this computer in right now, and whether that is the work of
/// a relay the resolver in effect answers with.
async fn check() -> (BTreeMap<String, SiteState>, bool) {
    let mut set = JoinSet::new();
    for service in targets::ai() {
        let Some(host) = service.probe.first().cloned() else { continue };
        let id = service.id();
        set.spawn(async move { (id, look(host).await) });
    }
    // The country the services see without any resolver in between: asked at a real address.
    let home = match rkn_probe::public_addrs(HOME).await.first() {
        Some(ip) => rkn_probe::seen_from(HOME, *ip).await,
        None => None,
    };
    let home_open = home.as_deref().is_some_and(lets_in);
    let mut looks = Vec::new();
    while let Some(result) = set.join_next().await {
        if let Ok(found) = result {
            looks.push(found);
        }
    }
    // A service that tells the country is the proof: closed from home, open as the system reaches it.
    let relay = !home_open && looks.iter().any(|(_, l)| l.country.as_deref().is_some_and(lets_in));
    let states = looks
        .into_iter()
        .map(|(id, l)| {
            let state = match &l.country {
                _ if !l.resolved => SiteState::DnsDown,
                Some(country) if lets_in(country) => SiteState::Open,
                Some(_) => SiteState::Geoblocked,
                None if !l.opens => SiteState::Blocked,
                // It does not tell the country: open when home is, or when a relay proven on the
                // other services stands in for this one too.
                None if home_open || (relay && l.relayed) => SiteState::Open,
                None => SiteState::Geoblocked,
            };
            (id, state)
        })
        .collect();
    (states, relay)
}

/// `zapretUI-svc ai`: the DNS Windows is set to and whether the neural networks let in right now.
pub fn cli() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let system = sysdns::read();
    let servers: Vec<String> = system.servers.iter().map(ToString::to_string).collect();
    println!("DNS системы: {}", if servers.is_empty() { "не определён".into() } else { servers.join(", ") });
    println!("DoH системы: {}", if system.doh.is_empty() { "не задан".into() } else { system.doh.join(", ") });
    let (states, relay) = runtime.block_on(check());
    for service in targets::ai() {
        let state = states.get(&service.id()).copied().unwrap_or(SiteState::Unknown);
        println!("{:<10} {:?}", service.name, state);
    }
    println!("Нейросети открывает DNS: {}", if relay { "да" } else { "нет" });
    Ok(())
}
