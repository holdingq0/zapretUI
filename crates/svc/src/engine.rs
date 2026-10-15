use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use rkn_core::paths;
use rkn_core::recipe::Recipe;
use rkn_core::reserve::Reserve;
use rkn_core::sites::{SiteMode, Sites};
use rkn_engine::{Filter, Plan, Profile, Proto, lua_files, port_exclude_filter};
use rkn_search::RESERVED_PORTS;
use sha2::{Digest, Sha256};
use windows_service::service::{ServiceAccess, ServiceState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const SUMS: &str = include_str!("../../../resources/winws2/SHA256SUMS");
const VERSION: &str = include_str!("../../../resources/winws2/VERSION");

pub const EXTRA_FILES: [&str; 2] = ["VERSION", "LICENSE-zapret2.txt"];

pub fn version() -> &'static str {
    VERSION.trim()
}

pub fn manifest() -> impl Iterator<Item = (&'static str, &'static str)> {
    SUMS.lines().filter_map(|line| {
        let (hash, name) = line.split_once(' ')?;
        Some((hash.trim(), name.trim().trim_start_matches('*')))
    })
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn verify(dir: &Path) -> anyhow::Result<()> {
    for (hash, name) in manifest() {
        let got = sha256_file(&dir.join(name)).with_context(|| format!("нет файла {name}"))?;
        if got != hash {
            bail!("файл {name} не совпадает с официальной сборкой {}", version());
        }
    }
    Ok(())
}

/// winws2 runs in its engine folder and gets the lists by relative path: "../state/x" when installed,
/// "../data/state/x" in a portable copy.
fn from_engine_dir(path: PathBuf) -> PathBuf {
    let engine = paths::engine_dir();
    let base = engine.parent().unwrap_or(&engine);
    match path.strip_prefix(base) {
        Ok(rel) => PathBuf::from("..").join(rel),
        Err(_) => path,
    }
}

pub const VOICE_PORTS: &str = "19294-19344,50000-50100";

pub fn build_plan(sites: &Sites, reserve: &Reserve, probed: bool, direct: bool) -> Option<Plan> {
    let has = |mode: SiteMode| sites.modes.values().any(|m| *m == mode);
    let host_filter = |ports: &str, l7: &[&str]| {
        let mut filter = Filter {
            ports: ports.into(),
            l7: l7.iter().map(|s| s.to_string()).collect(),
            ..Filter::default()
        };
        filter.hostlist_auto = Some(from_engine_dir(paths::autohostlist_file()));
        if has(SiteMode::Always) {
            filter.hostlist.push(from_engine_dir(paths::always_file()));
        }
        if probed {
            filter.hostlist.push(from_engine_dir(paths::probed_file()));
        }
        if direct {
            filter.hostlist_exclude.push(from_engine_dir(paths::direct_file()));
        }
        if has(SiteMode::Never) {
            filter.hostlist_exclude.push(from_engine_dir(paths::exclude_file()));
        }
        filter
    };

    let candidates: [(&str, Proto, Filter, &Vec<Recipe>); 4] = [
        ("tls", Proto::Tcp, host_filter("443", &["tls"]), &reserve.tls),
        ("http", Proto::Tcp, host_filter("80", &["http"]), &reserve.http),
        ("quic", Proto::Udp, host_filter("443", &["quic"]), &reserve.quic),
        (
            "voice",
            Proto::Udp,
            Filter {
                ports: VOICE_PORTS.into(),
                l7: vec!["discord".into(), "stun".into()],
                ..Filter::default()
            },
            &reserve.voice,
        ),
    ];

    let profiles: Vec<Profile> = candidates
        .into_iter()
        .filter(|(_, _, _, recipes)| !recipes.is_empty())
        .map(|(name, proto, filter, recipes)| Profile {
            name: name.into(),
            proto,
            filter,
            recipes: recipes.clone(),
        })
        .collect();
    if profiles.is_empty() {
        return None;
    }

    let ports = |proto: Proto| {
        let mut list: Vec<&str> = profiles
            .iter()
            .filter(|p| p.proto == proto)
            .map(|p| p.filter.ports.as_str())
            .collect();
        list.dedup();
        list.join(",")
    };
    Some(Plan {
        wf_tcp_out: ports(Proto::Tcp),
        wf_udp_out: ports(Proto::Udp),
        raw_filter: Some(port_exclude_filter(RESERVED_PORTS.0, RESERVED_PORTS.1)),
        lua_files: lua_files(),
        debug_log: None,
        profiles,
    })
}

pub fn unload_driver() {
    let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else {
        return;
    };
    let Ok(service) = manager.open_service("WinDivert", ServiceAccess::STOP | ServiceAccess::QUERY_STATUS) else {
        return;
    };
    match service.query_status() {
        Ok(status) if status.current_state == ServiceState::Stopped => {}
        _ => match service.stop() {
            Ok(_) => tracing::info!("WinDivert driver unloaded"),
            Err(e) => tracing::warn!("WinDivert driver stays loaded: {e}"),
        },
    }
}
