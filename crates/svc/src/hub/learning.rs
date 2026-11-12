// The search for a strategy and the background checks, as the tuner reports them.

use super::*;

impl Hub {
    pub(super) fn stop_learning(&self, inner: &Inner) -> bool {
        if inner.learning.is_none() {
            return false;
        }
        self.stop.send_modify(|n| *n += 1);
        true
    }

    pub fn begin_learning(&self) -> Option<LearnCtx> {
        let mut inner = self.lock();
        if !inner.config.enabled || inner.learning.is_some() || inner.updating {
            return None;
        }
        let problem = self.engine_files().err();
        if let Some(problem) = problem {
            if inner.problem.as_ref() != Some(&problem) {
                let (Problem::Missing(text) | Problem::Failed(text)) = &problem;
                self.emit(Event::new(Level::Error, text.clone()));
                inner.problem = Some(problem);
                self.publish(&mut inner);
            }
            return None;
        }
        let label = network_label(&inner.network);
        let first = inner.memory.is_none();
        let reason = inner.learn_reason.take();
        let text = match &reason {
            _ if first => format!("Подбираю способ обхода: {label}"),
            Some(reason) => format!("Переподбираю способ обхода. {reason}"),
            None => "Переподбираю способ обхода".into(),
        };
        self.emit(Event::new(Level::Info, text));
        let why = match reason {
            _ if first => "Первый запуск в этой сети".to_string(),
            Some(reason) => reason,
            None => "Запущено вручную".to_string(),
        };
        inner.learning = Some(phase_text(Phase::Probing).into());
        inner.learn = Some(Box::new(Learn { phase: Phase::Probing, percent: 0, log: Vec::new(), reason: Some(why) }));
        self.publish(&mut inner);
        Some(LearnCtx {
            network: inner.network.clone(),
            engine_dir: self.engine_dir.clone(),
            extra: inner.extra_hosts(),
            seed: inner.reserve(),
            unfixed: inner.memory.as_ref().map(|m| m.unfixed.clone()).unwrap_or_default(),
        })
    }

    pub fn learn_probed(&self, line: Option<LogLine>, done: usize, total: usize) {
        let mut inner = self.lock();
        let Some(learn) = inner.learn.as_mut() else { return };
        learn.percent = (done * 10 / total.max(1)) as u8;
        if let Some(line) = line {
            push_line(learn, line);
        }
        self.publish_progress(&mut inner);
    }

    pub fn learn_view(&self, view: View, class_no: usize, classes: usize) {
        let mut inner = self.lock();
        let last = class_no + 1 == classes;
        let phase = match view.phase {
            Phase::Done if !last => Phase::Round2,
            other => other,
        };
        let Some(learn) = inner.learn.as_mut() else { return };
        let share = (class_no as f32 + view.percent) / classes.max(1) as f32;
        learn.percent = (10.0 + 90.0 * share).clamp(0.0, 100.0) as u8;
        learn.phase = phase;
        if let Some(line) = view.line {
            push_line(learn, line);
        }
        inner.learning = Some(phase_text(phase).into());
        self.publish_progress(&mut inner);
    }

    pub fn abandon_learning(&self) {
        let mut inner = self.lock();
        inner.learning = None;
        inner.learn = None;
        self.publish(&mut inner);
    }

    /// The user stopped learning (button or pause). The old reserve keeps working and the
    /// background check will not start a new search on its own for a while.
    pub fn learning_stopped(&self) {
        let mut inner = self.lock();
        inner.learning = None;
        inner.learn = None;
        inner.last_learn = Some(Instant::now());
        if inner.config.enabled {
            let text = if inner.memory.is_some() {
                "Подбор остановлен. Работает прежний способ обхода"
            } else {
                "Подбор остановлен. Для этой сети способ обхода не подобран"
            };
            self.emit(Event::new(Level::Info, text));
        }
        self.publish(&mut inner);
    }

    pub fn finish_learning(&self, ctx: &LearnCtx, learned: Learned) {
        let mut inner = self.lock();
        inner.learning = None;
        if let Some(learn) = inner.learn.as_mut() {
            learn.percent = 100;
            learn.phase = if learned.error.is_some() { Phase::Failed } else { Phase::Done };
            if learned.whitelist {
                let text = "Открываются только российские сайты. Обход здесь не поможет".into();
                push_line(learn, LogLine { kind: LineKind::Problem, key: "Белый список".into(), text });
            } else if learned.reserve.is_empty() && learned.unfixed.is_empty() && learned.error.is_none() {
                let text = "Блокировок по DPI нет, обход не нужен".into();
                push_line(learn, LogLine { kind: LineKind::Stage, key: "Итог".into(), text });
            }
        }
        inner.last_learn = Some(Instant::now());
        let current = inner.network.id == ctx.network.id;
        if current {
            inner.whitelist = learned.whitelist;
            inner.checks = learned
                .probes
                .iter()
                .filter(|p| !p.control && p.target.tls && (learned.error.is_some() || !p.outcome.verdict.fixable()))
                .map(|p| (p.target.host.clone(), p.outcome.verdict))
                .collect();
        }
        if let Some(e) = &learned.error {
            self.emit(Event::new(Level::Error, format!("Подбор способа обхода не удался: {e}")));
            self.publish(&mut inner);
            return;
        }
        if learned.whitelist {
            let text = "Похоже на белый список оператора: открываются только разрешённые сайты. Обход здесь не поможет";
            self.emit(Event::new(Level::Warn, text));
            self.publish(&mut inner);
            return;
        }

        let pick = |verdict: Verdict| -> Vec<&Probed> {
            learned.probes.iter().filter(|p| !p.control && p.outcome.verdict == verdict).collect()
        };
        let name_of = |host: &String| targets::name(host);
        let list = |hosts: &[String]| unique(hosts.iter().map(name_of));
        let secs = learned.took.as_secs().max(1);
        if !learned.reserve.is_empty() {
            let best = learned.reserve.tls.first().or(learned.reserve.http.first()).map(|r| r.label()).unwrap_or_default();
            let text = format!(
                "Способ обхода подобран за {secs} с, проверено вариантов: {}. Лучший: {best}. Через обход: {}",
                learned.tried,
                list(&learned.blocked)
            );
            self.emit(Event::new(Level::Info, text));
        } else if learned.unfixed.is_empty() {
            self.emit(Event::new(Level::Info, "Блокировок по DPI не найдено. Обход не нужен"));
        }
        if !learned.unfixed.is_empty() {
            let text = format!("Не удалось подобрать способ обхода для: {}", list(&learned.unfixed));
            self.emit(Event::new(Level::Warn, text));
        }
        let by_ip = pick(Verdict::IpBlocked);
        if !by_ip.is_empty() {
            let text = format!("{}: блокировка по IP-адресу, обход DPI здесь не поможет", tuner::names(&by_ip));
            self.emit(Event::new(Level::Warn, text));
        }
        let dns = pick(Verdict::DnsSpoofed);
        if !dns.is_empty() {
            let text = format!("{}: провайдер подменяет DNS. Поможет DoH в настройках Windows", tuner::names(&dns));
            self.emit(Event::new(Level::Warn, text));
        }

        let blocked = &learned.blocked;
        let direct: BTreeMap<String, u64> = if current {
            inner
                .memory
                .as_ref()
                .map(|m| {
                    m.direct
                        .iter()
                        .filter(|(host, _)| !blocked.iter().any(|b| targets::within(host, b)))
                        .map(|(host, at)| (host.clone(), *at))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Default::default()
        };
        let memory = NetMemory {
            name: ctx.network.name.clone(),
            learned_at: now(),
            blocked: learned.blocked,
            unfixed: learned.unfixed,
            reserve: learned.reserve,
            direct,
        };
        if let Err(e) = memory.save(&ctx.network.id) {
            self.emit(Event::new(Level::Warn, format!("Не удалось сохранить способ обхода: {e:#}")));
        }
        if current {
            inner.memory = Some(memory);
            sync_lists(&mut inner);
            self.apply_locked(&mut inner);
            let _ = self.jobs.send(Job::Check);
        }
        self.publish(&mut inner);
    }

    pub fn begin_check(&self) -> Option<Vec<String>> {
        let mut inner = self.lock();
        if inner.learning.is_some() || inner.checking {
            return None;
        }
        inner.checking = true;
        self.publish(&mut inner);
        Some(inner.extra_hosts())
    }

    /// Returns the catalog services that stopped opening through the bypass, when a new search
    /// would be due. The caller checks them again before blaming the provider.
    pub fn finish_check(&self, probes: Vec<Probed>) -> Vec<Probed> {
        let mut inner = self.lock();
        inner.checking = false;
        let was_whitelist = inner.whitelist;
        inner.whitelist = tuner::whitelist(&probes);
        if inner.whitelist && !was_whitelist {
            let text = "Белый список оператора: открываются только российские сайты. Обход здесь не поможет";
            self.emit(Event::new(Level::Warn, text));
        }
        let sites: Vec<&Probed> = probes.iter().filter(|p| !p.control && p.target.tls).collect();
        inner.checks = sites.iter().map(|p| (p.target.host.clone(), p.outcome.verdict)).collect();

        let working = sites.iter().filter(|p| matches!(p.outcome.verdict, Verdict::Ok | Verdict::DnsSpoofed)).count();
        let failing: Vec<&Probed> = sites.iter().copied().filter(|p| p.outcome.verdict.fixable()).collect();
        if !failing.is_empty() {
            let text = format!("Проверка: открываются {working} из {}. Не открываются: {}", sites.len(), tuner::names(&failing));
            self.emit(Event::new(Level::Warn, text));
        }
        let suspects = if tuner::online(&probes) && watching(&inner) {
            let unfixed: Vec<String> = inner.memory.as_ref().map(|m| m.unfixed.clone()).unwrap_or_default();
            // Sites outside the catalog never start a search: one slow site is no reason to retune the rest.
            failing
                .into_iter()
                .filter(|p| !unfixed.contains(&p.target.host) && targets::service_of(&p.target.host).is_some())
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        self.publish(&mut inner);
        suspects
    }

    /// A repeated probe of sites that failed the check: its results replace the old ones.
    pub fn rechecked(&self, probes: &[Probed]) {
        let mut inner = self.lock();
        for p in probes.iter().filter(|p| !p.control && p.target.tls) {
            inner.checks.insert(p.target.host.clone(), p.outcome.verdict);
        }
        self.publish(&mut inner);
    }

    /// The suspects failed every repeated check too. A new search starts if one is due; when the
    /// strategy was found just now and still does not open them, a search cannot help them: they
    /// are remembered as not fixable and stop starting searches.
    pub fn confirm_relearn(&self, still: &[Probed]) -> bool {
        let mut inner = self.lock();
        if still.is_empty() || !watching(&inner) {
            return false;
        }
        let refs: Vec<&Probed> = still.iter().collect();
        let fresh = inner.memory.as_ref().is_some_and(|m| now().saturating_sub(m.learned_at) < RELEARN_AFTER.as_secs());
        if fresh {
            let network = inner.network.id.clone();
            if let Some(memory) = inner.memory.as_mut() {
                for p in &refs {
                    if !memory.unfixed.contains(&p.target.host) {
                        memory.unfixed.push(p.target.host.clone());
                    }
                }
                memory.unfixed.sort();
                if let Err(e) = memory.save(&network) {
                    tracing::warn!("network memory: {e:#}");
                }
            }
            let text = format!(
                "{}: подобранный способ обхода не помогает, похоже на блокировку по адресу. Переподбор из-за этого не запускается",
                tuner::names(&refs)
            );
            self.emit(Event::new(Level::Warn, text));
            self.publish(&mut inner);
            return false;
        }
        // A search of its own never starts during a game: trial engines and bursts of probes are
        // not worth a lag spike. The first check after the game picks it up.
        if !inner.learn_due() || !inner.procs.is_empty() {
            return false;
        }
        let names: Vec<String> = refs.iter().map(|p| p.name.clone()).collect();
        let others = inner
            .checks
            .iter()
            .any(|(host, v)| matches!(v, Verdict::Ok) && !names.contains(&targets::name(host)));
        let verb = if names.iter().collect::<BTreeSet<_>>().len() == 1 { "перестал" } else { "перестали" };
        let rest = if others { ". Остальные сайты работают как обычно" } else { "" };
        inner.learn_reason = Some(format!("{} {verb} открываться{rest}", tuner::names(&refs)));
        true
    }
}

/// The background check may act on what it finds: the bypass is on and has a strategy to judge.
pub(super) fn watching(inner: &Inner) -> bool {
    inner.config.enabled && !inner.whitelist && inner.learning.is_none() && inner.memory.is_some()
}

pub(super) fn push_line(learn: &mut Learn, line: LogLine) {
    learn.log.push(line);
    if learn.log.len() > LEARN_LOG {
        learn.log.remove(0);
    }
}

pub(super) fn phase_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Probing => "Проверяю, какие сайты режет провайдер",
        Phase::Round1 => "Этап 1 из 2: проверяю варианты обхода",
        Phase::Round2 => "Этап 2 из 2: перепроверяю лучшие варианты",
        Phase::Done => "Готово",
        Phase::Failed => "Подбор не удался",
    }
}
