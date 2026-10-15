use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Fooling {
    Md5,
    Badsum,
    Seq(i32),
    Ack(i32),
    Ts(i32),
    Ttl(u8),
    AutoTtl { delta: u8, min: u8, max: u8 },
    UnsetAck,
    SetSyn,
}

impl Fooling {
    pub fn arg(&self) -> String {
        match self {
            Self::Md5 => "tcp_md5".into(),
            Self::Badsum => "badsum".into(),
            Self::Seq(n) => format!("tcp_seq={n}"),
            Self::Ack(n) => format!("tcp_ack={n}:tcp_ts_up"),
            Self::Ts(n) => format!("tcp_ts={n}"),
            Self::Ttl(t) => format!("ip_ttl={t}"),
            Self::AutoTtl { delta, min, max } => format!("ip_autottl=-{delta},{min}-{max}"),
            Self::UnsetAck => "tcp_flags_unset=ACK".into(),
            Self::SetSyn => "tcp_flags_set=SYN".into(),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Md5 => "md5".into(),
            Self::Badsum => "badsum".into(),
            Self::Seq(n) => format!("seq{n:+}"),
            Self::Ack(n) => format!("ack{n:+}"),
            Self::Ts(n) => format!("ts{n:+}"),
            Self::Ttl(t) => format!("ttl{t}"),
            Self::AutoTtl { delta, .. } => format!("autottl-{delta}"),
            Self::UnsetAck => "noack".into(),
            Self::SetSyn => "syn".into(),
        }
    }
}

fn fooled(name: &str, inner: &str, fooling: &[Fooling], repeats: u8) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !inner.is_empty() {
        parts.push(inner.into());
    }
    parts.extend(fooling.iter().map(Fooling::label));
    if repeats > 1 {
        parts.push(format!("×{repeats}"));
    }
    format!("{name}({})", parts.join(" "))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Blob {
    DefaultTls,
    DefaultHttp,
    DefaultQuic,
    Zeros,
    Hex(String),
    Named(String),
}

impl Blob {
    pub fn arg(&self) -> String {
        match self {
            Self::DefaultTls => "fake_default_tls".into(),
            Self::DefaultHttp => "fake_default_http".into(),
            Self::DefaultQuic => "fake_default_quic".into(),
            Self::Zeros => "0x00000000".into(),
            Self::Hex(h) => format!("0x{h}"),
            Self::Named(n) => n.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TlsMod {
    Rnd,
    DupSid,
    PadEncap,
    Sni(String),
}

impl TlsMod {
    fn arg(&self) -> String {
        match self {
            Self::Rnd => "rnd".into(),
            Self::DupSid => "dupsid".into(),
            Self::PadEncap => "padencap".into(),
            Self::Sni(s) => format!("sni={s}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Fake {
        blob: Blob,
        fooling: Vec<Fooling>,
        repeats: u8,
        tls_mod: Vec<TlsMod>,
    },
    Multisplit {
        pos: String,
        seqovl: Option<u32>,
        seqovl_pattern: Option<Blob>,
    },
    Multidisorder {
        pos: String,
        seqovl: Option<String>,
    },
    Fakedsplit {
        pos: String,
        fooling: Vec<Fooling>,
        repeats: u8,
    },
    Fakeddisorder {
        pos: String,
        fooling: Vec<Fooling>,
        repeats: u8,
    },
    Hostfakesplit {
        host: Option<String>,
        fooling: Vec<Fooling>,
        repeats: u8,
    },
    Syndata {
        blob: Option<Blob>,
    },
    Wssize {
        wsize: u16,
        scale: u8,
    },
    Oob,
    Udplen {
        increment: i32,
    },
}

fn push_fooling(parts: &mut Vec<String>, fooling: &[Fooling]) {
    parts.extend(fooling.iter().map(Fooling::arg));
}

fn push_repeats(parts: &mut Vec<String>, repeats: u8) {
    if repeats > 1 {
        parts.push(format!("repeats={repeats}"));
    }
}

impl Action {
    pub fn lua_desync(&self) -> String {
        let mut p: Vec<String> = Vec::new();
        match self {
            Self::Fake { blob, fooling, repeats, tls_mod } => {
                p.push("fake".into());
                p.push(format!("blob={}", blob.arg()));
                push_fooling(&mut p, fooling);
                if !tls_mod.is_empty() {
                    let mods: Vec<String> = tls_mod.iter().map(TlsMod::arg).collect();
                    p.push(format!("tls_mod={}", mods.join(",")));
                }
                push_repeats(&mut p, *repeats);
            }
            Self::Multisplit { pos, seqovl, seqovl_pattern } => {
                p.push("multisplit".into());
                p.push(format!("pos={pos}"));
                if let Some(n) = seqovl {
                    p.push(format!("seqovl={n}"));
                }
                if let Some(b) = seqovl_pattern {
                    p.push(format!("seqovl_pattern={}", b.arg()));
                }
            }
            Self::Multidisorder { pos, seqovl } => {
                p.push("multidisorder".into());
                p.push(format!("pos={pos}"));
                if let Some(s) = seqovl {
                    p.push(format!("seqovl={s}"));
                }
            }
            Self::Fakedsplit { pos, fooling, repeats } => {
                p.push("fakedsplit".into());
                p.push(format!("pos={pos}"));
                push_fooling(&mut p, fooling);
                push_repeats(&mut p, *repeats);
            }
            Self::Fakeddisorder { pos, fooling, repeats } => {
                p.push("fakeddisorder".into());
                p.push(format!("pos={pos}"));
                push_fooling(&mut p, fooling);
                push_repeats(&mut p, *repeats);
            }
            Self::Hostfakesplit { host, fooling, repeats } => {
                p.push("hostfakesplit".into());
                if let Some(h) = host {
                    p.push(format!("host={h}"));
                }
                push_fooling(&mut p, fooling);
                push_repeats(&mut p, *repeats);
            }
            Self::Syndata { blob } => {
                p.push("syndata".into());
                if let Some(b) = blob {
                    p.push(format!("blob={}", b.arg()));
                }
            }
            Self::Wssize { wsize, scale } => {
                p.push("wssize".into());
                p.push(format!("wsize={wsize}"));
                p.push(format!("scale={scale}"));
            }
            Self::Oob => p.push("oob".into()),
            Self::Udplen { increment } => {
                p.push("udplen".into());
                p.push(format!("increment={increment}"));
            }
        }
        p.join(":")
    }

    pub fn label(&self) -> String {
        match self {
            Self::Fake { blob, fooling, repeats, tls_mod } => {
                let what = match blob {
                    Blob::Zeros => "zeros".to_string(),
                    Blob::DefaultTls | Blob::DefaultHttp | Blob::DefaultQuic if tls_mod.is_empty() => String::new(),
                    Blob::DefaultTls | Blob::DefaultHttp | Blob::DefaultQuic => {
                        tls_mod.iter().map(TlsMod::arg).collect::<Vec<_>>().join(",")
                    }
                    Blob::Hex(_) => "hex".into(),
                    Blob::Named(n) => n.clone(),
                };
                fooled("fake", &what, fooling, *repeats)
            }
            Self::Multisplit { pos, seqovl, .. } => match seqovl {
                Some(n) => format!("split({pos} ovl{n})"),
                None => format!("split({pos})"),
            },
            Self::Multidisorder { pos, seqovl } => match seqovl {
                Some(n) => format!("disorder({pos} ovl{n})"),
                None => format!("disorder({pos})"),
            },
            Self::Fakedsplit { pos, fooling, repeats } => fooled("fakedsplit", pos, fooling, *repeats),
            Self::Fakeddisorder { pos, fooling, repeats } => fooled("fakeddisorder", pos, fooling, *repeats),
            Self::Hostfakesplit { host, fooling, repeats } => {
                fooled("hostfakesplit", host.as_deref().unwrap_or(""), fooling, *repeats)
            }
            Self::Syndata { .. } => "syndata".into(),
            Self::Wssize { wsize, scale } => format!("wssize({wsize}:{scale})"),
            Self::Oob => "oob".into(),
            Self::Udplen { increment } => format!("udplen({increment:+})"),
        }
    }

    pub fn cost(&self) -> u32 {
        match self {
            Self::Multisplit { .. } | Self::Multidisorder { .. } | Self::Oob | Self::Udplen { .. } => 1,
            Self::Wssize { .. } | Self::Syndata { .. } => 2,
            Self::Fake { repeats, .. } => 2 + *repeats as u32,
            Self::Fakedsplit { repeats, .. }
            | Self::Fakeddisorder { repeats, .. }
            | Self::Hostfakesplit { repeats, .. } => 3 + *repeats as u32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Payload {
    TlsClientHello,
    HttpReq,
    QuicInitial,
    Known,
    All,
}

impl Payload {
    pub fn arg(&self) -> &'static str {
        match self {
            Self::TlsClientHello => "tls_client_hello",
            Self::HttpReq => "http_req",
            Self::QuicInitial => "quic_initial",
            Self::Known => "known",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Recipe {
    pub payload: Payload,
    pub actions: Vec<Action>,
}

impl Recipe {
    pub fn args(&self) -> Vec<String> {
        let mut out = vec![format!("--payload={}", self.payload.arg())];
        out.extend(self.actions.iter().map(|a| format!("--lua-desync={}", a.lua_desync())));
        out
    }

    pub fn cost(&self) -> u32 {
        self.actions.iter().map(Action::cost).sum()
    }

    pub fn is_safe(&self) -> bool {
        self.args().iter().all(|arg| arg.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_=:,+.".contains(&b)))
    }

    pub fn label(&self) -> String {
        self.actions.iter().map(Action::label).collect::<Vec<_>>().join(" + ")
    }
}
