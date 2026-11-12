// Requests from the window.

use super::*;

impl Hub {
    pub fn handle(&self, request: Request) -> Message {
        let mut inner = self.lock();
        let reply = match request {
            Request::Status | Request::Subscribe => return Message::Status(Box::new(status(&mut inner))),
            Request::GetConfig => return Message::Config(inner.config.clone()),
            Request::SetEnabled { enabled } => {
                if inner.config.enabled != enabled {
                    inner.config.enabled = enabled;
                    let text = match enabled {
                        true => "Обход включён",
                        false if self.stop_learning(&inner) => "Обход поставлен на паузу, подбор остановлен",
                        false => "Обход поставлен на паузу. Драйвер выгружен",
                    };
                    self.apply_locked(&mut inner);
                    self.emit(Event::new(Level::Info, text));
                    if enabled {
                        let job = if inner.memory.is_none() { Job::Learn } else { Job::Check };
                        let _ = self.jobs.send(job);
                    }
                }
                inner.config.save()
            }
            Request::SetConfig { mut config } => {
                config.check_interval_min = config.check_interval_min.clamp(2, 60);
                // Pause has its own request; a settings screen opened earlier must not undo it.
                config.enabled = inner.config.enabled;
                inner.config = config;
                inner.config.save()
            }
            Request::SetSiteMode { host, mode } => {
                let Some(host) = normalize_host(&host) else {
                    return Message::Error { message: "Это не похоже на адрес сайта".into() };
                };
                let group = targets::group(&host);
                let (host, name) = (group.id, group.name);
                if inner.sites.mode(&host) != mode {
                    let before = inner.plan().map(|p| p.args());
                    inner.sites.set(host.clone(), mode);
                    let text = match mode {
                        SiteMode::Auto => format!("{name}: обход включается автоматически"),
                        SiteMode::Always => format!("{name}: всегда через обход"),
                        SiteMode::Never => format!("{name}: всегда напрямую"),
                    };
                    let saved = inner.sites.save(&targets::expand);
                    // winws2 rereads the list files it already has; only a new command line (the first
                    // "always" or "never" site, voice for Discord) needs a restart.
                    if inner.plan().map(|p| p.args()) != before {
                        self.apply_locked(&mut inner);
                    }
                    self.emit(Event::new(Level::Info, text).host(host));
                    saved
                } else {
                    Ok(())
                }
            }
            Request::Relearn => {
                if inner.learning.is_some() {
                    self.emit(Event::new(Level::Info, "Подбор способа обхода уже идёт"));
                } else {
                    if !inner.config.enabled {
                        inner.config.enabled = true;
                        self.apply_locked(&mut inner);
                        self.emit(Event::new(Level::Info, "Обход включён для подбора"));
                    }
                    let _ = self.jobs.send(Job::Learn);
                }
                inner.config.save()
            }
            Request::StopLearning => {
                self.stop_learning(&inner);
                Ok(())
            }
            Request::Shutdown if paths::portable() => {
                self.quit();
                Ok(())
            }
            Request::Shutdown => return Message::Error { message: "Служба выключается только из системы".into() },
            Request::CheckUpdate | Request::InstallUpdate if !update::enabled() => {
                return Message::Error { message: "В этой сборке обновления выключены".into() };
            }
            Request::CheckUpdate => {
                let _ = self.updates.send(Ask::Check);
                Ok(())
            }
            Request::InstallUpdate => {
                let _ = self.updates.send(Ask::Install);
                Ok(())
            }
        };
        self.publish(&mut inner);
        match reply {
            Ok(()) => Message::Ok,
            Err(e) => Message::Error { message: format!("{e:#}") },
        }
    }
}
