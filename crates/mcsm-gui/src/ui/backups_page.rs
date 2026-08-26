//! World backups: create `tar --zstd` archives and restore them.

use adw::prelude::*;
use mcsm_core::ops::backup::{self, BackupEntry};
use mcsm_core::ops::server::scope_active;
use mcsm_core::properties::Properties;
use mcsm_core::util::read_to_string_opt;
use relm4::prelude::*;

use crate::context::Context;
use crate::ui::widgets::human_bytes;

pub struct BackupsPage {
    ctx: Context,
    entries: Vec<BackupEntry>,
    busy: bool,
    status: String,
    /// Row index awaiting a second click to confirm a destructive restore.
    pending_restore: Option<usize>,
    page: adw::PreferencesPage,
    groups: Vec<adw::PreferencesGroup>,
}

#[derive(Debug)]
pub enum BackupsInput {
    Reload,
    CreateNow,
    Restore(usize),
    TaskDone(Result<String, String>),
}

#[relm4::component(pub)]
impl Component for BackupsPage {
    type Init = Context;
    type Input = BackupsInput;
    type Output = ();
    type CommandOutput = BackupsInput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,

            #[local_ref]
            page -> adw::PreferencesPage { set_vexpand: true },

            gtk::ActionBar {
                pack_start = &gtk::Label {
                    add_css_class: "dim-label",
                    #[watch]
                    set_label: &model.status,
                },
                pack_end = &gtk::Button {
                    set_label: "Back up world now",
                    add_css_class: "suggested-action",
                    #[watch]
                    set_sensitive: !model.busy,
                    connect_clicked => BackupsInput::CreateNow,
                },
            },
        }
    }

    fn init(
        ctx: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let page = adw::PreferencesPage::new();
        let mut model = BackupsPage {
            ctx,
            entries: Vec::new(),
            busy: false,
            status: String::new(),
            pending_restore: None,
            page: page.clone(),
            groups: Vec::new(),
        };
        model.reload();
        model.rebuild(&sender);
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            BackupsInput::Reload => {
                self.reload();
                self.rebuild(&sender);
            }
            BackupsInput::CreateNow => {
                self.busy = true;
                self.pending_restore = None;
                self.status = "Creating backup…".to_string();
                let ctx = self.ctx.clone();
                let level = self.level_name();
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            if scope_active().await {
                                let _ = out.send(BackupsInput::TaskDone(Err(
                                    "Stop the server first — archiving a live world can produce a torn backup."
                                        .to_string(),
                                )));
                                return;
                            }
                            let r = backup::create(&ctx.paths, &level)
                                .await
                                .map(|e| format!("Created {}", e.file_name))
                                .map_err(|e| e.to_string());
                            let _ = out.send(BackupsInput::TaskDone(r));
                        })
                        .drop_on_shutdown()
                });
            }
            BackupsInput::Restore(idx) => {
                if self.pending_restore != Some(idx) {
                    self.pending_restore = Some(idx);
                    self.status =
                        "Click Restore again to confirm — this replaces the current world.".to_string();
                    self.rebuild(&sender);
                    return;
                }
                self.pending_restore = None;
                let Some(entry) = self.entries.get(idx).cloned() else {
                    return;
                };
                self.busy = true;
                self.status = format!("Restoring {}…", entry.file_name);
                let ctx = self.ctx.clone();
                let level = self.level_name();
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            if scope_active().await {
                                let _ = out.send(BackupsInput::TaskDone(Err(
                                    "Stop the server before restoring a backup.".to_string(),
                                )));
                                return;
                            }
                            let r = backup::restore(&ctx.paths, &entry, &level)
                                .await
                                .map(|()| format!("Restored {}", entry.file_name))
                                .map_err(|e| e.to_string());
                            let _ = out.send(BackupsInput::TaskDone(r));
                        })
                        .drop_on_shutdown()
                });
            }
            BackupsInput::TaskDone(result) => {
                self.busy = false;
                self.status = match result {
                    Ok(s) => s,
                    Err(e) => format!("Failed: {e}"),
                };
                self.reload();
                self.rebuild(&sender);
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

impl BackupsPage {
    fn level_name(&self) -> String {
        read_to_string_opt(&self.ctx.paths.server_file("server.properties"))
            .ok()
            .flatten()
            .and_then(|t| Properties::parse(&t).get("level-name").map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "world".to_string())
    }

    fn reload(&mut self) {
        self.entries = backup::list(&self.ctx.paths).unwrap_or_default();
        let total: u64 = self.entries.iter().map(|e| e.size_bytes).sum();
        self.status = format!(
            "{} backup(s), {} total",
            self.entries.len(),
            human_bytes(total)
        );
    }

    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        for group in self.groups.drain(..) {
            self.page.remove(&group);
        }

        let group = adw::PreferencesGroup::new();
        group.set_title("World backups (data/backups)");
        if self.entries.is_empty() {
            group.set_description(Some("No backups yet."));
        }

        for (idx, entry) in self.entries.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&entry.file_name);
            row.set_subtitle(&human_bytes(entry.size_bytes));

            let restore = gtk::Button::with_label(if self.pending_restore == Some(idx) {
                "Confirm restore"
            } else {
                "Restore"
            });
            restore.set_valign(gtk::Align::Center);
            restore.set_sensitive(!self.busy);
            if self.pending_restore == Some(idx) {
                restore.add_css_class("destructive-action");
            }
            let s = sender.clone();
            restore.connect_clicked(move |_| s.input(BackupsInput::Restore(idx)));
            row.add_suffix(&restore);
            group.add(&row);
        }

        self.page.add(&group);
        self.groups.push(group);
    }
}
