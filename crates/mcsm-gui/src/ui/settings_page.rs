//! Settings: pick the Minecraft/Fabric version and install it, tune the memory
//! budget and JVM, point at a `java` binary, and accept the EULA.

use adw::prelude::*;
use mcsm_core::memory::MemoryBudget;
use mcsm_core::net::fabric::{self, GameVersion};
use mcsm_core::net::Http;
use mcsm_core::ops::install::{self, InstallPlan, InstallProgress};
use mcsm_core::state::GcPreset;
use relm4::prelude::*;

use crate::context::{Context, LAUNCHER_JAR};
use crate::ui::widgets::gib;

pub struct SettingsPage {
    ctx: Context,
    game_versions: Vec<GameVersion>,
    loader_versions: Vec<String>,
    installer_version: Option<String>,
    /// Minecraft ids currently offered (respecting the snapshot toggle).
    mc_choices: Vec<String>,
    selected_mc: Option<String>,
    selected_loader: Option<String>,
    installing: bool,
    status_line: String,
}

#[derive(Debug)]
pub enum SettingsInput {
    MetaLoaded(Result<Meta, String>),
    McSelected(u32),
    LoaderSelected(u32),
    SnapshotsToggled(bool),
    EulaToggled(bool),
    AutoRestartToggled(bool),
    GcSelected(u32),
    TotalCeilingChanged(f64),
    HeapChanged(f64),
    JavaPathEdited(String),
    Install,
    InstallProgress(String),
    InstallFinished(Result<(), String>),
    SaveJvm,
}

#[derive(Debug)]
pub enum SettingsOutput {
    /// Settings that affect other pages / the banner changed.
    Changed,
    /// A server install completed.
    Installed,
}

#[derive(Debug, Clone)]
pub struct Meta {
    game: Vec<GameVersion>,
    loaders: Vec<String>,
    installer: String,
}

#[relm4::component(pub)]
impl Component for SettingsPage {
    type Init = Context;
    type Input = SettingsInput;
    type Output = SettingsOutput;
    type CommandOutput = SettingsInput;

    view! {
        adw::PreferencesPage {
            add = &adw::PreferencesGroup {
                set_title: "Server version",
                #[watch]
                set_description: Some(model.status_line.as_str()),

                #[name = "mc_row"]
                adw::ComboRow {
                    set_title: "Minecraft version",
                    #[watch]
                    #[block_signal(mc_selected)]
                    set_model: Some(&string_list(&model.mc_choices)),
                    #[watch]
                    #[block_signal(mc_selected)]
                    set_selected: model.selected_index(&model.mc_choices, &model.selected_mc),
                    connect_selected_notify[sender] => move |row| {
                        sender.input(SettingsInput::McSelected(row.selected()));
                    } @mc_selected,
                },

                #[name = "loader_row"]
                adw::ComboRow {
                    set_title: "Fabric loader",
                    #[watch]
                    #[block_signal(loader_selected)]
                    set_model: Some(&string_list(&model.loader_versions)),
                    #[watch]
                    #[block_signal(loader_selected)]
                    set_selected: model.selected_index(&model.loader_versions, &model.selected_loader),
                    connect_selected_notify[sender] => move |row| {
                        sender.input(SettingsInput::LoaderSelected(row.selected()));
                    } @loader_selected,
                },

                adw::SwitchRow {
                    set_title: "Show Minecraft snapshots",
                    #[watch]
                    set_active: model.ctx.state.borrow().allow_snapshots,
                    connect_active_notify[sender] => move |row| {
                        sender.input(SettingsInput::SnapshotsToggled(row.is_active()));
                    },
                },

                adw::ActionRow {
                    set_title: "Install / reinstall server",
                    set_subtitle: "Downloads the vanilla server jar and the Fabric launcher into data/server",
                    add_suffix = &gtk::Button {
                        set_valign: gtk::Align::Center,
                        set_label: "Install",
                        add_css_class: "suggested-action",
                        #[watch]
                        set_sensitive: !model.installing
                            && model.selected_mc.is_some()
                            && model.selected_loader.is_some()
                            && model.installer_version.is_some(),
                        connect_clicked => SettingsInput::Install,
                    },
                },
            },

            add = &adw::PreferencesGroup {
                set_title: "Memory",
                #[watch]
                set_description: Some(model.budget_description().as_str()),

                #[name = "ceiling_row"]
                adw::SpinRow {
                    set_title: "Total ceiling (GiB)",
                    set_subtitle: "Hard limit for the app, the JVM and the world combined",
                    set_adjustment: Some(&gtk::Adjustment::new(9.0, 2.0, 64.0, 1.0, 1.0, 0.0)),
                    #[watch]
                    #[block_signal(ceiling_changed)]
                    set_value: model.ctx.state.borrow().memory.total_mib as f64 / 1024.0,
                    connect_value_notify[sender] => move |row| {
                        sender.input(SettingsInput::TotalCeilingChanged(row.value()));
                    } @ceiling_changed,
                },

                #[name = "heap_row"]
                adw::SpinRow {
                    set_title: "Java heap -Xmx (MiB)",
                    #[watch]
                    set_subtitle: &format!(
                        "Allowed range {}–{} MiB for this ceiling",
                        model.budget().xmx_min_mib.min(model.budget().xmx_max_mib),
                        model.budget().xmx_max_mib
                    ),
                    #[watch]
                    #[block_signal(heap_changed)]
                    set_adjustment: Some(&gtk::Adjustment::new(
                        model.budget().xmx_mib as f64,
                        model.budget().xmx_min_mib.min(model.budget().xmx_max_mib) as f64,
                        model.budget().xmx_max_mib as f64,
                        256.0, 256.0, 0.0,
                    )),
                    #[watch]
                    #[block_signal(heap_changed)]
                    set_value: model.budget().xmx_mib as f64,
                    connect_value_notify[sender] => move |row| {
                        sender.input(SettingsInput::HeapChanged(row.value()));
                    } @heap_changed,
                },

                #[name = "gc_row"]
                adw::ComboRow {
                    set_title: "GC flags",
                    set_model: Some(&string_list(&["Aikar's flags (no AlwaysPreTouch)", "Basic (-Xms/-Xmx only)"])),
                    #[watch]
                    #[block_signal(gc_selected)]
                    set_selected: match model.ctx.state.borrow().gc_preset { GcPreset::Aikar => 0, GcPreset::Basic => 1 },
                    connect_selected_notify[sender] => move |row| {
                        sender.input(SettingsInput::GcSelected(row.selected()));
                    } @gc_selected,
                },

                adw::SwitchRow {
                    set_title: "Auto-restart on crash",
                    set_subtitle: "Never restarts after an out-of-memory kill",
                    #[watch]
                    set_active: model.ctx.state.borrow().auto_restart,
                    connect_active_notify[sender] => move |row| {
                        sender.input(SettingsInput::AutoRestartToggled(row.is_active()));
                    },
                },
            },

            add = &adw::PreferencesGroup {
                set_title: "Java",

                #[name = "java_row"]
                adw::EntryRow {
                    set_title: "java binary (blank = use PATH)",
                    #[watch]
                    #[block_signal(java_edited)]
                    set_text: &model.ctx.state.borrow().java_path
                        .as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                    connect_changed[sender] => move |row| {
                        sender.input(SettingsInput::JavaPathEdited(row.text().to_string()));
                    } @java_edited,
                },

                adw::ActionRow {
                    set_title: "Apply Java & memory changes",
                    add_suffix = &gtk::Button {
                        set_valign: gtk::Align::Center,
                        set_label: "Save",
                        connect_clicked => SettingsInput::SaveJvm,
                    },
                },
            },

            add = &adw::PreferencesGroup {
                set_title: "Minecraft EULA",

                adw::SwitchRow {
                    set_title: "I accept the Minecraft EULA",
                    set_subtitle: "https://aka.ms/MinecraftEULA — required before the server can start",
                    #[watch]
                    set_active: model.ctx.state.borrow().eula_accepted,
                    connect_active_notify[sender] => move |row| {
                        sender.input(SettingsInput::EulaToggled(row.is_active()));
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
        let state = ctx.state.borrow();
        let model = SettingsPage {
            selected_mc: state.minecraft_version.clone(),
            selected_loader: state.loader_version.clone(),
            installer_version: state.installer_version.clone(),
            mc_choices: Vec::new(),
            game_versions: Vec::new(),
            loader_versions: Vec::new(),
            installing: false,
            status_line: "Loading available versions…".to_string(),
            ctx: ctx.clone(),
        };
        drop(state);

        let http = ctx.http.clone();
        sender.oneshot_command(async move { SettingsInput::MetaLoaded(load_meta(&http).await) });

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            SettingsInput::MetaLoaded(Ok(meta)) => {
                self.game_versions = meta.game;
                self.loader_versions = meta.loaders;
                self.installer_version = Some(meta.installer);
                self.refresh_mc_choices();
                if self.selected_mc.is_none() {
                    self.selected_mc = self.mc_choices.first().cloned();
                }
                if self.selected_loader.is_none() {
                    self.selected_loader = self.loader_versions.first().cloned();
                }
                self.status_line = "Pick a version and press Install.".to_string();
            }
            SettingsInput::MetaLoaded(Err(e)) => {
                self.status_line = format!("Could not load versions: {e}");
            }
            SettingsInput::McSelected(idx) => {
                self.selected_mc = self.mc_choices.get(idx as usize).cloned();
            }
            SettingsInput::LoaderSelected(idx) => {
                self.selected_loader = self.loader_versions.get(idx as usize).cloned();
            }
            SettingsInput::SnapshotsToggled(on) => {
                self.ctx.state.borrow_mut().allow_snapshots = on;
                let _ = self.ctx.save_state();
                self.refresh_mc_choices();
            }
            SettingsInput::EulaToggled(on) => {
                self.ctx.state.borrow_mut().eula_accepted = on;
                let _ = self.ctx.save_state();
                self.sync_eula_file();
                let _ = sender.output(SettingsOutput::Changed);
            }
            SettingsInput::AutoRestartToggled(on) => {
                self.ctx.state.borrow_mut().auto_restart = on;
                let _ = self.ctx.save_state();
            }
            SettingsInput::GcSelected(idx) => {
                self.ctx.state.borrow_mut().gc_preset =
                    if idx == 0 { GcPreset::Aikar } else { GcPreset::Basic };
                let _ = self.ctx.save_state();
            }
            SettingsInput::TotalCeilingChanged(gib_value) => {
                let mib = (gib_value * 1024.0).round() as u64;
                let mut st = self.ctx.state.borrow_mut();
                st.memory.total_mib = mib;
                // Re-clamp any explicit heap request into the new budget.
                if let Some(x) = st.memory.xmx_mib {
                    let b = MemoryBudget::new(mib, Some(x));
                    st.memory.xmx_mib = Some(b.xmx_mib);
                }
            }
            SettingsInput::HeapChanged(mib) => {
                let total = self.ctx.state.borrow().memory.total_mib;
                let b = MemoryBudget::new(total, Some(mib.round() as u64));
                self.ctx.state.borrow_mut().memory.xmx_mib = Some(b.xmx_mib);
            }
            SettingsInput::JavaPathEdited(text) => {
                let trimmed = text.trim();
                self.ctx.state.borrow_mut().java_path =
                    (!trimmed.is_empty()).then(|| trimmed.into());
            }
            SettingsInput::SaveJvm => {
                let _ = self.ctx.save_state();
                self.status_line = "Java & memory settings saved.".to_string();
                let _ = sender.output(SettingsOutput::Changed);
            }
            SettingsInput::Install => {
                let (Some(mc), Some(loader), Some(installer)) = (
                    self.selected_mc.clone(),
                    self.selected_loader.clone(),
                    self.installer_version.clone(),
                ) else {
                    return;
                };
                self.installing = true;
                self.status_line = "Installing…".to_string();
                let ctx = self.ctx.clone();
                let plan = InstallPlan {
                    minecraft_version: mc,
                    loader_version: loader,
                    installer_version: installer,
                    accept_eula: ctx.state.borrow().eula_accepted,
                };
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            let progress = {
                                let out = out.clone();
                                move |p: InstallProgress| {
                                    let line = match p {
                                        InstallProgress::Step(s) => s,
                                        InstallProgress::Download { name, downloaded, total } => {
                                            match total {
                                                Some(t) if t > 0 => format!(
                                                    "Downloading {name}: {}%",
                                                    downloaded * 100 / t
                                                ),
                                                _ => format!(
                                                    "Downloading {name}: {} KiB",
                                                    downloaded / 1024
                                                ),
                                            }
                                        }
                                    };
                                    let _ = out.send(SettingsInput::InstallProgress(line));
                                }
                            };
                            let result =
                                install::install(&ctx.http, &ctx.paths, &plan, progress).await;
                            let _ = out.send(SettingsInput::InstallFinished(
                                result.map_err(|e| e.to_string()),
                            ));
                        })
                        .drop_on_shutdown()
                });
            }
            SettingsInput::InstallProgress(line) => {
                self.status_line = line;
            }
            SettingsInput::InstallFinished(Ok(())) => {
                self.installing = false;
                self.status_line = "Server installed.".to_string();
                {
                    let mut st = self.ctx.state.borrow_mut();
                    st.minecraft_version = self.selected_mc.clone();
                    st.loader_version = self.selected_loader.clone();
                    st.installer_version = self.installer_version.clone();
                }
                let _ = self.ctx.save_state();
                let _ = sender.output(SettingsOutput::Installed);
                let _ = sender.output(SettingsOutput::Changed);
            }
            SettingsInput::InstallFinished(Err(e)) => {
                self.installing = false;
                self.status_line = format!("Install failed: {e}");
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        self.update(msg, sender, root);
    }
}

impl SettingsPage {
    fn budget(&self) -> MemoryBudget {
        self.ctx.state.borrow().budget()
    }

    fn budget_description(&self) -> String {
        let b = self.budget();
        let cap = if b.feasible {
            format!(
                "systemd scope: MemoryHigh {} · MemoryMax {} · projected JVM RSS {}",
                gib(b.scope_high_mib),
                gib(b.scope_max_mib),
                gib(b.projected_jvm_rss_mib()),
            )
        } else {
            "This ceiling is too low to run a server — raise it.".to_string()
        };
        cap
    }

    fn refresh_mc_choices(&mut self) {
        let snapshots = self.ctx.state.borrow().allow_snapshots;
        self.mc_choices = self
            .game_versions
            .iter()
            .filter(|v| snapshots || v.stable)
            .map(|v| v.version.clone())
            .collect();
        if let Some(sel) = &self.selected_mc {
            if !self.mc_choices.iter().any(|v| v == sel) {
                self.selected_mc = self.mc_choices.first().cloned();
            }
        }
    }

    fn selected_index(&self, choices: &[String], selected: &Option<String>) -> u32 {
        selected
            .as_ref()
            .and_then(|s| choices.iter().position(|c| c == s))
            .unwrap_or(0) as u32
    }

    fn sync_eula_file(&self) {
        let jar = self.ctx.paths.server_file(LAUNCHER_JAR);
        if !jar.is_file() {
            return;
        }
        let accepted = self.ctx.state.borrow().eula_accepted;
        let body = format!(
            "# Managed by minecraft-server-manager\n# https://aka.ms/MinecraftEULA\neula={accepted}\n"
        );
        let _ = mcsm_core::util::write_atomic(&self.ctx.paths.server_file("eula.txt"), body.as_bytes());
    }
}

async fn load_meta(http: &Http) -> Result<Meta, String> {
    let game = fabric::game_versions(http).await.map_err(|e| e.to_string())?;
    let loaders = fabric::loader_versions(http)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|l| l.stable)
        .map(|l| l.version)
        .collect();
    let installer = fabric::latest_stable_installer(http)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Meta {
        game,
        loaders,
        installer,
    })
}

fn string_list<S: AsRef<str>>(items: &[S]) -> gtk::StringList {
    let list = gtk::StringList::new(&[]);
    for item in items {
        list.append(item.as_ref());
    }
    list
}
