//! Files a device sends through Flux, as Share's receive jobs. A paired device sends
//! without asking: fluxd saves the file (to its download folder) and tells the user;
//! Share lists it with the rest of its history, and shows its progress while running.
//!
//! Share's service only runs on demand, so on start it catches up on what fluxd still
//! lists, from where it left off ([`Follower::seen_until`]).

use nyx_connect::{Client, Error, Snapshot};
use std::collections::HashMap;

/// Receive jobs from Flux: `flux-in:<transfer id>`.
pub const JOB_PREFIX: &str = "flux-in:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    Completed,
    Cancelled,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A transfer Share hasn't listed yet; `time` is when it started (Unix seconds).
    Begin {
        job: String,
        device: String,
        sender: String,
        name: String,
        total: u64,
        time: i64,
    },
    /// Bytes received so far.
    Progress {
        job: String,
        done: u64,
    },
    Saved {
        job: String,
        path: String,
    },
    End {
        job: String,
        end: End,
    },
}

/// What fluxd's incoming transfers did since the last snapshot.
#[derive(Debug, Default)]
pub struct Follower {
    /// Transfer id → (state, bytes done) as last seen.
    known: HashMap<String, (String, i64)>,
    seen_until: i64,
}

impl Follower {
    /// Transfers that ended at or before `seen_until` are already in Share's history.
    pub fn new(seen_until: i64) -> Self {
        Follower { known: HashMap::new(), seen_until }
    }

    /// The start time of the newest transfer that ended, for the next [`Follower::new`].
    pub fn seen_until(&self) -> i64 {
        self.seen_until
    }

    pub fn update(&mut self, snapshot: &Snapshot) -> Vec<Change> {
        let mut out = Vec::new();
        for t in snapshot.transfers.iter().filter(|t| t.incoming()) {
            let job = format!("{JOB_PREFIX}{}", t.id);
            let before = match self.known.get(&t.id) {
                Some(b) => b.clone(),
                None if t.finished() && t.time <= self.seen_until => {
                    self.known.insert(t.id.clone(), (t.state.clone(), t.done));
                    continue;
                }
                None => {
                    out.push(Change::Begin {
                        job: job.clone(),
                        device: t.device.clone(),
                        sender: if t.device_name.is_empty() { "a device".into() } else { t.device_name.clone() },
                        name: t.name.clone(),
                        total: t.size.max(0) as u64,
                        time: t.time,
                    });
                    (String::new(), 0)
                }
            };
            if t.done != before.1 && !t.finished() {
                out.push(Change::Progress { job: job.clone(), done: t.done.max(0) as u64 });
            }
            let was_over = matches!(before.0.as_str(), "done" | "failed" | "canceled");
            if t.finished() && !was_over {
                let end = match t.state.as_str() {
                    "done" => {
                        if !t.path.is_empty() {
                            out.push(Change::Saved { job: job.clone(), path: t.path.clone() });
                        }
                        End::Completed
                    }
                    "canceled" => End::Cancelled,
                    _ => End::Failed(if t.error.is_empty() { "The transfer stopped".into() } else { t.error.clone() }),
                };
                out.push(Change::End { job: job.clone(), end });
                self.seen_until = self.seen_until.max(t.time);
            }
            self.known.insert(t.id.clone(), (t.state.clone(), t.done));
        }
        out
    }

    /// fluxd went away: the transfers it was receiving won't finish.
    pub fn lost(&mut self) -> Vec<Change> {
        let running: Vec<String> = self
            .known
            .iter()
            .filter(|(_, (state, _))| !matches!(state.as_str(), "done" | "failed" | "canceled"))
            .map(|(id, _)| id.clone())
            .collect();
        running
            .into_iter()
            .map(|id| {
                self.known.remove(&id);
                Change::End { job: format!("{JOB_PREFIX}{id}"), end: End::Failed("Flux stopped".into()) }
            })
            .collect()
    }
}

/// Stop a file that's arriving.
pub fn cancel(socket: &std::path::Path, job: &str) -> Result<(), Error> {
    let id = job.strip_prefix(JOB_PREFIX).unwrap_or(job);
    Client::at(socket).cancel_transfer(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_connect::Transfer;

    fn t(id: &str, dir: &str, state: &str, done: i64, time: i64) -> Transfer {
        Transfer {
            id: id.into(),
            device: "phone1".into(),
            device_name: "Pandora".into(),
            name: format!("{id}.jpg"),
            path: if state == "done" { format!("/home/u/Downloads/{id}.jpg") } else { String::new() },
            size: 10,
            done,
            dir: dir.into(),
            state: state.into(),
            time,
            ..Default::default()
        }
    }

    fn snap(transfers: Vec<Transfer>) -> Snapshot {
        Snapshot { transfers, ..Default::default() }
    }

    #[test]
    fn catches_up_then_follows() {
        let mut f = Follower::new(100);
        let first = f.update(&snap(vec![
            t("old", "in", "done", 10, 90),
            t("missed", "in", "done", 10, 120),
            t("out", "out", "active", 5, 130),
            t("live", "in", "active", 4, 140),
        ]));
        let jobs: Vec<&Change> = first.iter().collect();
        assert!(!jobs.iter().any(|c| matches!(c, Change::Begin { job, .. } if job == "flux-in:old")), "already listed");
        assert!(!jobs.iter().any(|c| matches!(c, Change::Begin { job, .. } if job == "flux-in:out")), "sent, not received");
        assert!(first.contains(&Change::Saved { job: "flux-in:missed".into(), path: "/home/u/Downloads/missed.jpg".into() }));
        assert!(first.contains(&Change::End { job: "flux-in:missed".into(), end: End::Completed }));
        assert!(first.contains(&Change::Progress { job: "flux-in:live".into(), done: 4 }));
        assert_eq!(f.seen_until(), 120);

        let again = f.update(&snap(vec![t("missed", "in", "done", 10, 120), t("live", "in", "active", 4, 140)]));
        assert!(again.is_empty(), "nothing changed");
        let done = f.update(&snap(vec![t("live", "in", "done", 10, 140)]));
        assert_eq!(done.len(), 2);
        assert_eq!(f.seen_until(), 140);
        assert!(Follower::new(140).update(&snap(vec![t("live", "in", "done", 10, 140)])).is_empty(), "the next start skips it");
    }

    #[test]
    fn failures_and_a_lost_fluxd_end_jobs() {
        let mut f = Follower::new(0);
        let mut bad = t("bad", "in", "failed", 3, 5);
        bad.error = "The phone went away".into();
        let changes = f.update(&snap(vec![bad, t("going", "in", "active", 1, 6)]));
        assert!(changes.contains(&Change::End { job: "flux-in:bad".into(), end: End::Failed("The phone went away".into()) }));
        assert_eq!(f.lost(), vec![Change::End { job: "flux-in:going".into(), end: End::Failed("Flux stopped".into()) }]);
        assert!(f.lost().is_empty());
    }
}
