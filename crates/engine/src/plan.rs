use std::path::{Path, PathBuf};

use rkn_core::recipe::Recipe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub ports: String,
    pub l7: Vec<String>,
    pub hostlist_auto: Option<PathBuf>,
    pub hostlist: Vec<PathBuf>,
    pub hostlist_domains: Vec<String>,
    pub hostlist_exclude: Vec<PathBuf>,
    pub ipset: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub proto: Proto,
    pub filter: Filter,
    pub recipes: Vec<Recipe>,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub wf_tcp_out: String,
    pub wf_udp_out: String,
    pub raw_filter: Option<String>,
    pub lua_files: Vec<PathBuf>,
    pub debug_log: Option<PathBuf>,
    pub profiles: Vec<Profile>,
}

pub const LUA_FILES: [&str; 3] = ["zapret-lib.lua", "zapret-antidpi.lua", "zapret-auto.lua"];

pub fn lua_files() -> Vec<PathBuf> {
    LUA_FILES.iter().map(|f| PathBuf::from(format!("lua/{f}"))).collect()
}

pub fn port_filter(lo: u16, hi: u16) -> String {
    format!(
        "(tcp.SrcPort >= {lo} and tcp.SrcPort <= {hi}) or (tcp.DstPort >= {lo} and tcp.DstPort <= {hi}) or (udp.SrcPort >= {lo} and udp.SrcPort <= {hi}) or (udp.DstPort >= {lo} and udp.DstPort <= {hi})"
    )
}

pub fn port_exclude_filter(lo: u16, hi: u16) -> String {
    format!(
        "(tcp and ((outbound and (tcp.SrcPort < {lo} or tcp.SrcPort > {hi})) or (inbound and (tcp.DstPort < {lo} or tcp.DstPort > {hi})))) or (udp and ((outbound and (udp.SrcPort < {lo} or udp.SrcPort > {hi})) or (inbound and (udp.DstPort < {lo} or udp.DstPort > {hi}))))"
    )
}

fn path_arg(name: &str, path: &Path) -> String {
    format!("--{name}={}", path.display())
}

impl Profile {
    fn args(&self, first: bool) -> Vec<String> {
        let mut out = Vec::new();
        if first {
            out.push(format!("--name={}", self.name));
        } else {
            out.push(format!("--new={}", self.name));
        }
        let ports = &self.filter.ports;
        match self.proto {
            Proto::Tcp => out.push(format!("--filter-tcp={ports}")),
            Proto::Udp => out.push(format!("--filter-udp={ports}")),
        }
        if !self.filter.l7.is_empty() {
            out.push(format!("--filter-l7={}", self.filter.l7.join(",")));
        }
        for p in &self.filter.ipset {
            out.push(path_arg("ipset", p));
        }
        for p in &self.filter.hostlist {
            out.push(path_arg("hostlist", p));
        }
        if !self.filter.hostlist_domains.is_empty() {
            out.push(format!("--hostlist-domains={}", self.filter.hostlist_domains.join(",")));
        }
        for p in &self.filter.hostlist_exclude {
            out.push(path_arg("hostlist-exclude", p));
        }
        if let Some(p) = &self.filter.hostlist_auto {
            out.push(path_arg("hostlist-auto", p));
        }
        match self.recipes.as_slice() {
            [] => {}
            [single] => out.extend(single.args()),
            many => {
                out.push("--out-range=-s34228".into());
                out.push("--in-range=-s5556".into());
                out.push("--lua-desync=circular".into());
                out.push("--in-range=x".into());
                for (i, recipe) in many.iter().enumerate() {
                    let n = i + 1;
                    for arg in recipe.args() {
                        if arg.starts_with("--lua-desync=") {
                            out.push(format!("{arg}:strategy={n}"));
                        } else {
                            out.push(arg);
                        }
                    }
                }
            }
        }
        out
    }
}

impl Plan {
    pub fn args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.wf_tcp_out.is_empty() {
            out.push(format!("--wf-tcp-out={}", self.wf_tcp_out));
        }
        if !self.wf_udp_out.is_empty() {
            out.push(format!("--wf-udp-out={}", self.wf_udp_out));
        }
        if let Some(f) = &self.raw_filter {
            out.push(format!("--wf-raw-filter={f}"));
        }
        for f in &self.lua_files {
            out.push(format!("--lua-init=@{}", f.display()));
        }
        if let Some(p) = &self.debug_log {
            out.push(format!("--debug=@{}", p.display()));
        }
        for (i, profile) in self.profiles.iter().enumerate() {
            out.extend(profile.args(i == 0));
        }
        out
    }
}
