//! The main window: a sidebar that switches between pages held in an
//! `adw::ViewStack`, plus a banner for "install a server / accept the EULA".

use adw::prelude::*;
use relm4::prelude::*;

use crate::context::Context;
use crate::ui::access_page::{AccessInput, AccessOutput, AccessPage};
use crate::ui::backups_page::{BackupsInput, BackupsOutput, BackupsPage};
use crate::ui::dashboard::{DashboardInput, DashboardOutput, DashboardPage};
use crate::ui::files_page::{FilesInput, FilesPage};
use crate::ui::mods_page::{ModsInput, ModsPage};
use crate::ui::properties_page::{PropertiesInput, PropertiesPage};
use crate::ui::settings_page::{SettingsOutput, SettingsPage};

/// `(stack id, sidebar label, hover description)` in sidebar order.
const PAGES: [(&str, &str, &str); 7] = [
    ("dashboard", "Dashboard", "Start, stop and restart the server, watch its memory, read the console and send commands"),
    ("mods", "Mods", "Search Modrinth, install mods with their dependencies, enable/disable and update"),
    ("properties", "Properties", "Edit server.properties as a form, plus world-only settings (hardcore) stored in level.dat"),
    ("access", "Player access", "Operators, whitelist and bans — applied live when the server is running"),
    ("files", "Files", "A plain-text editor for any file under the data folder"),
    ("backups", "Backups", "Schedule automatic world backups, and create, restore or delete them"),
    ("settings", "Settings", "Minecraft/Fabric version, memory budget, Java, EULA, backup folder"),
];
const SETTINGS_INDEX: i32 = 6;

pub struct AppInit {
    pub context: Context,
}

pub struct App {
    ctx: Context,
    dashboard: Controller<DashboardPage>,
    mods: Controller<ModsPage>,
    properties: Controller<PropertiesPage>,
    access: Controller<AccessPage>,
    files: Controller<FilesPage>,
    backups: Controller<BackupsPage>,
    settings: Controller<SettingsPage>,
    stack: adw::ViewStack,
    banner: adw::Banner,
}

#[derive(Debug)]
pub enum AppMsg {
    ShowPage(i32),
    RefreshBanner,
    ReloadServerPages,
    /// Backups page asked for a manual backup.
    BackupNow,
    /// Auto-backup timer fired.
    AutoBackup,
    /// A backup was taken; refresh the Backups list.
    ReloadBackups,
    /// Player access page wants a console command run on the live server.
    RunServerCommand(String),
    /// Open the data folder in the file manager.
    OpenDataFolder,
}

#[relm4::component(pub)]
impl Component for App {
    type Init = AppInit;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some("Minecraft Server Manager"),
            set_default_width: 1040,
            set_default_height: 720,

            adw::NavigationSplitView {
                set_max_sidebar_width: 260.0,

                #[wrap(Some)]
                set_sidebar = &adw::NavigationPage {
                    set_title: "Minecraft Server Manager",
                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        add_top_bar = &adw::HeaderBar {},
                        #[wrap(Some)]
                        set_content = &gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,
                            #[local_ref]
                            sidebar_list -> gtk::ListBox {
                                add_css_class: "navigation-sidebar",
                                connect_row_activated[sender] => move |_, row| {
                                    sender.input(AppMsg::ShowPage(row.index()));
                                },
                            },
                        },
                    },
                },

                #[wrap(Some)]
                set_content = &adw::NavigationPage {
                    set_title: "Server",
                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        add_top_bar = &adw::HeaderBar {
                            pack_end = &gtk::Button {
                                set_icon_name: "folder-symbolic",
                                set_tooltip_text: Some("Open the data folder (server, world, mods, config)"),
                                connect_clicked => AppMsg::OpenDataFolder,
                            },
                        },
                        #[wrap(Some)]
                        set_content = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,

                            #[local_ref]
                            banner -> adw::Banner {
                                set_button_label: Some("Open Settings"),
                                connect_button_clicked => AppMsg::ShowPage(SETTINGS_INDEX),
                            },

                            #[local_ref]
                            stack -> adw::ViewStack {
                                set_vexpand: true,
                            },
                        },
                    },
                },
            }
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let ctx = init.context;

        let dashboard = DashboardPage::builder()
            .launch(ctx.clone())
            .forward(sender.input_sender(), |out| match out {
                DashboardOutput::OpenSettings => AppMsg::ShowPage(SETTINGS_INDEX),
                DashboardOutput::BackupsChanged => AppMsg::ReloadBackups,
            });
        let mods = ModsPage::builder().launch(ctx.clone()).detach();
        let properties = PropertiesPage::builder().launch(ctx.clone()).detach();
        let access = AccessPage::builder().launch(ctx.clone()).forward(
            sender.input_sender(),
            |out| match out {
                AccessOutput::RunCommand(cmd) => AppMsg::RunServerCommand(cmd),
            },
        );
        let files = FilesPage::builder().launch(ctx.clone()).detach();
        let backups = BackupsPage::builder().launch(ctx.clone()).forward(
            sender.input_sender(),
            |out| match out {
                BackupsOutput::BackupNowRequested => AppMsg::BackupNow,
                BackupsOutput::AutoBackupDue => AppMsg::AutoBackup,
            },
        );
        let settings = SettingsPage::builder().launch(ctx.clone()).forward(
            sender.input_sender(),
            |out| match out {
                SettingsOutput::Changed => AppMsg::RefreshBanner,
                SettingsOutput::Installed => AppMsg::ReloadServerPages,
                SettingsOutput::BackupDirChanged => AppMsg::ReloadBackups,
            },
        );

        let stack = adw::ViewStack::new();
        let banner = adw::Banner::new("");
        let sidebar_list = gtk::ListBox::new();

        let model = App {
            ctx,
            dashboard,
            mods,
            properties,
            access,
            files,
            backups,
            settings,
            stack: stack.clone(),
            banner: banner.clone(),
        };

        let widgets = view_output!();

        for (id, title, description) in PAGES {
            let child: &gtk::Widget = match id {
                "dashboard" => model.dashboard.widget().upcast_ref(),
                "mods" => model.mods.widget().upcast_ref(),
                "properties" => model.properties.widget().upcast_ref(),
                "access" => model.access.widget().upcast_ref(),
                "files" => model.files.widget().upcast_ref(),
                "backups" => model.backups.widget().upcast_ref(),
                "settings" => model.settings.widget().upcast_ref(),
                _ => unreachable!(),
            };
            model.stack.add_titled(child, Some(id), title);

            let label = gtk::Label::new(Some(title));
            label.set_xalign(0.0);
            label.set_margin_top(8);
            label.set_margin_bottom(8);
            label.set_margin_start(6);
            label.set_tooltip_text(Some(description));
            sidebar_list.append(&label);
        }
        if let Some(first) = sidebar_list.row_at_index(0) {
            sidebar_list.select_row(Some(&first));
        }

        model.refresh_banner();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            AppMsg::ShowPage(index) => {
                let Some((id, ..)) = PAGES.get(index.max(0) as usize) else {
                    return;
                };
                self.stack.set_visible_child_name(id);
                self.reload_page(id);
            }
            AppMsg::RefreshBanner => self.refresh_banner(),
            AppMsg::ReloadServerPages => {
                self.refresh_banner();
                let _ = self.dashboard.sender().send(DashboardInput::Reload);
                let _ = self.mods.sender().send(ModsInput::Reload);
                let _ = self.properties.sender().send(PropertiesInput::Reload);
                let _ = self.access.sender().send(AccessInput::Reload);
                let _ = self.backups.sender().send(BackupsInput::Reload);
                let _ = self.files.sender().send(FilesInput::Reload);
            }
            AppMsg::BackupNow => {
                let _ = self.dashboard.sender().send(DashboardInput::BackupNow);
            }
            AppMsg::AutoBackup => {
                let _ = self.dashboard.sender().send(DashboardInput::AutoBackup);
            }
            AppMsg::ReloadBackups => {
                let _ = self.backups.sender().send(BackupsInput::Reload);
            }
            AppMsg::RunServerCommand(cmd) => {
                let _ = self.dashboard.sender().send(DashboardInput::RunCommand(cmd));
            }
            AppMsg::OpenDataFolder => {
                let file = relm4::gtk::gio::File::for_path(&self.ctx.paths.data);
                relm4::gtk::FileLauncher::new(Some(&file)).launch(
                    relm4::gtk::Window::NONE,
                    relm4::gtk::gio::Cancellable::NONE,
                    |_| {},
                );
            }
        }
    }
}

impl App {
    fn reload_page(&self, id: &str) {
        match id {
            "mods" => {
                let _ = self.mods.sender().send(ModsInput::Reload);
            }
            "properties" => {
                let _ = self.properties.sender().send(PropertiesInput::Reload);
            }
            "access" => {
                let _ = self.access.sender().send(AccessInput::Reload);
            }
            "backups" => {
                let _ = self.backups.sender().send(BackupsInput::Reload);
            }
            "files" => {
                let _ = self.files.sender().send(FilesInput::Reload);
            }
            _ => {}
        }
    }

    fn refresh_banner(&self) {
        let state = self.ctx.state.borrow();
        let message = if state.minecraft_version.is_none() {
            Some("No server installed yet — open Settings to download one.")
        } else if !state.eula_accepted {
            Some("The Minecraft EULA has not been accepted — do so in Settings before starting the server.")
        } else {
            None
        };
        match message {
            Some(text) => {
                self.banner.set_title(text);
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
    }
}
