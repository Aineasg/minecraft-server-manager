//! `server.properties` editor: a typed form driven by
//! [`mcsm_core::properties_catalog`], backed by the comment-preserving
//! [`mcsm_core::properties::Properties`] writer.

use adw::prelude::*;
use mcsm_core::properties::Properties;
use mcsm_core::properties_catalog::{FieldKind, CATALOG};
use mcsm_core::util::{read_to_string_opt, write_atomic};
use relm4::prelude::*;

use crate::context::Context;

pub struct PropertiesPage {
    ctx: Context,
    props: Option<Properties>,
    dirty: bool,
    status: String,
    page: adw::PreferencesPage,
    groups: Vec<adw::PreferencesGroup>,
}

#[derive(Debug)]
pub enum PropertiesInput {
    Reload,
    Set(String, String),
    Save,
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
        }
    }
}

impl PropertiesPage {
    fn reload(&mut self, sender: &ComponentSender<Self>) {
        for group in self.groups.drain(..) {
            self.page.remove(&group);
        }
        self.dirty = false;

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

            match def.kind {
                FieldKind::Bool => {
                    let row = adw::SwitchRow::new();
                    row.set_title(def.label);
                    row.set_tooltip_text(Some(def.help));
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
                    row.set_tooltip_text(Some(def.help));
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
                    row.set_tooltip_text(Some(def.help));
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
                    row.set_tooltip_text(Some(def.help));
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
        group.set_description(Some("Keys not covered by the form above (mod configs, custom entries)."));
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
