//! World backups: automatic-backup schedule, manual create, restore, delete.
//!
//! Creating a backup is routed to the Dashboard (via [`BackupsOutput`]) so it
//! can `save-all flush` a running server first; this page owns the schedule,
//! the list, restore and delete.

use adw::prelude::*;
use gtk::glib;
use mcsm_core::ops::backup::{self, BackupEntry};
use mcsm_core::ops::server::scope_active;
use relm4::prelude::*;

use crate::context::Context;
use crate::ui::widgets::human_bytes;

/// Auto-backup interval choices: label and minutes (`0` = off).
const INTERVALS: [(&str, u64); 8] = [
    ("Off", 0),
    ("Every 15 minutes", 15),
    ("Every 30 minutes", 30),
    ("Every hour", 60),
    ("Every 2 hours", 120),
    ("Every 6 hours", 360),
    ("Every 12 hours", 720),
    ("Daily", 1440),
];

/// How many automatic backups to keep: label and count (`0` = keep all).
const KEEP_CHOICES: [(&str, u64); 6] = [
    ("3", 3),
    ("5", 5),
    ("10", 10),
    ("25", 25),
    ("50", 50),
    ("All", 0),
];

pub struct BackupsPage {
    ctx: Context,
    entries: Vec<BackupEntry>,
    busy: bool,
    status: String,
    /// Row index awaiting a confirming second click.
    pending_restore: Option<usize>,
    pending_delete: Option<usize>,
    /// The live auto-backup timer, if an interval is set.
    auto_timer: Option<glib::SourceId>,
    page: adw::PreferencesPage,
    groups: Vec<adw::PreferencesGroup>,
}

#[derive(Debug)]
pub enum BackupsInput {
    Reload,
    RequestBackup,
    SetInterval(u32),
    SetKeep(u32),
    Restore(usize),
    Delete(usize),
    TaskDone(Result<String, String>),
}

#[derive(Debug)]
pub enum BackupsOutput {
    /// The user pressed "Back up world now".
    BackupNowRequested,
    /// The auto-backup timer fired.
    AutoBackupDue,
}

#[relm4::component(pub)]
impl Component for BackupsPage {
    type Init = Context;
    type Input = BackupsInput;
    type Output = BackupsOutput;
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
                    connect_clicked => BackupsInput::RequestBackup,
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
            pending_delete: None,
            auto_timer: None,
            page: page.clone(),
            groups: Vec::new(),
        };
        model.reload();
        model.rebuild(&sender);
        model.rearm_timer(&sender);
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            BackupsInput::Reload => {
                self.reload();
                self.rebuild(&sender);
            }
            BackupsInput::RequestBackup => {
                self.status = "Backing up… (progress in the Dashboard console)".to_string();
                let _ = sender.output(BackupsOutput::BackupNowRequested);
            }
            BackupsInput::SetInterval(idx) => {
                let minutes = INTERVALS.get(idx as usize).map_or(0, |(_, m)| *m);
                if self.ctx.state.borrow().auto_backup_minutes == minutes {
                    return; // programmatic re-selection during rebuild
                }
                self.ctx.state.borrow_mut().auto_backup_minutes = minutes;
                let _ = self.ctx.save_state();
                self.rearm_timer(&sender);
                self.reload();
                self.rebuild(&sender);
            }
            BackupsInput::SetKeep(idx) => {
                let keep = KEEP_CHOICES.get(idx as usize).map_or(0, |(_, k)| *k);
                if self.ctx.state.borrow().auto_backup_keep == keep {
                    return;
                }
                self.ctx.state.borrow_mut().auto_backup_keep = keep;
                let _ = self.ctx.save_state();
            }
            BackupsInput::Restore(idx) => {
                self.pending_delete = None;
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
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            if scope_active().await {
                                let _ = out.send(BackupsInput::TaskDone(Err(
                                    "Stop the server before restoring a backup.".to_string(),
                                )));
                                return;
                            }
                            let level = backup::level_name(&ctx.paths);
                            let r = backup::restore(&ctx.paths, &entry, &level)
                                .await
                                .map(|()| format!("Restored {}", entry.file_name))
                                .map_err(|e| e.to_string());
                            let _ = out.send(BackupsInput::TaskDone(r));
                        })
                        .drop_on_shutdown()
                });
            }
            BackupsInput::Delete(idx) => {
                self.pending_restore = None;
                if self.pending_delete != Some(idx) {
                    self.pending_delete = Some(idx);
                    self.status = "Click Delete again to confirm.".to_string();
                    self.rebuild(&sender);
                    return;
                }
                self.pending_delete = None;
                if let Some(entry) = self.entries.get(idx).cloned() {
                    match backup::delete(&entry) {
                        Ok(()) => self.status = format!("Deleted {}", entry.file_name),
                        Err(e) => self.status = format!("Delete failed: {e}"),
                    }
                }
                self.reload();
                self.rebuild(&sender);
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

impl Drop for BackupsPage {
    fn drop(&mut self) {
        if let Some(id) = self.auto_timer.take() {
            id.remove();
        }
    }
}

impl BackupsPage {
    fn reload(&mut self) {
        self.entries = backup::list(&self.ctx.paths).unwrap_or_default();
        let total: u64 = self.entries.iter().map(|e| e.size_bytes).sum();
        let autos = self.entries.iter().filter(|e| e.is_automatic()).count();
        self.status = format!(
            "{} backup(s) ({autos} automatic), {} total",
            self.entries.len(),
            human_bytes(total)
        );
    }

    /// Cancel any running timer and start a fresh one if an interval is set.
    fn rearm_timer(&mut self, sender: &ComponentSender<Self>) {
        if let Some(id) = self.auto_timer.take() {
            id.remove();
        }
        let minutes = self.ctx.state.borrow().auto_backup_minutes;
        if minutes == 0 {
            return;
        }
        let sender = sender.clone();
        self.auto_timer = Some(glib::timeout_add_seconds_local(
            (minutes * 60) as u32,
            move || {
                let _ = sender.output(BackupsOutput::AutoBackupDue);
                glib::ControlFlow::Continue
            },
        ));
    }

    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        for group in self.groups.drain(..) {
            self.page.remove(&group);
        }
        self.add_schedule_group(sender);
        self.add_list_group(sender);
    }

    fn add_schedule_group(&mut self, sender: &ComponentSender<Self>) {
        let group = adw::PreferencesGroup::new();
        group.set_title("Automatic backups");
        group.set_description(Some(
            "Runs only while this app is open. A running server is flushed with `save-all` first.",
        ));

        let interval_labels: Vec<&str> = INTERVALS.iter().map(|(l, _)| *l).collect();
        let interval_row = adw::ComboRow::new();
        interval_row.set_title("Interval");
        interval_row.set_model(Some(&gtk::StringList::new(&interval_labels)));
        let current_min = self.ctx.state.borrow().auto_backup_minutes;
        interval_row.set_selected(
            INTERVALS
                .iter()
                .position(|(_, m)| *m == current_min)
                .unwrap_or(0) as u32,
        );
        let s = sender.clone();
        interval_row.connect_selected_notify(move |r| s.input(BackupsInput::SetInterval(r.selected())));
        group.add(&interval_row);

        let keep_labels: Vec<&str> = KEEP_CHOICES.iter().map(|(l, _)| *l).collect();
        let keep_row = adw::ComboRow::new();
        keep_row.set_title("Keep automatic backups");
        keep_row.set_subtitle("Oldest automatic backups past this count are deleted");
        keep_row.set_model(Some(&gtk::StringList::new(&keep_labels)));
        let current_keep = self.ctx.state.borrow().auto_backup_keep;
        keep_row.set_selected(
            KEEP_CHOICES
                .iter()
                .position(|(_, k)| *k == current_keep)
                .unwrap_or(2) as u32,
        );
        let s = sender.clone();
        keep_row.connect_selected_notify(move |r| s.input(BackupsInput::SetKeep(r.selected())));
        group.add(&keep_row);

        self.page.add(&group);
        self.groups.push(group);
    }

    fn add_list_group(&mut self, sender: &ComponentSender<Self>) {
        let group = adw::PreferencesGroup::new();
        group.set_title("World backups (data/backups)");
        if self.entries.is_empty() {
            group.set_description(Some("No backups yet."));
        }

        for (idx, entry) in self.entries.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&entry.file_name);
            let tag = if entry.is_automatic() { "automatic · " } else { "" };
            row.set_subtitle(&format!("{tag}{}", human_bytes(entry.size_bytes)));

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

            let delete = if self.pending_delete == Some(idx) {
                let b = gtk::Button::with_label("Confirm delete");
                b.add_css_class("destructive-action");
                b
            } else {
                gtk::Button::from_icon_name("user-trash-symbolic")
            };
            delete.set_valign(gtk::Align::Center);
            delete.add_css_class("flat");
            delete.set_sensitive(!self.busy);
            let s = sender.clone();
            delete.connect_clicked(move |_| s.input(BackupsInput::Delete(idx)));
            row.add_suffix(&delete);

            group.add(&row);
        }

        self.page.add(&group);
        self.groups.push(group);
    }
}
