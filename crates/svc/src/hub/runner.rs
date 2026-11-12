// Running winws2: orders for the engine thread and the thread itself.

use super::*;

/// An order for the engine thread. A newer one replaces an order not yet taken.
#[derive(Debug, Clone)]
pub(super) enum Order {
    /// Stop winws2; `unload` also takes the WinDivert driver out of the kernel.
    Stop { unload: bool },
    /// Start winws2 with this plan, stopping the one that runs. `retry`: a restart after a crash,
    /// whose failures `tick` counts.
    Run { plan: Plan, retry: bool },
}

impl Hub {
    pub fn apply(&self) {
        let mut inner = self.lock();
        self.apply_locked(&mut inner);
        self.publish(&mut inner);
    }

    pub(super) fn engine_files(&self) -> Result<(), Problem> {
        if !Winws2::new(self.engine_dir.clone()).installed() {
            return Err(Problem::Missing("Движок не установлен. Переустановите zapretUI".into()));
        }
        engine::verify(&self.engine_dir)
            .map_err(|e| Problem::Failed(format!("Файлы движка повреждены: {e:#}. Переустановите zapretUI")))
    }

    /// Hands an order to the engine thread. Until it is carried out the engine counts as not running.
    pub(super) fn order(&self, inner: &mut Inner, order: Order) {
        inner.order = Some(order);
        inner.busy = true;
        inner.up = false;
        let _ = self.kick.send(());
    }

    /// The bypass has caught up with the last change of settings: probes through it mean something.
    pub fn engine_settled(&self) -> bool {
        !self.lock().busy
    }

    /// Brings the engine in line with the settings, the network memory and the site lists. Only
    /// decides: winws2 itself is stopped and started by the engine thread.
    pub(super) fn apply_locked(&self, inner: &mut Inner) {
        // The update is replacing engine files: resume_after_update applies the settings afterwards.
        if inner.updating {
            return;
        }
        inner.retry_at = None;
        inner.crashes = 0;
        inner.last_error = None;
        let was = inner.problem.take();
        if !inner.config.enabled {
            self.order(inner, Order::Stop { unload: true });
            return;
        }
        inner.restart_at = None;
        let Some(plan) = inner.plan() else {
            inner.planned = false;
            self.order(inner, Order::Stop { unload: true });
            return;
        };
        inner.planned = true;
        match self.engine_files() {
            Ok(()) => {
                touch(paths::autohostlist_file());
                self.order(inner, Order::Run { plan, retry: false });
            }
            Err(problem) => {
                inner.problem = Some(problem);
                self.order(inner, Order::Stop { unload: false });
            }
        }
        if inner.problem != was {
            if let Some(Problem::Missing(text) | Problem::Failed(text)) = inner.problem.clone() {
                self.emit(Event::new(Level::Error, text));
            }
        }
    }

    /// The engine thread: the only place winws2 is started. A start waits for the driver for seconds,
    /// and none of that happens under the state lock: the pipe and the window stay live meanwhile.
    pub fn run_engine(&self) {
        let Some(kicks) = self.kicks.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        loop {
            let _ = kicks.recv_timeout(ENGINE_POLL);
            while kicks.try_recv().is_ok() {}
            // The engine lock first and the order under it: whoever stops the engine for good (an
            // update, the shutdown) clears the order and then waits for this lock, so no start slips in.
            let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            let order = self.lock().order.take();
            match order {
                None => {
                    let up = engine.running();
                    drop(engine);
                    let mut inner = self.lock();
                    if !inner.busy && inner.up != up {
                        inner.up = up;
                        self.publish(&mut inner);
                    }
                }
                Some(Order::Stop { unload }) => {
                    engine.stop();
                    drop(engine);
                    if unload {
                        engine::unload_driver();
                    }
                    let mut inner = self.lock();
                    if inner.order.is_none() {
                        inner.busy = false;
                        self.publish(&mut inner);
                    }
                }
                Some(Order::Run { plan, retry }) => {
                    let started = engine.start(&plan);
                    drop(engine);
                    let mut inner = self.lock();
                    // A newer order is waiting, or an update is about to stop the engine: this start
                    // is history, and its outcome must not schedule anything.
                    if inner.order.is_some() || inner.updating {
                        continue;
                    }
                    inner.busy = false;
                    inner.up = started.is_ok();
                    match (started, retry) {
                        (Ok(_), false) => {}
                        (Ok(_), true) => {
                            inner.last_error = None;
                            inner.restarted_at = Some(Instant::now());
                        }
                        // `tick` sees the engine down, counts the crash and schedules the next try.
                        (Err(e), true) => inner.last_error = Some(format!("{e:#}")),
                        (Err(e), false) => {
                            let text = format!("{e:#}");
                            self.emit(Event::new(Level::Warn, format!("winws2 не запустился: {text}. Повтор через 2 с")));
                            inner.last_error = Some(text);
                            inner.crashes = 1;
                            inner.retry_at = Some(Instant::now() + Duration::from_secs(2));
                        }
                    }
                    self.publish(&mut inner);
                }
            }
        }
    }

    /// Everything that holds engine files stops: the bypass and a search that may be running.
    /// Returns once winws2 is gone; a start that was under way is waited for and undone.
    pub fn stop_for_update(&self) {
        {
            let mut inner = self.lock();
            inner.updating = true;
            inner.order = None;
            inner.busy = false;
            inner.up = false;
            self.stop_learning(&inner);
            self.publish(&mut inner);
        }
        self.engine.lock().unwrap_or_else(|e| e.into_inner()).stop();
    }

    pub fn resume_after_update(&self) {
        let mut inner = self.lock();
        inner.updating = false;
        self.apply_locked(&mut inner);
        self.publish(&mut inner);
    }

    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.lock().order = None;
        let _ = self.kick.send(());
        self.engine.lock().unwrap_or_else(|e| e.into_inner()).stop();
        engine::unload_driver();
    }

    /// Trial instances of winws2 are gone; on pause the driver may unload now.
    pub fn trials_gone(&self) {
        let inner = self.lock();
        if !inner.config.enabled {
            engine::unload_driver();
        }
    }
}
