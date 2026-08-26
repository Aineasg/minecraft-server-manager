//! Player access editor for `ops.json`, `whitelist.json`,
//! `banned-players.json` and `banned-ips.json`.
//!
//! When the server is **running** each change is issued as a console command
//! (`op`, `whitelist add`, `ban`, …) so it takes effect immediately, then the
//! JSON the server wrote is re-read. When it is **stopped**, the JSON is edited
//! directly — new entries get an offline-mode UUID, and "Resolve UUIDs online"
//! swaps in real Mojang account UUIDs.

use std::time::Duration;

use adw::prelude::*;
use mcsm_core::access::{
    self, offline_uuid, parse_ip, AccessFile, BannedIp, BannedPlayer, OpEntry, WhitelistEntry,
};
use mcsm_core::net::mojang;
use mcsm_core::ops::server::scope_active;
use relm4::prelude::*;

use crate::context::Context;

/// A pending access change, carried through the "is the server running?" check.
#[derive(Debug, Clone)]
pub enum AccessAction {
    AddOp(String),
    AddWhitelist(String),
    AddBan(String),
    AddIpBan(String),
    Remove(AccessFile, usize),
}

pub struct AccessPage {
    ctx: Context,
    ops: Vec<OpEntry>,
    whitelist: Vec<WhitelistEntry>,
    bans: Vec<BannedPlayer>,
    ip_bans: Vec<BannedIp>,
    status: String,
    page: adw::PreferencesPage,
    groups: Vec<adw::PreferencesGroup>,
}

#[derive(Debug)]
pub enum AccessInput {
    Reload,
    AddOp(String),
    AddWhitelist(String),
    AddBan(String),
    AddIpBan(String),
    Remove(AccessFile, usize),
    /// Result of the running-state check for a queued action.
    Apply {
        running: bool,
        action: AccessAction,
    },
    ResolveOnline,
    ResolvedUuids(Vec<(String, String)>),
}

#[derive(Debug)]
pub enum AccessOutput {
    /// Run this console command on the live server.
    RunCommand(String),
}

#[relm4::component(pub)]
impl Component for AccessPage {
    type Init = Context;
    type Input = AccessInput;
    type Output = AccessOutput;
    type CommandOutput = AccessInput;

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
                    set_label: "Resolve UUIDs online",
                    set_tooltip_text: Some("Replace offline UUIDs with real Mojang account UUIDs"),
                    connect_clicked => AccessInput::ResolveOnline,
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
        let mut model = AccessPage {
            ctx,
            ops: Vec::new(),
            whitelist: Vec::new(),
            bans: Vec::new(),
            ip_bans: Vec::new(),
            status: String::new(),
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
            AccessInput::Reload => {
                self.reload();
                self.rebuild(&sender);
            }
            AccessInput::AddOp(name) => self.dispatch(AccessAction::AddOp(name), &sender),
            AccessInput::AddWhitelist(name) => {
                self.dispatch(AccessAction::AddWhitelist(name), &sender);
            }
            AccessInput::AddBan(name) => self.dispatch(AccessAction::AddBan(name), &sender),
            AccessInput::AddIpBan(ip) => self.dispatch(AccessAction::AddIpBan(ip), &sender),
            AccessInput::Remove(file, idx) => {
                self.dispatch(AccessAction::Remove(file, idx), &sender);
            }
            AccessInput::Apply { running, action } => self.apply(running, action, &sender),
            AccessInput::ResolveOnline => {
                let names: Vec<String> = self
                    .ops
                    .iter()
                    .map(|o| o.name.clone())
                    .chain(self.whitelist.iter().map(|w| w.name.clone()))
                    .chain(self.bans.iter().map(|b| b.name.clone()))
                    .collect();
                if names.is_empty() {
                    return;
                }
                self.status = "Resolving UUIDs…".to_string();
                let http = self.ctx.http.clone();
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            let mut resolved = Vec::new();
                            for name in names {
                                if let Ok(Some(uuid)) = mojang::lookup_uuid(&http, &name).await {
                                    resolved.push((name, uuid));
                                }
                            }
                            let _ = out.send(AccessInput::ResolvedUuids(resolved));
                        })
                        .drop_on_shutdown()
                });
            }
            AccessInput::ResolvedUuids(pairs) => {
                for (name, uuid) in &pairs {
                    for o in self.ops.iter_mut().filter(|o| &o.name == name) {
                        o.uuid = uuid.clone();
                    }
                    for w in self.whitelist.iter_mut().filter(|w| &w.name == name) {
                        w.uuid = uuid.clone();
                    }
                    for b in self.bans.iter_mut().filter(|b| &b.name == name) {
                        b.uuid = uuid.clone();
                    }
                }
                self.save(AccessFile::Ops);
                self.save(AccessFile::Whitelist);
                self.save(AccessFile::BannedPlayers);
                self.status = format!("Resolved {} name(s).", pairs.len());
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

impl AccessPage {
    fn reload(&mut self) {
        let dir = &self.ctx.paths.server;
        self.ops = access::load(dir, AccessFile::Ops).unwrap_or_default();
        self.whitelist = access::load(dir, AccessFile::Whitelist).unwrap_or_default();
        self.bans = access::load(dir, AccessFile::BannedPlayers).unwrap_or_default();
        self.ip_bans = access::load(dir, AccessFile::BannedIps).unwrap_or_default();
        self.status = format!(
            "{} ops · {} whitelisted · {} banned · {} IP bans",
            self.ops.len(),
            self.whitelist.len(),
            self.bans.len(),
            self.ip_bans.len()
        );
    }

    fn save(&mut self, file: AccessFile) {
        let dir = &self.ctx.paths.server;
        let result = match file {
            AccessFile::Ops => access::save(dir, file, &self.ops),
            AccessFile::Whitelist => access::save(dir, file, &self.whitelist),
            AccessFile::BannedPlayers => access::save(dir, file, &self.bans),
            AccessFile::BannedIps => access::save(dir, file, &self.ip_bans),
        };
        if let Err(e) = result {
            self.status = format!("Save failed: {e}");
        }
    }

    /// Check whether the server is running, then route the action accordingly.
    fn dispatch(&mut self, action: AccessAction, sender: &ComponentSender<Self>) {
        if let AccessAction::AddIpBan(ip) = &action {
            if let Err(e) = parse_ip(ip) {
                self.status = e.to_string();
                return;
            }
        }
        sender.command(move |out, shutdown| {
            shutdown
                .register(async move {
                    let running = scope_active().await;
                    let _ = out.send(AccessInput::Apply { running, action });
                })
                .drop_on_shutdown()
        });
    }

    fn apply(&mut self, running: bool, action: AccessAction, sender: &ComponentSender<Self>) {
        if running {
            match self.console_command(&action) {
                Some(cmd) => {
                    self.status = format!("Applied live: {cmd}");
                    let _ = sender.output(AccessOutput::RunCommand(cmd));
                    // Re-read after the server has rewritten its JSON files.
                    let s = sender.clone();
                    relm4::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(800)).await;
                        s.input(AccessInput::Reload);
                    });
                }
                None => self.status = "Nothing to do.".to_string(),
            }
        } else {
            self.apply_offline(&action);
            self.rebuild(sender);
        }
    }

    /// The console command for an action, using the *current* entry for removals.
    fn console_command(&self, action: &AccessAction) -> Option<String> {
        let trimmed = |s: &str| s.trim().to_string();
        Some(match action {
            AccessAction::AddOp(n) if !n.trim().is_empty() => format!("op {}", trimmed(n)),
            AccessAction::AddWhitelist(n) if !n.trim().is_empty() => {
                format!("whitelist add {}", trimmed(n))
            }
            AccessAction::AddBan(n) if !n.trim().is_empty() => format!("ban {}", trimmed(n)),
            AccessAction::AddIpBan(ip) => format!("ban-ip {}", parse_ip(ip).ok()?),
            AccessAction::Remove(AccessFile::Ops, i) => format!("deop {}", self.ops.get(*i)?.name),
            AccessAction::Remove(AccessFile::Whitelist, i) => {
                format!("whitelist remove {}", self.whitelist.get(*i)?.name)
            }
            AccessAction::Remove(AccessFile::BannedPlayers, i) => {
                format!("pardon {}", self.bans.get(*i)?.name)
            }
            AccessAction::Remove(AccessFile::BannedIps, i) => {
                format!("pardon-ip {}", self.ip_bans.get(*i)?.ip)
            }
            _ => return None,
        })
    }

    /// Edit the JSON files directly (server is stopped).
    fn apply_offline(&mut self, action: &AccessAction) {
        match action {
            AccessAction::AddOp(name) if !name.trim().is_empty() => {
                let name = name.trim();
                self.ops.push(OpEntry::new(offline_uuid(name), name));
                self.save(AccessFile::Ops);
                self.status = format!("Added op {name} — effective on next start");
            }
            AccessAction::AddWhitelist(name) if !name.trim().is_empty() => {
                let name = name.trim();
                self.whitelist.push(WhitelistEntry {
                    uuid: offline_uuid(name),
                    name: name.to_string(),
                });
                self.save(AccessFile::Whitelist);
                self.status = format!("Whitelisted {name} — effective on next start");
            }
            AccessAction::AddBan(name) if !name.trim().is_empty() => {
                let name = name.trim();
                self.bans.push(BannedPlayer {
                    uuid: offline_uuid(name),
                    name: name.to_string(),
                    created: String::new(),
                    source: "(Manager)".to_string(),
                    expires: "forever".to_string(),
                    reason: "Banned by an operator.".to_string(),
                });
                self.save(AccessFile::BannedPlayers);
                self.status = format!("Banned {name} — effective on next start");
            }
            AccessAction::AddIpBan(ip) => match parse_ip(ip) {
                Ok(addr) => {
                    self.ip_bans.push(BannedIp {
                        ip: addr.to_string(),
                        created: String::new(),
                        source: "(Manager)".to_string(),
                        expires: "forever".to_string(),
                        reason: "Banned by an operator.".to_string(),
                    });
                    self.save(AccessFile::BannedIps);
                    self.status = format!("Banned IP {addr} — effective on next start");
                }
                Err(e) => self.status = e.to_string(),
            },
            AccessAction::Remove(file, idx) => {
                match file {
                    AccessFile::Ops => drop_at(&mut self.ops, *idx),
                    AccessFile::Whitelist => drop_at(&mut self.whitelist, *idx),
                    AccessFile::BannedPlayers => drop_at(&mut self.bans, *idx),
                    AccessFile::BannedIps => drop_at(&mut self.ip_bans, *idx),
                }
                self.save(*file);
                self.status = "Removed — effective on next start".to_string();
            }
            _ => {}
        }
    }

    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        for group in self.groups.drain(..) {
            self.page.remove(&group);
        }

        let ops_rows: Vec<(String, String)> = self
            .ops
            .iter()
            .map(|o| (o.name.clone(), o.uuid.clone()))
            .collect();
        self.add_group(
            sender,
            "Operators (ops.json)",
            AccessFile::Ops,
            &ops_rows,
            "Player name",
            AccessInput::AddOp as fn(String) -> AccessInput,
        );

        let wl_rows: Vec<(String, String)> = self
            .whitelist
            .iter()
            .map(|w| (w.name.clone(), w.uuid.clone()))
            .collect();
        self.add_group(
            sender,
            "Whitelist (whitelist.json)",
            AccessFile::Whitelist,
            &wl_rows,
            "Player name",
            AccessInput::AddWhitelist,
        );

        let ban_rows: Vec<(String, String)> = self
            .bans
            .iter()
            .map(|b| (b.name.clone(), b.reason.clone()))
            .collect();
        self.add_group(
            sender,
            "Banned players (banned-players.json)",
            AccessFile::BannedPlayers,
            &ban_rows,
            "Player name",
            AccessInput::AddBan,
        );

        let ip_rows: Vec<(String, String)> = self
            .ip_bans
            .iter()
            .map(|b| (b.ip.clone(), b.reason.clone()))
            .collect();
        self.add_group(
            sender,
            "Banned IPs (banned-ips.json)",
            AccessFile::BannedIps,
            &ip_rows,
            "IP address",
            AccessInput::AddIpBan,
        );
    }

    fn add_group(
        &mut self,
        sender: &ComponentSender<Self>,
        title: &str,
        file: AccessFile,
        rows: &[(String, String)],
        add_placeholder: &str,
        make_add: fn(String) -> AccessInput,
    ) {
        let group = adw::PreferencesGroup::new();
        group.set_title(title);

        for (idx, (primary, secondary)) in rows.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(primary);
            if !secondary.is_empty() {
                row.set_subtitle(secondary);
            }
            let button = gtk::Button::from_icon_name("user-trash-symbolic");
            button.set_valign(gtk::Align::Center);
            button.add_css_class("flat");
            let s = sender.clone();
            button.connect_clicked(move |_| s.input(AccessInput::Remove(file, idx)));
            row.add_suffix(&button);
            group.add(&row);
        }

        let add_row = adw::EntryRow::new();
        add_row.set_title(add_placeholder);
        let add_button = gtk::Button::from_icon_name("list-add-symbolic");
        add_button.set_valign(gtk::Align::Center);
        add_button.add_css_class("flat");
        let s = sender.clone();
        let entry_for_button = add_row.clone();
        add_button.connect_clicked(move |_| {
            let text = entry_for_button.text().to_string();
            entry_for_button.set_text("");
            s.input(make_add(text));
        });
        let s2 = sender.clone();
        add_row.connect_entry_activated(move |r| {
            let text = r.text().to_string();
            r.set_text("");
            s2.input(make_add(text));
        });
        add_row.add_suffix(&add_button);
        group.add(&add_row);

        self.page.add(&group);
        self.groups.push(group);
    }
}

fn drop_at<T>(v: &mut Vec<T>, idx: usize) {
    if idx < v.len() {
        v.remove(idx);
    }
}
