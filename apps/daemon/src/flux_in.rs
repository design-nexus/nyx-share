//! Files devices send through Flux, as receive jobs, while the service runs. On start it
//! catches up from where it stopped: `$XDG_STATE_HOME/nyx-share/flux-received` holds the
//! start time of the newest transfer already listed.

use crate::state::{ReceiveEnd, Receiving, State};
use nyx_connect::{Event, Watch};
use nyx_share_flux::incoming::{Change, End, Follower};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// The first time, only the last day's files are listed.
const FIRST_LOOK_BACK: i64 = 24 * 60 * 60;

fn mark_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("nyx-share/flux-received")
}

fn load_mark() -> Option<i64> {
    std::fs::read_to_string(mark_path()).ok()?.trim().parse().ok()
}

fn save_mark(t: i64) {
    let path = mark_path();
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_ok()
    {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, t.to_string()).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

/// Follow fluxd until the returned watch is dropped.
pub fn start(state: &Arc<State>) -> Watch {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mut follower = Follower::new(load_mark().unwrap_or(now - FIRST_LOOK_BACK));
    let mut mark = follower.seen_until();
    // Jobs already in the history when the service started: left as they are.
    let mut listed: HashSet<String> = HashSet::new();
    let weak = Arc::downgrade(state);
    Watch::start(move |event| {
        let Some(state) = weak.upgrade() else { return };
        let changes = match event {
            Event::State(s) => follower.update(&s),
            Event::Unavailable(_) => follower.lost(),
            Event::Toast(_) => return,
        };
        for change in changes {
            apply(&state, &mut listed, change);
        }
        if follower.seen_until() != mark {
            mark = follower.seen_until();
            save_mark(mark);
        }
    })
}

fn apply(state: &State, listed: &mut HashSet<String>, change: Change) {
    match change {
        Change::Begin { job, device, sender, name, total, time } => {
            if state.has_job(&job) {
                listed.insert(job);
                return;
            }
            // fluxd tells the user itself when a file arrives.
            state.begin_receive(Receiving {
                id: job,
                destination: format!("{}{device}", nyx_share_flux::PREFIX),
                sender,
                names: vec![name],
                total,
                notify: false,
                created: Some(time),
            });
        }
        Change::Progress { job, .. } | Change::Saved { job, .. } | Change::End { job, .. } if listed.contains(&job) => {}
        Change::Progress { job, done } => state.receive_done(&job, done),
        Change::Saved { job, path } => state.received(&job, path),
        Change::End { job, end } => state.end_receive(
            &job,
            match end {
                End::Completed => ReceiveEnd::Completed,
                End::Cancelled => ReceiveEnd::Cancelled,
                End::Failed(reason) => ReceiveEnd::Failed(reason),
            },
        ),
    }
}
