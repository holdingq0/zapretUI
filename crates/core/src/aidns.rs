// DNS for the neural networks. ChatGPT, Claude and the like are not cut by the provider: they
// refuse visitors from Russia themselves, so no bypass strategy opens them. Some public resolvers
// answer for these names with the address of their own relay, which passes the TLS stream on
// untouched from a country the service does accept. Only the names of the catalog's neural
// networks are asked there; the rest of the computer's DNS stays as it is.

use std::net::IpAddr;

pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub doh: &'static str,
    /// The resolver's own addresses: its name is not asked from the system DNS, which may lie.
    pub ips: &'static [&'static str],
    /// It answers on port 53 as well: the fallback when its DoH is out of reach.
    pub plain: bool,
}

pub const PRESETS: [Preset; 2] = [
    // HTTP/2 only, and nothing on port 53.
    Preset { id: "dns-ai", name: "dns-ai.ru", doh: "https://dns.dns-ai.ru/dns-query", ips: &["194.87.196.13", "45.151.102.219"], plain: false },
    Preset { id: "bezmezhau", name: "bezmezhau.com", doh: "https://dns.bezmezhau.com/dns-query", ips: &["143.20.64.55", "31.58.216.201"], plain: true },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doh {
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Where the names of the neural networks are asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub name: String,
    pub doh: Option<Doh>,
    pub ips: Vec<IpAddr>,
    pub plain: bool,
}

impl Upstream {
    /// The DoH address as it is written.
    pub fn url(&self) -> Option<String> {
        self.doh.as_ref().map(|d| match d.port {
            443 => format!("https://{}{}", d.host, d.path),
            port => format!("https://{}:{port}{}", d.host, d.path),
        })
    }
}

fn parse_doh(url: &str) -> Option<Doh> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/dns-query"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, 443),
    };
    let host = host.to_ascii_lowercase();
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| !label.is_empty() && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    let path_ok = path.len() <= 200 && path.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'<' | b'>' | b'\\'));
    (host_ok && path_ok && port != 0).then(|| Doh { host, port, path: path.to_owned() })
}

/// Reads the setting: a preset's id, a DoH address (`https://host/dns-query`) or plain addresses
/// of a resolver. None for an empty or unreadable one: DNS for the neural networks is off.
pub fn upstream(spec: &str) -> Option<Upstream> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if let Some(preset) = PRESETS.iter().find(|p| p.id == spec) {
        return Some(Upstream {
            name: preset.name.to_owned(),
            doh: parse_doh(preset.doh),
            ips: preset.ips.iter().filter_map(|ip| ip.parse().ok()).collect(),
            plain: preset.plain,
        });
    }
    if spec.starts_with("https://") {
        let doh = parse_doh(spec)?;
        // An address in place of the name needs no lookup.
        let ips = doh.host.parse().ok().into_iter().collect();
        return Some(Upstream { name: doh.host.clone(), doh: Some(doh), ips, plain: false });
    }
    let parts: Vec<&str> = spec.split([',', ';', ' ']).filter(|p| !p.is_empty()).collect();
    let ips: Vec<IpAddr> = parts.iter().filter_map(|p| p.parse().ok()).collect();
    if ips.is_empty() || ips.len() != parts.len() || ips.len() > 4 || ips.iter().any(|ip| ip.is_loopback() || ip.is_unspecified()) {
        return None;
    }
    Some(Upstream { name: ips[0].to_string(), doh: None, ips, plain: true })
}

/// A resolver of the user's own as it is kept in the settings, or None when the text is not one.
pub fn custom(text: &str) -> Option<String> {
    let text = text.trim();
    if text.len() > 300 || PRESETS.iter().any(|p| p.id == text) {
        return None;
    }
    upstream(text).map(|_| text.to_owned())
}

/// What the window calls the chosen resolver.
pub fn label(spec: &str) -> Option<String> {
    upstream(spec).map(|u| u.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_read() {
        for preset in &PRESETS {
            let up = upstream(preset.id).unwrap();
            assert!(up.doh.is_some() && up.ips.len() == preset.ips.len(), "{}", preset.id);
            assert_eq!(custom(preset.id), None);
        }
    }

    #[test]
    fn a_resolver_of_ones_own() {
        let up = upstream(" https://DNS.example.org:8443/q?x=1 ").unwrap();
        assert_eq!(up.doh, Some(Doh { host: "dns.example.org".into(), port: 8443, path: "/q?x=1".into() }));
        assert!(!up.plain && up.ips.is_empty());
        assert_eq!(upstream("https://dns.example.org").unwrap().doh.unwrap().path, "/dns-query");
        let plain = upstream("1.2.3.4, 2001:db8::1").unwrap();
        assert!(plain.doh.is_none() && plain.plain && plain.ips.len() == 2);
        assert_eq!(label("9.9.9.9").as_deref(), Some("9.9.9.9"));
    }

    #[test]
    fn what_is_not_a_resolver() {
        for bad in ["", "http://dns.example.org/dns-query", "https://", "https://nodot/x", "https://a b.com/x", "dns.example.org", "1.2.3.4 word", "127.0.0.1", "0.0.0.0"] {
            assert_eq!(upstream(bad), None, "{bad}");
        }
    }
}
