use std::sync::OnceLock;

use rkn_core::ipc::Kind;
use rkn_probe::Target;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub path: Option<String>,
}

impl Entry {
    pub fn target(&self) -> Target {
        let mut target = if self.http { Target::http(self.host.as_str()) } else { Target::https(self.host.as_str()) };
        if let Some(path) = &self.path {
            target.path = path.clone();
        }
        target
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Service {
    pub name: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub probe: Vec<String>,
    #[serde(default)]
    pub probe_http: Vec<String>,
    #[serde(default)]
    pub processes: Vec<String>,
    #[serde(default)]
    pub launcher: Option<String>,
    /// What the HTTPS probes fetch instead of "/": a big file shows a cut at 16-20 KB.
    #[serde(default)]
    pub path: Option<String>,
    /// Name of this hosting's subnets in resources/ipset-hosting.txt: once the probe sees the cut,
    /// every site on these addresses goes through the bypass, not only the probed host.
    #[serde(default)]
    pub ipset: Option<String>,
    /// Goes through the bypass on every network, whatever the probe says.
    #[serde(default)]
    pub always: bool,
}

impl Service {
    pub fn id(&self) -> String {
        match self.domains.first() {
            Some(domain) => domain.clone(),
            None => {
                let slug: String = self
                    .name
                    .to_ascii_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                format!("game:{slug}")
            }
        }
    }
}

pub fn by_name(name: &str) -> Option<&'static Service> {
    catalog().service.iter().find(|s| s.name == name)
}

/// A service that is always bypassed owns this host.
pub fn forced(host: &str) -> bool {
    catalog().service.iter().filter(|s| s.always).any(|s| s.domains.iter().any(|d| within(host, d)))
}

/// Not a row in the window: a hosting whose subnets are watched in the background.
pub fn background(host: &str) -> bool {
    service_of(host).is_some_and(|s| s.ipset.is_some())
}

pub fn games() -> impl Iterator<Item = &'static Service> {
    catalog().service.iter().filter(|s| s.kind != Kind::Site)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub service: Vec<Service>,
    #[serde(default)]
    pub control: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub name: String,
}

pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        toml::from_str(include_str!("../../../resources/targets.toml")).unwrap_or_else(|e| {
            tracing::error!("targets.toml: {e}");
            Catalog::default()
        })
    })
}

pub fn sites() -> Vec<Entry> {
    let mut out = Vec::new();
    for service in &catalog().service {
        for host in &service.probe {
            out.push(Entry { name: service.name.clone(), host: host.clone(), http: false, path: service.path.clone() });
        }
        for host in &service.probe_http {
            out.push(Entry { name: service.name.clone(), host: host.clone(), http: true, path: None });
        }
    }
    out
}

pub fn within(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

pub fn service_of(host: &str) -> Option<&'static Service> {
    catalog()
        .service
        .iter()
        .filter_map(|s| s.domains.iter().filter(|d| within(host, d)).map(String::len).max().map(|len| (len, s)))
        .max_by_key(|(len, _)| *len)
        .map(|(_, s)| s)
}

pub fn base_domain(host: &str) -> String {
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return host.to_owned();
    }
    let second = labels[n - 2];
    let keep = if labels[n - 1].len() == 2 && ["com", "net", "org", "co", "gov", "edu", "ac", "spb", "msk"].contains(&second) {
        3
    } else {
        2
    };
    labels[n - keep.min(n)..].join(".")
}

pub fn group(host: &str) -> Group {
    match service_of(host) {
        Some(service) => Group { id: service.id(), name: service.name.clone() },
        None => {
            let base = base_domain(host);
            let name = catalog().control.iter().find(|e| within(&e.host, &base)).map(|e| e.name.clone()).unwrap_or(base.clone());
            Group { id: base, name }
        }
    }
}

/// What goes through the bypass for an entry: a whole catalog service, or just that host and its
/// subdomains. A blocked play.google.com must not pull all of google.com into the bypass.
pub fn expand(host: &str) -> Vec<String> {
    match service_of(host) {
        Some(service) => service.domains.clone(),
        None => vec![host.to_owned()],
    }
}

pub fn name(host: &str) -> String {
    group(host).name
}

/// Subnets of the catalog's hostings whose service is in `blocked`, one per line for --ipset.
pub fn hosting_ipset(blocked: &[String]) -> Vec<String> {
    let names: Vec<&str> = catalog()
        .service
        .iter()
        .filter(|s| blocked.iter().any(|b| s.domains.iter().any(|d| within(b, d))))
        .filter_map(|s| s.ipset.as_deref())
        .collect();
    include_str!("../../../resources/ipset-hosting.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once(' '))
        .filter(|(name, _)| names.contains(name))
        .map(|(_, net)| net.trim().to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_reads_and_holds_together() {
        let catalog = catalog();
        assert!(!catalog.service.is_empty() && !catalog.control.is_empty());
        let mut ids: Vec<String> = catalog.service.iter().map(Service::id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), catalog.service.len(), "two services share an id");
        for service in &catalog.service {
            if let Some(launcher) = &service.launcher {
                assert!(by_name(launcher).is_some(), "{}: no launcher {launcher}", service.name);
            }
            if service.ipset.is_some() {
                let id = service.id();
                assert!(!hosting_ipset(std::slice::from_ref(&id)).is_empty(), "{}: no subnets", service.name);
            }
            for host in service.probe.iter().chain(&service.probe_http) {
                assert_eq!(service_of(host).map(|s| &s.name), Some(&service.name), "{host}");
            }
        }
        assert!(hosting_ipset(&[]).is_empty());
        assert!(hosting_ipset(&["youtube.com".to_string()]).is_empty());
    }

    #[test]
    fn a_host_belongs_to_its_service() {
        assert_eq!(service_of("www.youtube.com").map(|s| s.name.as_str()), Some("YouTube"));
        assert_eq!(service_of("rr1---sn-abc.googlevideo.com").map(|s| s.name.as_str()), Some("YouTube"));
        assert!(service_of("notyoutube.com").is_none());
        assert!(service_of("example.org").is_none());
        assert!(within("a.b.com", "b.com") && within("b.com", "b.com"));
        assert!(!within("ab.com", "b.com"));
    }

    #[test]
    fn the_bypass_takes_a_whole_service_but_only_one_foreign_host() {
        assert!(expand("discord.gg").contains(&"discord.com".to_string()));
        assert_eq!(expand("play.google.com"), ["play.google.com"]);
        assert_eq!(group("cdn.discordapp.com").name, "Discord");
        assert_eq!(group("a.b.example.com").id, "example.com");
    }

    #[test]
    fn discord_is_always_bypassed_and_hostings_stay_in_the_background() {
        assert!(forced("discord.com") && forced("gateway.discord.gg") && forced("cdn.discordapp.com"));
        assert!(!forced("youtube.com") && !forced("example.org"));
        assert!(background("proof.ovh.net") && background("nbg1-speed.hetzner.com"));
        assert!(!background("discord.com") && !background("example.org"));
    }

    #[test]
    fn base_domains() {
        assert_eq!(base_domain("a.b.example.com"), "example.com");
        assert_eq!(base_domain("example.com"), "example.com");
        assert_eq!(base_domain("www.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(base_domain("shop.example.com.tr"), "example.com.tr");
    }
}
