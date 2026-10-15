use std::sync::OnceLock;

pub struct Icon {
    pub slug: &'static str,
    pub domains: Vec<&'static str>,
    pub path: &'static str,
}

const DATA: &str = include_str!("../../../resources/icons.tsv");

fn all() -> &'static [Icon] {
    static ICONS: OnceLock<Vec<Icon>> = OnceLock::new();
    ICONS.get_or_init(|| {
        DATA.lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .filter_map(|l| {
                let mut parts = l.split('\t');
                let slug = parts.next()?;
                let domains = parts.next()?.split(',').filter(|d| !d.is_empty()).collect();
                Some(Icon { slug, domains, path: parts.next()? })
            })
            .collect()
    })
}

pub fn by_slug(slug: &str) -> Option<&'static Icon> {
    all().iter().find(|i| i.slug == slug)
}

pub fn for_host(host: &str) -> Option<&'static Icon> {
    all().iter().find(|i| i.domains.iter().any(|d| host == *d || host.ends_with(&format!(".{d}"))))
}

pub fn for_site(id: &str, slug: Option<&str>) -> Option<&'static Icon> {
    slug.and_then(by_slug).or_else(|| for_host(id))
}

/// Globe for sites without a brand icon (stroked, not filled).
pub const GLOBE: &str = "M12 2.8a9.2 9.2 0 1 0 0 18.4a9.2 9.2 0 1 0 0-18.4zM2.8 12h18.4M12 2.8c2.6 2.6 3.9 5.6 3.9 9.2s-1.3 6.6-3.9 9.2c-2.6-2.6-3.9-5.6-3.9-9.2s1.3-6.6 3.9-9.2z";
pub const BACK: &str = "M15 6l-6 6l6 6";
pub const WIFI: &str = "M2 8.8a15 15 0 0 1 20 0M5 12.6a10 10 0 0 1 14 0M8.5 16.3a5 5 0 0 1 7 0M12 19.6v0.01";
pub const ETHERNET: &str = "M5 7h14a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2zM8 19v-3M12 19v-3M16 19v-3M9 7V4h6v3";
