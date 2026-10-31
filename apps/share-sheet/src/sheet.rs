//! The Share Sheet window: what is being shared, where it can go, and how the
//! transfers are doing. Transfers belong to the service and outlive the window.

use crate::prefs;
use crate::tile::{self, Tile};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use nyx_share_client::{Client, Event, Job, JobState, PreferencesUpdate, RequestInfo};
use nyx_share_core::{ContentKind, Destination, ItemInfo, Section, units};
use nyx_share_dbus::ShareSheetProxyBlocking;
use nyx_ui::widgets;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Read;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MORE: &str = "\u{0}more";
/// Largest image read for a preview.
const PREVIEW_LIMIT: u64 = 64 << 20;

struct Conn {
    client: Client,
    sheet: ShareSheetProxyBlocking<'static>,
    request: String,
    token: String,
}

struct JobRow {
    root: gtk::Box,
    title: gtk::Label,
    caption: gtk::Label,
    bar: gtk::LevelBar,
    actions: gtk::Box,
    destination: Destination,
    options: String,
    started: Instant,
    finished: bool,
}

struct Sheet {
    window: gtk::ApplicationWindow,
    conn: Arc<Conn>,
    info: RefCell<RequestInfo>,
    banner_slot: gtk::Box,
    sections: gtk::Box,
    transfers: gtk::Box,
    rows: gtk::Box,
    note: gtk::Box,
    tiles: RefCell<HashMap<String, Tile>>,
    flows: RefCell<Vec<(gtk::FlowBox, Vec<String>)>>,
    jobs: RefCell<Vec<(String, JobRow)>>,
    /// Updates for jobs that arrived before their row existed: a quick job can
    /// finish before `Choose` has even returned.
    early: RefCell<HashMap<String, Job>>,
    show_hidden: Cell<bool>,
    closed: Cell<bool>,
}

fn connect(request: String, token: String) -> Result<(Arc<Conn>, RequestInfo), String> {
    let client = Client::connect().map_err(|e| e.to_string())?;
    let sheet = client.sheet().map_err(|e| e.to_string())?;
    let json = sheet.get_request(&request, &token).map_err(|e| nyx_share_client::Error::from(e).to_string())?;
    let info = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    Ok((Arc::new(Conn { client, sheet, request, token }), info))
}

pub fn open(app: &gtk::Application, request: String, token: String) {
    let hold = app.hold();
    let app = app.clone();
    nyx_core::cmd::background(
        move || connect(request, token),
        move |result| {
            match result {
                Ok((conn, info)) => build(&app, conn, info),
                Err(e) => failed(&app, &e),
            }
            drop(hold);
        },
    );
}

pub(crate) fn window(app: &gtk::Application) -> (gtk::ApplicationWindow, nyx_ui::chrome::AppHeader, gtk::Box) {
    let window = gtk::ApplicationWindow::builder().application(app).title("Share").resizable(false).build();
    window.add_css_class("share-window");
    let header = nyx_ui::chrome::app_header(&window, "send-to-symbolic", "Share");
    remove_window_controls(&header.root);
    let close = widgets::icon_button("window-close-symbolic", "Close (Esc)");
    let w = window.clone();
    close.connect_clicked(move |_| w.close());
    header.end.append(&close);
    let root = widgets::vbox(0);
    root.append(&header.root);
    let body = widgets::vbox(0);
    body.add_css_class("share-body");
    body.set_size_request(528, -1);
    root.append(&body);
    window.set_child(Some(&root));
    nyx_ui::keys::install(&window, nyx_ui::keys::Common { close: true, ..Default::default() });
    let keys = gtk::EventControllerKey::new();
    let w = window.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    (window, header, body)
}

/// The sheet has its own close button, shown whether or not the window floats.
fn remove_window_controls(header: &gtk::WindowHandle) {
    let Some(right) = header.child().and_downcast::<gtk::CenterBox>().and_then(|bar| bar.end_widget()) else { return };
    let Some(right) = right.downcast_ref::<gtk::Box>() else { return };
    let mut next = right.first_child();
    while let Some(w) = next {
        next = w.next_sibling();
        if w.is::<gtk::WindowControls>() {
            right.remove(&w);
        }
    }
}

pub(crate) fn failed(app: &gtk::Application, message: &str) {
    let (window, _, body) = window(app);
    body.append(&widgets::empty_state(
        "dialog-warning-symbolic",
        "This share can't be opened",
        &glib::markup_escape_text(message),
        None,
    ));
    window.present();
}

fn build(app: &gtk::Application, conn: Arc<Conn>, info: RequestInfo) {
    let (window, header, body) = window(app);
    header.set_subtitle(&summary(&info).2);
    let banner_slot = widgets::vbox(0);
    body.append(&banner_slot);
    body.append(&preview(&info, &conn));
    let sections = widgets::vbox(0);
    body.append(&sections);

    let transfers = widgets::vbox(0);
    transfers.append(&section_title("Transfers"));
    let rows = widgets::vbox(0);
    rows.add_css_class("card");
    rows.add_css_class("share-jobs");
    transfers.append(&rows);
    let note = widgets::hbox(8);
    note.add_css_class("share-note");
    let note_icon = gtk::Image::from_icon_name("dialog-information-symbolic");
    note_icon.set_pixel_size(14);
    note.append(&note_icon);
    note.append(&widgets::label("Sending continues if you close this window.", "dim"));
    transfers.append(&note);
    transfers.set_visible(false);
    body.append(&transfers);

    let sheet = Rc::new(Sheet {
        window: window.clone(),
        conn,
        info: RefCell::new(info),
        banner_slot,
        sections,
        transfers,
        rows,
        note,
        tiles: RefCell::default(),
        flows: RefCell::default(),
        jobs: RefCell::default(),
        early: RefCell::default(),
        show_hidden: Cell::new(false),
        closed: Cell::new(false),
    });
    install_actions(&sheet);
    sheet.render();
    listen(&sheet);

    window.present();
    sheet.focus_first();
    sheet.check_choices();
    // The window owns the sheet: this handler holds the only strong reference
    // and is dropped when the window is destroyed.
    window.connect_close_request(move |_| {
        if !sheet.closed.replace(true) {
            let conn = sheet.conn.clone();
            std::thread::spawn(move || conn.sheet.dismiss(&conn.request, &conn.token));
        }
        glib::Propagation::Proceed
    });
}

/// Follow the service's signals for as long as the window is open.
fn listen(sheet: &Rc<Sheet>) {
    let (tx, rx) = async_channel::unbounded::<Event>();
    let conn = sheet.conn.clone();
    std::thread::spawn(move || {
        let Ok(events) = conn.client.events() else { return };
        for event in events {
            if tx.send_blocking(event).is_err() {
                break;
            }
        }
    });
    let weak = Rc::downgrade(sheet);
    glib::spawn_future_local(async move {
        while let Ok(event) = rx.recv().await {
            let Some(sheet) = weak.upgrade() else { break };
            match event {
                Event::JobChanged(job) => sheet.job_changed(&job),
                Event::DestinationsChanged => sheet.refresh(),
            }
        }
    });
}

pub(crate) fn section_title(text: &str) -> gtk::Label {
    let l = widgets::label(text, "share-section-title");
    l.set_xalign(0.0);
    l
}

/// The preview's title and subtitle, and the header's subtitle.
fn summary(info: &RequestInfo) -> (String, String, String) {
    let items = &info.items;
    let label = |i: &ItemInfo| match i.kind {
        ContentKind::Url => host(i.preview.as_deref().unwrap_or_default()),
        ContentKind::Text => first_line(i.preview.as_deref().unwrap_or_default()),
        _ => i.name.clone(),
    };
    let size: u64 = items.iter().filter(|i| !i.kind.is_inline()).map(|i| i.size).sum();
    let all = |k: ContentKind| items.iter().all(|i| i.kind == k);
    let (title, mut subtitle, header) = match items.as_slice() {
        [one] => match one.kind {
            ContentKind::Url => (label(one), one.preview.clone().unwrap_or_default(), "Link".to_string()),
            ContentKind::Text => (label(one), format!("{} characters", one.size), "Text".to_string()),
            ContentKind::Image => (one.name.clone(), format!("1 image · {}", units::size(one.size)), "1 image".to_string()),
            ContentKind::File => (one.name.clone(), format!("1 file · {}", units::size(one.size)), "1 file".to_string()),
        },
        many => {
            let noun = if all(ContentKind::Image) { "images" } else { "items" };
            let title = match many {
                [a, b] => format!("{} and {}", label(a), label(b)),
                [a, b, rest @ ..] => format!("{}, {} and {} more", label(a), label(b), rest.len()),
                _ => String::new(),
            };
            let count = format!("{} {noun}", many.len());
            let sub = if size > 0 { format!("{count} · {}", units::size(size)) } else { count.clone() };
            (title, sub, count)
        }
    };
    if let Some(app) = &info.app_name {
        subtitle.push_str(&format!(" · from {app}"));
    }
    (title, subtitle, header)
}

fn host(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if host.is_empty() { url.to_string() } else { host.to_string() }
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    let short: String = line.chars().take(80).collect();
    if short.len() < line.len() { format!("{short}…") } else { short }
}

fn item_icon(item: &ItemInfo) -> gio::Icon {
    match item.kind {
        ContentKind::Url => gio::ThemedIcon::new("web-browser-symbolic").upcast(),
        ContentKind::Text => gio::ThemedIcon::new("text-x-generic-symbolic").upcast(),
        _ => match &item.mime {
            Some(m) => gio::content_type_get_symbolic_icon(m),
            None => gio::ThemedIcon::new("text-x-generic-symbolic").upcast(),
        },
    }
}

fn preview(info: &RequestInfo, conn: &Arc<Conn>) -> gtk::Box {
    let card = widgets::hbox(14);
    card.add_css_class("card");
    card.add_css_class("share-preview");
    let thumbs = widgets::hbox(6);
    thumbs.set_valign(gtk::Align::Center);
    let size = if info.items.len() == 1 { 46 } else { 40 };
    for (index, item) in info.items.iter().enumerate().take(3) {
        let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        frame.add_css_class("preview-thumb");
        frame.set_size_request(size, size);
        frame.set_hexpand(false);
        frame.set_overflow(gtk::Overflow::Hidden);
        let icon = gtk::Image::from_gicon(&item_icon(item));
        icon.set_pixel_size(18);
        icon.set_halign(gtk::Align::Center);
        icon.set_valign(gtk::Align::Center);
        icon.set_size_request(size, size);
        frame.append(&icon);
        thumbs.append(&frame);
        if item.kind == ContentKind::Image {
            load_thumbnail(conn, index as u32, &frame, size);
        }
    }
    card.append(&thumbs);
    let (title, subtitle, _) = summary(info);
    let text = widgets::vbox(2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    let t = widgets::label(&title, "preview-title");
    t.set_xalign(0.0);
    t.set_ellipsize(gtk::pango::EllipsizeMode::End);
    t.set_max_width_chars(36);
    let s = widgets::label(&subtitle, "preview-subtitle");
    s.set_xalign(0.0);
    s.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    s.set_max_width_chars(44);
    text.append(&t);
    text.append(&s);
    card.append(&text);
    card
}

/// Decoded thumbnail pixels: width, height, stride, alpha, bytes.
type Pixels = (i32, i32, usize, bool, glib::Bytes);

fn load_thumbnail(conn: &Arc<Conn>, index: u32, frame: &gtk::Box, size: i32) {
    let conn = conn.clone();
    let frame = frame.clone();
    // Twice the size, for HiDPI screens.
    let px = size * 2;
    nyx_core::cmd::background(
        move || -> Option<Pixels> {
            use gtk::gdk_pixbuf::{InterpType, Pixbuf};
            let fd = conn.sheet.open_item(&conn.request, &conn.token, index).ok()?;
            let file = std::fs::File::from(std::os::fd::OwnedFd::from(fd));
            let mut bytes = Vec::new();
            file.take(PREVIEW_LIMIT).read_to_end(&mut bytes).ok()?;
            let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
            let full = Pixbuf::from_stream(&stream, gio::Cancellable::NONE).ok()?;
            let side = full.width().min(full.height());
            let square = full.new_subpixbuf((full.width() - side) / 2, (full.height() - side) / 2, side, side);
            let thumb = square.scale_simple(px, px, InterpType::Bilinear)?;
            Some((thumb.width(), thumb.height(), thumb.rowstride() as usize, thumb.has_alpha(), thumb.read_pixel_bytes()))
        },
        move |pixels| {
            let Some((w, h, stride, alpha, bytes)) = pixels else { return };
            let format = if alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
            let texture = gdk::MemoryTexture::new(w, h, format, &bytes, stride);
            while let Some(child) = frame.first_child() {
                frame.remove(&child);
            }
            let image = gtk::Image::from_paintable(Some(&texture));
            image.set_pixel_size(size);
            frame.append(&image);
        },
    );
}

fn install_actions(sheet: &Rc<Sheet>) {
    let group = gio::SimpleActionGroup::new();
    for name in ["pin", "hide", "left", "right"] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(sheet);
        action.connect_activate(move |action, target| {
            let (Some(sheet), Some(id)) = (weak.upgrade(), target.and_then(|t| t.get::<String>())) else { return };
            sheet.arrange(action.name().as_str(), &id);
        });
        group.add_action(&action);
    }
    sheet.window.insert_action_group("dest", Some(&group));
}

impl Sheet {
    /// For checks and screenshots: `NYX_SHARE_CHECK_CHOOSE=save:/tmp/a;clipboard`
    /// chooses those destinations as if clicked, folders included.
    fn check_choices(self: &Rc<Self>) {
        let Ok(choices) = std::env::var("NYX_SHARE_CHECK_CHOOSE") else { return };
        for choice in choices.split(';').filter(|c| !c.is_empty()) {
            let (id, folder) = choice.split_once(':').map_or((choice, None), |(id, f)| (id, Some(f)));
            let Some(d) = self.destination(id) else { continue };
            let options = folder.map(|f| serde_json::json!({ "folder": f }).to_string()).unwrap_or_else(|| "{}".into());
            self.send(d, options);
        }
    }

    fn destination(&self, id: &str) -> Option<Destination> {
        self.info.borrow().destinations.iter().find(|d| d.id == id).cloned()
    }

    fn render(self: &Rc<Self>) {
        while let Some(child) = self.sections.first_child() {
            self.sections.remove(&child);
        }
        self.tiles.borrow_mut().clear();
        self.flows.borrow_mut().clear();
        let info = self.info.borrow().clone();
        let show_hidden = self.show_hidden.get();
        let shown = |d: &&Destination| !d.hidden || show_hidden;
        let hidden = info.destinations.iter().filter(|d| d.hidden).count();
        let pick = |section: Section| -> Vec<&Destination> {
            info.destinations.iter().filter(shown).filter(|d| !d.pinned && d.section == section).collect()
        };
        let pinned: Vec<&Destination> = info.destinations.iter().filter(shown).filter(|d| d.pinned).collect();
        let groups = [
            ("Pinned", pinned),
            ("My devices", pick(Section::Devices)),
            ("Nearby", pick(Section::Nearby)),
            ("Apps & services", pick(Section::Apps)),
        ];
        let last = groups.iter().rposition(|(_, list)| !list.is_empty());
        let mut any = false;
        for (i, (title, list)) in groups.iter().enumerate() {
            let more = hidden > 0 && (Some(i) == last || last.is_none() && i == groups.len() - 1);
            if list.is_empty() && !more {
                continue;
            }
            any = true;
            self.add_section(title, list, more.then_some(hidden));
        }
        if !any {
            self.sections.append(&widgets::empty_state(
                "dialog-information-symbolic",
                "Nothing here can take this",
                "No destination accepts these items yet.",
                None,
            ));
        }
        self.restore_tile_states();
    }

    fn add_section(self: &Rc<Self>, title: &str, list: &[&Destination], more: Option<usize>) {
        let section = widgets::vbox(0);
        section.append(&section_title(title));
        let flow = gtk::FlowBox::new();
        flow.add_css_class("share-tiles");
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_homogeneous(true);
        flow.set_min_children_per_line(6);
        flow.set_max_children_per_line(6);
        flow.set_activate_on_single_click(true);
        flow.set_column_spacing(2);
        flow.set_row_spacing(2);
        let mut ids = Vec::new();
        for d in list {
            let tile = Tile::new(d);
            flow.append(&tile.child);
            ids.push(d.id.clone());
            self.tiles.borrow_mut().insert(d.id.clone(), tile);
        }
        if let Some(hidden) = more {
            let tile = Tile::more(hidden, self.show_hidden.get());
            flow.append(&tile.child);
            ids.push(MORE.into());
            self.tiles.borrow_mut().insert(MORE.into(), tile);
        }
        let weak = Rc::downgrade(self);
        flow.connect_child_activated(move |flow, child| {
            let Some(sheet) = weak.upgrade() else { return };
            if let Some(id) = sheet.id_of(flow, child) {
                sheet.activate(&id);
            }
        });
        self.add_menu_gestures(&flow);
        let weak = Rc::downgrade(self);
        flow.connect_keynav_failed(move |flow, direction| {
            let Some(sheet) = weak.upgrade() else { return glib::Propagation::Proceed };
            sheet.focus_neighbour(flow, direction)
        });
        section.append(&flow);
        self.sections.append(&section);
        self.flows.borrow_mut().push((flow, ids));
    }

    fn id_of(&self, flow: &gtk::FlowBox, child: &gtk::FlowBoxChild) -> Option<String> {
        let flows = self.flows.borrow();
        let (_, ids) = flows.iter().find(|(f, _)| f == flow)?;
        ids.get(child.index() as usize).cloned()
    }

    fn focus_first(&self) {
        if let Some(child) = self.flows.borrow().first().and_then(|(f, _)| f.child_at_index(0)) {
            child.grab_focus();
        }
    }

    /// Arrow keys continue into the next or previous section.
    fn focus_neighbour(&self, flow: &gtk::FlowBox, direction: gtk::DirectionType) -> glib::Propagation {
        let flows = self.flows.borrow();
        let Some(i) = flows.iter().position(|(f, _)| f == flow) else { return glib::Propagation::Proceed };
        let target = match direction {
            gtk::DirectionType::Down => flows.get(i + 1).and_then(|(f, _)| f.child_at_index(0)),
            gtk::DirectionType::Up if i > 0 => {
                let (f, ids) = &flows[i - 1];
                f.child_at_index(ids.len() as i32 - 1)
            }
            _ => None,
        };
        match target {
            Some(child) => {
                child.grab_focus();
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    }

    fn add_menu_gestures(self: &Rc<Self>, flow: &gtk::FlowBox) {
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        let weak = Rc::downgrade(self);
        let f = flow.clone();
        click.connect_pressed(move |_, _, x, y| {
            let (Some(sheet), Some(child)) = (weak.upgrade(), f.child_at_pos(x as i32, y as i32)) else { return };
            sheet.menu_for(&f, &child);
        });
        flow.add_controller(click);

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        let f = flow.clone();
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(sheet) = weak.upgrade() else { return glib::Propagation::Proceed };
            let focus = gtk::prelude::GtkWindowExt::focus(&sheet.window);
            let Some(child) = focus.and_then(|w| w.downcast::<gtk::FlowBoxChild>().ok()) else {
                return glib::Propagation::Proceed;
            };
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            let Some(id) = sheet.id_of(&f, &child).filter(|id| id != MORE) else { return glib::Propagation::Proceed };
            match key {
                gdk::Key::Menu => sheet.menu_for(&f, &child),
                gdk::Key::F10 if shift => sheet.menu_for(&f, &child),
                gdk::Key::Left if ctrl => sheet.arrange("left", &id),
                gdk::Key::Right if ctrl => sheet.arrange("right", &id),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        flow.add_controller(keys);
    }

    fn menu_for(&self, flow: &gtk::FlowBox, child: &gtk::FlowBoxChild) {
        let Some(id) = self.id_of(flow, child).filter(|id| id != MORE) else { return };
        let Some(d) = self.destination(&id) else { return };
        let menu = gio::Menu::new();
        let item = |label: &str, action: &str| {
            let i = gio::MenuItem::new(Some(label), None);
            i.set_action_and_target_value(Some(&format!("dest.{action}")), Some(&id.to_variant()));
            i
        };
        let order = gio::Menu::new();
        order.append_item(&item(if d.pinned { "Unpin" } else { "Pin" }, "pin"));
        order.append_item(&item("Move left", "left"));
        order.append_item(&item("Move right", "right"));
        menu.append_section(None, &order);
        let visibility = gio::Menu::new();
        visibility.append_item(&item(if d.hidden { "Show" } else { "Hide" }, "hide"));
        menu.append_section(None, &visibility);
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_has_arrow(false);
        popover.set_parent(child);
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }

    /// Pin, hide or move a destination, then show the new arrangement.
    fn arrange(self: &Rc<Self>, action: &str, id: &str) {
        let neighbour = {
            let flows = self.flows.borrow();
            flows.iter().find_map(|(_, ids)| {
                let i = ids.iter().position(|x| x == id)?;
                let j = match action {
                    "left" => i.checked_sub(1)?,
                    "right" => i + 1,
                    _ => return Some(None),
                };
                Some(ids.get(j).filter(|n| *n != MORE).cloned())
            })
        };
        let neighbour = neighbour.flatten();
        if matches!(action, "left" | "right") && neighbour.is_none() {
            return;
        }
        let (action, id) = (action.to_string(), id.to_string());
        let conn = self.conn.clone();
        let weak = Rc::downgrade(self);
        nyx_core::cmd::background(
            move || -> Result<RequestInfo, String> {
                let all = conn.client.destinations(&[], 0).map_err(|e| e.to_string())?;
                let ids =
                    |f: &dyn Fn(&Destination) -> bool| all.iter().filter(|d| f(d)).map(|d| d.id.clone()).collect::<Vec<_>>();
                let toggle = |mut list: Vec<String>| {
                    if let Some(i) = list.iter().position(|x| *x == id) {
                        list.remove(i);
                    } else {
                        list.push(id.clone());
                    }
                    list
                };
                let update = match action.as_str() {
                    "pin" => PreferencesUpdate { pinned: Some(toggle(ids(&|d| d.pinned))), ..Default::default() },
                    "hide" => PreferencesUpdate { hidden: Some(toggle(ids(&|d| d.hidden))), ..Default::default() },
                    _ => {
                        let mut order = ids(&|_| true);
                        let a = order.iter().position(|x| *x == id);
                        let b = neighbour.as_ref().and_then(|n| order.iter().position(|x| x == n));
                        if let (Some(a), Some(b)) = (a, b) {
                            order.swap(a, b);
                        }
                        PreferencesUpdate { order: Some(order), ..Default::default() }
                    }
                };
                conn.client.set_preferences(&update).map_err(|e| e.to_string())?;
                let json = conn.sheet.get_request(&conn.request, &conn.token).map_err(|e| e.to_string())?;
                serde_json::from_str(&json).map_err(|e| e.to_string())
            },
            move |result| {
                let Some(sheet) = weak.upgrade() else { return };
                match result {
                    Ok(info) => {
                        *sheet.info.borrow_mut() = info;
                        sheet.render();
                    }
                    Err(e) => sheet.banner(&e),
                }
            },
        );
    }

    /// Fetch the destinations again (another sheet or Settings changed them).
    fn refresh(self: &Rc<Self>) {
        let conn = self.conn.clone();
        let weak = Rc::downgrade(self);
        nyx_core::cmd::background(
            move || {
                conn.sheet.get_request(&conn.request, &conn.token).ok().and_then(|j| serde_json::from_str::<RequestInfo>(&j).ok())
            },
            move |info| {
                if let (Some(sheet), Some(info)) = (weak.upgrade(), info) {
                    *sheet.info.borrow_mut() = info;
                    sheet.render();
                }
            },
        );
    }

    fn banner(&self, message: &str) {
        while let Some(child) = self.banner_slot.first_child() {
            self.banner_slot.remove(&child);
        }
        if message.is_empty() {
            return;
        }
        let b = widgets::banner(&glib::markup_escape_text(message), true);
        b.set_margin_bottom(12);
        self.banner_slot.append(&b);
    }

    fn activate(self: &Rc<Self>, id: &str) {
        if id == MORE {
            self.show_hidden.set(!self.show_hidden.get());
            self.render();
            return;
        }
        let Some(d) = self.destination(id) else { return };
        if !d.available {
            self.banner(d.status.as_deref().unwrap_or("That destination isn't available right now"));
            return;
        }
        if self.jobs.borrow().iter().any(|(_, r)| r.destination.id == d.id && !r.finished) {
            return;
        }
        self.banner("");
        if d.needs_folder {
            self.choose_folder(d);
        } else {
            self.send(d, "{}".into());
        }
    }

    fn choose_folder(self: &Rc<Self>, d: Destination) {
        let dialog = gtk::FileDialog::builder().title("Save to folder").accept_label("Save here").modal(true).build();
        let start = prefs::get()
            .last_folder
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| glib::user_special_dir(glib::UserDirectory::Downloads));
        if let Some(start) = start {
            dialog.set_initial_folder(Some(&gio::File::for_path(start)));
        }
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            let chosen = dialog.select_folder_future(Some(&window)).await;
            let Some(sheet) = weak.upgrade() else { return };
            match chosen {
                Ok(folder) => match folder.path() {
                    Some(path) => {
                        let path = path.display().to_string();
                        prefs::update(|p| p.last_folder = Some(path.clone()));
                        let options = serde_json::json!({ "folder": path }).to_string();
                        sheet.send(d, options);
                    }
                    None => sheet.banner("Choose a folder on this computer"),
                },
                Err(e) if e.matches(gtk::DialogError::Dismissed) || e.matches(gtk::DialogError::Cancelled) => {}
                Err(e) => sheet.banner(&format!("Couldn't choose a folder: {e}")),
            }
        });
    }

    fn send(self: &Rc<Self>, d: Destination, options: String) {
        if let Some(tile) = self.tiles.borrow().get(&d.id) {
            tile.set_state(tile::State::Busy(None));
        }
        let conn = self.conn.clone();
        let weak = Rc::downgrade(self);
        let (dest_id, opts) = (d.id.clone(), options.clone());
        nyx_core::cmd::background(
            move || {
                let id = conn
                    .sheet
                    .choose(&conn.request, &conn.token, &dest_id, &opts)
                    .map_err(|e| nyx_share_client::Error::from(e).to_string())?;
                let job = conn.client.job(&id).map_err(|e| e.to_string())?;
                Ok::<Job, String>(job)
            },
            move |result| {
                let Some(sheet) = weak.upgrade() else { return };
                match result {
                    Ok(job) => {
                        let job = match sheet.early.borrow_mut().remove(&job.id) {
                            Some(seen) if newer(&seen, &job) => seen,
                            _ => job,
                        };
                        sheet.add_job(&job, d, options);
                        sheet.job_changed(&job);
                    }
                    Err(e) => {
                        if let Some(tile) = sheet.tiles.borrow().get(&d.id) {
                            tile.set_state(tile::State::Idle);
                        }
                        sheet.banner(&e);
                    }
                }
            },
        );
    }

    fn add_job(self: &Rc<Self>, job: &Job, destination: Destination, options: String) {
        let root = widgets::hbox(12);
        root.add_css_class("share-job");
        let mini = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        mini.add_css_class("dest-icon");
        mini.add_css_class("mini");
        mini.add_css_class(&format!("tint-{}", destination.tint));
        mini.set_valign(gtk::Align::Start);
        mini.set_hexpand(false);
        let icon = gtk::Image::from_icon_name(&destination.icon);
        icon.set_pixel_size(15);
        icon.set_size_request(28, 28);
        mini.append(&icon);
        root.append(&mini);
        let text = widgets::vbox(2);
        text.set_hexpand(true);
        let title = widgets::label("", "job-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(40);
        let caption = widgets::label("", "job-caption");
        caption.set_xalign(0.0);
        caption.set_wrap(true);
        caption.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        caption.set_max_width_chars(48);
        let bar = gtk::LevelBar::new();
        bar.set_margin_top(7);
        text.append(&title);
        text.append(&caption);
        text.append(&bar);
        root.append(&text);
        let actions = widgets::hbox(4);
        actions.set_valign(gtk::Align::Start);
        root.append(&actions);
        if self.rows.first_child().is_some() {
            root.add_css_class("following");
        }
        self.rows.prepend(&root);
        let row = JobRow { root, title, caption, bar, actions, destination, options, started: Instant::now(), finished: false };
        self.jobs.borrow_mut().push((job.id.clone(), row));
        self.transfers.set_visible(true);
        self.relink_rows();
    }

    /// Rows after the first get a divider line.
    fn relink_rows(&self) {
        let mut next = self.rows.first_child();
        let mut first = true;
        while let Some(w) = next {
            if first {
                w.remove_css_class("following");
            } else {
                w.add_css_class("following");
            }
            first = false;
            next = w.next_sibling();
        }
    }

    fn job_changed(self: &Rc<Self>, job: &Job) {
        let mut jobs = self.jobs.borrow_mut();
        let Some((_, row)) = jobs.iter_mut().find(|(id, _)| *id == job.id) else {
            // Possibly ours, from a Choose that hasn't returned yet.
            let mut early = self.early.borrow_mut();
            // Other windows' jobs pass through here too.
            if early.len() > 64 {
                early.clear();
            }
            if early.get(&job.id).is_none_or(|seen| newer(job, seen)) {
                early.insert(job.id.clone(), job.clone());
            }
            return;
        };
        if row.finished && !job.state.finished() {
            return;
        }
        let d = row.destination.clone();
        let tile_state = match job.state {
            JobState::Queued | JobState::Running => tile::State::Busy(job.fraction()),
            JobState::Completed => tile::State::Done,
            JobState::Failed => tile::State::Failed,
            JobState::Cancelled => tile::State::Idle,
        };
        if let Some(tile) = self.tiles.borrow().get(&d.id) {
            tile.set_state(tile_state);
        }
        row.title.set_text(&job_title(job, &d, &row.options));
        row.caption.set_text(&job_caption(job, row.started));
        row.caption.set_visible(!row.caption.text().is_empty());
        row.bar.set_value(job.fraction().unwrap_or(0.0));
        row.bar.set_visible(!job.state.finished());
        row.root.remove_css_class("failed");
        if job.state == JobState::Failed {
            row.root.add_css_class("failed");
        }
        let was_finished = row.finished;
        row.finished = job.state.finished();
        if !was_finished || row.actions.first_child().is_none() {
            self.job_actions(row, job);
        }
        let running = jobs.iter().any(|(_, r)| !r.finished);
        let all_done = jobs.iter().all(|(_, r)| r.finished);
        drop(jobs);
        self.note.set_visible(running);
        // A copy is instant and has nothing more to show: close like a menu would.
        if job.state == JobState::Completed && d.provider == "clipboard" && all_done {
            let window = self.window.downgrade();
            glib::timeout_add_local_once(Duration::from_millis(900), move || {
                if let Some(w) = window.upgrade() {
                    w.close();
                }
            });
        }
    }

    fn job_actions(self: &Rc<Self>, row: &JobRow, job: &Job) {
        while let Some(child) = row.actions.first_child() {
            row.actions.remove(&child);
        }
        let id = job.id.clone();
        match job.state {
            JobState::Queued | JobState::Running => {
                let cancel = widgets::icon_button("process-stop-symbolic", "Cancel");
                let conn = self.conn.clone();
                cancel.connect_clicked(move |b| {
                    b.set_sensitive(false);
                    let (conn, id) = (conn.clone(), id.clone());
                    std::thread::spawn(move || conn.client.cancel(&id));
                });
                row.actions.append(&cancel);
            }
            JobState::Completed => {
                if let Some(first) = job.outputs.first() {
                    let show = widgets::icon_button("folder-open-symbolic", "Show in folder");
                    let path = std::path::PathBuf::from(first);
                    show.connect_clicked(move |_| nyx_core::cmd::show_in_folder(&path));
                    row.actions.append(&show);
                }
            }
            JobState::Failed => {
                let retry = gtk::Button::with_label("Retry");
                let weak: Weak<Sheet> = Rc::downgrade(self);
                let (d, options, root) = (row.destination.clone(), row.options.clone(), row.root.clone());
                retry.connect_clicked(move |_| {
                    let Some(sheet) = weak.upgrade() else { return };
                    sheet.rows.remove(&root);
                    sheet.jobs.borrow_mut().retain(|(_, r)| r.root != root);
                    sheet.relink_rows();
                    sheet.send(d.clone(), options.clone());
                });
                row.actions.append(&retry);
            }
            JobState::Cancelled => {}
        }
    }

    /// After a re-render, tiles show the state of their latest job again.
    fn restore_tile_states(&self) {
        let tiles = self.tiles.borrow();
        for (_, row) in self.jobs.borrow().iter() {
            if let Some(tile) = tiles.get(&row.destination.id) {
                let failed = row.root.has_css_class("failed");
                tile.set_state(match (row.finished, failed) {
                    (false, _) => tile::State::Busy(Some(row.bar.value())),
                    (true, true) => tile::State::Failed,
                    (true, false) => tile::State::Done,
                });
            }
        }
    }
}

/// Whether update `a` comes after `b` for the same job.
fn newer(a: &Job, b: &Job) -> bool {
    match (a.state.finished(), b.state.finished()) {
        (true, false) => true,
        (false, true) => false,
        _ => a.bytes_done >= b.bytes_done,
    }
}

fn job_title(job: &Job, d: &Destination, options: &str) -> String {
    match job.state {
        JobState::Completed => job.message.clone().unwrap_or_else(|| "Done".into()),
        JobState::Failed => format!("Couldn't share with {}", d.name),
        JobState::Cancelled => "Cancelled".into(),
        JobState::Queued | JobState::Running => {
            let what = if job.items == 1 { "1 item".to_string() } else { format!("{} items", job.items) };
            if d.provider == "clipboard" {
                "Copying".into()
            } else if d.needs_folder {
                let folder: Option<String> = serde_json::from_str::<serde_json::Value>(options)
                    .ok()
                    .and_then(|v| v.get("folder").and_then(|f| f.as_str()).map(str::to_string));
                let place = folder
                    .as_deref()
                    .and_then(|f| std::path::Path::new(f).file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "the folder".into());
                format!("Saving {what} to {place}")
            } else {
                format!("Sending {what} to {}", d.name)
            }
        }
    }
}

fn job_caption(job: &Job, started: Instant) -> String {
    match job.state {
        JobState::Failed => job.message.clone().unwrap_or_default(),
        JobState::Completed => match job.outputs.as_slice() {
            [one] => nyx_core::paths::pretty(std::path::Path::new(one)),
            _ => String::new(),
        },
        JobState::Cancelled => job.message.clone().filter(|m| m != "Cancelled").unwrap_or_default(),
        JobState::Queued | JobState::Running if job.bytes_total > 0 => {
            let mut parts = vec![format!("{} of {}", units::size(job.bytes_done), units::size(job.bytes_total))];
            let secs = started.elapsed().as_secs_f64();
            if secs > 0.5 && job.bytes_done > 0 {
                let rate = job.bytes_done as f64 / secs;
                parts.push(format!("{}/s", units::size(rate as u64)));
                let left = (job.bytes_total - job.bytes_done) as f64 / rate;
                parts.push(match left {
                    l if l < 1.5 => "about 1 s left".into(),
                    l if l < 90.0 => format!("about {:.0} s left", l),
                    l => format!("about {:.0} min left", l / 60.0),
                });
            }
            parts.join(" · ")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(state: JobState, done: u64) -> Job {
        Job {
            id: "j".into(),
            destination: "save".into(),
            destination_name: "Save to folder".into(),
            direction: nyx_share_core::Direction::Send,
            state,
            items: 1,
            bytes_done: done,
            bytes_total: 10,
            message: None,
            outputs: Vec::new(),
            created: 0,
            updated: 0,
        }
    }

    #[test]
    fn a_finished_update_beats_the_snapshot_taken_after_choose() {
        assert!(newer(&job(JobState::Completed, 10), &job(JobState::Running, 0)));
        assert!(!newer(&job(JobState::Running, 0), &job(JobState::Completed, 10)));
        assert!(newer(&job(JobState::Running, 6), &job(JobState::Running, 2)));
        assert!(!newer(&job(JobState::Running, 2), &job(JobState::Running, 6)));
    }
}
