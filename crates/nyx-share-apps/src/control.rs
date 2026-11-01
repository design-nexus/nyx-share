//! Nyx apps' control interface (`nyx_core::control`): each running app serves
//! `io.github.design_nexus.nyx.Control` on its own bus name and object path,
//! with `Call(name, arguments) → {"ok": …} | {"error": "…"}`. It is what Nyx
//! Assistant uses; Share calls the same tools, as the user's explicit choice.

use serde_json::Value;
use std::time::{Duration, Instant};

pub const INTERFACE: &str = "io.github.design_nexus.nyx.Control";

/// A Nyx app reached through its control interface.
#[derive(Clone, Debug)]
pub struct App {
    pub name: &'static str,
    /// The GApplication id, which is also its bus name.
    pub id: String,
    /// Started when the app isn't running; None for test copies.
    pub program: Option<&'static str>,
}

impl App {
    /// `env` overrides the id for checks against a test copy (which is never started).
    pub fn new(name: &'static str, id: &str, program: &'static str, env: &str) -> Self {
        match std::env::var(env).ok().filter(|s| !s.is_empty()) {
            Some(test) => Self { name, id: test, program: None },
            None => Self { name, id: id.into(), program: Some(program) },
        }
    }

    fn path(&self) -> String {
        format!("/{}", self.id.replace('.', "/").replace('-', "_"))
    }

    pub fn installed(&self) -> bool {
        self.program.is_none_or(|p| nyx_share_providers::process::find(p).is_some())
    }

    pub fn running(&self, bus: &zbus::blocking::Connection) -> bool {
        zbus::blocking::fdo::DBusProxy::new(bus)
            .ok()
            .and_then(|p| p.name_has_owner(self.id.as_str().try_into().ok()?).ok())
            .unwrap_or(false)
    }

    /// Start the app if it isn't running and wait for it on the bus.
    pub fn ensure(&self, bus: &zbus::blocking::Connection) -> Result<(), String> {
        if self.running(bus) {
            return Ok(());
        }
        let program =
            self.program.and_then(nyx_share_providers::process::find).ok_or_else(|| format!("{} isn't running", self.name))?;
        nyx_share_providers::process::detached(&program).spawn().map_err(|e| format!("Couldn't start {}: {e}", self.name))?;
        let start = Instant::now();
        while !self.running(bus) {
            if start.elapsed() > Duration::from_secs(20) {
                return Err(format!("{} didn't start", self.name));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    }

    /// Call one tool. The app's own error sentence comes back as the error.
    pub fn call(&self, bus: &zbus::blocking::Connection, tool: &str, args: &Value) -> Result<Value, String> {
        let reply = bus
            .call_method(Some(self.id.as_str()), self.path().as_str(), Some(INTERFACE), "Call", &(tool, args.to_string()))
            .map_err(|e| format!("{} didn't answer: {e}", self.name))?;
        let body: String = reply.body().deserialize().map_err(|e| format!("{} answered oddly: {e}", self.name))?;
        let value: Value = serde_json::from_str(&body).map_err(|e| format!("{} answered oddly: {e}", self.name))?;
        match (value.get("ok"), value.get("error")) {
            (Some(ok), _) => Ok(ok.clone()),
            (_, Some(e)) => Err(e.as_str().unwrap_or("it refused").to_string()),
            _ => Err(format!("{} answered oddly", self.name)),
        }
    }
}

pub fn session() -> Result<zbus::blocking::Connection, String> {
    zbus::blocking::Connection::session().map_err(|e| format!("No session bus: {e}"))
}
