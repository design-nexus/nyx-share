//! File names that are safe to create in a folder the user chose.

/// The name used when nothing usable is left of a shared name.
pub const FALLBACK: &str = "Shared file";

/// Reduce a caller's name to one safe file name: no directories, no control
/// characters, no leading dots or dashes, no reserved names, at most 255 bytes.
pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim().trim_start_matches(['.', '-', ' ']).trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return FALLBACK.to_string();
    }
    truncate(trimmed, 255)
}

/// Shorten to `max` bytes on a character boundary, keeping a short extension.
fn truncate(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let (stem, ext) = split(name);
    let ext = if ext.len() <= 16 { ext } else { "" };
    let mut end = max.saturating_sub(ext.len());
    while !stem.is_char_boundary(end.min(stem.len())) {
        end -= 1;
    }
    format!("{}{ext}", &stem[..end.min(stem.len())])
}

/// Split into a stem and an extension that includes its dot ("" if none).
/// A leading dot isn't an extension, and `.tar.*` stays together.
pub fn split(name: &str) -> (&str, &str) {
    let dot = match name.rfind('.') {
        Some(0) | None => return (name, ""),
        Some(i) => i,
    };
    let stem = &name[..dot];
    if let Some(tar) = stem.strip_suffix(".tar")
        && !tar.is_empty()
    {
        return (tar, &name[tar.len()..]);
    }
    (stem, &name[dot..])
}

/// The `n`th alternative for a name that is taken: "report (2).pdf".
pub fn alternative(name: &str, n: u32) -> String {
    let (stem, ext) = split(name);
    let suffix = format!(" ({n})");
    let room = 255usize.saturating_sub(ext.len() + suffix.len());
    let mut end = stem.len().min(room);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}{ext}", &stem[..end])
}

/// The hidden name a file is written under until it is complete. `tag` keeps
/// two transfers of the same name apart.
pub fn partial(name: &str, tag: &str) -> String {
    let mut end = name.len().min(200);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!(".{}.{tag}.nyx-share-partial", &name[..end])
}

/// A name for shared text saved as a file.
pub fn for_text(name: Option<&str>) -> String {
    match name.map(sanitize) {
        Some(n) if n != FALLBACK && !n.contains('.') => format!("{n}.txt"),
        Some(n) if n != FALLBACK => n,
        _ => "Shared text.txt".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_directories_and_traversal() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("..\\..\\boot.ini"), "boot.ini");
        assert_eq!(sanitize("/abs/path/photo.jpg"), "photo.jpg");
        assert_eq!(sanitize(".."), FALLBACK);
        assert_eq!(sanitize("."), FALLBACK);
        assert_eq!(sanitize("dir/"), FALLBACK);
        assert_eq!(sanitize(""), FALLBACK);
    }

    #[test]
    fn removes_control_characters_and_hidden_prefixes() {
        assert_eq!(sanitize("a\0b\nc.txt"), "abc.txt");
        assert_eq!(sanitize(".bashrc"), "bashrc");
        assert_eq!(sanitize("-rf"), "rf");
        assert_eq!(sanitize("  name.txt. "), "name.txt");
    }

    #[test]
    fn limits_length_on_character_boundaries() {
        let long = format!("{}.pdf", "é".repeat(200));
        let s = sanitize(&long);
        assert!(s.len() <= 255);
        assert!(s.ends_with(".pdf"));
    }

    #[test]
    fn numbers_alternatives_before_the_extension() {
        assert_eq!(alternative("report.pdf", 2), "report (2).pdf");
        assert_eq!(alternative("archive.tar.gz", 3), "archive (3).tar.gz");
        assert_eq!(alternative("README", 2), "README (2)");
        assert_eq!(alternative(".hidden", 2), ".hidden (2)");
        assert!(alternative(&"x".repeat(255), 10).len() <= 255);
    }

    #[test]
    fn partial_names_are_hidden_and_bounded() {
        assert_eq!(partial("a.txt", "1f"), ".a.txt.1f.nyx-share-partial");
        assert!(partial(&"x".repeat(255), "ffffffffffffffff").len() < 255);
    }

    #[test]
    fn names_text_files() {
        assert_eq!(for_text(None), "Shared text.txt");
        assert_eq!(for_text(Some("Notes")), "Notes.txt");
        assert_eq!(for_text(Some("list.md")), "list.md");
        assert_eq!(for_text(Some("../")), "Shared text.txt");
    }
}
