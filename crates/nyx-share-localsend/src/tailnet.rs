//! Devices on the user's tailnet, from the Tailscale CLI. Multicast doesn't
//! cross Tailscale, so LocalSend devices there are contacted directly.
//! Tailscale's own access rules decide what can be reached.

use serde_json::Value;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct TailPeer {
    pub name: String,
    /// The MagicDNS name, without the trailing dot.
    pub dns: String,
    pub ip: Ipv4Addr,
    pub os: String,
    pub online: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Missing,
    Stopped,
    Running { me: Ipv4Addr, peers: Vec<TailPeer> },
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Missing => "missing",
            Status::Stopped => "stopped",
            Status::Running { .. } => "running",
        }
    }
}

/// Tailscale's CGNAT range, 100.64.0.0/10.
pub fn is_tailscale(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (64..128).contains(&o[1])
        }
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// Ask `tailscale status --json`, giving up after three seconds.
pub fn status() -> Status {
    let Some(program) = nyx_share_providers::process::find("tailscale") else { return Status::Missing };
    let Ok(mut child) = Command::new(program)
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Status::Missing;
    };
    let mut out = String::new();
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let _ = stdout.by_ref().take(4 << 20).read_to_string(&mut out);
        out
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(3) => std::thread::sleep(Duration::from_millis(30)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Status::Stopped;
            }
        }
    }
    parse(&reader.join().unwrap_or_default())
}

pub fn parse(json: &str) -> Status {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Status::Stopped };
    if v.get("BackendState").and_then(Value::as_str) != Some("Running") {
        return Status::Stopped;
    }
    let ipv4 = |node: &Value| -> Option<Ipv4Addr> {
        node.get("TailscaleIPs")?.as_array()?.iter().filter_map(Value::as_str).find_map(|s| s.parse().ok())
    };
    let Some(me) = v.get("Self").and_then(ipv4) else { return Status::Stopped };
    let peers = v
        .get("Peer")
        .and_then(Value::as_object)
        .map(|peers| {
            peers
                .values()
                .filter_map(|p| {
                    Some(TailPeer {
                        name: p.get("HostName")?.as_str()?.to_string(),
                        dns: p.get("DNSName").and_then(Value::as_str).unwrap_or_default().trim_end_matches('.').to_string(),
                        ip: ipv4(p)?,
                        os: p.get("OS").and_then(Value::as_str).unwrap_or_default().to_string(),
                        online: p.get("Online").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Status::Running { me, peers }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tailscale_status() {
        let json = r#"{"BackendState":"Running","Self":{"HostName":"Hades","TailscaleIPs":["100.68.211.7","fd7a:115c:a1e0::1"]},
            "Peer":{"k1":{"HostName":"-=Pandora=-","DNSName":"pandora.tail.ts.net.","OS":"android","TailscaleIPs":["100.94.97.89"],"Online":true},
                    "k2":{"HostName":"Surface","OS":"windows","TailscaleIPs":["100.73.188.54"],"Online":false}}}"#;
        let Status::Running { me, mut peers } = parse(json) else { panic!() };
        assert_eq!(me, Ipv4Addr::new(100, 68, 211, 7));
        peers.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(peers[0].name, "-=Pandora=-");
        assert_eq!(peers[0].dns, "pandora.tail.ts.net");
        assert!(peers[0].online && !peers[1].online);
        assert_eq!(parse(r#"{"BackendState":"Stopped"}"#), Status::Stopped);
        assert_eq!(parse("not json"), Status::Stopped);
    }

    #[test]
    fn recognises_tailscale_addresses() {
        assert!(is_tailscale(&"100.94.97.89".parse().unwrap()));
        assert!(!is_tailscale(&"100.128.0.1".parse().unwrap()));
        assert!(!is_tailscale(&"192.168.1.5".parse().unwrap()));
        assert!(is_tailscale(&"fd7a:115c:a1e0::5".parse().unwrap()));
    }
}
