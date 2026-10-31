//! The Share Sheet asking about files offered by a nearby device, then showing
//! them arrive. Nothing is saved unless the person accepts.

use crate::sheet;
use gtk::prelude::*;
use gtk::{gdk, glib};
use nyx_share_client::{Client, Event, Job, JobState};
use nyx_share_core::units;
use nyx_share_dbus::{IncomingInfo, ShareSheetProxyBlocking};
use nyx_ui::widgets;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

struct Conn {
    client: Client,
    sheet: ShareSheetProxyBlocking<'static>,
    session: String,
    token: String,
}

struct View {
    window: gtk::ApplicationWindow,
    conn: Arc<Conn>,
    info: IncomingInfo,
    banner_slot: gtk::Box,
    buttons: gtk::Box,
    progress: gtk::Box,
    title: gtk::Label,
    caption: gtk::Label,
    bar: gtk::LevelBar,
    actions: gtk::Box,
    accepted: Cell<Option<Instant>>,
    last: RefCell<Option<Job>>,
    closed: Cell<bool>,
}

/// Over `parent` (the Share window) when it's open, else on its own.
pub fn open(app: &gtk::Application, session: String, token: String, parent: Option<gtk::ApplicationWindow>) {
    let hold = app.hold();
    let app = app.clone();
    nyx_core::cmd::background(
        move || -> Result<(Arc<Conn>, IncomingInfo), String> {
            let client = Client::connect().map_err(|e| e.to_string())?;
            let sheet = client.sheet().map_err(|e| e.to_string())?;
            let json = sheet.get_incoming(&session, &token).map_err(|e| nyx_share_client::Error::from(e).to_string())?;
            let info = serde_json::from_str(&json).map_err(|e| e.to_string())?;
            Ok((Arc::new(Conn { client, sheet, session, token }), info))
        },
        move |result| {
            match result {
                Ok((conn, info)) => build(&app, conn, info, parent),
                Err(e) => sheet::failed(&app, &e),
            }
            drop(hold);
        },
    );
}

fn device_icon(device_type: Option<&str>) -> &'static str {
    match device_type {
        Some("mobile") => "phone-symbolic",
        Some("web") => "web-browser-symbolic",
        Some("headless" | "server") => "network-server-symbolic",
        _ => "computer-symbolic",
    }
}

/// "4BAD DE53 … 52FA": enough to compare with the sending device.
fn short_key(fingerprint: &str) -> String {
    let f = fingerprint.to_ascii_uppercase();
    if f.len() < 16 {
        return f;
    }
    format!("{} {} … {} {}", &f[..4], &f[4..8], &f[f.len() - 8..f.len() - 4], &f[f.len() - 4..])
}

fn files_phrase(n: usize) -> String {
    if n == 1 { "1 file".into() } else { format!("{n} files") }
}

fn build(app: &gtk::Application, conn: Arc<Conn>, info: IncomingInfo, parent: Option<gtk::ApplicationWindow>) {
    let (window, header, body) = sheet::window(app);
    if let Some(parent) = parent {
        window.set_transient_for(Some(&parent));
        window.set_modal(true);
    }
    header.set_subtitle("Incoming");
    let banner_slot = widgets::vbox(0);
    body.append(&banner_slot);

    let card = widgets::hbox(14);
    card.add_css_class("card");
    card.add_css_class("share-preview");
    let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tile.add_css_class("dest-icon");
    tile.add_css_class("tint-teal");
    tile.set_valign(gtk::Align::Center);
    let icon = gtk::Image::from_icon_name(device_icon(info.device_type.as_deref()));
    icon.set_pixel_size(22);
    icon.set_size_request(44, 44);
    tile.append(&icon);
    card.append(&tile);
    let text = widgets::vbox(2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    let headline = if info.message_only {
        format!("{} sent you a message", info.sender)
    } else {
        format!("{} wants to send you {}", info.sender, files_phrase(info.files.len()))
    };
    let t = widgets::label(&headline, "preview-title");
    t.set_xalign(0.0);
    t.set_wrap(true);
    t.set_max_width_chars(40);
    let mut sub = vec!["LocalSend".to_string(), info.address.clone()];
    if !info.message_only {
        sub.insert(0, units::size(info.total));
    }
    let s = widgets::label(&sub.join(" · "), "preview-subtitle");
    s.set_xalign(0.0);
    text.append(&t);
    text.append(&s);
    card.append(&text);
    body.append(&card);

    if info.message_only {
        let message: String = info.files.iter().filter_map(|f| f.text.clone()).collect::<Vec<_>>().join("\n\n");
        let shown: String = message.chars().take(4000).collect();
        let m = widgets::label(&shown, "incoming-message");
        m.set_xalign(0.0);
        m.set_wrap(true);
        m.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        m.set_selectable(true);
        m.set_max_width_chars(60);
        let frame = widgets::vbox(0);
        frame.add_css_class("card");
        frame.add_css_class("incoming-files");
        let scroll = gtk::ScrolledWindow::builder().child(&m).propagate_natural_height(true).max_content_height(260).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        frame.append(&scroll);
        body.append(&sheet::section_title("Message"));
        body.append(&frame);
    } else {
        body.append(&sheet::section_title("Files"));
        let list = widgets::vbox(0);
        list.add_css_class("card");
        list.add_css_class("incoming-files");
        for f in info.files.iter().take(6) {
            let row = widgets::hbox(12);
            row.add_css_class("incoming-file");
            let name = widgets::label(&f.name, "");
            name.set_xalign(0.0);
            name.set_hexpand(true);
            name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            name.set_max_width_chars(40);
            row.append(&name);
            row.append(&widgets::label(&units::size(f.size), "dim"));
            list.append(&row);
        }
        if info.files.len() > 6 {
            let more = widgets::label(&format!("and {} more", info.files.len() - 6), "dim");
            more.set_xalign(0.0);
            more.add_css_class("incoming-file");
            list.append(&more);
        }
        body.append(&list);
    }

    let key = if info.verified {
        format!("Device key {}", short_key(&info.fingerprint))
    } else {
        format!("Unverified device: its certificate didn't match key {}", short_key(&info.fingerprint))
    };
    let mut notes = vec![key];
    if !info.message_only {
        notes.push(format!("Saves to {}", nyx_core::paths::pretty(std::path::Path::new(&info.folder))));
    }
    let note = widgets::label(&notes.join(" · "), "share-note");
    note.set_xalign(0.0);
    note.set_wrap(true);
    body.append(&note);

    let progress = widgets::vbox(0);
    progress.append(&sheet::section_title("Transfer"));
    let row = widgets::hbox(12);
    row.add_css_class("card");
    row.add_css_class("share-job");
    let column = widgets::vbox(2);
    column.set_hexpand(true);
    let title = widgets::label("", "job-title");
    title.set_xalign(0.0);
    let caption = widgets::label("", "job-caption");
    caption.set_xalign(0.0);
    caption.set_wrap(true);
    let bar = gtk::LevelBar::new();
    bar.set_margin_top(7);
    column.append(&title);
    column.append(&caption);
    column.append(&bar);
    row.append(&column);
    let actions = widgets::hbox(4);
    actions.set_valign(gtk::Align::Start);
    row.append(&actions);
    progress.append(&row);
    progress.set_visible(false);
    body.append(&progress);

    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    buttons.add_css_class("incoming-buttons");
    body.append(&buttons);

    let view = Rc::new(View {
        window: window.clone(),
        conn,
        info,
        banner_slot,
        buttons,
        progress,
        title,
        caption,
        bar,
        actions,
        accepted: Cell::new(None),
        last: RefCell::new(None),
        closed: Cell::new(false),
    });
    view.show_buttons();
    listen(&view);
    window.present();
    // For checks and screenshots: `NYX_SHARE_CHECK_ANSWER=accept|decline` answers after two seconds.
    if let Ok(answer) = std::env::var("NYX_SHARE_CHECK_ANSWER") {
        let weak = Rc::downgrade(&view);
        glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
            if let Some(v) = weak.upgrade() {
                v.answer(answer == "accept");
            }
        });
    }
    window.connect_close_request(move |_| {
        if !view.closed.replace(true) {
            let conn = view.conn.clone();
            std::thread::spawn(move || conn.sheet.dismiss_incoming(&conn.session, &conn.token));
        }
        glib::Propagation::Proceed
    });
}

fn listen(view: &Rc<View>) {
    let (tx, rx) = async_channel::unbounded::<Job>();
    let conn = view.conn.clone();
    std::thread::spawn(move || {
        let Ok(events) = conn.client.events() else { return };
        for event in events {
            if let Event::JobChanged(job) = event
                && job.id == conn.session
                && tx.send_blocking(job).is_err()
            {
                break;
            }
        }
    });
    let weak = Rc::downgrade(view);
    glib::spawn_future_local(async move {
        while let Ok(job) = rx.recv().await {
            let Some(view) = weak.upgrade() else { break };
            view.job_changed(&job);
        }
    });
}

impl View {
    fn banner(&self, message: &str) {
        while let Some(child) = self.banner_slot.first_child() {
            self.banner_slot.remove(&child);
        }
        let b = widgets::banner(&glib::markup_escape_text(message), true);
        b.set_margin_bottom(12);
        self.banner_slot.append(&b);
    }

    fn clear_buttons(&self) {
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }
    }

    fn show_buttons(self: &Rc<Self>) {
        self.clear_buttons();
        if self.info.answer == "expired" {
            self.banner("This request has expired.");
            self.add_close();
            return;
        }
        if self.info.message_only {
            let copy = gtk::Button::with_label("Copy");
            let text: String = self.info.files.iter().filter_map(|f| f.text.clone()).collect::<Vec<_>>().join("\n\n");
            let w = self.window.clone();
            copy.connect_clicked(move |b| {
                gdk::prelude::DisplayExt::clipboard(&WidgetExt::display(&w)).set_text(&text);
                b.set_label("Copied");
            });
            self.buttons.append(&copy);
            self.add_close();
            return;
        }
        let decline = gtk::Button::with_label("Decline");
        let accept = gtk::Button::with_label("Accept");
        accept.add_css_class("suggested-action");
        let weak = Rc::downgrade(self);
        decline.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.answer(false);
            }
        });
        let weak = Rc::downgrade(self);
        accept.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.answer(true);
            }
        });
        self.buttons.append(&decline);
        self.buttons.append(&accept);
        decline.grab_focus();
    }

    fn add_close(&self) {
        let close = gtk::Button::with_label("Close");
        close.add_css_class("suggested-action");
        let w = self.window.clone();
        close.connect_clicked(move |_| w.close());
        self.buttons.append(&close);
        close.grab_focus();
    }

    fn answer(self: &Rc<Self>, accept: bool) {
        self.buttons.set_sensitive(false);
        let conn = self.conn.clone();
        let weak = Rc::downgrade(self);
        nyx_core::cmd::background(
            move || {
                conn.sheet
                    .answer_incoming(&conn.session, &conn.token, accept)
                    .map_err(|e| nyx_share_client::Error::from(e).to_string())
            },
            move |result| {
                let Some(v) = weak.upgrade() else { return };
                v.buttons.set_sensitive(true);
                match (result, accept) {
                    (Ok(()), true) => {
                        v.accepted.set(Some(Instant::now()));
                        v.clear_buttons();
                        v.progress.set_visible(true);
                        v.title.set_text(&format!("Waiting for {} to start sending", v.info.sender));
                        v.bar.set_visible(false);
                        if let Some(job) = v.last.borrow().clone() {
                            v.job_changed(&job);
                        }
                    }
                    (Ok(()), false) => v.window.close(),
                    (Err(e), _) => {
                        v.banner(&e);
                        v.clear_buttons();
                        v.add_close();
                    }
                }
            },
        );
    }

    fn job_changed(self: &Rc<Self>, job: &Job) {
        *self.last.borrow_mut() = Some(job.clone());
        let Some(started) = self.accepted.get() else { return };
        self.progress.set_visible(true);
        while let Some(child) = self.actions.first_child() {
            self.actions.remove(&child);
        }
        match job.state {
            JobState::Queued | JobState::Running => {
                self.title.set_text(&format!("Receiving {} from {}", files_phrase(job.items as usize), self.info.sender));
                let mut parts = vec![format!("{} of {}", units::size(job.bytes_done), units::size(job.bytes_total))];
                let secs = started.elapsed().as_secs_f64();
                if secs > 0.5 && job.bytes_done > 0 {
                    parts.push(format!("{}/s", units::size((job.bytes_done as f64 / secs) as u64)));
                }
                self.caption.set_text(&parts.join(" · "));
                self.bar.set_visible(true);
                self.bar.set_value(job.fraction().unwrap_or(0.0));
                let cancel = widgets::icon_button("process-stop-symbolic", "Cancel");
                let conn = self.conn.clone();
                cancel.connect_clicked(move |b| {
                    b.set_sensitive(false);
                    let conn = conn.clone();
                    std::thread::spawn(move || conn.client.cancel(&conn.session));
                });
                self.actions.append(&cancel);
            }
            JobState::Completed => {
                self.title.set_text(job.message.as_deref().unwrap_or("Received"));
                self.caption.set_text(&match job.outputs.as_slice() {
                    [one] => nyx_core::paths::pretty(std::path::Path::new(one)),
                    _ => nyx_core::paths::pretty(std::path::Path::new(&self.info.folder)),
                });
                self.bar.set_visible(false);
                if let Some(first) = job.outputs.first() {
                    let show = widgets::icon_button("folder-open-symbolic", "Show in folder");
                    let path = std::path::PathBuf::from(first);
                    show.connect_clicked(move |_| nyx_core::cmd::show_in_folder(&path));
                    self.actions.append(&show);
                }
                self.clear_buttons();
                self.add_close();
            }
            JobState::Failed | JobState::Cancelled => {
                self.title.set_text(if job.state == JobState::Cancelled { "Cancelled" } else { "The transfer failed" });
                self.caption.set_text(job.message.as_deref().filter(|m| *m != "Cancelled").unwrap_or_default());
                self.bar.set_visible(false);
                self.clear_buttons();
                self.add_close();
            }
        }
    }
}
