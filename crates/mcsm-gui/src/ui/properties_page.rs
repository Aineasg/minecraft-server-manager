//! `server.properties` editor: a typed form driven by
//! [`mcsm_core::properties_catalog`], backed by the comment-preserving
//! [`mcsm_core::properties::Properties`] writer.

use adw::prelude::*;
use mcsm_core::ops::backup;
use mcsm_core::ops::level_dat::{self, WorldSettings};
use mcsm_core::ops::server::scope_active;
use mcsm_core::properties::Properties;
use mcsm_core::properties_catalog::{restart_required, FieldKind, CATALOG};
use mcsm_core::util::{read_to_string_opt, write_atomic};
use relm4::prelude::*;

use crate::context::Context;

const DIFFICULTIES: [&str; 4] = ["peaceful", "easy", "normal", "hard"];

pub struct PropertiesPage {
    ctx: Context,
    props: Option<Properties>,
    dirty: bool,
    status: String,
    /// World settings from `level.dat`, when the world has been generated.
    world: Option<WorldSettings>,
    world_level: String,
    /// The server is running, so `level.dat` must not be touched.
    world_locked: bool,
    /// Rows in the "World (level.dat)" group, so their sensitivity can be
    /// toggled when the server's running state is learned asynchronously.
    world_rows: Vec<gtk::Widget>,
    page: adw::PreferencesPage,
    groups: Vec<adw::PreferencesGroup>,
}

#[derive(Debug)]
pub enum PropertiesInput {
    Reload,
    Set(String, String),
    Save,
    SetHardcore(bool),
    SetDifficulty(u8),
    SetLockDifficulty(bool),
    /// Async result of checking whether the server is running.
    WorldLocked(bool),
}

#[relm4::component(pub)]
impl Component for PropertiesPage {
    type Init = Context;
    type Input = PropertiesInput;
    type Output = ();
    type CommandOutput = ();

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,

            #[local_ref]
            page -> adw::PreferencesPage {
                set_vexpand: true,
            },

            gtk::ActionBar {
                pack_start = &gtk::Label {
                    add_css_class: "dim-label",
                    #[watch]
                    set_label: &model.status,
                },
                pack_end = &gtk::Button {
                    set_label: "Save",
                    set_tooltip_text: Some("Write server.properties, preserving comments and key order"),
                    add_css_class: "suggested-action",
                    #[watch]
                    set_sensitive: model.dirty,
                    connect_clicked => PropertiesInput::Save,
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
        let mut model = PropertiesPage {
            ctx,
            props: None,
            dirty: false,
            status: String::new(),
            world: None,
            world_level: "world".to_string(),
            world_locked: true, // assume locked until the async check says otherwise
            world_rows: Vec::new(),
            page: page.clone(),
            groups: Vec::new(),
        };
        model.reload(&sender);
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            PropertiesInput::Reload => self.reload(&sender),
            PropertiesInput::Set(key, value) => {
                if let Some(props) = &mut self.props {
                    props.set(&key, &value);
                    self.dirty = true;
                    self.status = "Unsaved changes".to_string();
                }
            }
            PropertiesInput::Save => {
                let Some(props) = &self.props else { return };
                let path = self.ctx.paths.server_file("server.properties");
                match write_atomic(&path, props.render().as_bytes()) {
                    Ok(()) => {
                        self.dirty = false;
                        self.status = "Saved".to_string();
                    }
                    Err(e) => self.status = format!("Save failed: {e}"),
                }
            }
            PropertiesInput::SetHardcore(on) => {
                self.mutate_world(|w| w.hardcore = on);
            }
            PropertiesInput::SetDifficulty(level) => {
                self.mutate_world(|w| w.difficulty = level);
            }
            PropertiesInput::SetLockDifficulty(on) => {
                self.mutate_world(|w| w.difficulty_locked = on);
            }
            PropertiesInput::WorldLocked(locked) => {
                self.world_locked = locked;
                for row in &self.world_rows {
                    row.set_sensitive(!locked);
                }
            }
        }
    }
}

impl PropertiesPage {
    fn reload(&mut self, sender: &ComponentSender<Self>) {
        for group in self.groups.drain(..) {
            self.page.remove(&group);
        }
        self.world_rows.clear();
        self.dirty = false;

        // World settings from level.dat (hardcore etc.), if the world exists.
        self.world_level = backup::level_name(&self.ctx.paths);
        self.world = level_dat::read(&self.ctx.paths, &self.world_level)
            .ok()
            .flatten();
        if self.world.is_some() {
            let group = self.build_world_group(sender);
            self.page.add(&group);
            self.groups.push(group);

            // Learn whether the server is running (rows stay disabled until then).
            let s = sender.clone();
            relm4::spawn(async move {
                let active = scope_active().await;
                s.input(PropertiesInput::WorldLocked(active));
            });
        }

        let path = self.ctx.paths.server_file("server.properties");
        let text = match read_to_string_opt(&path) {
            Ok(Some(t)) => t,
            Ok(None) => {
                self.props = None;
                self.status =
                    "server.properties does not exist yet — it is created the first time the server runs.".to_string();
                let placeholder = adw::PreferencesGroup::new();
                placeholder.set_description(Some(&self.status));
                self.page.add(&placeholder);
                self.groups.push(placeholder);
                return;
            }
            Err(e) => {
                self.status = format!("Could not read server.properties: {e}");
                return;
            }
        };

        let props = Properties::parse(&text);
        let known = self.build_known_group(&props, sender);
        let other = self.build_other_group(&props, sender);
        self.page.add(&known);
        self.groups.push(known);
        if let Some(other) = other {
            self.page.add(&other);
            self.groups.push(other);
        }
        self.props = Some(props);
        self.status = "Loaded".to_string();
    }

    /// Apply a change to the world's `level.dat`. Refuses while the server runs.
    fn mutate_world(&mut self, change: impl FnOnce(&mut WorldSettings)) {
        if self.world_locked {
            self.status = "Stop the server before changing world settings.".to_string();
            return;
        }
        let Some(mut settings) = self.world else {
            return;
        };
        change(&mut settings);
        match level_dat::write(&self.ctx.paths, &self.world_level, &settings) {
            Ok(()) => {
                self.world = Some(settings);
                self.status =
                    "Saved to level.dat — effective on the next server start.".to_string();
            }
            Err(e) => self.status = format!("level.dat write failed: {e}"),
        }
    }

    fn build_world_group(&mut self, sender: &ComponentSender<Self>) -> adw::PreferencesGroup {
        let w = self.world.unwrap_or(WorldSettings {
            hardcore: false,
            difficulty: 2,
            difficulty_locked: false,
        });
        let enabled = !self.world_locked;

        let group = adw::PreferencesGroup::new();
        group.set_title("World (level.dat)");
        group.set_description(Some(
            "Stored in the world itself, not server.properties. The server must be stopped to change these.",
        ));

        let hardcore = adw::SwitchRow::new();
        hardcore.set_title("Hardcore");
        hardcore.set_subtitle(
            "Spectator on death, difficulty forced to hard. server.properties cannot change this after the world exists.",
        );
        hardcore.set_active(w.hardcore);
        hardcore.set_sensitive(enabled);
        let s = sender.clone();
        hardcore
            .connect_active_notify(move |r| s.input(PropertiesInput::SetHardcore(r.is_active())));
        group.add(&hardcore);
        self.world_rows.push(hardcore.upcast::<gtk::Widget>());

        let difficulty = adw::ComboRow::new();
        difficulty.set_title("Difficulty (world)");
        difficulty.set_subtitle(
            "server.properties `difficulty` overrides this on start — keep them in sync.",
        );
        difficulty.set_model(Some(&gtk::StringList::new(&DIFFICULTIES)));
        difficulty.set_selected(u32::from(w.difficulty.min(3)));
        difficulty.set_sensitive(enabled);
        let s = sender.clone();
        difficulty.connect_selected_notify(move |r| {
            s.input(PropertiesInput::SetDifficulty(r.selected().min(3) as u8));
        });
        group.add(&difficulty);
        self.world_rows.push(difficulty.upcast::<gtk::Widget>());

        let lock = adw::SwitchRow::new();
        lock.set_title("Lock difficulty");
        lock.set_subtitle(
            "Players cannot change difficulty in-game. Not available in server.properties.",
        );
        lock.set_active(w.difficulty_locked);
        lock.set_sensitive(enabled);
        let s = sender.clone();
        lock.connect_active_notify(move |r| {
            s.input(PropertiesInput::SetLockDifficulty(r.is_active()));
        });
        group.add(&lock);
        self.world_rows.push(lock.upcast::<gtk::Widget>());

        group
    }

    fn build_known_group(
        &self,
        props: &Properties,
        sender: &ComponentSender<Self>,
    ) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        group.set_title("server.properties");

        for def in CATALOG {
            let current = props.get(def.key).unwrap_or(def.default).to_string();
            let key = def.key.to_string();
            let s = sender.clone();
            let help = if restart_required(def.key) {
                format!("{} · takes effect on the next server start", def.help)
            } else {
                def.help.to_string()
            };
            let help = help.as_str();

            match def.kind {
                FieldKind::Bool => {
                    let row = adw::SwitchRow::new();
                    row.set_title(def.label);
                    row.set_tooltip_text(Some(help));
                    row.set_active(current.eq_ignore_ascii_case("true"));
                    row.connect_active_notify(move |r| {
                        s.input(PropertiesInput::Set(
                            key.clone(),
                            if r.is_active() { "true" } else { "false" }.to_string(),
                        ));
                    });
                    group.add(&row);
                }
                FieldKind::Int { min, max } => {
                    let row = adw::SpinRow::new(
                        Some(&gtk::Adjustment::new(
                            current.parse().unwrap_or(0.0),
                            min as f64,
                            max as f64,
                            1.0,
                            10.0,
                            0.0,
                        )),
                        1.0,
                        0,
                    );
                    row.set_title(def.label);
                    row.set_tooltip_text(Some(help));
                    row.connect_value_notify(move |r| {
                        s.input(PropertiesInput::Set(
                            key.clone(),
                            (r.value().round() as i64).to_string(),
                        ));
                    });
                    group.add(&row);
                }
                FieldKind::Choice(options) => {
                    // Show exactly what the file says even if it is not one of the
                    // known options (older worlds carry values like `level-type=default`).
                    let mut opts: Vec<String> = options.iter().map(|s| (*s).to_string()).collect();
                    if !opts.contains(&current) {
                        opts.push(current.clone());
                    }
                    let refs: Vec<&str> = opts.iter().map(String::as_str).collect();
                    let row = adw::ComboRow::new();
                    row.set_title(def.label);
                    row.set_tooltip_text(Some(help));
                    row.set_model(Some(&gtk::StringList::new(&refs)));
                    if let Some(pos) = opts.iter().position(|o| *o == current) {
                        row.set_selected(pos as u32);
                    }
                    row.connect_selected_notify(move |r| {
                        if let Some(v) = opts.get(r.selected() as usize) {
                            s.input(PropertiesInput::Set(key.clone(), v.clone()));
                        }
                    });
                    group.add(&row);
                }
                FieldKind::Text => {
                    let row = adw::EntryRow::new();
                    row.set_title(def.label);
                    row.set_tooltip_text(Some(help));
                    row.set_text(&current);
                    row.connect_changed(move |r| {
                        s.input(PropertiesInput::Set(key.clone(), r.text().to_string()));
                    });
                    group.add(&row);
                }
            }
        }
        group
    }

    fn build_other_group(
        &self,
        props: &Properties,
        sender: &ComponentSender<Self>,
    ) -> Option<adw::PreferencesGroup> {
        let unknown: Vec<(String, String)> = props
            .iter()
            .filter(|(k, _)| mcsm_core::properties_catalog::PropDef::find(k).is_none())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if unknown.is_empty() {
            return None;
        }

        let group = adw::PreferencesGroup::new();
        group.set_title("Other keys");
        group.set_description(Some(
            "Keys not covered by the form above (mod configs, custom entries).",
        ));
        for (key, value) in unknown {
            let row = adw::EntryRow::new();
            row.set_title(&key);
            row.set_text(&value);
            let s = sender.clone();
            let k = key.clone();
            row.connect_changed(move |r| {
                s.input(PropertiesInput::Set(k.clone(), r.text().to_string()));
            });
            group.add(&row);
        }
        Some(group)
    }
}
