//! Just enough HTTP/1.1 for LocalSend: JSON requests and streamed uploads, one
//! request per connection. Bodies are read by Content-Length or chunked.

use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};

const MAX_HEAD: usize = 32 * 1024;

#[derive(Debug, Default)]
pub struct Head {
    /// Request method, or empty for a response.
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub status: u16,
    headers: Vec<(String, String)>,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length").and_then(|v| v.trim().parse().ok())
    }

    fn chunked(&self) -> bool {
        self.header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn read_line(r: &mut impl BufRead, budget: &mut usize) -> io::Result<String> {
    let mut line = Vec::new();
    let n = r.take(*budget as u64).read_until(b'\n', &mut line)?;
    if n == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    *budget = budget.saturating_sub(n);
    if !line.ends_with(b"\n") {
        return Err(bad("header too long"));
    }
    let s = String::from_utf8(line).map_err(|_| bad("header isn't UTF-8"))?;
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(()) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_headers(r: &mut impl BufRead, budget: &mut usize, head: &mut Head) -> io::Result<()> {
    loop {
        let line = read_line(r, budget)?;
        if line.is_empty() {
            return Ok(());
        }
        let (k, v) = line.split_once(':').ok_or_else(|| bad("malformed header"))?;
        if head.headers.len() >= 100 {
            return Err(bad("too many headers"));
        }
        head.headers.push((k.trim().to_string(), v.trim().to_string()));
    }
}

pub fn read_request(r: &mut impl BufRead) -> io::Result<Head> {
    let mut budget = MAX_HEAD;
    let line = read_line(r, &mut budget)?;
    let mut parts = line.split(' ');
    let (method, target) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    if method.is_empty() || !target.starts_with('/') {
        return Err(bad("malformed request line"));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    let mut head = Head { method: method.to_string(), path: path.to_string(), query, ..Default::default() };
    read_headers(r, &mut budget, &mut head)?;
    Ok(head)
}

pub fn read_response_head(r: &mut impl BufRead) -> io::Result<Head> {
    let mut budget = MAX_HEAD;
    let line = read_line(r, &mut budget)?;
    let status = line.split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| bad("malformed status line"))?;
    let mut head = Head { status, ..Default::default() };
    read_headers(r, &mut budget, &mut head)?;
    Ok(head)
}

/// The body of a message with this head. Responses without a length run to the
/// end of the connection; requests without one have no body.
pub fn body<'a, R: BufRead + 'a>(r: &'a mut R, head: &Head) -> Box<dyn Read + 'a> {
    if head.chunked() {
        Box::new(Chunked { inner: r, left: 0, done: false })
    } else if let Some(n) = head.content_length() {
        Box::new(r.take(n))
    } else if head.method.is_empty() && head.status != 204 {
        Box::new(r)
    } else {
        Box::new(io::empty())
    }
}

/// Read a whole body, refusing more than `limit` bytes.
pub fn read_limited(mut body: impl Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    body.by_ref().take(limit + 1).read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        return Err(bad("body too large"));
    }
    Ok(out)
}

struct Chunked<'a, R: BufRead> {
    inner: &'a mut R,
    left: u64,
    done: bool,
}

impl<R: BufRead> Read for Chunked<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut budget = 1024;
            let line = read_line(self.inner, &mut budget)?;
            let size = line.split(';').next().unwrap_or_default().trim();
            self.left = u64::from_str_radix(size, 16).map_err(|_| bad("malformed chunk size"))?;
            if self.left == 0 {
                // Trailers, then the blank line.
                let mut budget = MAX_HEAD;
                while !read_line(self.inner, &mut budget)?.is_empty() {}
                self.done = true;
                return Ok(0);
            }
        }
        let want = buf.len().min(self.left as usize);
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= n as u64;
        if self.left == 0 {
            let mut crlf = [0u8; 2];
            self.inner.read_exact(&mut crlf)?;
        }
        Ok(n)
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    }
}

pub fn respond(w: &mut impl Write, status: u16, json: Option<&str>) -> io::Result<()> {
    let body = json.unwrap_or("");
    let kind = if json.is_some() { "Content-Type: application/json\r\n" } else { "" };
    write!(w, "HTTP/1.1 {status} {}\r\n{kind}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", reason(status), body.len())?;
    w.flush()
}

/// A POST with a body of known length, streamed from `body`.
pub struct Post<'a> {
    pub target: String,
    pub content_type: &'static str,
    pub length: u64,
    pub body: Box<dyn Read + Send + 'a>,
}

pub fn send_request(
    w: &mut impl Write,
    host: &str,
    post: &mut Post<'_>,
    progress: &mut dyn FnMut(u64) -> io::Result<()>,
) -> io::Result<()> {
    let (target, content_type, length) = (&post.target, post.content_type, post.length);
    write!(
        w,
        "POST {target} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: NyxShare\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
    )?;
    let body = &mut post.body;
    let mut buf = vec![0u8; 256 * 1024];
    let mut sent = 0u64;
    while sent < length {
        let want = buf.len().min((length - sent) as usize);
        let n = body.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the file got shorter while sending"));
        }
        w.write_all(&buf[..n])?;
        sent += n as u64;
        progress(n as u64)?;
    }
    w.flush()
}

/// Percent-encode a query value.
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn parses_requests_with_queries_and_bodies() {
        let raw = b"POST /api/localsend/v2/upload?sessionId=a%20b&fileId=1&token=t HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhelloEXTRA";
        let mut r = BufReader::new(&raw[..]);
        let head = read_request(&mut r).unwrap();
        assert_eq!(head.path, "/api/localsend/v2/upload");
        assert_eq!(head.query["sessionId"], "a b");
        assert_eq!(read_limited(body(&mut r, &head), 100).unwrap(), b"hello");
    }

    #[test]
    fn reads_chunked_bodies() {
        let raw =
            b"POST /u HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: y\r\n\r\n";
        let mut r = BufReader::new(&raw[..]);
        let head = read_request(&mut r).unwrap();
        assert_eq!(read_limited(body(&mut r, &head), 100).unwrap(), b"hello world");
    }

    #[test]
    fn refuses_oversized_heads_and_bodies() {
        let mut huge = b"GET /".to_vec();
        huge.extend(std::iter::repeat_n(b'a', 40_000));
        huge.extend(b" HTTP/1.1\r\n\r\n");
        assert!(read_request(&mut BufReader::new(&huge[..])).is_err());
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 10\r\n\r\n0123456789";
        let mut r = BufReader::new(&raw[..]);
        let head = read_request(&mut r).unwrap();
        assert!(read_limited(body(&mut r, &head), 4).is_err());
        assert!(read_request(&mut BufReader::new(&b"nonsense\r\n\r\n"[..])).is_err());
    }

    #[test]
    fn round_trips_a_response() {
        let mut out = Vec::new();
        respond(&mut out, 200, Some(r#"{"a":1}"#)).unwrap();
        let mut r = BufReader::new(&out[..]);
        let head = read_response_head(&mut r).unwrap();
        assert_eq!(head.status, 200);
        assert_eq!(read_limited(body(&mut r, &head), 100).unwrap(), br#"{"a":1}"#);
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
    }
}
