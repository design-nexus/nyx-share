//! A stand-in for Nyx Cloud's service, for checks on a private bus:
//! `fake_cloud NAME DIR` serves one account whose "mount" is DIR, unmounted
//! until Mount is called. Run with NYX_SHARE_CLOUD_BUS=NAME on the daemon.

use std::sync::{Arc, Mutex};

struct Fake {
    dir: String,
    mounted: Arc<Mutex<bool>>,
}

#[zbus::interface(name = "io.github.design_nexus.nyx.Cloud1")]
impl Fake {
    fn list_remotes(&self) -> String {
        let mounted = *self.mounted.lock().unwrap();
        serde_json::json!([{
            "id": "drive-1", "name": "Test Drive", "provider": "drive", "connection": "connected",
            "mount_status": if mounted { "mounted" } else { "unmounted" },
            "mount_path": if mounted { self.dir.clone() } else { String::new() },
            "error": "", "storage_enabled": true
        }])
        .to_string()
    }

    fn mount(&self, id: &str) -> zbus::fdo::Result<String> {
        if id != "drive-1" {
            return Err(zbus::fdo::Error::Failed("Account not found".into()));
        }
        let flag = self.mounted.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            *flag.lock().unwrap() = true;
        });
        Ok("op-1".into())
    }
}

fn main() -> zbus::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (name, dir) = (args[1].clone(), args[2].clone());
    let _conn = zbus::blocking::connection::Builder::session()?
        .name(name)?
        .serve_at("/io/github/design_nexus/nyx/Cloud1", Fake { dir, mounted: Arc::default() })?
        .build()?;
    loop {
        std::thread::park();
    }
}
