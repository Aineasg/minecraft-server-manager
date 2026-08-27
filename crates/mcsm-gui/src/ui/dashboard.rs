//! Dashboard: start/stop the server, watch its memory, read the console, and
//! send commands.

use std::path::PathBuf;
use std::time::Duration;

use adw::prelude::*;
use mcsm_core::ops::backup;
use mcsm_core::ops::server::{
    scope_active, stop_orphan_scope, ServerConfig, ServerEvent, ServerHandle, Status, SCOPE_UNIT,
};
use relm4::prelude::*;
use tokio::sync::{mpsc, oneshot};

use crate::context::Context;
use crate::ui::widgets::gib;

/// Restart no more than this many times before giving up.
const MAX_RESTARTS: u32 = 3;

/// Cap on console lines held in the view. When it is exceeded the oldest are
/// dropped back to [`CONSOLE_TRIM_TO`]. This only bounds the scrollback the
/// widget has to render, so a long-running or chatty server can't grow it
/// without limit; the server's own rolling log is on disk in
/// `data/server/logs/` (written by the JVM, whose working directory is
/// `data/server`), so trimming the view loses nothing permanently.
const MAX_CONSOLE_LINES: i32 = 5_000;
const CONSOLE_TRIM_TO: i32 = 4_000;

/// Message from the model to the running supervisor task.
#[derive(Debug)]
enum Control {
    Console(String),
    Stop,
    /// Flush the world and archive it from inside the task that owns the server
    /// handle. Replies with the new backup's file name, or an error string.
    Backup {
        level: String,
        backup_dir: PathBuf,
        auto: bool,
        reply: oneshot::Sender<Result<String, String>>,
    },
}

pub struct DashboardPage {
    ctx: Context,
    status: Status,
    hard_cap: Option<bool>,
    /// Sender into the live supervisor task; `Some` only while running.
    control: Option<mpsc::Sender<Control>>,
    mem_current_mib: u64,
    mem_peak_mib: u64,
    mem_max_mib: u64,
    /// Set while we are deliberately stopping, to suppress auto-restart.
    stopping: bool,
    /// A user-requested restart is in flight: relaunch even on a clean exit.
    pending_restart: bool,
    restart_count: u32,
    /// A server scope is active that this app did not start (previous session,
    /// or launched by hand). We can stop it but cannot stream its console.
    orphan: bool,
    /// A backup task is running; a second one must not start (see [`Self::run_backup`]).
    backup_in_flight: bool,
    /// Set for the duration of a backup when its target folder fell back from
    /// an unwritable configured one, so the outcome message can explain it.
    backup_note: Option<String>,
    console: gtk::TextBuffer,
    console_view: gtk::TextView,
}

#[derive(Debug)]
pub enum DashboardInput {
    Start,
    /// Result of the `java -version` pre-flight kicked off by [`Start`].
    JavaChecked(Result<String, String>),
    Stop,
    Restart,
    SendCommand(String),
    ClearConsole,
    /// Settings or install state changed.
    Reload,
    /// A server scope from outside this app was found on startup.
    OrphanDetected,
    OrphanStopped(Result<(), String>),
    /// Take a manual world backup (routed here so it can flush a live server).
    BackupNow,
    /// The automatic-backup timer fired.
    AutoBackup,
    /// Send a console command to the running server (from the Player access page).
    RunCommand(String),
    BackupFinished {
        auto: bool,
        result: Result<String, String>,
    },
}

#[derive(Debug)]
pub enum DashboardOutput {
    OpenSettings,
    /// A backup attempt finished. `Ok` carries a status line for the Backups
    /// page (and means its list should refresh); `Err` carries the reason it
    /// failed, so the page the user triggered it from can show it.
    BackupDone(Result<String, String>),
}

#[relm4::component(pub)]
impl Component for DashboardPage {
    type Init = Context;
    type Input = DashboardInput;
    type Output = DashboardOutput;
    type CommandOutput = ServerEvent;

    view! {
        adw::PreferencesPage {
            add = &adw::PreferencesGroup {
                set_title: "Server",

                #[wrap(Some)]
                set_header_suffix = &gtk::Box {
                    set_spacing: 6,

                    gtk::Button {
                        set_label: "Start",
                        set_tooltip_text: Some("Launch the server inside a memory-capped systemd scope"),
                        add_css_class: "suggested-action",
                        #[watch]
                        set_sensitive: model.can_start(),
                        connect_clicked => DashboardInput::Start,
                    },
                    gtk::Button {
                        #[watch]
                        set_label: if model.orphan { "Stop external" } else { "Stop" },
                        set_tooltip_text: Some("Send `stop`, then force-stop if it does not exit in time"),
                        #[watch]
                        set_sensitive: model.is_active(),
                        connect_clicked => DashboardInput::Stop,
                    },
                    gtk::Button {
                        set_label: "Restart",
                        set_tooltip_text: Some("Stop the server and start it again with the current settings"),
                        #[watch]
                        set_sensitive: model.control.is_some(),
                        connect_clicked => DashboardInput::Restart,
                    },
                },

                adw::ActionRow {
                    set_title: "Status",
                    set_tooltip_text: Some("Whether the kernel is enforcing the memory ceiling for this run"),
                    #[watch]
                    set_subtitle: model.status_detail(),
                    add_suffix = &gtk::Label {
                        add_css_class: "dim-label",
                        #[watch]
                        set_label: &model.status_text(),
                    },
                },

                adw::ActionRow {
                    set_title: "Memory (whole process tree)",
                    set_tooltip_text: Some("Live cgroup usage vs the hard cap, with the peak so far"),
                    #[watch]
                    set_subtitle: &model.memory_detail(),
                    #[watch]
                    set_visible: model.mem_max_mib > 0,
                    add_suffix = &gtk::LevelBar {
                        set_width_request: 220,
                        set_valign: gtk::Align::Center,
                        #[watch]
                        set_min_value: 0.0,
                        #[watch]
                        set_max_value: model.mem_max_mib.max(1) as f64,
                        #[watch]
                        set_value: model.mem_current_mib as f64,
                    },
                },
            },

            add = &adw::PreferencesGroup {
                set_title: "Console",
                set_vexpand: true,

                #[wrap(Some)]
                set_header_suffix = &gtk::Button {
                    set_label: "Clear",
                    set_tooltip_text: Some("Clear the console view (the server's own log stays in data/server/logs)"),
                    connect_clicked => DashboardInput::ClearConsole,
                },

                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_min_content_height: 260,
                    add_css_class: "card",

                    #[local_ref]
                    console_view -> gtk::TextView {
                        set_editable: false,
                        set_monospace: true,
                        set_cursor_visible: false,
                        set_left_margin: 8,
                        set_right_margin: 8,
                        set_top_margin: 8,
                        set_bottom_margin: 8,
                        set_wrap_mode: gtk::WrapMode::WordChar,
                    },
                },

                gtk::Entry {
                    set_placeholder_text: Some("Type a server command and press Enter (e.g. say hello)"),
                    #[watch]
                    set_sensitive: model.control.is_some(),
                    connect_activate[sender] => move |entry| {
                        let text = entry.text().to_string();
                        if !text.trim().is_empty() {
                            sender.input(DashboardInput::SendCommand(text));
                            entry.set_text("");
                        }
                    },
                },
            },
        }
    }

    fn init(
        ctx: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let console = gtk::TextBuffer::new(None);
        let console_view = gtk::TextView::with_buffer(&console);

        let model = DashboardPage {
            ctx,
            status: Status::Stopped,
            hard_cap: None,
            control: None,
            mem_current_mib: 0,
            mem_peak_mib: 0,
            mem_max_mib: 0,
            stopping: false,
            pending_restart: false,
            restart_count: 0,
            orphan: false,
            backup_in_flight: false,
            backup_note: None,
            console,
            console_view: console_view.clone(),
        };

        // Notice a server scope left behind by a previous (crashed) session or
        // started by hand. We cannot stream its console, but we can stop it.
        {
            let sender = sender.clone();
            relm4::spawn_local(async move {
                if scope_active().await {
                    sender.input(DashboardInput::OrphanDetected);
                }
            });
        }

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            DashboardInput::Start => {
                if self.control.is_some() || self.orphan {
                    return;
                }
                if !self.ctx.state.borrow().ready_to_launch() {
                    self.append("[manager] Server is not installed or the EULA is not accepted — open Settings.\n");
                    let _ = sender.output(DashboardOutput::OpenSettings);
                    return;
                }
                if !self.ctx.state.borrow().budget().feasible {
                    self.append(
                        "[manager] The memory ceiling is too low to run a server — raise it in Settings.\n",
                    );
                    let _ = sender.output(DashboardOutput::OpenSettings);
                    return;
                }
                let java = self.ctx.state.borrow().java_command();
                self.append("[manager] Checking Java…\n");
                let sender = sender.clone();
                relm4::spawn(async move {
                    let r = mcsm_core::ops::server::check_java(&java)
                        .await
                        .map_err(|e| e.to_string());
                    sender.input(DashboardInput::JavaChecked(r));
                });
            }
            DashboardInput::JavaChecked(result) => {
                if self.control.is_some() || self.orphan {
                    return;
                }
                match result {
                    Ok(banner) => {
                        self.append(&format!("[manager] {banner}\n"));
                        self.stopping = false;
                        self.spawn_supervisor(self.ctx.server_config(), &sender);
                    }
                    Err(e) => {
                        self.append(&format!(
                            "[manager] Java check failed: {e}\n\
                             [manager] Set a working Java path in Settings (Java 21+).\n"
                        ));
                        let _ = sender.output(DashboardOutput::OpenSettings);
                    }
                }
            }
            DashboardInput::Stop => {
                if self.orphan {
                    self.append("[manager] Stopping the external server scope…\n");
                    let s = sender.clone();
                    relm4::spawn_local(async move {
                        s.input(DashboardInput::OrphanStopped(
                            stop_orphan_scope().await.map_err(|e| e.to_string()),
                        ));
                    });
                    return;
                }
                self.stopping = true;
                self.pending_restart = false;
                self.restart_count = 0;
                self.send_control(Control::Stop);
            }
            DashboardInput::Restart => {
                self.append("[manager] Restarting…\n");
                self.stopping = false;
                self.pending_restart = true;
                self.restart_count = 0;
                self.send_control(Control::Stop);
            }
            DashboardInput::SendCommand(line) => {
                self.append(&format!("> {line}\n"));
                self.send_control(Control::Console(line));
            }
            DashboardInput::ClearConsole => {
                self.console.set_text("");
            }
            DashboardInput::Reload => {
                // Nothing cached that needs refreshing while stopped; the next
                // Start picks up new settings automatically.
            }
            DashboardInput::OrphanDetected => {
                self.orphan = true;
                self.status = Status::Running;
                self.append(&format!(
                    "[manager] A server is already running under {SCOPE_UNIT} (previous session or started by hand). \
                     Live console and memory are unavailable for it; use “Stop external” to shut it down.\n"
                ));
            }
            DashboardInput::OrphanStopped(result) => match result {
                Ok(()) => {
                    self.orphan = false;
                    self.status = Status::Stopped;
                    self.append("[manager] External server stopped.\n");
                }
                Err(e) => self.append(&format!("[manager] Could not stop it: {e}\n")),
            },
            DashboardInput::BackupNow => self.run_backup(false, &sender),
            DashboardInput::AutoBackup => self.run_backup(true, &sender),
            DashboardInput::RunCommand(cmd) => {
                if self.control.is_some() {
                    self.append(&format!("> {cmd}\n"));
                    self.send_control(Control::Console(cmd));
                } else {
                    self.append(&format!(
                        "[manager] `{cmd}` not sent — the server is not running.\n"
                    ));
                }
            }
            DashboardInput::BackupFinished { auto, result } => {
                self.backup_in_flight = false;
                let kind = if auto { "Auto-backup" } else { "Backup" };
                let note = self.backup_note.take();
                match &result {
                    Ok(name) => self.append(&format!("[manager] {kind}: created {name}\n")),
                    Err(e) => self.append(&format!("[manager] {kind} failed: {e}\n")),
                }
                if auto && result.is_ok() {
                    let keep = self.ctx.state.borrow().auto_backup_keep as usize;
                    match backup::prune_auto(&self.ctx.resolve_backup_dir().path, keep) {
                        Ok(n) if n > 0 => {
                            self.append(&format!("[manager] Pruned {n} old auto-backup(s).\n"));
                        }
                        Err(e) => {
                            self.append(&format!("[manager] Auto-backup prune failed: {e}\n"))
                        }
                        _ => {}
                    }
                }
                // Report the outcome to the page the user triggered it from —
                // the Backups page otherwise never learns why nothing appeared.
                let outcome = match result {
                    Ok(name) => Ok(match note {
                        Some(n) => format!("Backed up {name} — {n}"),
                        None => format!("Backed up {name}"),
                    }),
                    Err(e) => Err(match note {
                        Some(n) => format!("{e} ({n})"),
                        None => e,
                    }),
                };
                let _ = sender.output(DashboardOutput::BackupDone(outcome));
            }
        }
        self.scroll_console();
    }

    fn update_cmd(
        &mut self,
        event: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match event {
            ServerEvent::Launched { hard_cap } => {
                self.hard_cap = Some(hard_cap);
                self.status = Status::Starting;
                self.append(if hard_cap {
                    "[manager] Launched inside a systemd scope with an enforced memory cap.\n"
                } else {
                    "[manager] user systemd unavailable — running WITHOUT a hard memory cap.\n"
                });
            }
            ServerEvent::LaunchFailed(why) => {
                self.control = None;
                self.status = Status::Crashed;
                self.stopping = false;
                self.pending_restart = false;
                self.restart_count = 0;
                self.append(&format!("[manager] Could not start the server: {why}\n"));
            }
            ServerEvent::Status(s) => self.status = s,
            ServerEvent::Log(lines) => {
                let mut text = lines.join("\n");
                text.push('\n');
                self.append(&text);
            }
            ServerEvent::Memory {
                current_mib,
                peak_mib,
                max_mib,
                ..
            } => {
                self.mem_current_mib = current_mib;
                self.mem_peak_mib = peak_mib.unwrap_or(self.mem_peak_mib.max(current_mib));
                self.mem_max_mib = max_mib;
            }
            ServerEvent::Warning(w) => self.append(&format!("[manager] {w}\n")),
            ServerEvent::Exited { code, oom_killed } => {
                self.control = None;
                self.mem_current_mib = 0;
                let clean = code == Some(0);

                if oom_killed {
                    self.append("[manager] Server was OOM-killed. Lower the heap or remove mods before restarting.\n");
                    self.restart_count = 0;
                } else if clean {
                    self.append("[manager] Server stopped.\n");
                    self.restart_count = 0;
                } else {
                    self.append(&format!(
                        "[manager] Server exited unexpectedly (code {}).\n",
                        code.map_or_else(|| "signal".into(), |c| c.to_string())
                    ));
                }

                let auto = self.ctx.state.borrow().auto_restart;
                let want_restart =
                    self.pending_restart || (!self.stopping && !clean && !oom_killed && auto);
                self.pending_restart = false;

                if want_restart && self.restart_count < MAX_RESTARTS {
                    self.restart_count += 1;
                    let delay = Duration::from_secs(2u64.saturating_mul(self.restart_count.into()));
                    self.append(&format!(
                        "[manager] Auto-restart {}/{MAX_RESTARTS} in {}s…\n",
                        self.restart_count,
                        delay.as_secs()
                    ));
                    let sender = sender.clone();
                    relm4::spawn_local(async move {
                        tokio::time::sleep(delay).await;
                        sender.input(DashboardInput::Start);
                    });
                } else if want_restart {
                    self.append("[manager] Giving up after repeated failures.\n");
                    self.restart_count = 0;
                }
                self.stopping = false;
            }
        }
        self.scroll_console();
    }
}

impl DashboardPage {
    fn can_start(&self) -> bool {
        let state = self.ctx.state.borrow();
        // An infeasible ceiling clamps the heap to zero, and `-Xmx0M` fails at
        // launch with a raw JVM error. Refuse here instead — Settings already
        // explains that the ceiling is too low.
        self.control.is_none() && !self.orphan && state.ready_to_launch() && state.budget().feasible
    }

    fn is_active(&self) -> bool {
        self.control.is_some() || self.orphan
    }

    fn status_text(&self) -> String {
        match self.status {
            Status::Stopped => "Stopped",
            Status::Starting => "Starting…",
            Status::Running => "Running",
            Status::Stopping => "Stopping…",
            Status::Crashed => "Crashed",
        }
        .to_string()
    }

    fn status_detail(&self) -> &'static str {
        match self.hard_cap {
            Some(true) => "Hard memory cap: enforced by systemd",
            Some(false) => "Hard memory cap: unavailable (no user systemd)",
            None => "Not started yet",
        }
    }

    fn memory_detail(&self) -> String {
        if self.mem_max_mib == 0 {
            return "Waiting for the first sample…".into();
        }
        format!(
            "{} / {}  ·  peak {}",
            gib(self.mem_current_mib),
            gib(self.mem_max_mib),
            gib(self.mem_peak_mib),
        )
    }

    fn append(&self, text: &str) {
        let mut end = self.console.end_iter();
        self.console.insert(&mut end, text);

        if self.console.line_count() > MAX_CONSOLE_LINES {
            let mut start = self.console.start_iter();
            if let Some(mut cut) = self
                .console
                .iter_at_line(self.console.line_count() - CONSOLE_TRIM_TO)
            {
                self.console.delete(&mut start, &mut cut);
            }
        }
    }

    fn scroll_console(&self) {
        let mut end = self.console.end_iter();
        self.console_view
            .scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
    }

    fn send_control(&self, msg: Control) {
        if let Some(tx) = &self.control {
            let tx = tx.clone();
            relm4::spawn_local(async move {
                let _ = tx.send(msg).await;
            });
        }
    }

    /// Take a world backup. When a server is running this hands the job to the
    /// supervisor task so it can `save-all flush` first; otherwise it archives
    /// straight from disk. Either way the outcome comes back as
    /// [`DashboardInput::BackupFinished`].
    fn run_backup(&mut self, auto: bool, sender: &ComponentSender<Self>) {
        // `backup::create` documents that it is never run concurrently — it
        // sweeps stray `.part` files on entry, so two overlapping runs would
        // delete each other's scratch file, and same-second runs collide on the
        // archive name. Nothing else serialises them: the manual button and the
        // auto timer both land here and each spawns a detached task. This is
        // the one place that starts a backup, so the guard belongs here.
        if self.backup_in_flight {
            // The in-flight run will emit its own `BackupDone`, which clears the
            // Backups page's "busy" state — so this request is simply folded
            // into it rather than reported as a failure (the auto timer firing
            // mid-backup is normal, not an error).
            self.append("[manager] A backup is already running — skipping this one.\n");
            return;
        }
        self.backup_in_flight = true;

        let level = backup::level_name(&self.ctx.paths);
        // Use the folder that is actually writable now — the configured one may
        // point at a drive that is not mounted, or a path from another machine.
        let resolved = self.ctx.resolve_backup_dir();
        let backup_dir = resolved.path;
        if let Some(configured) = resolved.fell_back_from {
            self.append(&format!(
                "[manager] Backup folder {} is not writable — using {} instead.\n",
                configured.display(),
                backup_dir.display(),
            ));
            self.backup_note = Some(format!(
                "configured folder {} was not writable, saved to {}",
                configured.display(),
                backup_dir.display(),
            ));
        }
        self.append(&format!(
            "[manager] {} backup starting…\n",
            if auto { "Automatic" } else { "Manual" }
        ));

        if let Some(control) = &self.control {
            let control = control.clone();
            let sender = sender.clone();
            relm4::spawn(async move {
                let (reply_tx, reply_rx) = oneshot::channel();
                let sent = control
                    .send(Control::Backup {
                        level,
                        backup_dir,
                        auto,
                        reply: reply_tx,
                    })
                    .await
                    .is_ok();
                let result = if sent {
                    reply_rx
                        .await
                        .unwrap_or_else(|_| Err("backup task was dropped".into()))
                } else {
                    Err("server task is no longer running".into())
                };
                sender.input(DashboardInput::BackupFinished { auto, result });
            });
        } else {
            let paths = self.ctx.paths.clone();
            let sender = sender.clone();
            relm4::spawn(async move {
                let result = backup::create(&paths, &backup_dir, &level, auto)
                    .await
                    .map(|entry| entry.file_name)
                    .map_err(|e| e.to_string());
                sender.input(DashboardInput::BackupFinished { auto, result });
            });
        }
    }

    fn spawn_supervisor(&mut self, config: ServerConfig, sender: &ComponentSender<Self>) {
        let (control_tx, mut control_rx) = mpsc::channel::<Control>(32);
        self.control = Some(control_tx);
        let paths = self.ctx.paths.clone();

        sender.command(move |out, shutdown| {
            shutdown
                .register(async move {
                    let (evt_tx, mut evt_rx) = mpsc::channel::<ServerEvent>(256);
                    let (handle, _outcome) = match ServerHandle::start(config, evt_tx).await {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = out.send(ServerEvent::LaunchFailed(e.to_string()));
                            return;
                        }
                    };

                    // The model drops its `Control` sender as soon as it sees
                    // `Exited`, so `control_rx` closes while the event stream is
                    // still draining its final Status/Log messages. A closed
                    // `recv()` resolves instantly on every poll, so this branch
                    // must be disabled rather than merely ignored — otherwise
                    // `select!` picks it every iteration and the task spins a
                    // core until `evt_rx` closes (which, if the cgroup outlives
                    // the JVM, may be a long time). Breaking instead would drop
                    // the handle and lose those last console lines.
                    let mut control_open = true;

                    loop {
                        tokio::select! {
                            maybe_ev = evt_rx.recv() => match maybe_ev {
                                Some(ev) => {
                                    if out.send(ev).is_err() {
                                        break;
                                    }
                                }
                                None => break,
                            },
                            maybe_ctl = control_rx.recv(), if control_open => match maybe_ctl {
                                Some(Control::Console(line)) => {
                                    let _ = handle.send_command(&line).await;
                                }
                                Some(Control::Stop) => handle.stop().await,
                                Some(Control::Backup { level, backup_dir, auto, reply }) => {
                                    let _ = handle.send_command("save-all flush").await;
                                    tokio::time::sleep(Duration::from_secs(2)).await;
                                    let r = backup::create(&paths, &backup_dir, &level, auto)
                                        .await
                                        .map(|entry| entry.file_name)
                                        .map_err(|e| e.to_string());
                                    let _ = reply.send(r);
                                }
                                None => control_open = false,
                            },
                        }
                    }
                    drop(handle);
                })
                .drop_on_shutdown()
        });
    }
}
