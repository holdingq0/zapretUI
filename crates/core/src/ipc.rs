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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Site,
    Launcher,
    Game,
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
    /// Found by the bypass itself, not part of the catalog.
    #[serde(default)]
    pub found: bool,
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

pub fn encode<T: Serialize>(value: &T) -> String {
    let mut s = serde_json::to_string(value).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}
