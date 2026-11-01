//! Linked devices: one entry for a phone or computer reached several ways (a
//! saved computer, Flux, Bluetooth, LocalSend). The user links the routes; names
//! are only ever a suggestion.

use crate::{ContentKind, Destination, Section};
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "device:";
/// Providers whose destinations are devices that can be linked.
pub const LINKABLE: [&str; 4] = ["computers", "flux", "bluetooth", "localsend"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkedDevice {
    pub id: String,
    pub name: String,
    /// Destination ids, most preferred first.
    pub routes: Vec<String>,
}

impl LinkedDevice {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.chars().count() > 80 {
            return Err("A device needs a name of up to 80 characters".into());
        }
        if self.routes.len() < 2 || self.routes.len() > 6 {
            return Err("Link two to six ways of reaching the device".into());
        }
        let linkable = ["computer:", "flux:", "bluetooth:", "localsend:"];
        if let Some(bad) = self.routes.iter().find(|r| !linkable.iter().any(|p| r.starts_with(p)) || r.len() > 512) {
            return Err(format!("{bad} isn't a device that can be linked"));
        }
        Ok(())
    }
}

/// A route counts as reachable unless its status says it's offline.
fn online(d: &Destination) -> bool {
    d.available && !d.status.as_deref().is_some_and(|s| s.starts_with("Offline"))
}

/// Replace each linked device's routes with one destination for the device.
/// Routes not present now (a nearby device that's away) are skipped; a device
/// with none of its routes present isn't listed.
pub fn link(dests: Vec<Destination>, devices: &[LinkedDevice]) -> Vec<Destination> {
    let mut out = Vec::with_capacity(dests.len());
    let mut combined: Vec<Destination> = Vec::new();
    for device in devices {
        let members: Vec<&Destination> = device.routes.iter().filter_map(|r| dests.iter().find(|d| &d.id == r)).collect();
        let Some(best) = members.iter().find(|d| online(d)).or(members.first()).copied() else { continue };
        let mut kinds: Vec<ContentKind> = members.iter().flat_map(|d| d.kinds.iter().copied()).collect();
        kinds.sort();
        kinds.dedup();
        let names: Vec<String> = members.iter().filter(|d| online(d)).filter_map(|d| route_name(d)).collect();
        combined.push(Destination {
            id: format!("{PREFIX}{}", device.id),
            provider: "devices".into(),
            name: device.name.clone(),
            icon: best.icon.clone(),
            tint: best.tint.clone(),
            section: Section::Devices,
            kinds,
            max_items: if members.iter().any(|d| d.max_items.is_none()) {
                None
            } else {
                members.iter().filter_map(|d| d.max_items).max()
            },
            needs_folder: false,
            available: members.iter().any(|d| d.available),
            status: Some(if names.is_empty() { "Offline".into() } else { names.join(" · ") }),
            pinned: false,
            hidden: false,
            default: false,
        });
    }
    for d in dests {
        if !devices.iter().any(|dev| dev.routes.contains(&d.id)) {
            out.push(d);
        }
    }
    out.extend(combined);
    out
}

/// How a route is described in the combined status: "Flux", "Bluetooth", "LAN"…
pub fn route_name(d: &Destination) -> Option<String> {
    Some(match d.provider.as_str() {
        "flux" => "Flux".into(),
        "bluetooth" => "Bluetooth".into(),
        "localsend" => "Nearby".into(),
        "computers" => d.status.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "Computer".into()),
        _ => return None,
    })
}

/// The routes to try for a share, in order: reachable ones first (in the user's
/// order), then the rest; only routes that take these items.
pub fn routes_for<'a>(
    device: &LinkedDevice,
    dests: &'a [Destination],
    kinds: &[ContentKind],
    count: usize,
) -> Vec<&'a Destination> {
    let members: Vec<&Destination> = device
        .routes
        .iter()
        .filter_map(|r| dests.iter().find(|d| &d.id == r))
        .filter(|d| d.available && d.accepts(kinds, count))
        .collect();
    let (up, down): (Vec<_>, Vec<_>) = members.into_iter().partition(|d| online(d));
    up.into_iter().chain(down).collect()
}

/// Devices that look like the same one reached several ways: linkable
/// destinations with the same name, not linked yet. Only offered to the user.
pub fn suggestions(dests: &[Destination], devices: &[LinkedDevice]) -> Vec<(String, Vec<String>)> {
    let linked: Vec<&String> = devices.iter().flat_map(|d| d.routes.iter()).collect();
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for d in dests.iter().filter(|d| LINKABLE.contains(&d.provider.as_str()) && !linked.contains(&&d.id)) {
        let key = d.name.trim().to_lowercase();
        match groups.iter_mut().find(|(name, _)| name.trim().to_lowercase() == key) {
            Some((_, ids)) => ids.push(d.id.clone()),
            None => groups.push((d.name.clone(), vec![d.id.clone()])),
        }
    }
    groups.retain(|(_, ids)| ids.len() > 1);
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(id: &str, provider: &str, status: &str, kinds: &[ContentKind]) -> Destination {
        Destination {
            id: id.into(),
            provider: provider.into(),
            name: "Pandora".into(),
            icon: "phone-symbolic".into(),
            tint: "green".into(),
            section: Section::Devices,
            kinds: kinds.to_vec(),
            max_items: None,
            needs_folder: false,
            available: true,
            status: Some(status.into()),
            pinned: false,
            hidden: false,
            default: false,
        }
    }

    #[test]
    fn links_routes_into_one_device() {
        use ContentKind::*;
        let dests = vec![
            route("flux:p", "flux", "Offline · Flux sends files when it's back", &[Text, Url, File]),
            route("bluetooth:aa", "bluetooth", "Bluetooth", &[Text, File]),
            route("save", "folder", "", &[File]),
        ];
        let device =
            LinkedDevice { id: "1".into(), name: "My phone".into(), routes: vec!["flux:p".into(), "bluetooth:aa".into()] };
        let out = link(dests.clone(), std::slice::from_ref(&device));
        assert_eq!(out.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["save", "device:1"]);
        let d = &out[1];
        assert_eq!((d.name.as_str(), d.status.as_deref()), ("My phone", Some("Bluetooth")));
        assert_eq!(d.kinds, vec![Text, Url, File]);
        // Bluetooth is up and Flux is offline: Bluetooth first. A link only Flux takes.
        let order: Vec<&str> = routes_for(&device, &dests, &[File], 1).iter().map(|d| d.id.as_str()).collect();
        assert_eq!(order, ["bluetooth:aa", "flux:p"]);
        assert_eq!(routes_for(&device, &dests, &[Url], 1).iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["flux:p"]);
        assert!(suggestions(&dests, &[device]).is_empty());
        assert_eq!(
            suggestions(&dests, &[]),
            vec![("Pandora".to_string(), vec!["flux:p".to_string(), "bluetooth:aa".to_string()])]
        );
        assert!(
            LinkedDevice { routes: vec!["save".into(), "flux:p".into()], name: "x".into(), ..Default::default() }
                .validate()
                .is_err()
        );
    }
}
