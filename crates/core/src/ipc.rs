use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::sites::SiteMode;

pub const PIPE_NAME: &str = "zapretUI.svc";
pub const SERVICE_NAME: &str = "zapretUI";
pub const LEGACY_SERVICE_NAME: &str = "rkn";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    SetEnabled { enabled: bool },
    GetConfig,
    SetConfig { config: Config },
    SetSiteMode { host: String, mode: SiteMode },
    Relearn,
    StopLearning,
    CheckUpdate,
    InstallUpdate,
    /// Portable copy only: the window is closing, stop the bypass and exit.
    Shutdown,
    Subscribe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Message {
    Status(Box<Status>),
    Config(Config),
    Event(Event),
    Ok,
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Working,
    Paused,
    Learning,
    Tuning,
    Whitelist,
    EngineMissing,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteState {
    Checking,
    Direct,
    Bypassed,
    Tuning,
    Partial,
    Blocked,
    IpBlocked,
    DnsSpoofed,
    Geoblocked,
    /// A neural network that lets this computer in right now.
    Open,
    /// The name of a neural network gets no answer from DNS.
    DnsDown,
    /// Not checked yet. Also what a state added by a newer service reads as: a window that is a
    /// version behind must still understand the status, or it never learns it has to hand over.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Site,
    Launcher,
    Game,
    /// A neural network: closed for Russia by the service itself, opened by DNS and not by the bypass.
    Ai,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteStatus {
    pub id: String,
    pub name: String,
    pub state: SiteState,
    pub mode: SiteMode,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub via: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Probing,
    Round1,
    Round2,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Site,
    Stage,
    Chosen,
    Spare,
    Problem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub kind: LineKind,
    pub key: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Learn {
    pub phase: Phase,
    pub percent: u8,
    pub log: Vec<LogLine>,
    /// Why this search runs: a new network, a service that stopped opening, or the button.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Link {
    Wifi,
    Ethernet,
    #[default]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub network: Option<String>,
    #[serde(default)]
    pub link: Link,
    pub sites: Vec<SiteStatus>,
    pub message: Option<String>,
    #[serde(default)]
    pub learn: Option<Box<Learn>>,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub update: Update,
    #[serde(default)]
    pub portable: bool,
    #[serde(default)]
    pub dns: SystemDns,
    /// The neural networks. Rows of their own and not part of `sites`: their kind and states are
    /// newer than the windows of 0.1.5, which skip a field they do not know and would choke on an
    /// unknown state inside `sites`.
    #[serde(default)]
    pub ai: Vec<SiteStatus>,
}

/// The DNS Windows itself is set to, for the settings screen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemDns {
    pub servers: Vec<String>,
    /// DoH addresses set for those servers in Windows.
    pub doh: Vec<String>,
    /// It answers for the neural networks with a relay of its own: they open without a resolver
    /// chosen in the program.
    pub relay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    /// No release source configured in this build.
    #[default]
    Off,
    Idle,
    Checking,
    Latest,
    Available,
    Downloading,
    Installing,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Update {
    pub state: UpdateState,
    /// The newest release seen, when it is newer than this build.
    pub version: Option<String>,
    pub error: Option<String>,
    pub checked_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub at: u64,
    pub level: Level,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl Event {
    pub fn new(level: Level, text: impl Into<String>) -> Self {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self { at, level, text: text.into(), host: None }
    }

    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status as the window of 0.1.5 reads it: the states it knew and nothing else.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum OldState {
        Unknown,
        Checking,
        Direct,
        Bypassed,
        Tuning,
        Partial,
        Blocked,
        IpBlocked,
        DnsSpoofed,
        Geoblocked,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum OldKind {
        Site,
        Launcher,
        Game,
    }

    #[derive(Debug, Deserialize)]
    struct OldSite {
        #[allow(dead_code)]
        state: OldState,
        #[allow(dead_code)]
        kind: OldKind,
    }

    #[derive(Debug, Deserialize)]
    struct OldStatus {
        sites: Vec<OldSite>,
        version: String,
    }

    fn row(state: SiteState, kind: Kind) -> SiteStatus {
        SiteStatus { id: "a.com".into(), name: "A".into(), state, mode: SiteMode::Auto, kind, icon: None, running: false, via: false }
    }

    #[test]
    fn an_older_window_still_reads_the_status() {
        let status = Status {
            state: State::Working,
            network: None,
            link: Link::Other,
            sites: vec![row(SiteState::Bypassed, Kind::Site), row(SiteState::Unknown, Kind::Game)],
            message: None,
            learn: None,
            version: "9.9.9".into(),
            update: Update::default(),
            portable: false,
            dns: SystemDns { servers: vec!["1.2.3.4".into()], doh: Vec::new(), relay: true },
            ai: vec![row(SiteState::Open, Kind::Ai), row(SiteState::DnsDown, Kind::Ai)],
        };
        let wire = encode(&Message::Status(Box::new(status)));
        let value: serde_json::Value = serde_json::from_str(&wire).unwrap();
        let old: OldStatus = serde_json::from_value(value["data"].clone()).expect("the window of 0.1.5 must read this");
        assert_eq!(old.sites.len(), 2);
        assert_eq!(old.version, "9.9.9");
    }

    #[test]
    fn a_state_from_a_newer_service_reads_as_unknown() {
        let state: SiteState = serde_json::from_str("\"something_new\"").unwrap();
        assert_eq!(state, SiteState::Unknown);
    }
}

pub fn encode<T: Serialize>(value: &T) -> String {
    let mut s = serde_json::to_string(value).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}
