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
