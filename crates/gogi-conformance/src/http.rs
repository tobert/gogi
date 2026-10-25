//! A blocking JSON client.

use std::io::Read;
use std::time::Duration;

use gogi_proto::ids::REQUEST_ID_HEADER;
use serde_json::Value;

pub struct Http {
    base: String,
    agent: ureq::Agent,
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    /// The `request-id` header, when the response has one.
    pub request_id: Option<String>,
    pub text: String,
}

/// A response read as bytes.
pub struct Bytes {
    pub status: u16,
    /// By lowercase name.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Bytes {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

impl Response {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }
}

impl Http {
    pub fn new(base: &str) -> Http {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(600)).build();
        Http { base: base.trim_end_matches('/').to_string(), agent }
    }

    pub fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response, String> {
        self.raw_with(method, path, body, &[])
    }

    /// Sends a request with extra headers, such as `traceparent`.
    pub fn raw_with(&self, method: &str, path: &str, body: Option<&Value>, headers: &[(&str, &str)]) -> Result<Response, String> {
        let mut req = self.agent.request(method, &format!("{}{}", self.base, path));
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let sent = match body {
            Some(b) => req.set("Content-Type", "application/json").send_string(&b.to_string()),
            None => req.call(),
        };
        let resp = match sent {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(format!("{} {}: {}", method, path, e)),
        };
        let status = resp.status();
        let content_type = resp.header("content-type").unwrap_or("").to_string();
        let request_id = resp.header(REQUEST_ID_HEADER).map(String::from);
        let mut text = String::new();
        resp.into_reader().read_to_string(&mut text).map_err(|e| format!("{} {}: reading the response: {}", method, path, e))?;
        Ok(Response { status, content_type, request_id, text })
    }

    /// Sends bytes, or nothing, with headers, and returns the status, the response headers by
    /// lowercase name, and the body's bytes.
    pub fn bytes(&self, method: &str, path: &str, body: Option<&[u8]>, headers: &[(&str, &str)]) -> Result<Bytes, String> {
        let mut req = self.agent.request(method, &format!("{}{}", self.base, path));
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let sent = match body {
            Some(b) => req.send_bytes(b),
            None => req.call(),
        };
        let resp = match sent {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(format!("{} {}: {}", method, path, e)),
        };
        let status = resp.status();
        let headers = resp.headers_names().into_iter().filter_map(|n| resp.header(&n).map(|v| (n.to_ascii_lowercase(), v.to_string()))).collect();
        let mut body = Vec::new();
        resp.into_reader().read_to_end(&mut body).map_err(|e| format!("{} {}: reading the response: {}", method, path, e))?;
        Ok(Bytes { status, headers, body })
    }

    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value), String> {
        self.raw(method, path, body).map(|r| (r.status, r.json()))
    }
}

/// The events of a server-sent event stream: the `event:` name and the `data:` text.
pub fn sse_events(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut name = String::from("message");
        let mut data = String::new();
        for line in block.lines() {
            if let Some(n) = line.strip_prefix("event:") {
                name = n.trim().to_string();
            } else if let Some(d) = line.strip_prefix("data:") {
                data.push_str(d.trim_start());
            }
        }
        if !data.is_empty() {
            out.push((name, data));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_carry_their_name_and_json() {
        let e = sse_events("event: token\ndata: {\"n\":1}\n\nevent: done\ndata: {\"content\":\"x\"}\n\n");
        assert_eq!(e.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["token", "done"]);
        assert_eq!(e[0].1, "{\"n\":1}");
    }
}
