//! Sending to a LocalSend receiver: prepare-upload, then one upload per file.

use crate::dto::{DeviceInfo, FileDto, PrepareUpload, PrepareUploadResponse};
use crate::http;
use crate::identity::Identity;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

pub struct Target {
    pub fingerprint: String,
    pub addr: IpAddr,
    pub port: u16,
}

pub enum Source<'a> {
    Bytes(&'a [u8]),
    /// Read with positional reads: its offset may be shared.
    File(&'a File),
}

pub struct Outgoing<'a> {
    pub name: String,
    pub size: u64,
    pub mime: String,
    /// Text messages carry their text here, as LocalSend does.
    pub preview: Option<String>,
    pub source: Source<'a>,
}

#[derive(Debug, PartialEq)]
pub enum Error {
    Declined,
    Busy,
    PinRequired,
    Unreachable(String),
    Failed(String),
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declined => f.write_str("The other device declined"),
            Self::Busy => f.write_str("The other device is busy with another transfer"),
            Self::PinRequired => f.write_str("The other device asks for a PIN, which Share doesn't support yet"),
            Self::Unreachable(e) => write!(f, "Couldn't reach the other device: {e}"),
            Self::Failed(e) => f.write_str(e),
            Self::Cancelled => f.write_str("Cancelled"),
        }
    }
}

struct AtReader<'a> {
    file: &'a File,
    offset: u64,
}

impl Read for AtReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.file.read_at(buf, self.offset)?;
        self.offset += n as u64;
        Ok(n)
    }
}

struct Connection {
    tcp: TcpStream,
    tls: rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
}

fn connect(identity: &Identity, target: &Target) -> Result<Connection, Error> {
    let config = crate::tls::client(identity, &target.fingerprint).map_err(|e| Error::Failed(e.to_string()))?;
    let addr = SocketAddr::new(target.addr, target.port);
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| Error::Unreachable(e.to_string()))?;
    tcp.set_nodelay(true).ok();
    let name = rustls::pki_types::ServerName::IpAddress(target.addr.into());
    let conn = rustls::ClientConnection::new(config, name).map_err(|e| Error::Failed(e.to_string()))?;
    let clone = tcp.try_clone().map_err(|e| Error::Failed(e.to_string()))?;
    Ok(Connection { tcp: clone, tls: rustls::StreamOwned::new(conn, tcp) })
}

fn tls_error(e: &io::Error) -> Error {
    let text = e.to_string();
    if text.contains("ApplicationVerificationFailure") || text.contains("InvalidCertificate") {
        Error::Failed("The other device isn't the one that was announced (its certificate changed)".into())
    } else {
        Error::Unreachable(text)
    }
}

/// One request with a body, run on its own thread so cancellation can close
/// the connection while waiting (the receiver may take minutes to answer).
fn exchange(
    identity: &Identity,
    target: &Target,
    mut post: http::Post<'_>,
    idle: Duration,
    cancel: &AtomicBool,
    progress: &(dyn Fn(u64) + Sync),
) -> Result<(u16, Vec<u8>), Error> {
    let Connection { tcp, mut tls } = connect(identity, target)?;
    tls.sock.set_read_timeout(Some(idle)).ok();
    tls.sock.set_write_timeout(Some(Duration::from_secs(60))).ok();
    let host = format!("{}:{}", target.addr, target.port);
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let worker = scope.spawn(move || {
            let mut stopped = false;
            let sent = http::send_request(&mut tls, &host, &mut post, &mut |n| {
                if cancel.load(Ordering::Relaxed) {
                    stopped = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                progress(n);
                Ok(())
            });
            let result = sent.and_then(|()| {
                let mut reader = BufReader::new(&mut tls);
                let head = http::read_response_head(&mut reader)?;
                let body = http::read_limited(http::body(&mut reader, &head), 1 << 20)?;
                Ok((head.status, body))
            });
            let _ = tx.send(());
            (result, stopped)
        });
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) if cancel.load(Ordering::Relaxed) => {
                    let _ = tcp.shutdown(std::net::Shutdown::Both);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        let (result, stopped) = worker.join().unwrap_or((Err(io::ErrorKind::Other.into()), false));
        if stopped || cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        result.map_err(|e| tls_error(&e))
    })
}

/// Send `files` to `target`. Returns how many files the receiver took
/// (it may accept only some, or none for text it just displays).
pub fn send(
    identity: &Identity,
    me: &DeviceInfo,
    target: &Target,
    files: &[Outgoing<'_>],
    cancel: &AtomicBool,
    progress: &(dyn Fn(u64) + Sync),
) -> Result<usize, Error> {
    let ids: Vec<String> = (0..files.len()).map(|_| uuid::Uuid::new_v4().to_string()).collect();
    let dtos: HashMap<String, FileDto> = files
        .iter()
        .zip(&ids)
        .map(|(f, id)| {
            let dto = FileDto {
                id: id.clone(),
                file_name: f.name.clone(),
                size: f.size,
                file_type: f.mime.clone(),
                sha256: None,
                preview: f.preview.clone(),
            };
            (id.clone(), dto)
        })
        .collect();
    let mut info = me.clone();
    info.announce = None;
    let request = serde_json::to_vec(&PrepareUpload { info, files: dtos }).map_err(|e| Error::Failed(e.to_string()))?;
    let length = request.len() as u64;
    let quiet = |_| {};
    // Waiting for the person on the other side to accept.
    let post = http::Post {
        target: "/api/localsend/v2/prepare-upload".into(),
        content_type: "application/json",
        length,
        body: Box::new(io::Cursor::new(request)),
    };
    let (status, body) = exchange(identity, target, post, Duration::from_secs(600), cancel, &quiet)?;
    let session = match status {
        200 => serde_json::from_slice::<PrepareUploadResponse>(&body)
            .map_err(|e| Error::Failed(format!("The other device sent an unexpected answer: {e}")))?,
        204 => return Ok(0),
        401 => return Err(Error::PinRequired),
        403 => return Err(Error::Declined),
        409 => return Err(Error::Busy),
        429 => return Err(Error::Failed("The other device is receiving too many requests; try again shortly".into())),
        s => return Err(Error::Failed(format!("The other device answered {s}"))),
    };
    let mut accepted = 0;
    for (file, id) in files.iter().zip(&ids) {
        let Some(token) = session.files.get(id) else { continue };
        let path = format!(
            "/api/localsend/v2/upload?sessionId={}&fileId={}&token={}",
            http::encode(&session.session_id),
            http::encode(id),
            http::encode(token)
        );
        let body: Box<dyn Read + Send> = match file.source {
            Source::Bytes(b) => Box::new(io::Cursor::new(b)),
            Source::File(f) => Box::new(AtReader { file: f, offset: 0 }),
        };
        let post = http::Post { target: path, content_type: "application/octet-stream", length: file.size, body };
        let result = exchange(identity, target, post, Duration::from_secs(120), cancel, progress);
        match result {
            Ok((200, _)) => accepted += 1,
            Ok((s, _)) => {
                let _ = cancel_session(identity, target, &session.session_id);
                return Err(Error::Failed(match s {
                    403 => "The other device stopped the transfer".into(),
                    422 => "The file arrived damaged (checksum mismatch)".into(),
                    s => format!("The other device answered {s} during the upload"),
                }));
            }
            Err(e) => {
                let _ = cancel_session(identity, target, &session.session_id);
                return Err(e);
            }
        }
    }
    Ok(accepted)
}

/// Introduce ourselves to a device; its answer says who it is, and the pinned
/// handshake proves it holds the certificate it announced.
pub fn register(identity: &Identity, me: &DeviceInfo, target: &Target) -> Result<DeviceInfo, Error> {
    let never = AtomicBool::new(false);
    let mut info = me.clone();
    info.announce = None;
    let body = serde_json::to_vec(&info).map_err(|e| Error::Failed(e.to_string()))?;
    let length = body.len() as u64;
    let post = http::Post {
        target: "/api/localsend/v2/register".into(),
        content_type: "application/json",
        length,
        body: Box::new(io::Cursor::new(body)),
    };
    let (status, reply) = exchange(identity, target, post, Duration::from_secs(5), &never, &|_| {})?;
    if status != 200 {
        return Err(Error::Failed(format!("register answered {status}")));
    }
    let mut theirs: DeviceInfo = serde_json::from_slice(&reply).map_err(|e| Error::Failed(e.to_string()))?;
    theirs.fingerprint = target.fingerprint.to_ascii_uppercase();
    Ok(theirs)
}

fn cancel_session(identity: &Identity, target: &Target, session: &str) -> Result<(), Error> {
    let never = AtomicBool::new(false);
    let path = format!("/api/localsend/v2/cancel?sessionId={}", http::encode(session));
    let post = http::Post { target: path, content_type: "application/json", length: 0, body: Box::new(io::empty()) };
    exchange(identity, target, post, Duration::from_secs(5), &never, &|_| {}).map(|_| ())
}
