use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    Text,
    Url,
    Image,
    File,
}

impl ContentKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Self::Text),
            "url" => Some(Self::Url),
            "image" => Some(Self::Image),
            "file" => Some(Self::File),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Url => "url",
            Self::Image => "image",
            Self::File => "file",
        }
    }

    /// Text and links travel inline; images and files as a file descriptor.
    pub fn is_inline(self) -> bool {
        matches!(self, Self::Text | Self::Url)
    }
}

/// One item as a caller describes it. `fd` indexes the call's file-descriptor
/// array. `path` is only accepted from unsandboxed callers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemSpec {
    pub kind: ContentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fd: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl ItemSpec {
    pub fn text(text: impl Into<String>) -> Self {
        Self { kind: ContentKind::Text, text: Some(text.into()), name: None, mime: None, fd: None, path: None }
    }

    pub fn url(url: impl Into<String>) -> Self {
        Self { kind: ContentKind::Url, text: Some(url.into()), name: None, mime: None, fd: None, path: None }
    }

    pub fn fd(kind: ContentKind, index: u32, name: Option<String>, mime: Option<String>) -> Self {
        Self { kind, text: None, name, mime, fd: Some(index), path: None }
    }
}

/// What the service reports about an accepted item. Never the file itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemInfo {
    pub kind: ContentKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    pub size: u64,
    /// The start of a text or a link, for the preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// For a file from a shared folder, the folders it sits in, from the shared
    /// folder's own name down: "Trip/day 1".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_items: usize,
    pub max_text_bytes: usize,
    pub max_name_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_items: 200, max_text_bytes: 1024 * 1024, max_name_bytes: 255 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentError {
    Empty,
    TooManyItems(usize),
    MissingText(usize),
    TextTooLong(usize),
    BadUrl(usize),
    MissingData(usize),
    BothFdAndPath(usize),
    UnexpectedData(usize),
    BadFdIndex(usize),
    ReusedFd(usize),
    UnusedFds,
    BadMime(usize),
    NameTooLong(usize),
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = |i: &usize| i + 1;
        match self {
            Self::Empty => write!(f, "There is nothing to share"),
            Self::TooManyItems(max) => write!(f, "Share at most {max} items at a time"),
            Self::MissingText(i) => write!(f, "Item {} has no text", n(i)),
            Self::TextTooLong(i) => write!(f, "Item {} is too long to share as text", n(i)),
            Self::BadUrl(i) => write!(f, "Item {} isn't a link that can be shared", n(i)),
            Self::MissingData(i) => write!(f, "Item {} has no file descriptor", n(i)),
            Self::BothFdAndPath(i) => write!(f, "Item {} has both a file descriptor and a path", n(i)),
            Self::UnexpectedData(i) => write!(f, "Item {} is text but carries a file", n(i)),
            Self::BadFdIndex(i) => write!(f, "Item {} refers to a file descriptor that wasn't sent", n(i)),
            Self::ReusedFd(i) => write!(f, "Item {} reuses another item's file descriptor", n(i)),
            Self::UnusedFds => write!(f, "The request sent file descriptors that no item uses"),
            Self::BadMime(i) => write!(f, "Item {} has an invalid content type", n(i)),
            Self::NameTooLong(i) => write!(f, "Item {} has a name that is too long", n(i)),
        }
    }
}

impl std::error::Error for ContentError {}

/// Schemes a shared link may use. `file:`, `data:` and `javascript:` are refused.
const URL_SCHEMES: &[&str] = &["http", "https", "mailto", "tel", "sms", "geo", "magnet", "ftp", "sftp", "smb", "nfs"];

fn valid_url(url: &str) -> bool {
    if url.is_empty() || url.len() > 8192 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once(':') else { return false };
    let scheme = scheme.to_ascii_lowercase();
    URL_SCHEMES.contains(&scheme.as_str())
        && !rest.is_empty()
        && (!scheme.starts_with("http") || rest.len() > 2 && rest.starts_with("//"))
}

fn valid_mime(mime: &str) -> bool {
    let Some((top, sub)) = mime.split_once('/') else { return false };
    let token = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "!#$&-^_.+".contains(c));
    let sub = sub.split(';').next().unwrap_or_default().trim();
    mime.len() <= 255 && token(top) && token(sub)
}

/// Check a request's items against `fds` sent file descriptors.
pub fn validate(items: &[ItemSpec], fds: usize, limits: &Limits) -> Result<(), ContentError> {
    if items.is_empty() {
        return Err(ContentError::Empty);
    }
    if items.len() > limits.max_items {
        return Err(ContentError::TooManyItems(limits.max_items));
    }
    let mut used = vec![false; fds];
    for (i, item) in items.iter().enumerate() {
        // Long names are shortened when saved; this only bounds what a caller can send.
        if item.name.as_ref().is_some_and(|n| n.len() > 4096) {
            return Err(ContentError::NameTooLong(i));
        }
        if item.mime.as_ref().is_some_and(|m| !valid_mime(m)) {
            return Err(ContentError::BadMime(i));
        }
        if item.kind.is_inline() {
            if item.fd.is_some() || item.path.is_some() {
                return Err(ContentError::UnexpectedData(i));
            }
            let text = item.text.as_deref().ok_or(ContentError::MissingText(i))?;
            if text.is_empty() {
                return Err(ContentError::MissingText(i));
            }
            if text.len() > limits.max_text_bytes {
                return Err(ContentError::TextTooLong(i));
            }
            if item.kind == ContentKind::Url && !valid_url(text.trim()) {
                return Err(ContentError::BadUrl(i));
            }
            continue;
        }
        match (item.fd, item.path.as_deref()) {
            (Some(_), Some(_)) => return Err(ContentError::BothFdAndPath(i)),
            (None, None) => return Err(ContentError::MissingData(i)),
            (None, Some("")) => return Err(ContentError::MissingData(i)),
            (None, Some(_)) => {}
            (Some(fd), None) => {
                let slot = used.get_mut(fd as usize).ok_or(ContentError::BadFdIndex(i))?;
                if *slot {
                    return Err(ContentError::ReusedFd(i));
                }
                *slot = true;
            }
        }
    }
    if used.iter().any(|u| !u) {
        return Err(ContentError::UnusedFds);
    }
    Ok(())
}

/// The distinct kinds in a set of items, in a stable order.
pub fn kinds_of<'a>(kinds: impl IntoIterator<Item = &'a ContentKind>) -> Vec<ContentKind> {
    let mut v: Vec<ContentKind> = kinds.into_iter().copied().collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(items: &[ItemSpec], fds: usize) -> Result<(), ContentError> {
        validate(items, fds, &Limits::default())
    }

    #[test]
    fn accepts_text_links_and_files() {
        let items = [
            ItemSpec::text("Hello"),
            ItemSpec::url("https://example.com/a?b=c"),
            ItemSpec::fd(ContentKind::File, 0, Some("a.pdf".into()), Some("application/pdf".into())),
            ItemSpec::fd(ContentKind::Image, 1, None, Some("image/png".into())),
        ];
        assert_eq!(check(&items, 2), Ok(()));
    }

    #[test]
    fn refuses_empty_and_oversized_requests() {
        assert_eq!(check(&[], 0), Err(ContentError::Empty));
        let many: Vec<_> = (0..201).map(|_| ItemSpec::text("x")).collect();
        assert_eq!(check(&many, 0), Err(ContentError::TooManyItems(200)));
        let long = ItemSpec::text("x".repeat(1024 * 1024 + 1));
        assert_eq!(check(&[long], 0), Err(ContentError::TextTooLong(0)));
    }

    #[test]
    fn refuses_dangerous_links() {
        for bad in
            ["javascript:alert(1)", "file:///etc/passwd", "data:text/html,x", "https:", "https:example", "http://a b", "nope"]
        {
            assert_eq!(check(&[ItemSpec::url(bad)], 0), Err(ContentError::BadUrl(0)), "{bad}");
        }
        for good in ["mailto:a@b.c", "geo:1,2", "https://x.y", "HTTPS://X.Y/z"] {
            assert_eq!(check(&[ItemSpec::url(good)], 0), Ok(()), "{good}");
        }
    }

    #[test]
    fn file_descriptors_must_match_items() {
        let f = |i| ItemSpec::fd(ContentKind::File, i, None, None);
        assert_eq!(check(&[f(1)], 1), Err(ContentError::BadFdIndex(0)));
        assert_eq!(check(&[f(0), f(0)], 1), Err(ContentError::ReusedFd(1)));
        assert_eq!(check(&[f(0)], 2), Err(ContentError::UnusedFds));
        let mut both = f(0);
        both.path = Some("/tmp/x".into());
        assert_eq!(check(&[both], 1), Err(ContentError::BothFdAndPath(0)));
        let mut none = f(0);
        none.fd = None;
        assert_eq!(check(&[none], 0), Err(ContentError::MissingData(0)));
        let mut text_with_fd = ItemSpec::text("x");
        text_with_fd.fd = Some(0);
        assert_eq!(check(&[text_with_fd], 1), Err(ContentError::UnexpectedData(0)));
    }

    #[test]
    fn checks_content_types() {
        let mut item = ItemSpec::fd(ContentKind::File, 0, None, Some("text/plain; charset=utf-8".into()));
        assert_eq!(check(std::slice::from_ref(&item), 1), Ok(()));
        item.mime = Some("not a type".into());
        assert_eq!(check(&[item], 1), Err(ContentError::BadMime(0)));
    }

    #[test]
    fn json_shape_is_stable() {
        let item: ItemSpec = serde_json::from_str(r#"{"kind":"file","name":"a.txt","fd":0,"future":1}"#).unwrap();
        assert_eq!(item.kind, ContentKind::File);
        assert_eq!(serde_json::to_string(&ItemSpec::text("hi")).unwrap(), r#"{"kind":"text","text":"hi"}"#);
    }
}
