use crate::content::ContentKind;
use serde::{Deserialize, Serialize};

/// Where a destination sits in the Share Sheet when it isn't pinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Section {
    /// Computers, phones and NAS folders the user set up.
    Devices,
    /// Devices discovered on the local network.
    Nearby,
    /// Apps and services on this computer.
    Apps,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    /// Stable across restarts, e.g. `clipboard` or `localsend:<fingerprint>`.
    pub id: String,
    pub provider: String,
    pub name: String,
    /// A symbolic icon name.
    pub icon: String,
    /// The glyph colour: one of the Nyx swatch names.
    pub tint: String,
    pub section: Section,
    pub kinds: Vec<ContentKind>,
    /// At most this many items per share, if limited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u32>,
    /// The Share Sheet asks for a folder before sending.
    #[serde(default)]
    pub needs_folder: bool,
    pub available: bool,
    /// How it is reached ("LAN", "Tailscale") or why it can't be used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub hidden: bool,
}

impl Destination {
    /// True if every item kind is accepted and the count fits.
    pub fn accepts(&self, kinds: &[ContentKind], count: usize) -> bool {
        count > 0
            && !kinds.is_empty()
            && kinds.iter().all(|k| self.kinds.contains(k))
            && self.max_items.is_none_or(|max| count <= max as usize)
    }
}

/// The user's arrangement, owned by the service.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub order: Vec<String>,
    pub pinned: Vec<String>,
    pub hidden: Vec<String>,
}

impl Preferences {
    /// Apply a partial update: each list present in `update` replaces the current one.
    pub fn merge(&mut self, update: PreferencesUpdate) {
        if let Some(order) = update.order {
            self.order = dedup(order);
        }
        if let Some(pinned) = update.pinned {
            self.pinned = dedup(pinned);
        }
        if let Some(hidden) = update.hidden {
            self.hidden = dedup(hidden);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreferencesUpdate {
    pub order: Option<Vec<String>>,
    pub pinned: Option<Vec<String>>,
    pub hidden: Option<Vec<String>>,
}

fn dedup(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter().filter(|id| id.len() <= 512 && seen.insert(id.clone())).collect()
}

/// Mark pinned and hidden destinations and sort them: ids in `order` first, in
/// that order, then the rest as providers listed them.
pub fn arrange(mut dests: Vec<Destination>, prefs: &Preferences) -> Vec<Destination> {
    for d in &mut dests {
        d.pinned = prefs.pinned.contains(&d.id);
        d.hidden = prefs.hidden.contains(&d.id);
    }
    let rank = |d: &Destination| prefs.order.iter().position(|id| *id == d.id).unwrap_or(usize::MAX);
    dests.sort_by_key(rank);
    dests
}

#[cfg(test)]
mod tests {
    use super::*;
    use ContentKind::*;

    fn dest(id: &str, kinds: &[ContentKind], max: Option<u32>) -> Destination {
        Destination {
            id: id.into(),
            provider: "test".into(),
            name: id.into(),
            icon: "x".into(),
            tint: "graphite".into(),
            section: Section::Apps,
            kinds: kinds.to_vec(),
            max_items: max,
            needs_folder: false,
            available: true,
            status: None,
            pinned: false,
            hidden: false,
        }
    }

    #[test]
    fn filters_by_kind_and_count() {
        let clipboard = dest("clipboard", &[Text, Url, Image], Some(1));
        assert!(clipboard.accepts(&[Url], 1));
        assert!(!clipboard.accepts(&[Image], 2));
        assert!(!clipboard.accepts(&[File], 1));
        assert!(!clipboard.accepts(&[Text, File], 2));
        assert!(!clipboard.accepts(&[], 0));
        let folder = dest("save", &[Text, Image, File], None);
        assert!(folder.accepts(&[Image, File], 120));
        assert!(!folder.accepts(&[Url], 1));
    }

    #[test]
    fn arranges_by_preferences() {
        let all = vec![dest("a", &[Text], None), dest("b", &[Text], None), dest("c", &[Text], None)];
        let prefs = Preferences { order: vec!["c".into(), "a".into()], pinned: vec!["b".into()], hidden: vec!["a".into()] };
        let out = arrange(all, &prefs);
        let ids: Vec<_> = out.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
        assert!(out[1].hidden && !out[1].pinned);
        assert!(out[2].pinned);
    }

    #[test]
    fn partial_updates_keep_other_lists() {
        let mut prefs = Preferences { order: vec!["a".into()], pinned: vec!["a".into()], hidden: vec![] };
        prefs.merge(PreferencesUpdate { hidden: Some(vec!["b".into(), "b".into()]), ..Default::default() });
        assert_eq!(prefs.order, ["a"]);
        assert_eq!(prefs.pinned, ["a"]);
        assert_eq!(prefs.hidden, ["b"]);
    }
}
