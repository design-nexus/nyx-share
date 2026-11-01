use crate::{config, notify};
use nyx_share_core::{ContentKind, Destination, Direction, Job, JobState, Preferences, arrange, kinds_of, summarize};
use nyx_share_dbus::{INTERFACE, PATH};
use nyx_share_localsend::{Nearby, Offer};
use nyx_share_providers::{Payload, Provider, SendError, SendOptions, Task};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, RwLock};
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

/// What became of an incoming offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    Pending,
    Accepted,
    Declined,
    Expired,
}

/// An offer from a nearby device, shown in a Share Sheet. Keyed by session id,
/// which is also the id of its receive job.
pub struct Incoming {
    pub token: String,
    pub offer: Offer,
    pub answer: Answer,
    /// The sheet showing it has closed.
    pub dismissed: bool,
    pub folder: String,
}

struct Entry {
    job: Job,
    cancel: Arc<AtomicBool>,
    /// Notify when it finishes (false while a Share Sheet shows it).
    notify: bool,
    /// ...but only if it took at least this long.
    quiet_for: Duration,
    started: Instant,
    last_emit: Instant,
}

pub struct State {
    providers: RwLock<Vec<Arc<dyn Provider>>>,
    prefs: Mutex<Preferences>,
    pub requests: Mutex<HashMap<String, Request>>,
    pub incoming: Mutex<HashMap<String, Incoming>>,
    pub answered: Condvar,
    jobs: Mutex<Vec<Entry>>,
    bus: OnceLock<zbus::Connection>,
    nearby: Mutex<Option<Arc<Nearby>>>,
    pub nas: Arc<RwLock<Vec<nyx_share_nas::Nas>>>,
    /// Bus names keeping the service running (open Share windows).
    pub holders: Mutex<std::collections::HashSet<String>>,
    last_activity: Mutex<Instant>,
    /// Slow calls still being answered, such as TestNas.
    calls: AtomicUsize,
    /// What failed jobs need to run again, for a while: their items stay open.
    retries: Mutex<HashMap<String, Retry>>,
}

struct Retry {
    destination: String,
    items: Arc<Vec<Payload>>,
    options: SendOptions,
    notify: bool,
    at: Instant,
}

/// Keeps the service from exiting idle while a slow call runs.
pub struct Call<'a>(&'a State);

impl Drop for Call<'_> {
    fn drop(&mut self) {
        self.0.calls.fetch_sub(1, Ordering::Relaxed);
        self.0.touch();
    }
}

/// Failed jobs keep their items this long, so they can be retried.
const RETRY_FOR: Duration = Duration::from_secs(30 * 60);
const RETRY_MAX: usize = 20;
/// An unanswered Share Sheet request is dropped after this long.
const REQUEST_LIFETIME: Duration = Duration::from_secs(30 * 60);

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Finished jobs survive the service exiting: `$XDG_STATE_HOME/nyx-share/history.json`.
fn history_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    base.join("nyx-share/history.json")
}

fn load_history() -> Vec<Job> {
    std::fs::read_to_string(history_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Job>>(&s).ok())
        .map(|jobs| jobs.into_iter().filter(|j| j.state.finished()).take(500).collect())
        .unwrap_or_default()
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

impl State {
    pub fn new(providers: Vec<Arc<dyn Provider>>, prefs: Preferences, nas: Arc<RwLock<Vec<nyx_share_nas::Nas>>>) -> Self {
        Self {
            providers: RwLock::new(providers),
            prefs: Mutex::new(prefs),
            requests: Mutex::new(HashMap::new()),
            incoming: Mutex::new(HashMap::new()),
            answered: Condvar::new(),
            jobs: Mutex::new(
                load_history()
                    .into_iter()
                    .rev()
                    .map(|job| Entry {
                        job,
                        cancel: Arc::new(AtomicBool::new(false)),
                        notify: false,
                        quiet_for: Duration::ZERO,
                        started: Instant::now(),
                        last_emit: Instant::now(),
                    })
                    .collect(),
            ),
            bus: OnceLock::new(),
            nearby: Mutex::new(None),
            nas,
            holders: Mutex::new(std::collections::HashSet::new()),
            last_activity: Mutex::new(Instant::now()),
            calls: AtomicUsize::new(0),
            retries: Mutex::new(HashMap::new()),
        }
    }

    pub fn attach(&self, conn: zbus::Connection) {
        let _ = self.bus.set(conn);
    }

    pub fn add_provider(&self, provider: Arc<dyn Provider>) {
        self.providers.write().unwrap_or_else(|e| e.into_inner()).push(provider);
    }

    pub fn remove_provider(&self, id: &str) {
        self.providers.write().unwrap_or_else(|e| e.into_inner()).retain(|p| p.id() != id);
    }

    pub fn nearby(&self) -> Option<Arc<Nearby>> {
        lock(&self.nearby).clone()
    }

    pub fn set_nearby(&self, nearby: Option<Arc<Nearby>>) {
        *lock(&self.nearby) = nearby;
    }

    pub fn touch(&self) {
        *lock(&self.last_activity) = Instant::now();
    }

    pub fn idle_for(&self) -> Duration {
        lock(&self.last_activity).elapsed()
    }

    pub fn call(&self) -> Call<'_> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.touch();
        Call(self)
    }

    pub fn busy(&self) -> bool {
        self.calls.load(Ordering::Relaxed) > 0
            || !lock(&self.requests).is_empty()
            || lock(&self.incoming).values().any(|i| !i.dismissed || i.answer == Answer::Pending)
            || lock(&self.jobs).iter().any(|e| !e.job.state.finished())
            || self.nearby().is_some_and(|n| n.busy())
            || !lock(&self.holders).is_empty()
    }

    pub fn expire_requests(&self) {
        let running: Vec<String> =
            lock(&self.jobs).iter().filter(|e| !e.job.state.finished()).map(|e| e.job.id.clone()).collect();
        lock(&self.requests).retain(|_, r| r.created.elapsed() < REQUEST_LIFETIME || r.jobs.iter().any(|j| running.contains(j)));
        lock(&self.incoming).retain(|id, i| !i.dismissed || running.contains(id));
    }

    pub fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.providers.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Every destination, arranged by the user's preferences.
    pub fn destinations(&self) -> Vec<Destination> {
        let all = self.providers().iter().flat_map(|p| p.destinations()).collect();
        arrange(all, &lock(&self.prefs))
    }

    pub fn destinations_for(&self, kinds: &[ContentKind], count: usize) -> Vec<Destination> {
        self.destinations().into_iter().filter(|d| d.accepts(kinds, count)).collect()
    }

    pub fn preferences(&self) -> Preferences {
        lock(&self.prefs).clone()
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
        let receiving = {
            let jobs = lock(&self.jobs);
            let Some(e) = jobs.iter().find(|e| e.job.id == id) else { return false };
            e.cancel.store(true, Ordering::Relaxed);
            e.job.direction == Direction::Receive
        };
        if receiving && let Some(n) = self.nearby() {
            n.cancel_incoming(id);
        }
        true
    }

    /// Stop holding back notifications for these jobs: their sheet closed.
    pub fn release(&self, ids: &[String]) {
        for e in lock(&self.jobs).iter_mut().filter(|e| ids.contains(&e.job.id)) {
            e.notify = true;
        }
    }

    fn add(&self, job: Job, cancel: Arc<AtomicBool>, notify: bool, quiet_for: Duration) {
        let mut jobs = lock(&self.jobs);
        let now = Instant::now();
        jobs.push(Entry { job, cancel, notify, quiet_for, started: now, last_emit: now });
        let keep = lock(&self.prefs).history_limit();
        let mut excess = jobs.iter().filter(|e| e.job.state.finished()).count().saturating_sub(keep);
        jobs.retain(|e| {
            let old = excess > 0 && e.job.state.finished();
            excess -= old as usize;
            !old
        });
    }

    fn save_history(&self) {
        let keep = lock(&self.prefs).history_limit();
        let finished: Vec<Job> =
            lock(&self.jobs).iter().rev().filter(|e| e.job.state.finished()).map(|e| e.job.clone()).take(keep).collect();
        let path = history_path();
        let Some(dir) = path.parent() else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, serde_json::to_vec(&finished).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }

    /// Change a job; signal it now if `force`, else at most five times a second.
    fn update(&self, id: &str, force: bool, change: impl FnOnce(&mut Job)) -> Option<(Job, bool)> {
        let (job, notify, emit) = {
            let mut jobs = lock(&self.jobs);
            let e = jobs.iter_mut().find(|e| e.job.id == id)?;
            change(&mut e.job);
            e.job.updated = now();
            let emit = force || e.last_emit.elapsed() >= Duration::from_millis(200);
            if emit {
                e.last_emit = Instant::now();
            }
            (e.job.clone(), e.notify && e.started.elapsed() >= e.quiet_for, emit)
        };
        if emit {
            self.emit(&job);
        }
        Some((job, notify))
    }

    /// Check a destination can take `items` and start sending on a worker thread.
    pub fn start(
        self: &Arc<Self>,
        destination: &str,
        items: Arc<Vec<Payload>>,
        options: SendOptions,
        notify: bool,
        quiet_for: Duration,
    ) -> fdo::Result<String> {
        let disabled = lock(&self.prefs).disabled.clone();
        if let Some(p) = self
            .providers()
            .into_iter()
            .find(|p| disabled.iter().any(|d| d == p.id()) && p.destinations().iter().any(|d| d.id == destination))
        {
            return Err(fdo::Error::Failed(format!("{} is turned off in Settings → Sharing", p.name())));
        }
        let (provider, dest) = self
            .providers()
            .into_iter()
            .find_map(|p| p.destinations().into_iter().find(|d| d.id == destination).map(|d| (p.clone(), d)))
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
            direction: Direction::Send,
            state: JobState::Running,
            items: items.len() as u32,
            summary: summarize(items.iter().map(|p| match p.info.kind {
                ContentKind::Text | ContentKind::Url => {
                    p.info.preview.as_deref().and_then(|t| t.lines().next()).unwrap_or("Text")
                }
                _ => p.info.name.as_str(),
            })),
            bytes_done: 0,
            step: None,
            retryable: false,
            bytes_total: items.iter().map(Payload::size).sum(),
            message: None,
            outputs: Vec::new(),
            created: t,
            updated: t,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.add(job, cancel.clone(), notify, quiet_for);
        let state = self.clone();
        let (dest_id, job_id) = (dest.id.clone(), id.clone());
        std::thread::spawn(move || {
            state.run(provider, &dest_id, &job_id, &items, &options, &cancel);
            if state.job(&job_id).is_some_and(|j| j.state == JobState::Failed) {
                let mut retries = lock(&state.retries);
                retries.retain(|_, r| r.at.elapsed() < RETRY_FOR);
                while retries.len() >= RETRY_MAX {
                    let Some(oldest) = retries.iter().min_by_key(|(_, r)| r.at).map(|(id, _)| id.clone()) else { break };
                    retries.remove(&oldest);
                }
                retries.insert(job_id, Retry { destination: dest_id, items, options, notify, at: Instant::now() });
            }
        });
        Ok(id)
    }

    /// Run a failed job again, as a new job, while its items are still kept.
    pub fn retry(self: &Arc<Self>, id: &str) -> fdo::Result<String> {
        let retry = {
            let mut retries = lock(&self.retries);
            retries.retain(|_, r| r.at.elapsed() < RETRY_FOR);
            retries.remove(id)
        };
        let Some(r) = retry else {
            return Err(fdo::Error::Failed("That transfer can't be retried from here any more; share it again".into()));
        };
        self.start(&r.destination, r.items, r.options, r.notify, Duration::ZERO)
    }

    /// Whether a failed job can still be retried.
    pub fn retryable(&self, id: &str) -> bool {
        lock(&self.retries).get(id).is_some_and(|r| r.at.elapsed() < RETRY_FOR)
    }

    /// Forget finished transfers (running ones stay), and their retries.
    pub fn clear_history(&self) {
        lock(&self.jobs).retain(|e| !e.job.state.finished());
        lock(&self.retries).clear();
        self.save_history();
    }

    /// Trim the history to the current limit.
    pub fn trim_history(&self) {
        let keep = lock(&self.prefs).history_limit();
        {
            let mut jobs = lock(&self.jobs);
            let mut excess = jobs.iter().filter(|e| e.job.state.finished()).count().saturating_sub(keep);
            jobs.retain(|e| {
                let old = excess > 0 && e.job.state.finished();
                excess -= old as usize;
                !old
            });
        }
        self.save_history();
    }

    fn run(
        &self,
        provider: Arc<dyn Provider>,
        destination: &str,
        id: &str,
        items: &[Payload],
        options: &SendOptions,
        cancel: &AtomicBool,
    ) {
        if let Some(job) = self.job(id) {
            self.emit(&job);
        }
        let progress = |bytes: u64| {
            self.update(id, false, |j| {
                j.bytes_done += bytes;
                j.step = None;
            });
        };
        let step = |text: Option<&str>| {
            self.update(id, true, |j| j.step = text.map(str::to_string));
        };
        let task = Task { items, options, cancel, progress: &progress, step: &step };
        let result = provider.send(destination, &task);
        let finished = self.update(id, true, |j| {
            j.step = None;
            match result {
                Ok(outcome) => {
                    j.state = JobState::Completed;
                    j.bytes_done = j.bytes_total;
                    j.message = Some(outcome.message);
                    j.outputs = outcome.outputs;
                }
                Err(SendError::Cancelled { outputs }) => {
                    j.state = JobState::Cancelled;
                    j.message = Some("Cancelled".into());
                    j.outputs = outputs;
                }
                Err(SendError::Failed { message, outputs }) => {
                    j.state = JobState::Failed;
                    j.message = Some(message);
                    j.outputs = outputs;
                }
            }
        });
        self.save_history();
        if let Some((job, true)) = finished
            && job.state != JobState::Cancelled
        {
            self.notify(&job);
        }
        self.touch();
    }

    /// A nearby device's accepted offer becomes a receive job (id = session).
    pub fn begin_receive(&self, offer: &Offer) {
        let t = now();
        let job = Job {
            id: offer.session.clone(),
            destination: format!("{}{}", nyx_share_localsend::PREFIX, offer.sender.fingerprint.to_ascii_uppercase()),
            destination_name: offer.sender.alias.clone(),
            direction: Direction::Receive,
            state: JobState::Running,
            items: offer.files.len() as u32,
            summary: summarize(offer.files.iter().map(|f| f.file_name.as_str())),
            bytes_done: 0,
            step: None,
            retryable: false,
            bytes_total: offer.total,
            message: None,
            outputs: Vec::new(),
            created: t,
            updated: t,
        };
        let watched = lock(&self.incoming).get(&offer.session).is_some_and(|i| !i.dismissed);
        self.add(job.clone(), Arc::new(AtomicBool::new(false)), !watched, Duration::ZERO);
        self.emit(&job);
        self.touch();
    }

    pub fn receive_progress(&self, session: &str, bytes: u64) {
        self.update(session, false, |j| j.bytes_done += bytes);
    }

    pub fn received(&self, session: &str, path: String) {
        self.update(session, true, |j| j.outputs.push(path));
    }

    pub fn end_receive(&self, session: &str, end: nyx_share_localsend::End) {
        use nyx_share_localsend::End;
        let finished = self.update(session, true, |j| match end {
            End::Completed => {
                j.state = JobState::Completed;
                j.bytes_done = j.bytes_total;
                let what = if j.items == 1 { "1 file".to_string() } else { format!("{} files", j.items) };
                j.message = Some(format!("Received {what} from {}", j.destination_name));
            }
            End::Cancelled => {
                j.state = JobState::Cancelled;
                j.message = Some("Cancelled".into());
            }
            End::Failed(reason) => {
                j.state = JobState::Failed;
                j.message = Some(reason);
            }
        });
        self.save_history();
        if let Some((job, true)) = finished {
            match job.state {
                JobState::Completed => {
                    let folder =
                        job.outputs.first().and_then(|p| std::path::Path::new(p).parent()).map(|p| p.display().to_string());
                    self.notify_text(
                        job.message.as_deref().unwrap_or_default(),
                        &folder.map(|f| format!("Saved in {f}")).unwrap_or_default(),
                    );
                }
                JobState::Failed => self.notify_text(
                    &format!("Couldn't receive from {}", job.destination_name),
                    job.message.as_deref().unwrap_or_default(),
                ),
                _ => {}
            }
        }
        self.touch();
    }

    fn emit(&self, job: &Job) {
        let Some(conn) = self.bus.get() else { return };
        let json = serde_json::to_string(job).unwrap_or_default();
        let _ =
            async_io::block_on(conn.emit_signal(None::<&str>, PATH, INTERFACE, "JobChanged", &(job.id.as_str(), json.as_str())));
    }

    pub fn destinations_changed(&self) {
        self.signal("DestinationsChanged");
    }

    pub fn nearby_changed(&self) {
        self.signal("NearbyChanged");
    }

    fn signal(&self, name: &str) {
        if let Some(conn) = self.bus.get() {
            let _ = async_io::block_on(conn.emit_signal(None::<&str>, PATH, INTERFACE, name, &()));
        }
    }

    /// Forget holders whose connection has closed.
    pub async fn drop_departed_holders(&self, conn: &zbus::Connection) {
        let names: Vec<String> = lock(&self.holders).iter().cloned().collect();
        if names.is_empty() {
            return;
        }
        let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else { return };
        for name in names {
            let gone = match zbus::names::BusName::try_from(name.as_str()) {
                Ok(bus) => !dbus.name_has_owner(bus).await.unwrap_or(false),
                Err(_) => true,
            };
            if gone {
                lock(&self.holders).remove(&name);
                self.touch();
            }
        }
    }

    pub fn notify_text(&self, summary: &str, body: &str) {
        if let Some(conn) = self.bus.get() {
            notify::send(conn, summary, body);
        }
    }

    fn notify(&self, job: &Job) {
        let message = job.message.as_deref().unwrap_or_default();
        match job.state {
            JobState::Completed => self.notify_text(message, ""),
            _ => self.notify_text(&format!("Couldn't share with {}", job.destination_name), message),
        }
    }
}
