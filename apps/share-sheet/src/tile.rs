//! One destination in the sheet: a satin tile over its name and a caption.

use gtk::prelude::*;
use nyx_share_core::{Destination, Section};
use std::cell::Cell;
use std::f64::consts::PI;
use std::rc::Rc;

#[derive(Clone, PartialEq)]
pub enum State {
    Idle,
    Busy(Option<f64>),
    /// Busy before any bytes move, with a word for what it's doing.
    Step(String),
    Done,
    Failed,
}

pub struct Tile {
    pub child: gtk::FlowBoxChild,
    caption: gtk::Label,
    ring: gtk::DrawingArea,
    progress: Rc<Cell<Option<f64>>>,
    idle_caption: String,
}

const TINTS: &[&str] = &["blue", "green", "amber", "teal", "graphite"];

impl Tile {
    pub fn new(d: &Destination) -> Self {
        let tint = if TINTS.contains(&d.tint.as_str()) { d.tint.as_str() } else { "graphite" };
        let status = (!d.available)
            .then(|| d.status.clone())
            .flatten()
            .or_else(|| d.status.clone().filter(|_| d.section != Section::Apps));
        let tile = Self::build(&d.name, &d.icon, tint, status.as_deref().unwrap_or_default());
        if !d.available {
            tile.child.add_css_class("unavailable");
            tile.child.set_tooltip_text(Some(&format!("{} — {}", d.name, d.status.as_deref().unwrap_or("unavailable"))));
        } else {
            tile.child.set_tooltip_text(Some(&d.name));
        }
        if d.hidden {
            tile.child.add_css_class("hidden-dest");
        }
        tile
    }

    /// The tile that shows or hides the hidden destinations.
    pub fn more(hidden: usize, expanded: bool) -> Self {
        let (name, caption) = if expanded { ("Less", String::new()) } else { ("More", format!("{hidden} hidden")) };
        let tile = Self::build(name, "view-more-horizontal-symbolic", "graphite", &caption);
        tile.child.set_tooltip_text(Some(if expanded { "Hide the hidden destinations" } else { "Show the hidden destinations" }));
        tile
    }

    fn build(name: &str, icon: &str, tint: &str, caption: &str) -> Self {
        let child = gtk::FlowBoxChild::new();
        child.add_css_class("dest");
        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        // Six to a row in the 528 px body; labels wrap inside this width.
        column.set_size_request(80, -1);

        let face = gtk::Overlay::new();
        face.set_halign(gtk::Align::Center);
        let plate = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        plate.set_size_request(54, 54);
        let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tile.add_css_class("dest-icon");
        tile.add_css_class(&format!("tint-{tint}"));
        tile.set_halign(gtk::Align::Center);
        tile.set_valign(gtk::Align::Center);
        tile.set_hexpand(true);
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(22);
        image.set_size_request(44, 44);
        tile.append(&image);
        plate.append(&tile);
        face.set_child(Some(&plate));

        let progress = Rc::new(Cell::new(None::<f64>));
        let ring = gtk::DrawingArea::new();
        ring.add_css_class("dest-ring");
        ring.set_can_target(false);
        let p = progress.clone();
        ring.set_draw_func(move |area, cr, w, h| {
            let Some(fraction) = p.get() else { return };
            let c = area.color();
            let (cx, cy, r) = (w as f64 / 2.0, h as f64 / 2.0, (w.min(h) as f64 / 2.0) - 2.0);
            cr.set_line_width(2.5);
            cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, 0.18);
            cr.arc(cx, cy, r, 0.0, 2.0 * PI);
            let _ = cr.stroke();
            cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, 1.0);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            cr.arc(cx, cy, r, -PI / 2.0, -PI / 2.0 + 2.0 * PI * fraction.max(0.02));
            let _ = cr.stroke();
        });
        face.add_overlay(&ring);
        column.append(&face);

        let label = gtk::Label::new(Some(name));
        label.add_css_class("dest-name");
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_lines(2);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_justify(gtk::Justification::Center);
        label.set_max_width_chars(4);
        column.append(&label);

        let caption_label = gtk::Label::new(Some(caption));
        caption_label.add_css_class("dest-caption");
        caption_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        caption_label.set_max_width_chars(4);
        caption_label.set_visible(!caption.is_empty());
        column.append(&caption_label);

        child.set_child(Some(&column));
        Self { child, caption: caption_label, ring, progress, idle_caption: caption.to_string() }
    }

    pub fn set_state(&self, state: State) {
        for class in ["busy", "done", "failed"] {
            self.child.remove_css_class(class);
        }
        let (class, caption, ring) = match state {
            State::Idle => (None, self.idle_caption.clone(), None),
            State::Busy(f) => (
                Some("busy"),
                f.map(|f| format!("{:.0}%", f * 100.0)).unwrap_or_else(|| "Sending".into()),
                Some(f.unwrap_or(0.0)),
            ),
            State::Step(word) => (Some("busy"), word, Some(0.0)),
            State::Done => (Some("done"), "Done".into(), None),
            State::Failed => (Some("failed"), "Failed".into(), None),
        };
        if let Some(class) = class {
            self.child.add_css_class(class);
        }
        self.caption.set_text(&caption);
        self.caption.set_visible(!caption.is_empty());
        self.progress.set(ring);
        self.ring.queue_draw();
    }
}
