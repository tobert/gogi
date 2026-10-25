//! A small HTTP/1.1 server: one thread per connection, `Connection: close`.
//!
//! Every response carries a `request-id` header, and every error body and `error` event carries
//! the same id as `request_id`. A `traceparent` header is accepted and ignored.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gogi_proto::generation::StreamEvent;
use gogi_proto::ids::REQUEST_ID_HEADER;
use serde_json::json;

use crate::api::{self, Resp};
use crate::state::Store;

/// A new request id: the process's start time and a count, so ids differ across requests and
/// across restarts.
fn request_id() -> String {
    static START: OnceLock<u64> = OnceLock::new();
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let start = *START.get_or_init(|| SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64));
    format!("req_{:016x}_{}", start, NEXT.fetch_add(1, Ordering::SeqCst))
}

pub fn serve(listener: TcpListener, store: Arc<Mutex<Store>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((conn, _)) => {
                let store = store.clone();
                std::thread::spawn(move || {
                    let _ = connection(conn, &store);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
            Err(_) => break,
        }
    }
}

fn status_line(code: u16) -> &'static str {
    match code {
        200 => "200 OK",
        201 => "201 Created",
        204 => "204 No Content",
        206 => "206 Partial Content",
        308 => "308 Permanent Redirect",
        400 => "400 Bad Request",
        404 => "404 Not Found",
        413 => "413 Payload Too Large",
        416 => "416 Range Not Satisfiable",
        503 => "503 Service Unavailable",
        _ => "500 Internal Server Error",
    }
}

fn connection(conn: TcpStream, store: &Mutex<Store>) -> std::io::Result<()> {
    conn.set_nonblocking(false)?;
    conn.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut out = conn.try_clone()?;
    let mut r = BufReader::new(conn);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut it = line.split_whitespace();
    let (method, target) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
    let mut len = None;
    let mut bad_len = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        line.clear();
        r.read_line(&mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                match v.trim().parse::<u64>() {
                    Ok(n) => len = Some(n),
                    Err(_) => bad_len = Some(v.trim().to_string()),
                }
            }
            // A repeated field joins with ", " to the one before it (RFC 9110 section 5.3).
            let name = k.trim().to_ascii_lowercase();
            match headers.iter_mut().find(|(n, _)| *n == name) {
                Some((_, value)) => {
                    value.push_str(", ");
                    value.push_str(v.trim());
                }
                None => headers.push((name, v.trim().to_string())),
            }
        }
    }
    let path = target.split('?').next().unwrap_or("");
    let limit = store.lock().unwrap().body_limit();
    let resp = match (bad_len, len) {
        (Some(v), _) => api::refuse_header("content-length", format!("nothing ran: content-length {:?} is not a number of bytes", v)),
        (None, Some(n)) if n > limit => api::refuse_size(n, limit),
        (None, n) => {
            let mut body = vec![0u8; n.unwrap_or(0) as usize];
            match r.read_exact(&mut body) {
                Ok(()) => api::handle(&mut store.lock().unwrap(), &method, path, &headers, &body),
                Err(e) => api::refuse_header("content-length", format!("nothing ran: the body ended before content-length {} bytes: {}", n.unwrap_or(0), e)),
            }
        }
    };
    let id = request_id();
    match resp {
        Resp::Json(code, mut v) => {
            if v.get("error").is_some() {
                v["request_id"] = json!(id);
            }
            let b = serde_json::to_vec(&v).unwrap();
            write!(out, "HTTP/1.1 {}\r\n{}: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", status_line(code), REQUEST_ID_HEADER, id, b.len())?;
            if method != "HEAD" {
                out.write_all(&b)?;
            }
        }
        Resp::Bytes { code, mime, body, headers } => {
            write!(out, "HTTP/1.1 {}\r\n{}: {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n", status_line(code), REQUEST_ID_HEADER, id, mime, body.len())?;
            for (k, v) in headers {
                write!(out, "{}: {}\r\n", k, v)?;
            }
            write!(out, "Connection: close\r\n\r\n")?;
            if method != "HEAD" {
                out.write_all(&body)?;
            }
        }
        Resp::Redirect(to) => write!(out, "HTTP/1.1 308 Permanent Redirect\r\n{}: {}\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", REQUEST_ID_HEADER, id, to)?,
        Resp::Empty(code) => write!(out, "HTTP/1.1 {}\r\n{}: {}\r\nConnection: close\r\n\r\n", status_line(code), REQUEST_ID_HEADER, id)?,
        Resp::Sse(events) => {
            write!(out, "HTTP/1.1 200 OK\r\n{}: {}\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", REQUEST_ID_HEADER, id)?;
            for e in events {
                let e = match e {
                    StreamEvent::Error(b) => StreamEvent::Error(b.with_request_id(id.clone())),
                    other => other,
                };
                write!(out, "event: {}\ndata: {}\n\n", e.name(), e.data())?;
            }
        }
    }
    out.flush()
}
