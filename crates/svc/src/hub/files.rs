// The files the service keeps next to winws2: the event log and the host lists the engine reads.

use super::*;

pub(super) fn events_file() -> PathBuf {
    paths::state_dir().join("events.jsonl")
}

pub(super) fn read_autohostlist() -> BTreeSet<String> {
    std::fs::read_to_string(paths::autohostlist_file())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// The event log is for reading by hand: keep its last lines only.
pub(super) fn trim_events() {
    let text = std::fs::read_to_string(events_file()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > 2 * EVENTS_KEPT {
        let body: String = lines[lines.len() - EVENTS_KEPT..].iter().map(|l| format!("{l}\n")).collect();
        let _ = paths::write_atomic(&events_file(), &body);
    }
}

pub(super) fn touch(path: PathBuf) {
    if !path.exists() {
        let _ = paths::write_atomic(&path, "");
    }
}

pub(super) fn fresh_direct(memory: &NetMemory) -> impl Iterator<Item = &String> {
    let now = now();
    memory.direct.iter().filter(move |(_, at)| now.saturating_sub(**at) < DIRECT_TTL).map(|(h, _)| h)
}

pub(super) fn sync_lists(inner: &mut Inner) {
    let memory = inner.memory.as_ref();
    let mut probed: Vec<String> = memory.map(|m| m.blocked.iter().flat_map(|h| targets::expand(h)).collect()).unwrap_or_default();
    let mut direct: Vec<String> = memory.map(|m| fresh_direct(m).cloned().collect()).unwrap_or_default();
    // Services that are bypassed on every network join the list without a probe.
    probed.extend(targets::catalog().service.iter().filter(|s| s.always).flat_map(|s| s.domains.iter().cloned()));
    probed.sort();
    probed.dedup();
    direct.sort();
    direct.dedup();
    let ipset = memory.map(|m| targets::hosting_ipset(&m.blocked)).unwrap_or_default();
    inner.has_probed = !probed.is_empty();
    inner.has_direct = !direct.is_empty();
    inner.has_ipset = !ipset.is_empty();
    for (path, list) in [(paths::probed_file(), probed), (paths::direct_file(), direct), (engine::ipset_file(), ipset)] {
        let body: String = list.iter().map(|h| format!("{h}\n")).collect();
        if let Err(e) = paths::write_atomic(&path, &body) {
            tracing::warn!("{}: {e}", path.display());
        }
    }
}

pub(super) fn drop_from_autohostlist(hosts: &[String]) {
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

pub(super) fn unique(names: impl IntoIterator<Item = String>) -> String {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out.join(", ")
}

pub(super) fn network_label(network: &Network) -> String {
    if network.name.is_empty() { "текущая сеть".into() } else { format!("сеть «{}»", network.name) }
}
