//! A blocking JSON client.

use std::io::Read;
use std::time::Duration;

use serde_json::Value;

pub struct Http {
    base: String,
    agent: ureq::Agent,
}

pub struct Reply {
    pub status: u16,
    pub content_type: String,
    pub text: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }
}

impl Http {
    pub fn new(base: &str) -> Http {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(600)).build();
        Http { base: base.trim_end_matches('/').to_string(), agent }
    }

    pub fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply, String> {
        let req = self.agent.request(method, &format!("{}{}", self.base, path));
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
        let mut text = String::new();
        resp.into_reader().read_to_string(&mut text).map_err(|e| format!("{} {}: reading the reply: {}", method, path, e))?;
        Ok(Reply { status, content_type, text })
    }

    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value), String> {
        self.raw(method, path, body).map(|r| (r.status, r.json()))
    }
}

/// The events of a server-sent event stream: the `event:` name and the `data:` JSON.
pub fn sse_events(text: &str) -> Vec<(String, Value)> {
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
            out.push((name, serde_json::from_str(&data).unwrap_or(Value::String(data))));
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
        assert_eq!(e[0].1["n"], 1);
    }
}
