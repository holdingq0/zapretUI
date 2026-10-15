use std::collections::HashSet;

use rkn_core::recipe::{Action, Blob, Fooling, Payload, Recipe, TlsMod};

use crate::priors::Priors;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    Tls,
    Http,
}

impl Class {
    pub fn payload(self) -> Payload {
        match self {
            Self::Tls => Payload::TlsClientHello,
            Self::Http => Payload::HttpReq,
        }
    }

    pub fn l7(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::Http => "http",
        }
    }

    pub fn port(self) -> u16 {
        match self {
            Self::Tls => 443,
            Self::Http => 80,
        }
    }

    fn blob(self) -> Blob {
        match self {
            Self::Tls => Blob::DefaultTls,
            Self::Http => Blob::DefaultHttp,
        }
    }

    fn splits(self) -> &'static [&'static str] {
        match self {
            Self::Tls => &[
                "2",
                "1",
                "sniext+1",
                "sniext+4",
                "host+1",
                "midsld",
                "1,midsld",
                "1,midsld,1220",
                "1,sniext+1,host+1,midsld-2,midsld,midsld+2,endhost-1",
            ],
            Self::Http => &["method+2", "midsld", "method+2,midsld"],
        }
    }

    fn ovl_splits(self) -> &'static [&'static str] {
        match self {
            Self::Tls => &["1", "10", "10,sniext+1", "10,sniext+4", "10,midsld"],
            Self::Http => &["method+2", "method+2,midsld"],
        }
    }

    fn ovl_pairs(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Tls => &[("1", "2"), ("sniext", "sniext+1"), ("sniext+3", "sniext+4"), ("midsld-1", "midsld"), ("1", "2,midsld")],
            Self::Http => &[("method+1", "method+2"), ("midsld-1", "midsld"), ("method+1", "method+2,midsld")],
        }
    }

    fn ovl_size(self) -> u32 {
        match self {
            Self::Tls => 681,
            Self::Http => 100,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub recipe: Recipe,
    pub features: Vec<(&'static str, String)>,
    pub group: String,
    pub prior: f64,
}

const REPEATS: [u8; 3] = [1, 6, 11];

fn foolings() -> Vec<(String, Vec<Fooling>)> {
    let mut out: Vec<(String, Vec<Fooling>)> = vec![
        ("ts".into(), vec![Fooling::Ts(-1000)]),
        ("md5".into(), vec![Fooling::Md5]),
        ("seq".into(), vec![Fooling::Seq(-3000)]),
        ("seqbig".into(), vec![Fooling::Seq(1_000_000)]),
        ("ack".into(), vec![Fooling::Ack(-66000)]),
        ("badsum".into(), vec![Fooling::Badsum]),
        ("noack".into(), vec![Fooling::UnsetAck]),
        ("syn".into(), vec![Fooling::SetSyn]),
    ];
    out.extend((1..=5).map(|delta| (format!("autottl{delta}"), vec![Fooling::AutoTtl { delta, min: 3, max: 20 }])));
    out.extend((1..=12).map(|ttl| (format!("ttl{ttl}"), vec![Fooling::Ttl(ttl)])));
    out
}

fn split(pos: &str) -> Action {
    Action::Multisplit { pos: pos.into(), seqovl: None, seqovl_pattern: None }
}

fn disorder(pos: &str) -> Action {
    Action::Multidisorder { pos: pos.into(), seqovl: None }
}

fn fake(blob: Blob, fooling: &[Fooling], repeats: u8, tls_mod: Vec<TlsMod>) -> Action {
    Action::Fake { blob, fooling: fooling.to_vec(), repeats, tls_mod }
}

struct Builder<'a> {
    class: Class,
    priors: &'a Priors,
    seen: HashSet<Recipe>,
    out: Vec<Candidate>,
}

impl Builder<'_> {
    fn add(&mut self, actions: Vec<Action>, family: &str, fooling: Option<&str>, extra: &[(&'static str, &str)]) {
        let recipe = Recipe { payload: self.class.payload(), actions };
        if !self.seen.insert(recipe.clone()) {
            return;
        }
        let mut features: Vec<(&'static str, String)> = vec![("family", family.into())];
        if let Some(f) = fooling {
            features.push(("fooling", f.into()));
        }
        features.extend(extra.iter().map(|(k, v)| (*k, (*v).to_string())));
        let weight: f64 = features.iter().map(|(k, v)| self.priors.weight(k, v)).product();
        let prior = weight / (1.0 + 0.03 * f64::from(recipe.cost()));
        let base = fooling.map(|f| f.trim_end_matches(|c: char| c.is_ascii_digit())).unwrap_or("");
        let group = format!("{family}:{base}");
        self.out.push(Candidate { recipe, features, group, prior });
    }
}

pub fn candidates(class: Class, priors: &Priors) -> Vec<Candidate> {
    let mut b = Builder { class, priors, seen: HashSet::new(), out: Vec::new() };
    let blob = class.blob();

    for pos in class.splits() {
        b.add(vec![split(pos)], "split", None, &[("pos", pos)]);
        b.add(vec![disorder(pos)], "disorder", None, &[("pos", pos)]);
    }
    for pos in class.ovl_splits() {
        let plain = Action::Multisplit { pos: (*pos).into(), seqovl: Some(1), seqovl_pattern: None };
        b.add(vec![plain], "seqovl", None, &[("pos", pos), ("ovl", "1")]);
        let patterned =
            Action::Multisplit { pos: (*pos).into(), seqovl: Some(class.ovl_size()), seqovl_pattern: Some(blob.clone()) };
        b.add(vec![patterned], "seqovl", None, &[("pos", pos), ("ovl", "pattern")]);
    }
    for (first, second) in class.ovl_pairs() {
        let action = Action::Multidisorder { pos: (*second).into(), seqovl: Some((*first).into()) };
        b.add(vec![action], "disorder_ovl", None, &[("pos", second), ("ovl", "pair")]);
    }

    for (key, fooling) in &foolings() {
        let key = Some(key.as_str());
        for repeats in REPEATS {
            let rep = repeats.to_string();
            let rep = rep.as_str();
            let plain = fake(blob.clone(), fooling, repeats, vec![]);
            b.add(vec![plain.clone()], "fake", key, &[("blob", "fake"), ("repeats", rep), ("mod", "none")]);
            let zeros = fake(Blob::Zeros, fooling, repeats, vec![]);
            b.add(vec![zeros], "fake", key, &[("blob", "zeros"), ("repeats", rep)]);
            if class == Class::Tls {
                let mods = [
                    ("rnd_dupsid", vec![TlsMod::Rnd, TlsMod::DupSid]),
                    ("padencap", vec![TlsMod::Rnd, TlsMod::DupSid, TlsMod::PadEncap]),
                ];
                for (mod_key, tls_mod) in mods {
                    let modded = fake(blob.clone(), fooling, repeats, tls_mod);
                    b.add(vec![modded], "fake", key, &[("blob", "fake"), ("repeats", rep), ("mod", mod_key)]);
                }
            }
            for pos in class.splits() {
                let extra = [("pos", *pos), ("repeats", rep)];
                b.add(vec![plain.clone(), split(pos)], "fake_split", key, &extra);
                b.add(vec![plain.clone(), disorder(pos)], "fake_disorder", key, &extra);
                let faked = Action::Fakedsplit { pos: (*pos).into(), fooling: fooling.clone(), repeats };
                b.add(vec![faked], "fakedsplit", key, &extra);
                let faked = Action::Fakeddisorder { pos: (*pos).into(), fooling: fooling.clone(), repeats };
                b.add(vec![faked], "fakeddisorder", key, &extra);
            }
            let hostfake = Action::Hostfakesplit { host: None, fooling: fooling.clone(), repeats };
            b.add(vec![hostfake], "hostfakesplit", key, &[("repeats", rep)]);
        }
    }
    b.out
}
