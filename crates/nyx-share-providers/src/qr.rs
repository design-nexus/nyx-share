//! A QR code for a link or a short text, made here and shown in the Share
//! Sheet. Nothing leaves the computer.

use crate::{Data, Outcome, Provider, SendError, Task};
use nyx_share_core::{ContentKind, Destination, Section};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// What fits a QR code that still scans from a screen.
pub const MAX_BYTES: usize = 1200;
/// White border around the code, in modules (the standard asks for 4).
const QUIET: usize = 4;
/// The image is at least this wide, in pixels.
const MIN_SIZE: usize = 640;
/// Codes older than this are removed when a new one is made.
const KEEP: Duration = Duration::from_secs(60 * 60);

pub struct Qr;

/// `$XDG_RUNTIME_DIR/nyx-share/qr`, private to the user.
fn dir() -> PathBuf {
    let runtime =
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(std::env::temp_dir);
    runtime.join("nyx-share/qr")
}

fn tidy(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
        if old.is_some_and(|age| age > KEEP) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The smallest code for `text`. Text beyond ASCII is marked as UTF-8 (ECI 26),
/// or readers guess another encoding.
fn encode(text: &str) -> Option<qrcode::QrCode> {
    use qrcode::{EcLevel, QrCode, Version, bits::Bits};
    let ec = EcLevel::M;
    if text.is_ascii() {
        return QrCode::with_error_correction_level(text.as_bytes(), ec).ok();
    }
    (1..=40).find_map(|v| {
        let mut bits = Bits::new(Version::Normal(v));
        bits.push_eci_designator(26).ok()?;
        bits.push_byte_data(text.as_bytes()).ok()?;
        bits.push_terminator(ec).ok()?;
        QrCode::with_bits(bits, ec).ok()
    })
}

/// The code as a greyscale PNG: black modules on white, with a quiet zone.
pub fn png(text: &str) -> Result<Vec<u8>, String> {
    let code = encode(text).ok_or_else(|| "Too long for a QR code".to_string())?;
    let modules = code.width();
    let colors = code.to_colors();
    let span = modules + 2 * QUIET;
    let scale = MIN_SIZE.div_ceil(span).max(1);
    let size = span * scale;
    let mut pixels = vec![255u8; size * size];
    for (i, c) in colors.iter().enumerate() {
        if *c != qrcode::Color::Dark {
            continue;
        }
        let (x, y) = ((i % modules + QUIET) * scale, (i / modules + QUIET) * scale);
        for row in y..y + scale {
            pixels[row * size + x..row * size + x + scale].fill(0);
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size as u32, size as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&pixels).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

impl Provider for Qr {
    fn id(&self) -> &'static str {
        "qr"
    }

    fn name(&self) -> &'static str {
        "QR code"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Url]
    }

    fn destinations(&self) -> Vec<Destination> {
        vec![Destination {
            id: "qr".into(),
            provider: self.id().into(),
            name: "QR code".into(),
            icon: "nyx-share-qr-symbolic".into(),
            tint: "graphite".into(),
            section: Section::Apps,
            kinds: self.kinds(),
            max_items: Some(1),
            needs_folder: false,
            needs_recipient: false,
            available: true,
            status: None,
            pinned: false,
            hidden: false,
            default: false,
        }]
    }

    fn send(&self, _destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let [item] = task.items else { return Err(SendError::failed("A QR code holds one text or link")) };
        let Data::Text(text) = &item.data else { return Err(SendError::failed("A QR code holds a text or a link")) };
        if text.len() > MAX_BYTES {
            return Err(SendError::failed(format!("Too long for a QR code: at most {MAX_BYTES} characters scan well")));
        }
        let bytes = png(text).map_err(SendError::failed)?;
        let dir = dir();
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|e| SendError::failed(e.to_string()))?;
        tidy(&dir);
        let path = dir.join(format!("{}.png", uuid::Uuid::new_v4().simple()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| SendError::failed(e.to_string()))?;
        std::io::Write::write_all(&mut f, &bytes).map_err(|e| SendError::failed(e.to_string()))?;
        (task.progress)(text.len() as u64);
        let what = if item.info.kind == ContentKind::Url { "the link" } else { "the text" };
        Ok(Outcome { message: format!("QR code for {what}"), outputs: vec![path.display().to_string()] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_scannable_size_png() {
        let bytes = png("https://design-nexus.github.io/nyx-design").unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!(info.width, info.height);
        assert!(info.width as usize >= MIN_SIZE);
        assert!(png(&"x".repeat(5000)).is_err());
    }

    #[test]
    fn decodes_back_to_the_text() {
        let Some(zbar) = crate::process::find("zbarimg") else { return };
        let text = "Grüße · https://example.com/a?b=1&c=2";
        let path = std::env::temp_dir().join(format!("nyx-share-qr-{}.png", uuid::Uuid::new_v4().simple()));
        std::fs::write(&path, png(text).unwrap()).unwrap();
        let out = std::process::Command::new(zbar).args(["--quiet", "--raw"]).arg(&path).output().unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end(), text);
    }
}
