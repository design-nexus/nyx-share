use crate::{config, notify};
use nyx_share_core::{ContentKind, Destination, Job, JobState, Preferences, arrange, kinds_of};
use nyx_share_dbus::{INTERFACE, PATH};
use nyx_share_providers::{Payload, Provider, SendError, SendOptions, Task};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zbus::fdo;

/// A request waiting in an open Share Sheet.
pub struct Request {
    pub token: String,
    pub items: Arc<Vec<Payload>>,
    pub app_name: Option<String>,
    pub sandboxed: bool,
    pub created: Instant,
    pub jobs: Vec<String>,
}

struct Entry {
    job: Job,
    cancel: Arc<AtomicBool>,
    /// Notify when it finishes (false while a Share Sheet shows it).
    notify: bool,
}

pub struct State {
    providers: Vec<Box<dyn Provider>>,
    prefs: Mutex<Preferences>,
    pub requests: Mutex<HashMap<String, Request>>,
    jobs: Mutex<Vec<Entry>>,
    bus: OnceLock<zbus::Connection>,
    last_activity: Mutex<Instant>,
}

/// Finished jobs kept for ListJobs.
const HISTORY: usize = 50;
/// An unanswered Share Sheet request is dropped after this long.
const REQUEST_LIFETIME: Duration = Duration::from_secs(30 * 60);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

impl State {
    pub fn new(providers: Vec<Box<dyn Provider>>, prefs: Preferences) -> Self {
        Self {
            providers,
            prefs: Mutex::new(prefs),
            requests: Mutex::new(HashMap::new()),
            jobs: Mutex::new(Vec::new()),
            bus: OnceLock::new(),
            last_activity: Mutex::new(Instant::now()),
        }
    }

    pub fn attach(&self, conn: zbus::Connection) {
        let _ = self.bus.set(conn);
    }

    pub fn touch(&self) {
        *lock(&self.last_activity) = Instant::now();
    }

    pub fn idle_for(&self) -> Duration {
        lock(&self.last_activity).elapsed()
    }

    pub fn busy(&self) -> bool {
        !lock(&self.requests).is_empty() || lock(&self.jobs).iter().any(|e| !e.job.state.finished())
    }

    pub fn expire_requests(&self) {
        let running: Vec<String> =
            lock(&self.jobs).iter().filter(|e| !e.job.state.finished()).map(|e| e.job.id.clone()).collect();
        lock(&self.requests).retain(|_, r| r.created.elapsed() < REQUEST_LIFETIME || r.jobs.iter().any(|j| running.contains(j)));
    }

    pub fn providers(&self) -> &[Box<dyn Provider>] {
        &self.providers
    }

    /// Every destination, arranged by the user's preferences.
    pub fn destinations(&self) -> Vec<Destination> {
        let all = self.providers.iter().flat_map(|p| p.destinations()).collect();
        arrange(all, &lock(&self.prefs))
    }

    pub fn destinations_for(&self, kinds: &[ContentKind], count: usize) -> Vec<Destination> {
        self.destinations().into_iter().filter(|d| d.accepts(kinds, count)).collect()
    }

    pub fn update_prefs(&self, update: nyx_share_core::PreferencesUpdate) -> std::io::Result<()> {
        let mut prefs = lock(&self.prefs);
        prefs.merge(update);
        config::save(&prefs)
    }

    pub fn jobs(&self) -> Vec<Job> {
        lock(&self.jobs).iter().rev().map(|e| e.job.clone()).collect()
    }

    pub fn job(&self, id: &str) -> Option<Job> {
        lock(&self.jobs).iter().find(|e| e.job.id == id).map(|e| e.job.clone())
    }

    pub fn cancel(&self, id: &str) -> bool {
        let jobs = lock(&self.jobs);
        let Some(e) = jobs.iter().find(|e| e.job.id == id) else { return false };
        e.cancel.store(true, Ordering::Relaxed);
        true
    }

    /// Stop holding back notifications for these jobs: their sheet closed.
    pub fn release(&self, ids: &[String]) {
        for e in lock(&self.jobs).iter_mut().filter(|e| ids.contains(&e.job.id)) {
            e.notify = true;
        }
    }

    /// Check a destination can take `items` and start sending on a worker thread.
    pub fn start(
        self: &Arc<Self>,
        destination: &str,
        items: Arc<Vec<Payload>>,
        options: SendOptions,
        notify: bool,
    ) -> fdo::Result<String> {
        let (index, dest) = self
            .providers
            .iter()
            .enumerate()
            .find_map(|(i, p)| p.destinations().into_iter().find(|d| d.id == destination).map(|d| (i, d)))
            .ok_or_else(|| fdo::Error::UnknownObject(format!("There is no destination called {destination}")))?;
        if !dest.available {
            return Err(fdo::Error::Failed(dest.status.unwrap_or_else(|| format!("{} isn't available", dest.name))));
        }
        let kinds = kinds_of(items.iter().map(|p| &p.info.kind));
        if !dest.accepts(&kinds, items.len()) {
            return Err(fdo::Error::InvalidArgs(format!("{} can't take these items", dest.name)));
        }
        if dest.needs_folder && options.folder.is_none() {
            return Err(fdo::Error::InvalidArgs(format!("{} needs a folder", dest.name)));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        let job = Job {
            id: id.clone(),
            destination: dest.id.clone(),
            destination_name: dest.name.clone(),
            state: JobState::Running,
            items: items.len() as u32,
            bytes_done: 0,
            bytes_total: items.iter().map(Payload::size).sum(),
            message: None,
            outputs: Vec::new(),
            created: t,
            updated: t,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut jobs = lock(&self.jobs);
            jobs.push(Entry { job: job.clone(), cancel: cancel.clone(), notify });
            let mut excess = jobs.iter().filter(|e| e.job.state.finished()).count().saturating_sub(HISTORY);
            jobs.retain(|e| {
                let old = excess > 0 && e.job.state.finished();
                excess -= old as usize;
                !old
            });
        }
        let state = self.clone();
        let dest_id = dest.id.clone();
        std::thread::spawn(move || state.run(index, &dest_id, &id, &items, &options, &cancel));
        Ok(job.id)
    }

    fn run(&self, provider: usize, destination: &str, id: &str, items: &[Payload], options: &SendOptions, cancel: &AtomicBool) {
        if let Some(job) = self.job(id) {
            self.emit(&job);
        }
        let started = Instant::now();
        let last_emit = AtomicU64::new(0);
        let progress = |bytes: u64| {
            let job = {
                let mut jobs = lock(&self.jobs);
                let Some(e) = jobs.iter_mut().find(|e| e.job.id == id) else { return };
                e.job.bytes_done += bytes;
                e.job.updated = now();
                e.job.clone()
            };
            // At most five progress signals a second.
            let ms = started.elapsed().as_millis() as u64;
            if ms.saturating_sub(last_emit.load(Ordering::Relaxed)) >= 200 {
                last_emit.store(ms, Ordering::Relaxed);
                self.emit(&job);
            }
        };
        let task = Task { items, options, cancel, progress: &progress };
        let result = self.providers[provider].send(destination, &task);
        let (job, notify) = {
            let mut jobs = lock(&self.jobs);
            let Some(e) = jobs.iter_mut().find(|e| e.job.id == id) else { return };
            match result {
                Ok(outcome) => {
                    e.job.state = JobState::Completed;
                    e.job.bytes_done = e.job.bytes_total;
                    e.job.message = Some(outcome.message);
                    e.job.outputs = outcome.outputs;
                }
                Err(SendError::Cancelled { outputs }) => {
                    e.job.state = JobState::Cancelled;
                    e.job.message = Some("Cancelled".into());
                    e.job.outputs = outputs;
                }
                Err(SendError::Failed { message, outputs }) => {
                    e.job.state = JobState::Failed;
                    e.job.message = Some(message);
                    e.job.outputs = outputs;
                }
            }
            e.job.updated = now();
            (e.job.clone(), e.notify)
        };
        self.emit(&job);
        if notify && job.state != JobState::Cancelled {
            self.notify(&job);
        }
        self.touch();
    }

    fn emit(&self, job: &Job) {
        let Some(conn) = self.bus.get() else { return };
        let json = serde_json::to_string(job).unwrap_or_default();
        let _ =
            async_io::block_on(conn.emit_signal(None::<&str>, PATH, INTERFACE, "JobChanged", &(job.id.as_str(), json.as_str())));
    }

    fn notify(&self, job: &Job) {
        let Some(conn) = self.bus.get() else { return };
        let message = job.message.as_deref().unwrap_or_default();
        match job.state {
            JobState::Completed => notify::send(conn, message, ""),
            _ => notify::send(conn, &format!("Couldn't share with {}", job.destination_name), message),
        }
    }
}
