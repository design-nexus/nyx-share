//! The Share window: nearby devices to send to, transfers, and the Nearby
//! settings. A compact floating window, like the Share Sheet. While it is open
//! the service keeps running, so this computer stays visible to nearby devices.

use crate::sheet;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use nyx_share_client::{
    Client, Content, ContentKind, Destination, Direction, Event, Job, JobState, NearbyStatus, NearbyUpdate, ShareOptions,
};
use nyx_share_core::{Section, units};
use nyx_ui::widgets;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
enum Chosen {
    File(PathBuf),
    Text(String),
}

impl Chosen {
    fn kind(&self) -> ContentKind {
        match self {
            Chosen::Text(t) if t.trim().starts_with("http://") || t.trim().starts_with("https://") => ContentKind::Url,
            Chosen::Text(_) => ContentKind::Text,
            Chosen::File(p) => {
                let image = p
                    .file_name()
                    .and_then(|n| nyx_share_core::mime::guess(&n.to_string_lossy()))
                    .is_some_and(|m| m.starts_with("image/"));
                if image { ContentKind::Image } else { ContentKind::File }
            }
        }
    }

    fn label(&self) -> String {
        match self {
            Chosen::File(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            Chosen::Text(t) => {
                let line = t.lines().next().unwrap_or_default().trim();
                let short: String = line.chars().take(60).collect();
                if short.len() < line.len() { format!("{short}…") } else { short }
            }
        }
    }
}

struct App {
    window: gtk::ApplicationWindow,
    header: nyx_ui::chrome::AppHeader,
    client: Arc<Client>,
    tabs: gtk::Box,
    back: gtk::Button,
    refresh: gtk::Button,
    stack: gtk::Stack,
    tray: gtk::Box,
    devices: gtk::Box,
    transfers: gtk::Box,
    settings: gtk::Box,
    visible: gtk::Switch,
    visible_label: gtk::Label,
    receiving: gtk::Label,
    footer: gtk::Box,
    items: RefCell<Vec<Chosen>>,
    dests: RefCell<Vec<Destination>>,
    jobs: RefCell<Vec<Job>>,
    /// The latest job started from this window, per destination.
    sent: RefCell<HashMap<String, (String, Instant)>>,
    nearby: RefCell<NearbyStatus>,
    page: RefCell<String>,
    updating: Cell<bool>,
    check_send: Cell<bool>,
}

thread_local! {
    static WINDOW: RefCell<Option<glib::WeakRef<gtk::ApplicationWindow>>> = const { RefCell::new(None) };
}

/// The open Share window, if any.
pub fn window() -> Option<gtk::ApplicationWindow> {
    WINDOW.with(|w| w.borrow().as_ref().and_then(|w| w.upgrade()))
}

pub fn present(app: &gtk::Application) {
    if let Some(w) = window() {
        w.present();
        return;
    }
    let hold = app.hold();
    let app = app.clone();
    nyx_core::cmd::background(
        || Client::connect().map_err(|e| e.to_string()),
        move |client| {
            match client {
                Ok(client) => build(&app, Arc::new(client)),
                Err(e) => sheet::failed(&app, &e),
            }
            drop(hold);
        },
    );
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

fn icon(name: &str, size: i32) -> gtk::Image {
    let i = gtk::Image::from_icon_name(name);
    i.set_pixel_size(size);
    i
}

fn short_key(dest: &Destination) -> String {
    let f = dest.id.strip_prefix("localsend:").unwrap_or_default();
    if f.len() < 8 { String::new() } else { format!("{} … {}", &f[..4], &f[f.len() - 4..]) }
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

const ARROWS: [(&str, &str); 2] = [
    ("nyx-share-up-symbolic", "M12 3.5l6.5 6.6-1.4 1.4-4.1-4.2V20h-2V7.3l-4.1 4.2-1.4-1.4z"),
    ("nyx-share-down-symbolic", "M12 20.5l-6.5-6.6 1.4-1.4 4.1 4.2V4h2v12.7l4.1-4.2 1.4 1.4z"),
];

/// Arrow icons for Transfers, which the icon theme doesn't have. Filled paths:
/// GTK fills every path of a symbolic icon.
fn install_icons() {
    let dir = nyx_core::paths::cache_dir().join("nyx-icons");
    for (name, path) in ARROWS {
        let file = dir.join(format!("{name}.svg"));
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\" viewBox=\"0 0 24 24\"><path fill=\"#000000\" d=\"{path}\"/></svg>\n"
        );
        if std::fs::read_to_string(&file).ok().as_deref() != Some(svg.as_str()) {
            let _ = nyx_core::cmd::atomic_write(&file, svg);
        }
    }
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(dir);
    }
}

fn build(app: &gtk::Application, client: Arc<Client>) {
    install_icons();
    let (window, header, body) = sheet::window(app);
    // The same width on every page.
    window.set_size_request(560, -1);
    header.set_subtitle("");
    let tabs = widgets::segmented(&widgets::opts(&[("nearby", "Nearby"), ("transfers", "Transfers")]), "nearby", |_| {});
    tabs.set_halign(gtk::Align::Center);
    header.center.append(&tabs);
    let back = widgets::icon_button("go-previous-symbolic", "Back (Esc)");
    back.set_visible(false);
    match header.subtitle.parent().and_downcast::<gtk::Box>() {
        Some(title) => title.prepend(&back),
        None => header.start.append(&back),
    }
    let refresh = widgets::icon_button("view-refresh-symbolic", "Look for devices (F5)");
    header.end.prepend(&refresh);

    let stack = gtk::Stack::new();
    stack.set_size_request(528, -1);
    stack.set_vhomogeneous(false);
    stack.set_hhomogeneous(true);
    stack.set_interpolate_size(true);
    let nearby = widgets::vbox(0);
    let tray = widgets::hbox(12);
    tray.add_css_class("card");
    tray.add_css_class("app-tray");
    nearby.append(&tray);
    let devices = widgets::vbox(0);
    nearby.append(&devices);
    stack.add_named(&nearby, Some("nearby"));
    let transfers = widgets::vbox(0);
    stack.add_named(&transfers, Some("transfers"));
    let settings = widgets::vbox(0);
    stack.add_named(&settings, Some("settings"));
    body.append(&stack);

    let footer = widgets::hbox(10);
    footer.add_css_class("app-footer");
    let visible = gtk::Switch::new();
    visible.set_valign(gtk::Align::Center);
    visible.set_tooltip_text(Some("Visible to nearby devices"));
    let visible_label = widgets::label("", "app-footer-title");
    let receiving = widgets::label("", "app-footer-note");
    receiving.set_hexpand(true);
    receiving.set_xalign(0.0);
    receiving.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let settings_button = nyx_ui::chrome::settings_button(|| {});
    footer.append(&visible);
    footer.append(&visible_label);
    footer.append(&receiving);
    footer.append(&settings_button);
    if let Some(root) = body.parent().and_downcast::<gtk::Box>() {
        root.append(&footer);
    }

    let me = Rc::new(App {
        window: window.clone(),
        header,
        client,
        tabs,
        back,
        refresh,
        stack,
        tray,
        devices,
        transfers,
        settings,
        visible,
        visible_label,
        receiving,
        footer,
        items: RefCell::default(),
        dests: RefCell::default(),
        jobs: RefCell::default(),
        sent: RefCell::default(),
        nearby: RefCell::default(),
        page: RefCell::new("nearby".into()),
        updating: Cell::new(false),
        check_send: Cell::new(std::env::var_os("NYX_SHARE_CHECK_SEND").is_some()),
    });
    me.wire(&settings_button);
    me.render_tray();
    me.render_devices();
    me.render_transfers();
    listen(&me);
    me.load();
    me.check_hooks();
    WINDOW.with(|w| *w.borrow_mut() = Some(window.downgrade()));
    window.present();
    window.connect_close_request(move |_| {
        // The window owns the app state through this handler.
        let _ = &me;
        glib::Propagation::Proceed
    });
}

fn listen(app: &Rc<App>) {
    let (tx, rx) = async_channel::unbounded::<Event>();
    let client = app.client.clone();
    std::thread::spawn(move || {
        let Ok(events) = client.events() else { return };
        for e in events {
            if tx.send_blocking(e).is_err() {
                break;
            }
        }
    });
    let weak = Rc::downgrade(app);
    glib::spawn_future_local(async move {
        while let Ok(e) = rx.recv().await {
            let Some(app) = weak.upgrade() else { break };
            match e {
                Event::JobChanged(job) => app.job_changed(job),
                Event::DestinationsChanged => app.load_destinations(),
                Event::NearbyChanged => app.load_nearby(),
            }
        }
    });
}

impl App {
    /// For checks and screenshots: `NYX_SHARE_CHECK_ADD=/a:/b` puts files in the
    /// tray, `NYX_SHARE_CHECK_PAGE=transfers|settings` opens a page, and
    /// `NYX_SHARE_CHECK_SEND=1` sends to the first device once one appears.
    fn check_hooks(self: &Rc<Self>) {
        if let Ok(paths) = std::env::var("NYX_SHARE_CHECK_ADD") {
            self.add_files(paths.split(':').filter(|p| !p.is_empty()).map(PathBuf::from).collect());
        }
        if let Ok(page) = std::env::var("NYX_SHARE_CHECK_PAGE") {
            self.show(&page);
        }
    }

    fn wire(self: &Rc<Self>, settings_button: &gtk::Button) {
        // Segmented buttons: the first two buttons of the box are the tabs.
        let mut child = self.tabs.first_child();
        for page in ["nearby", "transfers"] {
            let Some(button) = child.clone().and_downcast::<gtk::ToggleButton>() else { break };
            let weak = Rc::downgrade(self);
            button.connect_toggled(move |b| {
                if b.is_active()
                    && let Some(app) = weak.upgrade()
                {
                    app.show(page);
                }
            });
            child = button.next_sibling();
        }
        let weak = Rc::downgrade(self);
        self.back.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.show("nearby");
            }
        });
        let weak = Rc::downgrade(self);
        settings_button.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.show("settings");
            }
        });
        let weak = Rc::downgrade(self);
        self.refresh.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.scan();
            }
        });
        let weak = Rc::downgrade(self);
        self.visible.connect_active_notify(move |s| {
            let Some(app) = weak.upgrade() else { return };
            if app.updating.get() {
                return;
            }
            let on = s.is_active();
            app.set_nearby(NearbyUpdate { visibility: Some(if on { "running" } else { "off" }.into()), ..Default::default() });
        });

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(app) = weak.upgrade() else { return glib::Propagation::Proceed };
            match key {
                gdk::Key::F5 => app.scan(),
                gdk::Key::comma if mods.contains(gdk::ModifierType::CONTROL_MASK) => app.show("settings"),
                gdk::Key::Escape if app.page.borrow().as_str() == "settings" => app.show("nearby"),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.window.add_controller(keys);

        let drop = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop.set_types(&[gdk::FileList::static_type(), String::static_type()]);
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |_, value, _, _| {
            let Some(app) = weak.upgrade() else { return false };
            if let Ok(list) = value.get::<gdk::FileList>() {
                let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
                app.add_files(paths);
                return true;
            }
            if let Ok(text) = value.get::<String>() {
                app.add(Chosen::Text(text));
                return true;
            }
            false
        });
        self.window.add_controller(drop);
    }

    fn show(self: &Rc<Self>, page: &str) {
        *self.page.borrow_mut() = page.to_string();
        let settings = page == "settings";
        self.tabs.set_visible(!settings);
        self.back.set_visible(settings);
        self.refresh.set_visible(page == "nearby");
        self.footer.set_visible(!settings);
        self.header.set_subtitle(if settings { "Settings" } else { "" });
        if settings {
            self.render_settings();
        }
        if page == "transfers" {
            self.load_jobs();
        }
        if !settings {
            let mut child = self.tabs.first_child();
            for p in ["nearby", "transfers"] {
                if let Some(b) = child.clone().and_downcast::<gtk::ToggleButton>() {
                    if p == page && !b.is_active() {
                        b.set_active(true);
                    }
                    child = b.next_sibling();
                }
            }
        }
        self.stack.set_visible_child_name(page);
    }

    /// Keep the service running while the window is open, then fetch everything.
    fn load(self: &Rc<Self>) {
        let client = self.client.clone();
        std::thread::spawn(move || {
            let _ = client.hold();
            let _ = client.scan();
        });
        self.load_destinations();
        self.load_nearby();
        self.load_jobs();
    }

    fn scan(self: &Rc<Self>) {
        let client = self.client.clone();
        std::thread::spawn(move || client.scan());
        self.load_destinations();
    }

    fn load_destinations(self: &Rc<Self>) {
        let client = self.client.clone();
        let weak = Rc::downgrade(self);
        nyx_core::cmd::background(
            move || client.destinations(&[], 0),
            move |result| {
                let Some(app) = weak.upgrade() else { return };
                if let Ok(all) = result {
                    *app.dests.borrow_mut() = all
                        .into_iter()
                        .filter(|d| matches!(d.section, Section::Nearby | Section::Devices) && !d.hidden)
                        .collect();
                    app.render_devices();
                    let first = app.dests.borrow().first().map(|d| d.id.clone());
                    if let Some(id) = first.filter(|_| app.check_send.replace(false)) {
                        app.send(&id);
                    }
                }
            },
        );
    }

    fn load_nearby(self: &Rc<Self>) {
        let client = self.client.clone();
        let weak = Rc::downgrade(self);
        nyx_core::cmd::background(
            move || client.nearby(),
            move |result| {
                let Some(app) = weak.upgrade() else { return };
                if let Ok(status) = result {
                    *app.nearby.borrow_mut() = status;
                    app.render_footer();
                    app.render_devices();
                    if app.page.borrow().as_str() == "settings" {
                        app.render_settings();
                    }
                }
            },
        );
    }

    fn load_jobs(self: &Rc<Self>) {
        let client = self.client.clone();
        let weak = Rc::downgrade(self);
        nyx_core::cmd::background(
            move || client.jobs(),
            move |result| {
                let Some(app) = weak.upgrade() else { return };
                if let Ok(jobs) = result {
                    *app.jobs.borrow_mut() = jobs;
                    app.render_transfers();
                    app.render_devices();
                }
            },
        );
    }

    fn set_nearby(self: &Rc<Self>, update: NearbyUpdate) {
        let client = self.client.clone();
        let weak = Rc::downgrade(self);
        nyx_core::cmd::background(
            move || client.set_nearby(&update).map_err(|e| e.to_string()),
            move |result| {
                let Some(app) = weak.upgrade() else { return };
                if let Err(e) = result {
                    app.toast(&e);
                }
                app.load_nearby();
            },
        );
    }

    fn toast(&self, message: &str) {
        self.receiving.set_text(message);
        self.receiving.add_css_class("error");
    }

    fn job_changed(self: &Rc<Self>, job: Job) {
        {
            let mut jobs = self.jobs.borrow_mut();
            match jobs.iter_mut().find(|j| j.id == job.id) {
                Some(j) => *j = job,
                None => jobs.insert(0, job),
            }
        }
        self.render_transfers();
        self.render_devices();
    }

    // ---- Tray ----

    fn add_files(self: &Rc<Self>, paths: Vec<PathBuf>) {
        let mut items = self.items.borrow_mut();
        for p in paths.into_iter().filter(|p| p.is_file()) {
            if !items.iter().any(|i| matches!(i, Chosen::File(q) if *q == p)) {
                items.push(Chosen::File(p));
            }
        }
        drop(items);
        self.render_tray();
        self.render_devices();
    }

    fn add(self: &Rc<Self>, item: Chosen) {
        self.items.borrow_mut().push(item);
        self.render_tray();
        self.render_devices();
    }

    fn render_tray(self: &Rc<Self>) {
        clear(&self.tray);
        let items = self.items.borrow().clone();
        if items.is_empty() {
            self.tray.add_css_class("idle");
            let drop_icon = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            drop_icon.add_css_class("app-drop");
            let i = icon("document-send-symbolic", 20);
            i.set_size_request(40, 40);
            drop_icon.append(&i);
            self.tray.append(&drop_icon);
            let text = widgets::vbox(1);
            text.set_hexpand(true);
            text.set_valign(gtk::Align::Center);
            let t = widgets::label("Drop files to send", "preview-title");
            t.set_xalign(0.0);
            let s = widgets::label("Or choose files or text, then a device.", "preview-subtitle");
            s.set_xalign(0.0);
            text.append(&t);
            text.append(&s);
            self.tray.append(&text);
            let files = widgets::labeled_button("document-open-symbolic", "Files…");
            files.set_valign(gtk::Align::Center);
            let weak = Rc::downgrade(self);
            files.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.choose_files();
                }
            });
            let text_button = widgets::labeled_button("format-justify-left-symbolic", "Text…");
            text_button.set_valign(gtk::Align::Center);
            let weak = Rc::downgrade(self);
            text_button.connect_clicked(move |_| {
                let weak = weak.clone();
                widgets::ask_text("Send text or a link", "Type or paste what to send.", "", "Add", move |text| {
                    if let Some(app) = weak.upgrade()
                        && !text.trim().is_empty()
                    {
                        app.add(Chosen::Text(text));
                    }
                });
            });
            self.tray.append(&files);
            self.tray.append(&text_button);
            return;
        }
        self.tray.remove_css_class("idle");
        let thumbs = widgets::hbox(6);
        thumbs.set_valign(gtk::Align::Center);
        for item in items.iter().take(3) {
            let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            frame.add_css_class("preview-thumb");
            frame.set_overflow(gtk::Overflow::Hidden);
            let gicon: gio::Icon = match (item, item.kind()) {
                (_, ContentKind::Url) => gio::ThemedIcon::new("web-browser-symbolic").upcast(),
                (Chosen::File(p), _) => {
                    let mime =
                        p.file_name().and_then(|n| nyx_share_core::mime::guess(&n.to_string_lossy())).unwrap_or("text/plain");
                    gio::content_type_get_symbolic_icon(mime)
                }
                _ => gio::ThemedIcon::new("text-x-generic-symbolic").upcast(),
            };
            let i = gtk::Image::from_gicon(&gicon);
            i.set_pixel_size(18);
            i.set_size_request(40, 40);
            frame.append(&i);
            if let (Chosen::File(path), ContentKind::Image) = (item, item.kind()) {
                load_thumb(path.clone(), &frame);
            }
            thumbs.append(&frame);
        }
        self.tray.append(&thumbs);
        let text = widgets::vbox(1);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let labels: Vec<String> = items.iter().map(Chosen::label).collect();
        let title = nyx_share_core::summarize(labels.iter().map(String::as_str)).unwrap_or_default();
        let t = widgets::label(&title, "preview-title");
        t.set_xalign(0.0);
        t.set_ellipsize(gtk::pango::EllipsizeMode::End);
        t.set_max_width_chars(36);
        let size: u64 =
            items.iter().filter_map(|i| if let Chosen::File(p) = i { p.metadata().ok().map(|m| m.len()) } else { None }).sum();
        let count = if items.len() == 1 { "1 item".to_string() } else { format!("{} items", items.len()) };
        let sub = if size > 0 { format!("{count} · {}", units::size(size)) } else { count };
        let s = widgets::label(&sub, "preview-subtitle");
        s.set_xalign(0.0);
        text.append(&t);
        text.append(&s);
        self.tray.append(&text);
        let add = widgets::icon_button("list-add-symbolic", "Add files");
        let weak = Rc::downgrade(self);
        add.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.choose_files();
            }
        });
        let clear_button = widgets::icon_button("window-close-symbolic", "Clear");
        let weak = Rc::downgrade(self);
        clear_button.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.items.borrow_mut().clear();
                app.render_tray();
                app.render_devices();
            }
        });
        self.tray.append(&add);
        self.tray.append(&clear_button);
    }

    fn choose_files(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Choose files to send").accept_label("Choose").modal(true).build();
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            let Ok(model) = dialog.open_multiple_future(Some(&window)).await else { return };
            let Some(app) = weak.upgrade() else { return };
            let paths =
                (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gio::File>()).filter_map(|f| f.path()).collect();
            app.add_files(paths);
        });
    }

    // ---- Devices ----

    fn render_devices(self: &Rc<Self>) {
        clear(&self.devices);
        let head = widgets::hbox(8);
        head.add_css_class("share-section-title");
        head.append(&widgets::label("Nearby devices", ""));
        let status = self.nearby.borrow().clone();
        if status.enabled {
            head.append(&widgets::label("Looking", "app-live"));
        }
        self.devices.append(&head);
        if !status.enabled {
            let (title, desc) = match &status.problem {
                Some(p) => ("Nearby sharing isn't working", p.clone()),
                None => ("Nearby sharing is off", "Turn it on to see devices and receive from them.".to_string()),
            };
            let weak = Rc::downgrade(self);
            let action: widgets::Action<'_> = Some((
                "Turn on",
                Box::new(move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_nearby(NearbyUpdate { visibility: Some("running".into()), ..Default::default() });
                    }
                }),
            ));
            let e = widgets::empty_state("network-wireless-disabled-symbolic", title, &glib::markup_escape_text(&desc), action);
            e.add_css_class("card");
            e.add_css_class("app-empty");
            self.devices.append(&e);
            return;
        }
        let dests = self.dests.borrow().clone();
        if dests.is_empty() {
            let weak = Rc::downgrade(self);
            let action: widgets::Action<'_> = Some((
                "Look again",
                Box::new(move || {
                    if let Some(app) = weak.upgrade() {
                        app.scan();
                    }
                }),
            ));
            let e = widgets::empty_state(
                "network-wireless-symbolic",
                "No devices nearby yet",
                "Open LocalSend on your phone or computer, on the same network.",
                action,
            );
            e.add_css_class("card");
            e.add_css_class("app-empty");
            self.devices.append(&e);
            return;
        }
        let items = self.items.borrow().clone();
        let kinds = nyx_share_core::kinds_of(&items.iter().map(Chosen::kind).collect::<Vec<_>>());
        let list = widgets::vbox(0);
        list.add_css_class("card");
        list.add_css_class("app-devices");
        for (i, d) in dests.iter().enumerate() {
            let row = self.device_row(d, &items, &kinds);
            if i > 0 {
                row.add_css_class("following");
            }
            list.append(&row);
        }
        self.devices.append(&list);
    }

    fn job_for(&self, dest: &str) -> Option<(Job, Instant)> {
        let (id, started) = self.sent.borrow().get(dest).cloned()?;
        self.jobs.borrow().iter().find(|j| j.id == id).cloned().map(|j| (j, started))
    }

    fn device_row(self: &Rc<Self>, d: &Destination, items: &[Chosen], kinds: &[ContentKind]) -> gtk::Box {
        let row = widgets::hbox(12);
        row.add_css_class("app-device");
        let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tile.add_css_class("dest-icon");
        tile.add_css_class("mini");
        tile.add_css_class(&format!("tint-{}", d.tint));
        tile.set_valign(gtk::Align::Center);
        let i = icon(&d.icon, 18);
        i.set_size_request(36, 36);
        tile.append(&i);
        row.append(&tile);
        let text = widgets::vbox(1);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let name = widgets::label(&d.name, "job-title");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&name);
        let sub = widgets::label("", "job-caption");
        sub.set_xalign(0.0);
        sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&sub);
        row.append(&text);
        let job = self.job_for(&d.id);
        match job {
            Some((job, started)) if !job.state.finished() => {
                let pct = job.fraction().map(|f| format!("{:.0}%", f * 100.0)).unwrap_or_else(|| "Waiting".into());
                let secs = started.elapsed().as_secs_f64();
                let rate = (secs > 0.5 && job.bytes_done > 0)
                    .then(|| format!(" · {}/s", units::size((job.bytes_done as f64 / secs) as u64)));
                sub.set_text(&format!("Sending · {pct}{}", rate.unwrap_or_default()));
                let bar = gtk::LevelBar::new();
                bar.set_value(job.fraction().unwrap_or(0.0));
                bar.set_margin_top(5);
                text.append(&bar);
                let cancel = widgets::icon_button("process-stop-symbolic", "Cancel");
                let client = self.client.clone();
                let id = job.id.clone();
                cancel.connect_clicked(move |b| {
                    b.set_sensitive(false);
                    let (client, id) = (client.clone(), id.clone());
                    std::thread::spawn(move || client.cancel(&id));
                });
                row.append(&cancel);
                return row;
            }
            Some((job, _)) if job.state == JobState::Completed => {
                sub.set_text(job.message.as_deref().unwrap_or("Sent"));
                sub.add_css_class("ok");
            }
            Some((job, _)) if job.state == JobState::Failed => {
                sub.set_text(job.message.as_deref().unwrap_or("Couldn't send"));
                sub.add_css_class("error");
            }
            _ => {
                let key = short_key(d);
                let status = d.status.clone().unwrap_or_default();
                sub.set_text(&[status, key].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "));
            }
        }
        let can = !items.is_empty() && d.available && d.accepts(kinds, items.len());
        let again = self.job_for(&d.id).is_some_and(|(j, _)| j.state.finished());
        let send = gtk::Button::with_label(if again && can { "Again" } else { "Send" });
        send.set_valign(gtk::Align::Center);
        send.set_sensitive(can);
        if can && !again {
            send.add_css_class("suggested-action");
        }
        if !d.available {
            send.set_tooltip_text(d.status.as_deref());
        } else if items.is_empty() {
            send.set_tooltip_text(Some("Choose files or text first"));
        }
        let weak = Rc::downgrade(self);
        let id = d.id.clone();
        send.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.send(&id);
            }
        });
        row.append(&send);
        row
    }

    fn send(self: &Rc<Self>, dest: &str) {
        let items = self.items.borrow().clone();
        if items.is_empty() {
            return;
        }
        let client = self.client.clone();
        let (dest_id, key) = (dest.to_string(), dest.to_string());
        let weak = Rc::downgrade(self);
        nyx_core::cmd::background(
            move || -> Result<Job, String> {
                let mut content = Content::new();
                for item in &items {
                    match item {
                        Chosen::File(p) => {
                            content.file(p).map_err(|e| e.to_string())?;
                        }
                        Chosen::Text(t) if item.kind() == ContentKind::Url => {
                            content.url(t.trim());
                        }
                        Chosen::Text(t) => {
                            content.text(t.clone());
                        }
                    }
                }
                let options = ShareOptions { app_name: Some("Share".into()), notify: Some(false), ..Default::default() };
                let id = client.share_to(&dest_id, &content, &options).map_err(|e| e.to_string())?;
                client.job(&id).map_err(|e| e.to_string())
            },
            move |result: Result<Job, String>| {
                let Some(app) = weak.upgrade() else { return };
                match result {
                    Ok(job) => {
                        app.sent.borrow_mut().insert(key, (job.id.clone(), Instant::now()));
                        // A later signal may already be in the list; keep the newer one.
                        let known = app.jobs.borrow().iter().any(|j| j.id == job.id);
                        if !known {
                            app.jobs.borrow_mut().insert(0, job);
                        }
                        app.render_devices();
                        app.render_transfers();
                    }
                    Err(e) => app.toast(&e),
                }
            },
        );
    }

    // ---- Transfers ----

    fn render_transfers(self: &Rc<Self>) {
        clear(&self.transfers);
        let jobs = self.jobs.borrow().clone();
        if jobs.is_empty() {
            let e = widgets::empty_state(
                "folder-download-symbolic",
                "No transfers yet",
                "Files you send and receive appear here.",
                None,
            );
            e.add_css_class("card");
            e.add_css_class("app-empty");
            e.set_margin_top(2);
            self.transfers.append(&e);
            return;
        }
        let today = now() / 86400;
        let mut groups: Vec<(&str, Vec<Job>)> = vec![("Today", vec![]), ("Yesterday", vec![]), ("Earlier", vec![])];
        for j in jobs {
            let day = today - j.created / 86400;
            groups[(day.clamp(0, 2)) as usize].1.push(j);
        }
        for (title, list) in groups.into_iter().filter(|(_, l)| !l.is_empty()) {
            self.transfers.append(&sheet::section_title(title));
            let card = widgets::vbox(0);
            card.add_css_class("card");
            card.add_css_class("app-devices");
            for (i, j) in list.iter().enumerate() {
                let row = self.transfer_row(j);
                if i > 0 {
                    row.add_css_class("following");
                }
                card.append(&row);
            }
            self.transfers.append(&card);
        }
    }

    fn transfer_row(self: &Rc<Self>, j: &Job) -> gtk::Box {
        let row = widgets::hbox(12);
        row.add_css_class("app-device");
        let failed = matches!(j.state, JobState::Failed | JobState::Cancelled);
        let (name, class) = match (j.direction, failed) {
            (_, true) => ("dialog-warning-symbolic", "bad"),
            (Direction::Receive, false) => ("nyx-share-down-symbolic", "in"),
            (Direction::Send, false) => ("nyx-share-up-symbolic", "out"),
        };
        let dir = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dir.add_css_class("app-dir");
        dir.add_css_class(class);
        dir.set_valign(gtk::Align::Center);
        let i = icon(name, 15);
        i.set_size_request(30, 30);
        dir.append(&i);
        row.append(&dir);
        let text = widgets::vbox(1);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let what = j.summary.clone().unwrap_or_else(|| if j.items == 1 { "1 item".into() } else { format!("{} items", j.items) });
        let t = widgets::label(&what, "job-title");
        t.set_xalign(0.0);
        t.set_ellipsize(gtk::pango::EllipsizeMode::End);
        t.set_max_width_chars(34);
        let who = match j.direction {
            Direction::Receive => format!("From {}", j.destination_name),
            Direction::Send => format!("To {}", j.destination_name),
        };
        let size = if j.bytes_total > 0 { format!(" · {}", units::size(j.bytes_total)) } else { String::new() };
        let s = widgets::label(&format!("{who}{size}"), "job-caption");
        s.set_xalign(0.0);
        s.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&t);
        text.append(&s);
        row.append(&text);
        match j.state {
            JobState::Running | JobState::Queued => {
                let cancel = widgets::icon_button("process-stop-symbolic", "Cancel");
                let client = self.client.clone();
                let id = j.id.clone();
                cancel.connect_clicked(move |b| {
                    b.set_sensitive(false);
                    let (client, id) = (client.clone(), id.clone());
                    std::thread::spawn(move || client.cancel(&id));
                });
                cancel.set_valign(gtk::Align::Center);
                row.append(&cancel);
            }
            JobState::Completed if !j.outputs.is_empty() => {
                let show = widgets::icon_button("folder-open-symbolic", "Show in folder");
                let path = PathBuf::from(&j.outputs[0]);
                show.connect_clicked(move |_| nyx_core::cmd::show_in_folder(&path));
                show.set_valign(gtk::Align::Center);
                row.append(&show);
            }
            _ => {}
        }
        let right = widgets::vbox(2);
        right.set_valign(gtk::Align::Center);
        let status = match j.state {
            JobState::Running | JobState::Queued => {
                j.fraction().map(|f| format!("{:.0}%", f * 100.0)).unwrap_or_else(|| "Waiting".into())
            }
            JobState::Completed => if j.direction == Direction::Receive { "Received" } else { "Sent" }.into(),
            JobState::Cancelled => "Cancelled".into(),
            JobState::Failed => {
                if j.message.as_deref().is_some_and(|m| m.ends_with("declined")) {
                    "Declined".into()
                } else {
                    "Failed".into()
                }
            }
        };
        let st = widgets::label(&status, "app-status");
        st.set_xalign(1.0);
        if failed {
            st.add_css_class("error");
            if let Some(m) = &j.message {
                row.set_tooltip_text(Some(m));
            }
        }
        right.append(&st);
        if !j.state.finished() {
            let bar = gtk::LevelBar::new();
            bar.set_value(j.fraction().unwrap_or(0.0));
            bar.set_size_request(80, -1);
            right.append(&bar);
        } else {
            let when = glib::DateTime::from_unix_local(j.updated)
                .ok()
                .and_then(|d| d.format("%H:%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let w = widgets::label(&when, "app-when");
            w.set_xalign(1.0);
            right.append(&w);
        }
        row.append(&right);
        row
    }

    // ---- Settings and footer ----

    fn render_footer(self: &Rc<Self>) {
        let s = self.nearby.borrow().clone();
        self.updating.set(true);
        self.visible.set_active(s.visibility != "off");
        self.updating.set(false);
        self.receiving.remove_css_class("error");
        let folder =
            std::path::Path::new(&s.receive_folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if s.enabled {
            self.visible_label.set_text(&format!("Visible as {}", s.alias));
            self.receiving.set_text(&format!("Receiving to {folder}"));
            self.receiving.add_css_class("live");
        } else {
            self.visible_label.set_text("Not visible");
            self.receiving.set_text(s.problem.as_deref().unwrap_or("Nearby sharing is off"));
            self.receiving.remove_css_class("live");
        }
    }

    fn render_settings(self: &Rc<Self>) {
        clear(&self.settings);
        let s = self.nearby.borrow().clone();
        let group = |title: &str| {
            let t = widgets::label(title, "app-group-title");
            t.set_xalign(0.0);
            t
        };
        self.settings.append(&group("This computer"));
        let card = widgets::vbox(0);
        card.add_css_class("card");
        let entry = gtk::Entry::new();
        entry.set_text(&s.alias);
        entry.set_width_chars(16);
        entry.set_valign(gtk::Align::Center);
        let weak = Rc::downgrade(self);
        let apply = move |e: &gtk::Entry| {
            let Some(app) = weak.upgrade() else { return };
            let name = e.text().trim().to_string();
            if !name.is_empty() && name != app.nearby.borrow().alias {
                app.set_nearby(NearbyUpdate { alias: Some(name), ..Default::default() });
            }
        };
        let a = apply.clone();
        entry.connect_activate(move |e| a(e));
        let focus = gtk::EventControllerFocus::new();
        let e2 = entry.clone();
        focus.connect_leave(move |_| apply(&e2));
        entry.add_controller(focus);
        card.append(&widgets::row("Device name", "What nearby devices see.", Some(entry.upcast_ref())));
        let weak = Rc::downgrade(self);
        let (visible_row, _) = widgets::switch_row(
            "Visible to nearby devices",
            "While Share is open, and a minute after you share.",
            s.visibility != "off",
            move |on| {
                if let Some(app) = weak.upgrade() {
                    app.set_nearby(NearbyUpdate {
                        visibility: Some(if on { "running" } else { "off" }.into()),
                        ..Default::default()
                    });
                }
            },
        );
        card.append(&visible_row);
        let (desc, usable) = match s.tailscale_state.as_str() {
            "running" => ("Also look on your tailnet devices; they need LocalSend open.", true),
            "stopped" => ("Tailscale isn't connected on this computer.", true),
            _ => ("Install Tailscale to reach your devices from any network.", false),
        };
        let weak = Rc::downgrade(self);
        let (tailscale_row, tailscale) = widgets::switch_row("Find devices over Tailscale", desc, s.tailscale, move |on| {
            if let Some(app) = weak.upgrade() {
                app.set_nearby(NearbyUpdate { tailscale: Some(on), ..Default::default() });
            }
        });
        tailscale.set_sensitive(usable);
        card.append(&tailscale_row);
        let key = if s.fingerprint.len() >= 16 {
            let f = &s.fingerprint;
            format!("{} {} … {} {}", &f[..4], &f[4..8], &f[f.len() - 8..f.len() - 4], &f[f.len() - 4..])
        } else {
            "Made when Nearby first starts".into()
        };
        let key_label = widgets::label(&key, "dim");
        key_label.set_selectable(true);
        card.append(&widgets::row("Device key", "Others can check it matches what they see.", Some(key_label.upcast_ref())));
        self.settings.append(&card);

        self.settings.append(&group("Receiving"));
        let card = widgets::vbox(0);
        card.add_css_class("card");
        let folder_name =
            std::path::Path::new(&s.receive_folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let choose = gtk::Button::with_label(&folder_name);
        choose.set_valign(gtk::Align::Center);
        choose.set_tooltip_text(Some(&s.receive_folder));
        let weak = Rc::downgrade(self);
        let current = s.receive_folder.clone();
        choose.connect_clicked(move |_| {
            let Some(app) = weak.upgrade() else { return };
            let dialog = gtk::FileDialog::builder().title("Save received files to").accept_label("Choose").modal(true).build();
            dialog.set_initial_folder(Some(&gio::File::for_path(&current)));
            let weak = Rc::downgrade(&app);
            let window = app.window.clone();
            glib::spawn_future_local(async move {
                let Ok(folder) = dialog.select_folder_future(Some(&window)).await else { return };
                if let (Some(app), Some(path)) = (weak.upgrade(), folder.path()) {
                    app.set_nearby(NearbyUpdate { receive_folder: Some(path.display().to_string()), ..Default::default() });
                }
            });
        });
        card.append(&widgets::row("Save received files to", "", Some(choose.upcast_ref())));
        let ask = gtk::Switch::new();
        ask.set_active(true);
        ask.set_sensitive(false);
        ask.set_valign(gtk::Align::Center);
        card.append(&widgets::row("Ask before receiving", "Trusted devices come with My computers.", Some(ask.upcast_ref())));
        self.settings.append(&card);

        self.settings.append(&group("About"));
        let card = widgets::vbox(0);
        card.add_css_class("card");
        let version = widgets::label(&format!("Share {}", env!("CARGO_PKG_VERSION")), "dim");
        card.append(&widgets::row("Version", "Nearby sharing speaks the LocalSend protocol.", Some(version.upcast_ref())));
        self.settings.append(&card);
    }
}

fn load_thumb(path: PathBuf, frame: &gtk::Box) {
    let frame = frame.clone();
    nyx_core::cmd::background(
        move || -> Option<(i32, i32, usize, bool, glib::Bytes)> {
            use gtk::gdk_pixbuf::{InterpType, Pixbuf};
            let full = Pixbuf::from_file_at_scale(&path, 240, 240, true).ok()?;
            let side = full.width().min(full.height());
            let square = full.new_subpixbuf((full.width() - side) / 2, (full.height() - side) / 2, side, side);
            let t = square.scale_simple(80, 80, InterpType::Bilinear)?;
            Some((t.width(), t.height(), t.rowstride() as usize, t.has_alpha(), t.read_pixel_bytes()))
        },
        move |pixels| {
            let Some((w, h, stride, alpha, bytes)) = pixels else { return };
            let format = if alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
            let texture = gdk::MemoryTexture::new(w, h, format, &bytes, stride);
            clear(&frame);
            let image = gtk::Image::from_paintable(Some(&texture));
            image.set_pixel_size(40);
            frame.append(&image);
        },
    );
}
