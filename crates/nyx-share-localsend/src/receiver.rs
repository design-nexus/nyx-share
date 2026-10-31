//! Receiving: the HTTPS server other LocalSend devices register with and send
//! to. Every offer is put to the person at this computer first.

use crate::dto::{DeviceInfo, FileDto, PrepareUpload, PrepareUploadResponse};
use crate::http;
use crate::identity::Identity;
use nyx_share_core::names;
use nyx_share_providers::{SendError, free_bytes, save_stream};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Most files and bytes one offer may carry.
pub const MAX_FILES: usize = 1000;
/// Free space left untouched after a transfer.
const RESERVE: u64 = 512 << 20;
const MAX_CONNECTIONS: usize = 32;
/// A session with no upload for this long is over.
const SESSION_IDLE: Duration = Duration::from_secs(300);

/// What a device wants to send.
#[derive(Clone, Debug)]
pub struct Offer {
    pub session: String,
    pub sender: DeviceInfo,
    pub addr: IpAddr,
    /// The sender proved it holds the certificate it claims (mutual TLS).
    pub verified: bool,
    pub files: Vec<FileDto>,
    pub total: u64,
    /// Only text messages: shown, nothing to save.
    pub message_only: bool,
}

pub enum Decision {
    Accept,
    Decline,
}

pub enum End {
    Completed,
    Cancelled,
    Failed(String),
}

/// What the receiver needs from the service.
pub trait Host: Send + Sync {
    fn me(&self) -> DeviceInfo;
    /// A device registered with us (it may then appear as a destination).
    fn registered(&self, info: DeviceInfo, addr: IpAddr, verified: bool);
    /// Ask the person; blocks until they answer.
    fn offer(&self, offer: &Offer) -> Decision;
    /// An offer refused without asking, and why.
    fn refused(&self, offer: &Offer, reason: &str);
    fn receive_folder(&self) -> PathBuf;
    fn started(&self, offer: &Offer);
    fn progress(&self, session: &str, bytes: u64);
    fn saved(&self, session: &str, path: PathBuf);
    fn ended(&self, session: &str, end: End);
}

struct Session {
    offer: Offer,
    tokens: HashMap<String, String>,
    remaining: Vec<String>,
    folder: PathBuf,
    cancel: Arc<AtomicBool>,
    last: Instant,
    failed: Option<String>,
}

pub struct Receiver {
    pub port: u16,
    session: Mutex<Option<Session>>,
    /// Offers waiting for an answer, so a second one gets "busy".
    pending: AtomicBool,
    host: Arc<dyn Host>,
    config: Arc<rustls::ServerConfig>,
    connections: AtomicUsize,
    stop: AtomicBool,
}

impl Receiver {
    /// Listen on `port`, or another free port if it's taken (by the official
    /// app, say); the port is announced either way. Port 0 takes any.
    pub fn start(identity: &Identity, host: Arc<dyn Host>, port: u16) -> io::Result<Arc<Self>> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
            .or_else(|_| TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)))?;
        let port = listener.local_addr()?.port();
        let config = crate::tls::server(identity).map_err(io::Error::other)?;
        let me = Arc::new(Self {
            port,
            session: Mutex::new(None),
            pending: AtomicBool::new(false),
            host,
            config,
            connections: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let server = me.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if server.stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                if server.connections.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
                    server.connections.fetch_sub(1, Ordering::Relaxed);
                    continue;
                }
                let server = server.clone();
                std::thread::spawn(move || {
                    let _ = server.serve(stream);
                    server.connections.fetch_sub(1, Ordering::Relaxed);
                });
            }
        });
        let watcher = me.clone();
        std::thread::spawn(move || {
            while !watcher.stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(15));
                watcher.expire();
            }
        });
        Ok(me)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn busy(&self) -> bool {
        self.pending.load(Ordering::Relaxed) || self.lock().is_some()
    }

    /// The person stopped an incoming transfer.
    pub fn cancel(&self, session: &str) {
        let ended = {
            let mut s = self.lock();
            match s.as_ref() {
                Some(cur) if cur.offer.session == session => {
                    cur.cancel.store(true, Ordering::Relaxed);
                    s.take()
                }
                _ => None,
            }
        };
        if let Some(s) = ended {
            self.host.ended(session, s.failed.map_or(End::Cancelled, End::Failed));
        }
    }

    fn expire(&self) {
        let ended = {
            let mut s = self.lock();
            match s.as_ref() {
                Some(cur) if cur.last.elapsed() > SESSION_IDLE => s.take(),
                _ => None,
            }
        };
        if let Some(s) = ended {
            let reason = s.failed.unwrap_or_else(|| format!("{} stopped sending", s.offer.sender.alias));
            self.host.ended(&s.offer.session, End::Failed(reason));
        }
    }

    fn serve(&self, tcp: TcpStream) -> io::Result<()> {
        let peer = tcp.peer_addr()?.ip();
        tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(30)))?;
        let conn = rustls::ServerConnection::new(self.config.clone()).map_err(io::Error::other)?;
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        // Finish the handshake to learn the client's certificate.
        while tls.conn.is_handshaking() {
            tls.conn.complete_io(&mut tls.sock)?;
        }
        let client = crate::tls::peer_fingerprint(&tls.conn);
        let mut reader = BufReader::new(&mut tls);
        let head = http::read_request(&mut reader)?;
        let (status, json) = self.route(&head, &mut reader, peer, client.as_deref());
        drop(reader);
        http::respond(&mut tls, status, json.as_deref())?;
        tls.conn.send_close_notify();
        let _ = tls.flush();
        Ok(())
    }

    fn me_json(&self) -> String {
        let mut me = self.host.me();
        me.announce = None;
        serde_json::to_string(&me).unwrap_or_default()
    }

    fn route(
        &self,
        head: &http::Head,
        reader: &mut impl io::BufRead,
        peer: IpAddr,
        client: Option<&str>,
    ) -> (u16, Option<String>) {
        let path = head.path.trim_end_matches('/');
        match (head.method.as_str(), path) {
            ("GET", "/api/localsend/v2/info") => (200, Some(self.me_json())),
            ("POST", "/api/localsend/v2/register") => {
                let Ok(body) = http::read_limited(http::body(reader, head), 64 << 10) else { return (400, None) };
                let Ok(info) = serde_json::from_slice::<DeviceInfo>(&body) else { return (400, None) };
                let verified = client.is_some_and(|c| c.eq_ignore_ascii_case(&info.fingerprint));
                if info.fingerprint != self.host.me().fingerprint {
                    self.host.registered(info, peer, verified);
                }
                (200, Some(self.me_json()))
            }
            ("POST", "/api/localsend/v2/prepare-upload") => self.prepare(head, reader, peer, client),
            ("POST", "/api/localsend/v2/upload") => (self.upload(head, reader, peer), None),
            ("POST", "/api/localsend/v2/cancel") => {
                let session = head.query.get("sessionId").cloned().unwrap_or_default();
                let ours = self.lock().as_ref().is_some_and(|s| s.offer.session == session && s.offer.addr == peer);
                if ours {
                    self.cancel(&session);
                }
                (200, None)
            }
            _ => (404, None),
        }
    }

    fn prepare(
        &self,
        head: &http::Head,
        reader: &mut impl io::BufRead,
        peer: IpAddr,
        client: Option<&str>,
    ) -> (u16, Option<String>) {
        let Ok(body) = http::read_limited(http::body(reader, head), 16 << 20) else { return (400, None) };
        let Ok(request) = serde_json::from_slice::<PrepareUpload>(&body) else { return (400, None) };
        if request.files.is_empty() || request.files.len() > MAX_FILES {
            return (400, None);
        }
        if self.busy() || self.pending.swap(true, Ordering::Relaxed) {
            return (409, None);
        }
        let result = self.decide(request, peer, client);
        self.pending.store(false, Ordering::Relaxed);
        result
    }

    fn decide(&self, request: PrepareUpload, peer: IpAddr, client: Option<&str>) -> (u16, Option<String>) {
        let mut files: Vec<FileDto> = request.files.into_values().collect();
        files.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        for f in &mut files {
            f.file_name = names::sanitize(&f.file_name);
        }
        let total = files.iter().map(|f| f.size).sum();
        let message_only = files.iter().all(|f| f.file_type.starts_with("text/") && f.preview.is_some());
        let verified = client.is_some_and(|c| c.eq_ignore_ascii_case(&request.info.fingerprint));
        let offer = Offer {
            session: uuid::Uuid::new_v4().to_string(),
            sender: request.info,
            addr: peer,
            verified,
            files,
            total,
            message_only,
        };
        let folder = self.host.receive_folder();
        if !message_only {
            let free = free_bytes(&folder).unwrap_or(0);
            if free < total.saturating_add(RESERVE) {
                let reason =
                    format!("There isn't enough free space for {} in {}", nyx_share_core::units::size(total), folder.display());
                self.host.refused(&offer, &reason);
                return (403, None);
            }
        }
        if let Decision::Decline = self.host.offer(&offer) {
            return (403, None);
        }
        if message_only {
            return (204, None);
        }
        let tokens: HashMap<String, String> =
            offer.files.iter().map(|f| (f.id.clone(), uuid::Uuid::new_v4().simple().to_string())).collect();
        let response = PrepareUploadResponse { session_id: offer.session.clone(), files: tokens.clone() };
        self.host.started(&offer);
        *self.lock() = Some(Session {
            remaining: offer.files.iter().map(|f| f.id.clone()).collect(),
            offer,
            tokens,
            folder,
            cancel: Arc::new(AtomicBool::new(false)),
            last: Instant::now(),
            failed: None,
        });
        (200, serde_json::to_string(&response).ok())
    }

    fn upload(&self, head: &http::Head, reader: &mut impl io::BufRead, peer: IpAddr) -> u16 {
        let q = |k: &str| head.query.get(k).cloned().unwrap_or_default();
        let (session_id, file_id, token) = (q("sessionId"), q("fileId"), q("token"));
        let (file, folder, cancel) = {
            let mut s = self.lock();
            let Some(session) = s.as_mut() else { return 409 };
            if session.offer.session != session_id || session.offer.addr != peer {
                return 403;
            }
            if session.tokens.get(&file_id) != Some(&token) || !session.remaining.contains(&file_id) {
                return 403;
            }
            session.last = Instant::now();
            let file = session.offer.files.iter().find(|f| f.id == file_id).cloned().expect("listed");
            (file, session.folder.clone(), session.cancel.clone())
        };
        if head.content_length().is_some_and(|n| n != file.size) {
            return 400;
        }
        let mut hasher = file.sha256.as_ref().map(|_| Sha256::new());
        let mut body = Hashing { inner: http::body(reader, head), hasher: hasher.as_mut() };
        let progress = |n: u64| self.host.progress(&session_id, n);
        let saved = save_stream(&folder, &file.file_name, &mut body, file.size, &cancel, &progress);
        let mut status = 200;
        let mut finished = None;
        {
            let mut s = self.lock();
            let Some(session) = s.as_mut().filter(|s| s.offer.session == session_id) else {
                if let Ok(path) = saved {
                    self.host.saved(&session_id, path);
                }
                return 403;
            };
            session.last = Instant::now();
            match saved {
                Ok(path) => {
                    let expected = file.sha256.as_deref().map(str::to_ascii_lowercase);
                    let actual = hasher.map(|h| h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>());
                    if expected.is_some() && expected != actual {
                        let _ = std::fs::remove_file(&path);
                        session.failed = Some(format!("{} arrived damaged", file.file_name));
                        status = 422;
                    } else {
                        session.remaining.retain(|id| *id != file_id);
                        self.host.saved(&session_id, path);
                    }
                }
                Err(SendError::Cancelled { .. }) => status = 403,
                Err(SendError::Failed { message, .. }) => {
                    session.failed = Some(format!("{}: {message}", file.file_name));
                    status = 500;
                }
            }
            if session.remaining.is_empty() {
                finished = s.take();
            }
        }
        if let Some(done) = finished {
            let end = match done.failed {
                Some(reason) => End::Failed(reason),
                None => End::Completed,
            };
            self.host.ended(&session_id, end);
        }
        status
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Hashes what passes through, when the sender gave a checksum.
struct Hashing<'a, R: Read> {
    inner: R,
    hasher: Option<&'a mut Sha256>,
}

impl<R: Read> Read for Hashing<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if let Some(h) = self.hasher.as_mut() {
            h.update(&buf[..n]);
        }
        Ok(n)
    }
}
