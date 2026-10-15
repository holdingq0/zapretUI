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
}

impl Entry {
    pub fn target(&self) -> Target {
        if self.http { Target::http(self.host.as_str()) } else { Target::https(self.host.as_str()) }
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
            out.push(Entry { name: service.name.clone(), host: host.clone(), http: false });
        }
        for host in &service.probe_http {
            out.push(Entry { name: service.name.clone(), host: host.clone(), http: true });
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

pub fn expand(host: &str) -> Vec<String> {
    match service_of(host) {
        Some(service) => service.domains.clone(),
        None => vec![base_domain(host)],
    }
}

pub fn name(host: &str) -> String {
    group(host).name
}
