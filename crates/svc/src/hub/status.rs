// What the window is told: the overall state and one row per catalog service.

use super::*;

pub(super) fn status(inner: &mut Inner) -> Status {
    let running = inner.up;
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
        // Hostings are background work: the window does not complain about them.
        None if !inner.planned && unfixed.iter().any(|h| !targets::background(h)) => (
            State::Tuning,
            Some(format!(
                "Не удалось подобрать способ обхода для: {}",
                unique(unfixed.iter().filter(|h| !targets::background(h)).map(|h| targets::name(h)))
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
    let mut ai: Vec<SiteStatus> = Vec::new();
    for service in &targets::catalog().service {
        let id = service.id();
        let found = groups.remove(&id);
        if service.kind == Kind::Ai {
            ai.push(ai_row(inner, id, service));
            continue;
        }
        if service.kind == Kind::Site && service.probe.is_empty() && found.is_none() {
            continue;
        }
        // Hostings are probed and bypassed like the rest, but "Сайты на OVH" tells a user nothing.
        if service.ipset.is_some() {
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
        dns: inner.sysdns.clone(),
        ai,
        learn: inner.learn.clone(),
    }
}

/// A neural network: the bypass has no say in it. The row says whether it lets this computer in now.
fn ai_row(inner: &Inner, id: String, service: &targets::Service) -> SiteStatus {
    let state = inner.ai.get(&id).copied().unwrap_or(SiteState::Checking);
    SiteStatus {
        id,
        name: service.name.clone(),
        state,
        mode: SiteMode::Auto,
        kind: service.kind,
        icon: service.icon.clone(),
        running: false,
        via: false,
    }
}

pub(super) fn row(
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
