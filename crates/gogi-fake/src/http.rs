//! A small HTTP/1.1 server: one thread per connection, `Connection: close`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::api::{self, Resp};
use crate::state::Store;

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
        204 => "204 No Content",
        400 => "400 Bad Request",
        404 => "404 Not Found",
        413 => "413 Payload Too Large",
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
    let mut len = 0usize;
    loop {
        line.clear();
        r.read_line(&mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let path = target.split('?').next().unwrap_or("");
    let resp = api::handle(&mut store.lock().unwrap(), &method, path, &body);
    match resp {
        Resp::Json(code, v) => {
            let b = serde_json::to_vec(&v).unwrap();
            write!(out, "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", status_line(code), b.len())?;
            out.write_all(&b)?;
        }
        Resp::Empty(code) => write!(out, "HTTP/1.1 {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", status_line(code))?,
        Resp::Sse(events) => {
            write!(out, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n")?;
            for (name, data) in events {
                write!(out, "event: {}\ndata: {}\n\n", name, data)?;
            }
        }
    }
    out.flush()
}
