//! A plain-text editor for any file under `data/` — the escape hatch for
//! per-mod configs, `jvm-args.txt`, `eula.txt` and anything the typed forms
//! don't cover.

use std::path::PathBuf;

use adw::prelude::*;
use mcsm_core::util::write_atomic;
use relm4::prelude::*;

use crate::context::Context;
use crate::ui::widgets::human_bytes;

/// Files larger than this are shown read-only with a note (editing multi-MB
/// files in a TextView is pointless and slow).
const MAX_EDIT_BYTES: u64 = 2 * 1024 * 1024;

pub struct FilesPage {
    ctx: Context,
    files: Vec<PathBuf>,
    selected: Option<PathBuf>,
    editable: bool,
    dirty: bool,
    status: String,
    list: gtk::ListBox,
    buffer: gtk::TextBuffer,
}

#[derive(Debug)]
pub enum FilesInput {
    Reload,
    Select(usize),
    MarkDirty,
    Save,
}

#[relm4::component(pub)]
impl Component for FilesPage {
    type Init = Context;
    type Input = FilesInput;
    type Output = ();
    type CommandOutput = ();

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,

            gtk::Paned {
                set_vexpand: true,
                set_position: 260,
                set_shrink_start_child: false,

                #[wrap(Some)]
                set_start_child = &gtk::ScrolledWindow {
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    #[local_ref]
                    list -> gtk::ListBox {
                        add_css_class: "navigation-sidebar",
                        connect_row_activated[sender] => move |_, row| {
                            sender.input(FilesInput::Select(row.index() as usize));
                        },
                    },
                },

                #[wrap(Some)]
                set_end_child = &gtk::ScrolledWindow {
                    set_hexpand: true,
                    #[local_ref]
                    editor -> gtk::TextView {
                        set_monospace: true,
                        set_left_margin: 8,
                        set_right_margin: 8,
                        set_top_margin: 8,
                        set_bottom_margin: 8,
                        #[watch]
                        set_editable: model.editable,
                    },
                },
            },

            gtk::ActionBar {
                pack_start = &gtk::Button {
                    set_icon_name: "view-refresh-symbolic",
                    set_tooltip_text: Some("Rescan data/"),
                    connect_clicked => FilesInput::Reload,
                },
                pack_start = &gtk::Label {
                    add_css_class: "dim-label",
                    #[watch]
                    set_label: &model.status,
                },
                pack_end = &gtk::Button {
                    set_label: "Save",
                    add_css_class: "suggested-action",
                    #[watch]
                    set_sensitive: model.editable && model.dirty && model.selected.is_some(),
                    connect_clicked => FilesInput::Save,
                },
            },
        }
    }

    fn init(
        ctx: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let list = gtk::ListBox::new();
        let buffer = gtk::TextBuffer::new(None);
        let editor = gtk::TextView::with_buffer(&buffer);
        {
            let sender = sender.clone();
            buffer.connect_changed(move |_| sender.input(FilesInput::MarkDirty));
        }

        let mut model = FilesPage {
            ctx,
            files: Vec::new(),
            selected: None,
            editable: false,
            dirty: false,
            status: String::new(),
            list: list.clone(),
            buffer,
        };
        model.rescan();
        model.populate_list();

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            FilesInput::Reload => {
                self.rescan();
                self.populate_list();
            }
            FilesInput::Select(idx) => {
                let Some(path) = self.files.get(idx).cloned() else {
                    return;
                };
                self.load_file(&path);
            }
            FilesInput::MarkDirty => {
                if self.editable && self.selected.is_some() {
                    self.dirty = true;
                }
            }
            FilesInput::Save => {
                let Some(path) = self.selected.clone() else {
                    return;
                };
                let text = self
                    .buffer
                    .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false);
                match write_atomic(&path, text.as_bytes()) {
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

impl FilesPage {
    fn rescan(&mut self) {
        self.files = walk(&self.ctx.paths.data, 5);
        self.status = format!("{} file(s) under data/", self.files.len());
    }

    fn populate_list(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let base = &self.ctx.paths.data;
        for path in &self.files {
            let rel = path.strip_prefix(base).unwrap_or(path);
            let label = gtk::Label::new(Some(&rel.to_string_lossy()));
            label.set_xalign(0.0);
            label.set_margin_top(4);
            label.set_margin_bottom(4);
            label.set_margin_start(6);
            label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
            self.list.append(&label);
        }
    }

    fn load_file(&mut self, path: &std::path::Path) {
        self.selected = Some(path.to_path_buf());
        self.dirty = false;

        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        match std::fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) if size <= MAX_EDIT_BYTES => {
                    self.buffer.set_text(&text);
                    self.editable = true;
                    self.status = format!("{} — editable", human_bytes(size));
                }
                Ok(text) => {
                    self.buffer.set_text(&text);
                    self.editable = false;
                    self.status = format!("{} — too large to edit safely", human_bytes(size));
                }
                Err(_) => {
                    self.buffer.set_text("(binary file — not shown)");
                    self.editable = false;
                    self.status = format!("{} — binary", human_bytes(size));
                }
            },
            Err(e) => {
                self.editable = false;
                self.status = format!("Could not read file: {e}");
            }
        }
    }
}

/// Depth-limited recursive file list, skipping the pre-restore stashes and
/// obvious noise.
fn walk(root: &std::path::Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".pre-restore") || name == "cache" || name == "backups" {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_dir() && depth < max_depth => stack.push((path, depth + 1)),
                Ok(t) if t.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out.sort();
    out
}
