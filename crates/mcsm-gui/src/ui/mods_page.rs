//! Mods: search Modrinth, install with dependencies, enable/disable, update.

use std::collections::HashMap;

use adw::prelude::*;
use mcsm_core::net::modrinth::{SearchHit, SearchParams, Version};
use mcsm_core::ops::mods::{self, InstalledMod};
use relm4::prelude::*;

use crate::context::Context;

pub struct ModsPage {
    ctx: Context,
    installed: Vec<InstalledMod>,
    results: Vec<SearchHit>,
    /// sha512 of an installed jar -> the newer version available for it.
    updates: HashMap<String, Version>,
    busy: bool,
    status: String,
    page: adw::PreferencesPage,
    groups: Vec<adw::PreferencesGroup>,
}

#[derive(Debug)]
pub enum ModsInput {
    Reload,
    Search(String),
    SearchDone(Result<Vec<SearchHit>, String>),
    Install(String),
    CheckUpdates,
    UpdatesDone(Result<Vec<(String, Version)>, String>),
    ApplyUpdate(usize),
    Toggle(usize, bool),
    Remove(usize),
    TaskDone(Result<String, String>),
}

#[relm4::component(pub)]
impl Component for ModsPage {
    type Init = Context;
    type Input = ModsInput;
    type Output = ();
    type CommandOutput = ModsInput;

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
                    set_label: "Check for updates",
                    set_tooltip_text: Some("Ask Modrinth for newer builds of every installed mod for your version"),
                    #[watch]
                    set_sensitive: !model.busy && model.ctx.installed_versions().is_some(),
                    connect_clicked => ModsInput::CheckUpdates,
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
        let mut model = ModsPage {
            ctx,
            installed: Vec::new(),
            results: Vec::new(),
            updates: HashMap::new(),
            busy: false,
            status: String::new(),
            page: page.clone(),
            groups: Vec::new(),
        };
        model.rescan();
        model.rebuild(&sender);
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            ModsInput::Reload => {
                self.rescan();
                self.rebuild(&sender);
            }
            ModsInput::Search(query) => {
                let Some((mc, _loader)) = self.ctx.installed_versions() else {
                    self.status = "Install a server first (Settings).".to_string();
                    return;
                };
                if query.trim().is_empty() {
                    self.results.clear();
                    self.rebuild(&sender);
                    return;
                }
                self.busy = true;
                self.status = format!("Searching Modrinth for “{query}”…");
                let modrinth = self.ctx.modrinth.clone();
                let params = SearchParams::new(query, mc);
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            let r = modrinth
                                .search(&params)
                                .await
                                .map(|res| res.hits)
                                .map_err(|e| e.to_string());
                            let _ = out.send(ModsInput::SearchDone(r));
                        })
                        .drop_on_shutdown()
                });
            }
            ModsInput::SearchDone(Ok(hits)) => {
                self.busy = false;
                self.status = format!("{} result(s)", hits.len());
                self.results = hits;
                self.rebuild(&sender);
            }
            ModsInput::SearchDone(Err(e)) => {
                self.busy = false;
                self.status = format!("Search failed: {e}");
            }
            ModsInput::Install(project_id) => {
                let Some((mc, loader)) = self.ctx.installed_versions() else {
                    return;
                };
                self.busy = true;
                self.status = "Resolving dependencies…".to_string();
                let ctx = self.ctx.clone();
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            let result = mods::install_project(
                                &ctx.http,
                                &ctx.modrinth,
                                &ctx.paths,
                                &project_id,
                                &mc,
                                &loader,
                            )
                            .await;
                            let msg = result
                                .map(|r| {
                                    let mut s =
                                        format!("Installed {} file(s).", r.to_install.len());
                                    if !r.incompatible.is_empty() {
                                        s.push_str(&format!(
                                            " Warning: {} incompatible project(s) flagged.",
                                            r.incompatible.len()
                                        ));
                                    }
                                    s
                                })
                                .map_err(|e| e.to_string());
                            let _ = out.send(ModsInput::TaskDone(msg));
                        })
                        .drop_on_shutdown()
                });
            }
            ModsInput::CheckUpdates => {
                let Some((mc, loader)) = self.ctx.installed_versions() else {
                    return;
                };
                if self.installed.is_empty() {
                    self.status = "No mods installed.".to_string();
                    return;
                }
                self.busy = true;
                self.status = "Checking Modrinth for updates…".to_string();
                let modrinth = self.ctx.modrinth.clone();
                let installed = self.installed.clone();
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            let r = mods::check_updates(&modrinth, &installed, &mc, &loader)
                                .await
                                .map(|m| m.into_iter().collect())
                                .map_err(|e| e.to_string());
                            let _ = out.send(ModsInput::UpdatesDone(r));
                        })
                        .drop_on_shutdown()
                });
            }
            ModsInput::UpdatesDone(Ok(list)) => {
                self.busy = false;
                self.updates = list.into_iter().collect();
                self.status = format!("{} update(s) available", self.updates.len());
                self.rebuild(&sender);
            }
            ModsInput::UpdatesDone(Err(e)) => {
                self.busy = false;
                self.status = format!("Update check failed: {e}");
            }
            ModsInput::ApplyUpdate(idx) => {
                let Some(old) = self.installed.get(idx).cloned() else {
                    return;
                };
                let Some(new_version) = self.updates.get(&old.sha512).cloned() else {
                    return;
                };
                self.busy = true;
                self.status = format!("Updating {}…", old.display_name());
                let ctx = self.ctx.clone();
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            let r = async {
                                mods::install_version(&ctx.http, &ctx.paths, &new_version).await?;
                                mods::remove(&old)?;
                                Ok::<_, mcsm_core::Error>(())
                            }
                            .await;
                            let _ = out.send(ModsInput::TaskDone(
                                r.map(|()| "Updated.".to_string())
                                    .map_err(|e| e.to_string()),
                            ));
                        })
                        .drop_on_shutdown()
                });
            }
            ModsInput::Toggle(idx, enabled) => {
                if let Some(m) = self.installed.get(idx) {
                    if let Err(e) = mods::set_enabled(m, enabled) {
                        self.status = format!("Toggle failed: {e}");
                    }
                }
                self.rescan();
                self.rebuild(&sender);
            }
            ModsInput::Remove(idx) => {
                if let Some(m) = self.installed.get(idx) {
                    if let Err(e) = mods::remove(m) {
                        self.status = format!("Remove failed: {e}");
                    }
                }
                self.rescan();
                self.rebuild(&sender);
            }
            ModsInput::TaskDone(result) => {
                self.busy = false;
                self.status = match result {
                    Ok(s) => s,
                    Err(e) => format!("Failed: {e}"),
                };
                self.rescan();
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

impl ModsPage {
    fn rescan(&mut self) {
        self.installed = mods::scan(&self.ctx.paths).unwrap_or_default();
        // Drop update entries that no longer match an installed jar.
        let have: std::collections::HashSet<String> =
            self.installed.iter().map(|m| m.sha512.clone()).collect();
        self.updates.retain(|k, _| have.contains(k));
    }

    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        for group in self.groups.drain(..) {
            self.page.remove(&group);
        }

        if self.ctx.installed_versions().is_none() {
            let g = adw::PreferencesGroup::new();
            g.set_description(Some("Install a server in Settings before managing mods."));
            self.page.add(&g);
            self.groups.push(g);
            return;
        }

        self.add_search_group(sender);
        self.add_installed_group(sender);
    }

    fn add_search_group(&mut self, sender: &ComponentSender<Self>) {
        let group = adw::PreferencesGroup::new();
        group.set_title("Find mods on Modrinth");

        let search = adw::EntryRow::new();
        search.set_title("Search (server-side Fabric mods for your version)");
        let s = sender.clone();
        search.connect_entry_activated(move |r| s.input(ModsInput::Search(r.text().to_string())));
        let s2 = sender.clone();
        let search_clone = search.clone();
        let btn = gtk::Button::from_icon_name("system-search-symbolic");
        btn.set_valign(gtk::Align::Center);
        btn.add_css_class("flat");
        btn.connect_clicked(move |_| s2.input(ModsInput::Search(search_clone.text().to_string())));
        search.add_suffix(&btn);
        group.add(&search);

        for hit in &self.results {
            let row = adw::ActionRow::new();
            row.set_title(&glib_escape(&hit.title));
            row.set_subtitle(&format!(
                "{}  ·  {} downloads",
                truncate(&hit.description, 100),
                hit.downloads
            ));
            let install = gtk::Button::with_label("Install");
            install.set_valign(gtk::Align::Center);
            install.add_css_class("suggested-action");
            install.set_sensitive(!self.busy);
            let s = sender.clone();
            let pid = hit.project_id.clone();
            install.connect_clicked(move |_| s.input(ModsInput::Install(pid.clone())));
            row.add_suffix(&install);
            group.add(&row);
        }

        self.page.add(&group);
        self.groups.push(group);
    }

    fn add_installed_group(&mut self, sender: &ComponentSender<Self>) {
        let group = adw::PreferencesGroup::new();
        group.set_title(&format!("Installed ({})", self.installed.len()));

        if self.installed.is_empty() {
            group.set_description(Some(
                "Drop .jar files into data/server/mods, or install from search above.",
            ));
        }

        for (idx, m) in self.installed.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&glib_escape(m.display_name()));
            let mut subtitle = m.version_label().unwrap_or("unknown version").to_string();
            if !m.enabled {
                subtitle.push_str("  ·  disabled");
            }
            if let Some(update) = self.updates.get(&m.sha512) {
                subtitle.push_str(&format!("  ·  update: {}", update.version_number));
            }
            row.set_subtitle(&subtitle);

            if self.updates.contains_key(&m.sha512) {
                let up = gtk::Button::with_label("Update");
                up.set_valign(gtk::Align::Center);
                up.add_css_class("suggested-action");
                up.set_sensitive(!self.busy);
                let s = sender.clone();
                up.connect_clicked(move |_| s.input(ModsInput::ApplyUpdate(idx)));
                row.add_suffix(&up);
            }

            let toggle = gtk::Switch::new();
            toggle.set_valign(gtk::Align::Center);
            toggle.set_active(m.enabled);
            let s = sender.clone();
            toggle.connect_state_set(move |_, state| {
                s.input(ModsInput::Toggle(idx, state));
                gtk::glib::Propagation::Proceed
            });
            row.add_suffix(&toggle);

            let del = gtk::Button::from_icon_name("user-trash-symbolic");
            del.set_valign(gtk::Align::Center);
            del.add_css_class("flat");
            let s = sender.clone();
            del.connect_clicked(move |_| s.input(ModsInput::Remove(idx)));
            row.add_suffix(&del);

            group.add(&row);
        }

        self.page.add(&group);
        self.groups.push(group);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// Escape `&`, `<`, `>` so Pango markup in row titles is treated literally.
fn glib_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
