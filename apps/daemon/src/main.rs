//! nyx-share-daemon: the session service behind `io.github.design_nexus.nyx.Share1`.
//! Started by D-Bus activation; exits once it has been idle for a minute.

mod caller;
mod config;
mod items;
mod launch;
mod nearby;
mod notify;
mod service;
mod state;

use anyhow::Context;
use nyx_share_dbus::{BUS, PATH};
use state::State;
use std::sync::Arc;
use std::time::Duration;

const IDLE_EXIT: Duration = Duration::from_secs(60);

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version") {
        println!("nyx-share-daemon {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let providers = nyx_share_providers::builtin().into_iter().map(Arc::from).collect();
    let state = Arc::new(State::new(providers, config::load()));
    async_io::block_on(async {
        let conn = zbus::connection::Builder::session()?
            .serve_at(PATH, service::Share1(state.clone()))?
            .serve_at(PATH, service::Sheet1(state.clone()))?
            .name(BUS)?
            .build()
            .await
            .context("Couldn't take the Nyx Share bus name (is another copy running?)")?;
        state.attach(conn.clone());
        let nearby = state.clone();
        std::thread::spawn(move || nearby::start(nearby));
        loop {
            async_io::Timer::after(Duration::from_secs(15)).await;
            state.expire_requests();
            state.drop_departed_holders(&conn).await;
            if state.idle_for() >= IDLE_EXIT && !state.busy() {
                // Give the name up first so new calls start a fresh service.
                let _ = conn.release_name(BUS).await;
                if !state.busy() {
                    break;
                }
            }
        }
        anyhow::Ok(())
    })
}
