//! Running the Minecraft server as a supervised child process.
//!
//! The server JVM is launched inside a transient systemd **scope** so the kernel
//! can enforce a hard memory ceiling on the whole process tree
//! (`MemoryMax`/`MemoryHigh`/`MemorySwapMax`). The scope also lets us read the
//! tree's exact current and peak memory from one cgroup file instead of summing
//! `/proc` RSS (which double-counts shared pages).
//!
//! If a user systemd instance is not available we fall back to launching `java`
//! directly, with no hard cap — [`LaunchOutcome::hard_cap`] tells the caller
//! which happened so the UI can warn.
//!
//! This module reports what the process does through [`ServerEvent`]s; it does
//! **not** implement an auto-restart policy. The caller decides whether to
//! restart on [`ServerEvent::Exited`], and must never restart after
//! `oom_killed` (that is an infinite loop).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;

use crate::error::{Error, Result};
use crate::memory::MemoryBudget;

/// The fixed scope name. Fixed (not per-launch) so a surviving JVM from a
/// crashed GUI can always be found and re-attached to or stopped.
pub const SCOPE_UNIT: &str = "mcsm-server.scope";

/// How long to wait after `stop` before escalating to `systemctl stop` / kill.
const GRACEFUL_STOP: Duration = Duration::from_secs(40);

/// Log lines are coalesced and delivered at most this often.
const LOG_FLUSH_INTERVAL: Duration = Duration::from_millis(60);

/// A single log batch never exceeds this many lines (bursty modded startup).
const LOG_BATCH_MAX: usize = 500;

/// Lifecycle state, derived from process state and log parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Starting,
    Running,
    Stopping,
    /// Exited on its own without a stop request.
    Crashed,
}

/// Something the running server did.
#[derive(Debug, Clone)]
pub enum ServerEvent {
    Status(Status),
    /// A coalesced batch of console lines (stdout and stderr merged).
    Log(Vec<String>),
    /// A memory sample for the whole process tree.
    Memory {
        current_mib: u64,
        peak_mib: Option<u64>,
        max_mib: u64,
        high_mib: u64,
    },
    /// The process exited. `oom_killed` is true when the cgroup OOM-killer fired.
    Exited {
        code: Option<i32>,
        oom_killed: bool,
    },
    /// Non-fatal problem worth surfacing (fell back to no hard cap, cgroup files
    /// unreadable, ...).
    Warning(String),
}

/// Everything needed to launch the server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Working directory: `<root>/data/server`.
    pub server_dir: PathBuf,
    /// The `java` binary (absolute path or bare `java`).
    pub java: PathBuf,
    /// JVM arguments before `-jar` (heap, GC flags, user extras).
    pub jvm_args: Vec<String>,
    /// The launcher jar name inside `server_dir`.
    pub launcher_jar: String,
    pub budget: MemoryBudget,
}

impl ServerConfig {
    #[must_use]
    fn program_and_args(&self) -> (String, Vec<String>) {
        let mut args = self.jvm_args.clone();
        args.push("-jar".into());
        args.push(self.launcher_jar.clone());
        args.push("nogui".into());
        (self.java.to_string_lossy().into_owned(), args)
    }
}

/// What [`ServerHandle::start`] achieved.
#[derive(Debug, Clone, Copy)]
pub struct LaunchOutcome {
    /// True when the server runs inside a systemd scope with an enforced cap.
    pub hard_cap: bool,
}

/// A running (or just-launched) server. Dropping it aborts the background
/// tasks; it does **not** kill the server — use [`ServerHandle::stop`].
pub struct ServerHandle {
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    stop_requested: Arc<Notify>,
    tasks: Vec<JoinHandle<()>>,
    used_scope: bool,
}

impl ServerHandle {
    /// Launch the server and start streaming [`ServerEvent`]s into `events`.
    ///
    /// Fails immediately if a server is already running under [`SCOPE_UNIT`].
    pub async fn start(
        config: ServerConfig,
        events: mpsc::Sender<ServerEvent>,
    ) -> Result<(Self, LaunchOutcome)> {
        if scope_active().await {
            return Err(Error::ServerAlreadyRunning {
                unit: SCOPE_UNIT.to_string(),
            });
        }

        let use_scope = systemd_user_available().await;
        if !use_scope {
            let _ = events
                .send(ServerEvent::Warning(
                    "user systemd is unavailable; running without a hard memory cap".into(),
                ))
                .await;
        }

        let mut child = spawn_child(&config, use_scope)?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let _ = events.send(ServerEvent::Status(Status::Starting)).await;

        let stop_requested = Arc::new(Notify::new());
        let mut tasks = Vec::new();

        tasks.push(tokio::spawn(pump_logs(stdout, stderr, events.clone())));

        if use_scope {
            tasks.push(tokio::spawn(sample_memory(
                config.budget,
                events.clone(),
            )));
        }

        tasks.push(tokio::spawn(supervise(
            child,
            use_scope,
            stop_requested.clone(),
            events,
        )));

        Ok((
            Self {
                stdin: Arc::new(Mutex::new(stdin)),
                stop_requested,
                tasks,
                used_scope: use_scope,
            },
            LaunchOutcome {
                hard_cap: use_scope,
            },
        ))
    }

    /// Send a console command (no trailing newline needed).
    pub async fn send_command(&self, line: &str) -> Result<()> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or(Error::ServerNotRunning)?;
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(Error::IoBare)?;
        stdin.flush().await.map_err(Error::IoBare)?;
        Ok(())
    }

    /// Ask the server to shut down cleanly (`stop`), escalating to
    /// `systemctl --user stop` / SIGKILL if it does not exit in time.
    pub async fn stop(&self) {
        let _ = self.send_command("stop").await;
        self.stop_requested.notify_waiters();
    }

    #[must_use]
    pub fn used_scope(&self) -> bool {
        self.used_scope
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn spawn_child(config: &ServerConfig, use_scope: bool) -> Result<Child> {
    let (program, prog_args) = config.program_and_args();

    let mut cmd = if use_scope {
        let mut c = Command::new("systemd-run");
        c.arg("--user")
            .arg("--scope")
            .arg(format!("--unit={SCOPE_UNIT}"))
            .arg("--collect")
            .arg("--quiet")
            .arg(format!(
                "--working-directory={}",
                config.server_dir.display()
            ));
        for prop in config.budget.systemd_properties() {
            c.arg("-p").arg(prop);
        }
        c.arg("--").arg(&program).args(&prog_args);
        c
    } else {
        let mut c = Command::new(&program);
        c.args(&prog_args).current_dir(&config.server_dir);
        c
    };

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);

    cmd.spawn().map_err(|e| Error::Spawn(e.to_string()))
}

/// Merge stdout and stderr, coalescing lines into batches.
async fn pump_logs(
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    events: mpsc::Sender<ServerEvent>,
) {
    let mut out = BufReader::new(stdout).lines();
    let mut err = BufReader::new(stderr).lines();
    let mut batch: Vec<String> = Vec::new();
    let mut ticker = tokio::time::interval(LOG_FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut out_open = true;
    let mut err_open = true;

    while out_open || err_open {
        tokio::select! {
            line = out.next_line(), if out_open => match line {
                Ok(Some(l)) => push_line(&mut batch, l, &events).await,
                _ => out_open = false,
            },
            line = err.next_line(), if err_open => match line {
                Ok(Some(l)) => push_line(&mut batch, l, &events).await,
                _ => err_open = false,
            },
            _ = ticker.tick() => {
                if !batch.is_empty() {
                    let _ = events.send(ServerEvent::Log(std::mem::take(&mut batch))).await;
                }
            }
        }
    }
    if !batch.is_empty() {
        let _ = events.send(ServerEvent::Log(batch)).await;
    }
}

async fn push_line(batch: &mut Vec<String>, line: String, events: &mpsc::Sender<ServerEvent>) {
    if is_ready_line(&line) {
        let _ = events.send(ServerEvent::Status(Status::Running)).await;
    }
    batch.push(line);
    if batch.len() >= LOG_BATCH_MAX {
        let _ = events.send(ServerEvent::Log(std::mem::take(batch))).await;
    }
}

/// Vanilla and Fabric both print this once the world is loaded and the server
/// is accepting connections.
fn is_ready_line(line: &str) -> bool {
    line.contains("Done (") && line.contains("For help, type")
}

/// Poll the scope's cgroup memory files once a second.
async fn sample_memory(budget: MemoryBudget, events: mpsc::Sender<ServerEvent>) {
    let mut cgroup_dir = None;
    for _ in 0..20 {
        if let Some(dir) = scope_cgroup_dir().await {
            cgroup_dir = Some(dir);
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let Some(dir) = cgroup_dir else {
        let _ = events
            .send(ServerEvent::Warning(
                "could not locate the server cgroup; live memory readout unavailable".into(),
            ))
            .await;
        return;
    };

    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        ticker.tick().await;
        let Some(current) = read_cgroup_u64(&dir.join("memory.current")).await else {
            break; // scope gone
        };
        let peak = read_cgroup_u64(&dir.join("memory.peak")).await;
        let _ = events
            .send(ServerEvent::Memory {
                current_mib: current / (1024 * 1024),
                peak_mib: peak.map(|p| p / (1024 * 1024)),
                max_mib: budget.scope_max_mib,
                high_mib: budget.scope_high_mib,
            })
            .await;
    }
}

/// Own the child until it exits; escalate a stop request; report the outcome.
async fn supervise(
    mut child: Child,
    used_scope: bool,
    stop_requested: Arc<Notify>,
    events: mpsc::Sender<ServerEvent>,
) {
    let stop = stop_requested.notified();
    tokio::pin!(stop);

    // Wait for a natural exit, or for a stop request that we then enforce.
    let status = tokio::select! {
        result = child.wait() => result,
        () = &mut stop => {
            let _ = events.send(ServerEvent::Status(Status::Stopping)).await;
            tokio::select! {
                result = child.wait() => result,
                () = tokio::time::sleep(GRACEFUL_STOP) => {
                    let _ = events.send(ServerEvent::Warning(
                        "server did not stop in time; forcing shutdown".into(),
                    )).await;
                    if used_scope {
                        let _ = Command::new("systemctl")
                            .args(["--user", "stop", SCOPE_UNIT])
                            .status().await;
                    }
                    let _ = child.start_kill();
                    child.wait().await
                }
            }
        }
    };

    let oom_killed = if used_scope { scope_oom_killed().await } else { false };
    let code = status.ok().and_then(|s| s.code());
    let _ = events
        .send(ServerEvent::Exited { code, oom_killed })
        .await;
    let final_status = if code == Some(0) {
        Status::Stopped
    } else {
        Status::Crashed
    };
    let _ = events.send(ServerEvent::Status(final_status)).await;
}

// --- systemd / cgroup helpers -------------------------------------------------

/// True if `systemctl --user` can talk to a user manager.
pub async fn systemd_user_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "show", "--property=Version", "--value"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True if the server scope is currently active.
pub async fn scope_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", SCOPE_UNIT])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Stop a server scope left behind by a crashed GUI.
pub async fn stop_orphan_scope() -> Result<()> {
    let status = Command::new("systemctl")
        .args(["--user", "stop", SCOPE_UNIT])
        .status()
        .await
        .map_err(Error::IoBare)?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::msg("systemctl --user stop failed"))
    }
}

async fn scope_cgroup_dir() -> Option<PathBuf> {
    let out = Command::new("systemctl")
        .args([
            "--user",
            "show",
            SCOPE_UNIT,
            "--property=ControlGroup",
            "--value",
        ])
        .output()
        .await
        .ok()?;
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if rel.is_empty() {
        return None;
    }
    let dir = PathBuf::from("/sys/fs/cgroup").join(rel.trim_start_matches('/'));
    dir.is_dir().then_some(dir)
}

async fn read_cgroup_u64(path: &std::path::Path) -> Option<u64> {
    tokio::fs::read_to_string(path)
        .await
        .ok()?
        .trim()
        .parse()
        .ok()
}

async fn scope_oom_killed() -> bool {
    let Some(dir) = scope_cgroup_dir().await else {
        return false;
    };
    let Ok(text) = tokio::fs::read_to_string(dir.join("memory.events")).await else {
        return false;
    };
    text.lines()
        .find_map(|l| l.strip_prefix("oom_kill "))
        .and_then(|n| n.trim().parse::<u64>().ok())
        .is_some_and(|n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_ready_line() {
        assert!(is_ready_line(
            "[12:00:00] [Server thread/INFO]: Done (11.417s)! For help, type \"help\""
        ));
        assert!(!is_ready_line("[12:00:00] [Server thread/INFO]: Preparing spawn area: 74%"));
    }

    #[test]
    fn command_line_has_jar_and_nogui_last() {
        let cfg = ServerConfig {
            server_dir: "/srv/data/server".into(),
            java: "java".into(),
            jvm_args: vec!["-Xmx4096M".into(), "-XX:+UseG1GC".into()],
            launcher_jar: "fabric-server-launch.jar".into(),
            budget: MemoryBudget::default(),
        };
        let (prog, args) = cfg.program_and_args();
        assert_eq!(prog, "java");
        assert_eq!(
            args,
            vec![
                "-Xmx4096M",
                "-XX:+UseG1GC",
                "-jar",
                "fabric-server-launch.jar",
                "nogui"
            ]
        );
    }
}
