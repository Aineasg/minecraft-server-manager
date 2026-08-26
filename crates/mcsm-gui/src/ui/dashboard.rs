//! Dashboard: start/stop the server, watch its memory, read the console, and
//! send commands.

use std::time::Duration;

use adw::prelude::*;
use mcsm_core::ops::server::{scope_active, ServerConfig, ServerEvent, ServerHandle, Status};
use relm4::prelude::*;
use tokio::sync::mpsc;

use crate::context::Context;
use crate::ui::widgets::gib;

/// Restart no more than this many times before giving up.
const MAX_RESTARTS: u32 = 3;

/// Message from the model to the running supervisor task.
#[derive(Debug)]
enum Control {
    Console(String),
    Stop,
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
    console: gtk::TextBuffer,
    console_view: gtk::TextView,
}

#[derive(Debug)]
pub enum DashboardInput {
    Start,
    Stop,
    Restart,
    SendCommand(String),
    ClearConsole,
    /// Settings or install state changed.
    Reload,
}

#[derive(Debug)]
pub enum DashboardOutput {
    OpenSettings,
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
                        add_css_class: "suggested-action",
                        #[watch]
                        set_sensitive: model.can_start(),
                        connect_clicked => DashboardInput::Start,
                    },
                    gtk::Button {
                        set_label: "Stop",
                        #[watch]
                        set_sensitive: model.is_active(),
                        connect_clicked => DashboardInput::Stop,
                    },
                    gtk::Button {
                        set_label: "Restart",
                        #[watch]
                        set_sensitive: model.is_active(),
                        connect_clicked => DashboardInput::Restart,
                    },
                },

                adw::ActionRow {
                    set_title: "Status",
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
                    set_sensitive: model.status == Status::Running,
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
            console,
            console_view: console_view.clone(),
        };

        // Adopt a server left running by a previous (crashed) session.
        {
            let sender = sender.clone();
            relm4::spawn_local(async move {
                if scope_active().await {
                    sender.input(DashboardInput::Start);
                }
            });
        }

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            DashboardInput::Start => {
                if self.control.is_some() {
                    return;
                }
                if !self.ctx.state.borrow().ready_to_launch() {
                    self.append("[manager] Server is not installed or the EULA is not accepted — open Settings.\n");
                    let _ = sender.output(DashboardOutput::OpenSettings);
                    return;
                }
                self.stopping = false;
                self.spawn_supervisor(self.ctx.server_config(), &sender);
            }
            DashboardInput::Stop => {
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
                let want_restart = self.pending_restart
                    || (!self.stopping && !clean && !oom_killed && auto);
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
        self.control.is_none() && self.ctx.state.borrow().ready_to_launch()
    }

    fn is_active(&self) -> bool {
        self.control.is_some()
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

    fn spawn_supervisor(&mut self, config: ServerConfig, sender: &ComponentSender<Self>) {
        let (control_tx, mut control_rx) = mpsc::channel::<Control>(32);
        self.control = Some(control_tx);

        sender.command(move |out, shutdown| {
            shutdown.register(async move {
                let (evt_tx, mut evt_rx) = mpsc::channel::<ServerEvent>(256);
                let (handle, _outcome) = match ServerHandle::start(config, evt_tx).await {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = out.send(ServerEvent::Warning(format!("launch failed: {e}")));
                        let _ = out.send(ServerEvent::Exited {
                            code: None,
                            oom_killed: false,
                        });
                        return;
                    }
                };

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
                        maybe_ctl = control_rx.recv() => match maybe_ctl {
                            Some(Control::Console(line)) => {
                                let _ = handle.send_command(&line).await;
                            }
                            Some(Control::Stop) => handle.stop().await,
                            None => {}
                        },
                    }
                }
                drop(handle);
            })
            .drop_on_shutdown()
        });
    }
}
