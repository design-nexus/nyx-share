//! A real provider program in the real sandbox: what it can and can't reach.
//! Skipped where bubblewrap or user namespaces aren't available.

use nyx_share_core::{ContentKind, ItemInfo};
use nyx_share_external::External;
use nyx_share_providers::{Data, Payload, Provider, SendError, SendOptions, Task};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Reports what it sees: the item's bytes, the home folder, the session bus,
/// and how many network interfaces it has (only `lo` when it has no network).
const PROBE: &str = r#"#!/bin/bash
read -r request
fd=$(sed -n 's/.*"fd":\([0-9]*\).*/\1/p' <<< "$request")
content=$(cat <&"$fd")
homes=$(ls /home 2>/dev/null | wc -l)
real=$(cat "$REAL_HOME/.bashrc" 2>/dev/null | wc -c)
ifaces=$(($(wc -l < /proc/net/dev) - 2))
echo "{\"step\": \"Probing\"}"
echo "{\"progress\": ${#content}}"
echo "{\"done\": {\"message\": \"got=$content homes=$homes real=$real bus=${DBUS_SESSION_BUS_ADDRESS:-none} ifaces=$ifaces\"}}"
"#;

fn install(root: &Path, id: &str, script: &str, network: bool) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("provider.toml"),
        format!("id = \"{id}\"\nname = \"Probe\"\nexec = \"probe\"\nkinds = [\"file\", \"text\"]\n[permissions]\nnetwork = {network}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("probe"), script).unwrap();
    std::fs::set_permissions(dir.join("probe"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nyx-share-sandbox-{}-{}", std::process::id(), rand()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn rand() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
}

fn usable() -> bool {
    std::process::Command::new("bwrap")
        .args([
            "--unshare-all",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/bin",
            "/bin",
            "--symlink",
            "usr/lib",
            "/lib64",
            "--",
            "/usr/bin/true",
        ])
        .status()
        .is_ok_and(|s| s.success())
}

fn send(ext: &External, id: &str, file: &Path, cancel_after: Option<std::time::Duration>) -> Result<String, SendError> {
    let items = [Payload {
        info: ItemInfo {
            kind: ContentKind::File,
            name: "a.txt".into(),
            mime: None,
            size: std::fs::metadata(file).unwrap().len(),
            preview: None,
        },
        data: Data::File(std::fs::File::open(file).unwrap()),
    }];
    let (options, cancel) = (SendOptions::default(), AtomicBool::new(false));
    std::thread::scope(|s| {
        if let Some(after) = cancel_after {
            let cancel = &cancel;
            s.spawn(move || {
                std::thread::sleep(after);
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        }
        let task = Task { items: &items, options: &options, cancel: &cancel, progress: &|_| {}, step: &|_| {} };
        ext.send(&format!("ext:{id}"), &task).map(|o| o.message)
    })
}

#[test]
fn providers_see_only_their_items() {
    if !usable() {
        eprintln!("skipped: bubblewrap with user namespaces isn't available");
        return;
    }
    let root = scratch();
    let file = root.join("item.txt");
    std::fs::write(&file, "hello").unwrap();
    // SAFETY: tests in this file don't read the environment concurrently.
    unsafe { std::env::set_var("REAL_HOME", std::env::var("HOME").unwrap()) };
    let script = PROBE.replace("$REAL_HOME", &std::env::var("HOME").unwrap());
    install(&root.join("providers"), "offline", &script, false);
    install(&root.join("providers"), "online", &script, true);
    let ext = External::at(root.join("providers"), root.join("data"));
    assert_eq!(ext.destinations().len(), 2);

    let offline = send(&ext, "offline", &file, None).unwrap();
    eprintln!("offline: {offline}");
    assert!(offline.starts_with("got=hello homes=0 real=0 bus=none "), "{offline}");
    assert!(offline.ends_with("ifaces=1"), "only loopback without the network permission: {offline}");
    let online = send(&ext, "online", &file, None).unwrap();
    eprintln!("online: {online}");
    assert!(!online.ends_with("ifaces=1"), "the host's interfaces with it: {online}");
    assert!(root.join("data/offline").is_dir(), "each provider has its own data folder");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancelling_stops_the_provider() {
    if !usable() {
        return;
    }
    let root = scratch();
    let file = root.join("item.txt");
    std::fs::write(&file, "x").unwrap();
    install(
        &root.join("providers"),
        "sleeper",
        "#!/bin/bash\nread -r request\nsleep 30\necho '{\"done\":{\"message\":\"late\"}}'\n",
        false,
    );
    let ext = External::at(root.join("providers"), root.join("data"));
    let start = std::time::Instant::now();
    let result = send(&ext, "sleeper", &file, Some(std::time::Duration::from_millis(500)));
    assert!(matches!(result, Err(SendError::Cancelled { .. })), "{result:?}");
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    std::fs::remove_dir_all(root).unwrap();
}
