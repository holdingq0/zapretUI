use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use crate::job::Job;
use crate::plan::Plan;

// No console at all: CREATE_NO_WINDOW would still start a hidden conhost.exe next to every winws2.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
const READY_LINE: &str = "windivert initialized";
const READY_TIMEOUT: Duration = Duration::from_secs(8);
const KEEP_LINES: usize = 20;

#[derive(Default)]
struct Output {
    ready: AtomicBool,
    lines: Mutex<VecDeque<String>>,
}

impl Output {
    fn last(&self) -> Option<String> {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        lines.iter().rev().find(|l| !l.trim().is_empty()).cloned()
    }
}

fn pump(reader: impl Read + Send + 'static, output: Arc<Output>) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if line.contains(READY_LINE) {
                output.ready.store(true, Ordering::Release);
            }
            let mut lines = output.lines.lock().unwrap_or_else(|e| e.into_inner());
            lines.push_back(line);
            while lines.len() > KEEP_LINES {
                lines.pop_front();
            }
        }
    });
}

pub struct Winws2 {
    dir: PathBuf,
    child: Option<Child>,
    job: Option<Job>,
    background: bool,
}

impl Winws2 {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), child: None, job: None, background: false }
    }

    /// Below-normal priority, for throwaway instances that must not compete with games for the CPU.
    pub fn background(mut self) -> Self {
        self.background = true;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn exe(&self) -> PathBuf {
        self.dir.join("winws2.exe")
    }

    pub fn lua(&self, name: &str) -> PathBuf {
        self.dir.join("lua").join(name)
    }

    pub fn installed(&self) -> bool {
        ["winws2.exe", "cygwin1.dll", "WinDivert.dll", "WinDivert64.sys"]
            .iter()
            .all(|f| self.dir.join(f).is_file())
    }

    pub fn start(&mut self, plan: &Plan) -> anyhow::Result<u32> {
        self.stop();
        if !self.installed() {
            bail!("winws2 не найден в {}", self.dir.display());
        }
        let job = Job::new().context("job object")?;
        let args = plan.args();
        tracing::debug!("winws2 {}", args.join(" "));
        let mut child = Command::new(self.exe())
            .current_dir(&self.dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(DETACHED_PROCESS | if self.background { BELOW_NORMAL_PRIORITY_CLASS } else { 0 })
            .spawn()
            .context("запуск winws2")?;
        if let Err(e) = job.assign(&child) {
            let _ = child.kill();
            return Err(e);
        }
        let output = Arc::new(Output::default());
        if let Some(out) = child.stdout.take() {
            pump(out, output.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump(err, output.clone());
        }
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                let reason = output.last().unwrap_or_else(|| status.to_string());
                bail!("winws2 завершился сразу после запуска: {reason}");
            }
            if output.ready.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(150));
                break;
            }
            if Instant::now() >= deadline {
                tracing::warn!("winws2 did not report readiness in {READY_TIMEOUT:?}");
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if let Some(status) = child.try_wait()? {
            let reason = output.last().unwrap_or_else(|| status.to_string());
            bail!("winws2 завершился сразу после запуска: {reason}");
        }
        let pid = child.id();
        tracing::debug!(pid, "winws2 started");
        self.child = Some(child);
        self.job = Some(job);
        Ok(pid)
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            tracing::debug!("winws2 stopped");
        }
        self.job = None;
    }

    pub fn running(&mut self) -> bool {
        match self.child.as_mut().map(|c| c.try_wait()) {
            Some(Ok(None)) => true,
            Some(_) => {
                self.child = None;
                self.job = None;
                false
            }
            None => false,
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
}

impl Drop for Winws2 {
    fn drop(&mut self) {
        self.stop();
    }
}
