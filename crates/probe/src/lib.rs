mod dns;
mod http;
mod net;
mod tls;

use std::net::IpAddr;
use std::time::{Duration, Instant};

pub use net::PortBlock;

use dns::{doh_global, system_global};
use http::End;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    NoDns,
    DnsSpoofed,
    IpBlocked,
    TlsBlocked,
    Cut,
    HttpBlocked,
    Geoblocked,
    Failed,
}

impl Verdict {
    pub fn fixable(self) -> bool {
        matches!(self, Self::TlsBlocked | Self::Cut | Self::HttpBlocked)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub tls: bool,
    pub path: String,
}

impl Target {
    pub fn https(host: impl Into<String>) -> Self {
        Self { host: host.into(), tls: true, path: "/".into() }
    }

    pub fn http(host: impl Into<String>) -> Self {
        Self { host: host.into(), tls: false, path: "/".into() }
    }

    pub fn port(&self) -> u16 {
        if self.tls { 443 } else { 80 }
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub verdict: Verdict,
    pub ip: Option<IpAddr>,
    pub latency_ms: Option<u32>,
    pub bytes: usize,
    pub status: Option<u16>,
    pub detail: String,
}

impl Outcome {
    fn new(verdict: Verdict, ip: Option<IpAddr>, detail: impl Into<String>) -> Self {
        Self { verdict, ip, latency_ms: None, bytes: 0, status: None, detail: detail.into() }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Opts {
    pub connect: Duration,
    pub idle: Duration,
    pub want: usize,
}

impl Default for Opts {
    fn default() -> Self {
        Self { connect: Duration::from_secs(3), idle: Duration::from_secs(3), want: 64 * 1024 }
    }
}

const STUB_WORDS: [&str; 10] =
    ["warning", "block", "zapret", "rkn", "eais", "blacklist", "fz139", "lawfilter", "restrict", "stop"];

fn stub_redirect(target: &Target, location: Option<&str>) -> bool {
    let Some(location) = location else { return false };
    let lower = location.to_ascii_lowercase();
    let host = lower.split("://").nth(1).and_then(|r| r.split(['/', '?', ':']).next()).unwrap_or("");
    let own = host.is_empty() || host == target.host || host.ends_with(&format!(".{}", target.host)) || target.host.ends_with(&format!(".{host}"));
    !own && STUB_WORDS.iter().any(|w| lower.contains(w))
}

pub async fn fetch(target: &Target, ip: IpAddr, ports: Option<&PortBlock>, opts: Opts) -> Outcome {
    let started = Instant::now();
    let tcp = match net::connect(ip, target.port(), ports, opts.connect).await {
        Ok(s) => s,
        Err(e) => return Outcome::new(Verdict::IpBlocked, Some(ip), format!("TCP: {}", net::describe(&e))),
    };
    let rtt = started.elapsed();
    let fetched = if target.tls {
        match tls::handshake(tcp, &target.host, opts.idle).await {
            Ok(stream) => http::get(stream, target, opts.want, opts.idle).await,
            // A certificate the server did send but that does not check out is not DPI: a wrong clock
            // on this computer or a filter that swaps certificates. No bypass strategy fixes it.
            Err(e) if tls::bad_certificate(&e) => {
                return Outcome::new(Verdict::Failed, Some(ip), format!("сертификат не прошёл проверку: {e}"));
            }
            Err(e) => return Outcome::new(Verdict::TlsBlocked, Some(ip), format!("TLS: {}", net::describe(&e))),
        }
    } else {
        http::get(tcp, target, opts.want, opts.idle).await
    };

    let kb = fetched.bytes.div_ceil(1024);
    let complete = fetched.end == End::Complete;
    let broke = match fetched.end {
        End::Stall => "данные перестали приходить".to_string(),
        End::Broken(kind) => net::describe(&std::io::Error::from(kind)),
        End::Complete => String::new(),
    };
    let (verdict, detail) = match fetched.status {
        None if complete => (Verdict::Failed, "сервер закрыл соединение без ответа".to_string()),
        None if target.tls => (Verdict::Cut, format!("после рукопожатия {broke}")),
        None => (Verdict::HttpBlocked, format!("запрос без ответа: {broke}")),
        Some(_) if !target.tls && stub_redirect(target, fetched.location.as_deref()) => {
            (Verdict::HttpBlocked, "перенаправление на заглушку провайдера".to_string())
        }
        Some(451) => (Verdict::Geoblocked, "сервис закрыт для этого региона (451)".to_string()),
        Some(_) if !complete => (Verdict::Cut, format!("обрыв на {kb} КБ: {broke}")),
        Some(_) => (Verdict::Ok, String::new()),
    };
    Outcome {
        verdict,
        ip: Some(ip),
        latency_ms: Some(rtt.as_millis().min(u32::MAX as u128) as u32),
        bytes: fetched.bytes,
        status: fetched.status,
        detail,
    }
}

pub async fn probe(target: &Target, ports: Option<&PortBlock>, opts: Opts) -> Outcome {
    let system = system_global(&target.host).await;
    let mut last: Option<Outcome> = None;
    // A single address gets a second try: one lost connection is not a block by IP.
    let tries: Vec<IpAddr> = match system.as_slice() {
        [only] => vec![*only, *only],
        many => many.iter().take(2).copied().collect(),
    };
    for ip in &tries {
        let outcome = fetch(target, *ip, ports, opts).await;
        let retry = outcome.verdict == Verdict::IpBlocked;
        if outcome.verdict == Verdict::Ok {
            return outcome;
        }
        last = Some(outcome);
        if !retry {
            break;
        }
    }
    if last.as_ref().is_none_or(|o| o.verdict == Verdict::IpBlocked) {
        // No address from the system resolver, or none of them answers: big services rotate
        // addresses, and another one may be reachable.
        let spoofed = system.is_empty();
        for ip in doh_global(&target.host, &system).await.iter().take(2) {
            let outcome = fetch(target, *ip, ports, opts).await;
            if outcome.verdict == Verdict::Ok {
                // Only a resolver that gave no real address is lying. One that gave a blocked
                // address of the same service is not, and DoH would not fix the browser either.
                return match spoofed {
                    true => Outcome {
                        verdict: Verdict::DnsSpoofed,
                        detail: "провайдер выдаёт неверный адрес, по DoH сайт открывается".into(),
                        ..outcome
                    },
                    false => Outcome::new(Verdict::IpBlocked, outcome.ip, "часть адресов сервиса заблокирована по IP"),
                };
            }
            // Reachable at another address, and there the name is blocked: that is DPI, and the bypass helps.
            if outcome.verdict.fixable() || last.is_none() {
                last = Some(outcome);
            }
        }
    }
    last.unwrap_or_else(|| Outcome::new(Verdict::NoDns, None, "адрес сайта не найден"))
}
