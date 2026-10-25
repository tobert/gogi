use gogi_proto::content_id;
use gogi_proto::identity::Capability;
use gogi_proto::objects::{KEEP_TTL_MAX_S, KeepsPutResult, ObjectPut};
use serde_json::json;

use crate::Suite;
use crate::http::Bytes;

/// The largest object the suite sends to check `object_bytes`.
const MAX_PROBE_BYTES: u64 = 16 << 20;

impl Suite {
    /// Sends bytes and records the response's `request-id`. A transport failure is recorded.
    pub(crate) fn send_bytes(&mut self, what: &str, method: &str, path: &str, body: Option<&[u8]>, headers: &[(&str, &str)]) -> Option<Bytes> {
        match self.http.bytes(method, path, body, headers) {
            Ok(r) => {
                match r.header(gogi_proto::ids::REQUEST_ID_HEADER).filter(|i| gogi_proto::ids::is_request_id(i)) {
                    Some(id) => self.request_ids.push(id.to_string()),
                    None => self.request_id_faults.push(format!("{} {} returned {} with request-id {:?}", method, path, r.status, r.header(gogi_proto::ids::REQUEST_ID_HEADER))),
                }
                Some(r)
            }
            Err(e) => {
                self.r.fail(what, e);
                None
            }
        }
    }

    /// The bytes of this run's object: every byte value, so a server that reads text fails, then the
    /// run's nonce, so each run puts a new object.
    pub(crate) fn object_bytes(&self, label: &str) -> Vec<u8> {
        (0..=255u8).chain(format!("{}-{}", label, self.nonce).bytes()).collect()
    }

    /// Puts an object and returns its id, or `None` when the put fails.
    pub(crate) fn put_object(&mut self, what: &str, bytes: &[u8], mime: &str) -> Option<String> {
        let id = content_id(bytes);
        let r = self.send_bytes(what, "PUT", &format!("/council/v2/objects/{}", id), Some(bytes), &[("content-type", mime)])?;
        self.r.check(what, r.status == 201 || r.status == 200, format!("returned {}: {}", r.status, String::from_utf8_lossy(&r.body)));
        (r.status == 201 || r.status == 200).then_some(id)
    }

    pub(crate) fn objects(&mut self) {
        if !self.has(Capability::Objects) {
            self.r.skip("objects", "the server does not declare the objects capability");
            return;
        }
        let body = self.object_bytes("objects");
        let id = content_id(&body);
        let path = format!("/council/v2/objects/{}", id);
        let Some(p) = self.send_bytes("a new object is a 201", "PUT", &path, Some(&body), &[("content-type", "application/octet-stream")]) else { return };
        self.r.check("a new object is a 201", p.status == 201, format!("returned {}: {}", p.status, String::from_utf8_lossy(&p.body)));
        if let Some(put) = self.decode::<ObjectPut>("a put response has the contract's fields", &p.json()) {
            self.r.check("a put returns the id, the media type, and the size", put.id == id && put.mime == "application/octet-stream" && put.bytes == body.len() as u64, &put);
        }
        if let Some(again) = self.send_bytes("a put of a held object is a 200", "PUT", &path, Some(&body), &[("content-type", "image/png")]) {
            self.r.check("a put of a held object is a 200", again.status == 200, format!("returned {}", again.status));
            self.r.check("a later put keeps the first media type", again.json()["mime"] == json!("application/octet-stream"), again.json());
        }

        if let Some(g) = self.send_bytes("an object reads back", "GET", &path, None, &[]) {
            self.r.check("an object reads back bit for bit", g.status == 200 && g.body == body, format!("returned {} with {} bytes", g.status, g.body.len()));
            self.r.check("an object reads back with its media type", g.header("content-type") == Some("application/octet-stream"), g.header("content-type"));
        }
        if let Some(h) = self.send_bytes("HEAD names the size", "HEAD", &path, None, &[]) {
            self.r.check("HEAD names the size and sends no body", h.status == 200 && h.header("content-length") == Some(body.len().to_string().as_str()) && h.body.is_empty(), (h.status, h.header("content-length")));
        }
        let len = body.len();
        for (what, range, want) in [("a range reads its bytes", "bytes=2-5", 2..6), ("a range from a start reads to the end", &*format!("bytes={}-", len - 4), len - 4..len), ("a suffix range reads the last bytes", "bytes=-3", len - 3..len)] {
            if let Some(g) = self.send_bytes(what, "GET", &path, None, &[("range", range)]) {
                let content_range = format!("bytes {}-{}/{}", want.start, want.end - 1, len);
                self.r.check(what, g.status == 206 && g.body == body[want.clone()] && g.header("content-range") == Some(content_range.as_str()), (g.status, g.body.len(), g.header("content-range")));
            }
        }
        if let Some(g) = self.send_bytes("a range past the end is a 416", "GET", &path, None, &[("range", &format!("bytes={}-", len))]) {
            self.r.check("a range past the end is a 416", g.status == 416, g.status);
        }

        let other = content_id(b"not these bytes");
        if let Some(m) = self.send_bytes("bytes that hash to another id are a 400", "PUT", &format!("/council/v2/objects/{}", other), Some(&body), &[("content-type", "application/octet-stream")]) {
            let v = m.json();
            let message = v["error"]["message"].as_str().unwrap_or("");
            self.r.check("bytes that hash to another id are a 400 naming both ids", m.status == 400 && v["error"]["param"] == json!("id") && message.contains(&id) && message.contains(&other), &v);
        }
        if let Some(g) = self.send_bytes("a refused put holds nothing", "HEAD", &format!("/council/v2/objects/{}", other), None, &[]) {
            self.r.check("a refused put holds nothing", g.status == 404, g.status);
        }
        for (what, headers) in [("a put without content-type is a 400 naming it", vec![]), ("a put with a media type in capitals is a 400 naming it", vec![("content-type", "Application/Octet-Stream")]), ("a put with media type parameters is a 400 naming it", vec![("content-type", "text/plain; charset=utf-8")])] {
            let fresh = self.object_bytes(what);
            if let Some(m) = self.send_bytes(what, "PUT", &format!("/council/v2/objects/{}", content_id(&fresh)), Some(&fresh), &headers) {
                let v = m.json();
                self.r.check(what, m.status == 400 && v["error"]["param"] == json!("content-type"), &v);
            }
        }
        let short: String = id.chars().take("blake3:".len() + 16).collect();
        if let Some(g) = self.send_bytes("a short object id is a 400 naming it", "GET", &format!("/council/v2/objects/{}", short), None, &[]) {
            let v = g.json();
            self.r.check("a short object id is a 400 naming it", g.status == 400 && v["error"]["param"] == json!("id") && v["error"]["message"].as_str().unwrap_or("").contains(&short), &v);
        }
        if let Some(g) = self.send_bytes("an object the server does not hold is a 404", "GET", &format!("/council/v2/objects/{}", content_id(b"never put")), None, &[]) {
            let v = g.json();
            self.r.check("an object the server does not hold is a 404", g.status == 404 && v["error"]["type"] == json!("not_found"), &v);
        }
        match self.identity.as_ref().and_then(|i| i.limits.object_bytes) {
            Some(limit) if limit < MAX_PROBE_BYTES => {
                let big = vec![7u8; limit as usize + 1];
                if let Some(m) = self.send_bytes("an object past object_bytes is a 413", "PUT", &format!("/council/v2/objects/{}", content_id(&big)), Some(&big), &[("content-type", "application/octet-stream")]) {
                    let v = m.json();
                    self.r.check("an object past object_bytes is a 413", m.status == 413 && v["error"]["type"] == json!("too_large") && v["error"]["param"] == json!("object_bytes"), &v);
                }
            }
            Some(_) => self.r.skip("an object past object_bytes is a 413", "object_bytes is more than the suite sends"),
            None => self.r.fail("identity names object_bytes", "a server with objects names object_bytes in limits"),
        }
        self.keeps(&id);
    }

    fn keeps(&mut self, id: &str) {
        let owner = format!("gogi-conformance-{}", self.nonce);
        let path = format!("/council/v2/keeps/{}", owner);
        let unheld = content_id(b"never put either");
        self.refused("an owner that keeps nothing is a 404", "GET", &path, None, 404);
        let Some(p) = self.expect("a keep succeeds", "PUT", &path, Some(json!({"objects": [id, unheld], "ttl_s": 120})), 200) else { return };
        if let Some(r) = self.decode::<KeepsPutResult>("a keep response has the contract's fields", &p) {
            self.r.check("a keep on an object the server does not hold reports held false", r.objects.len() == 2 && r.objects[0].held && !r.objects[1].held, &p);
            self.r.check("a keep's expires_in_s is at most the ttl_s asked", r.expires_in_s.is_some_and(|e| 0 < e && e <= 120), &p);
        }
        if let Some(l) = self.expect("a keep list succeeds", "GET", &path, None, 200) {
            self.r.check("a keep list names the held objects only", l["objects"] == json!([id]) && l["owner"] == json!(owner), &l);
        }
        if let Some(p) = self.expect("a keep may last 30 days", "PUT", &path, Some(json!({"objects": [id], "ttl_s": KEEP_TTL_MAX_S})), 200) {
            self.r.check("a keep may last 30 days", p["expires_in_s"].as_u64().is_some_and(|e| e > 0 && e <= KEEP_TTL_MAX_S), &p);
        }
        self.refused("a keep past 30 days is a 400", "PUT", &path, Some(json!({"objects": [id], "ttl_s": KEEP_TTL_MAX_S + 1})), 400);
        let short: String = id.chars().take("blake3:".len() + 8).collect();
        self.refused_naming("a keep on a short object id is a 400 naming it", "PUT", &path, Some(json!({"objects": [short], "ttl_s": 60})), "objects", &short);
        if let Some(l) = self.expect("a refused keep set changes nothing", "GET", &path, None, 200) {
            self.r.check("a refused keep set changes nothing", l["objects"] == json!([id]), &l);
        }
        self.expect("a keep set is deleted", "DELETE", &path, None, 204);
        self.refused("a deleted keep set is a 404", "GET", &path, None, 404);
    }
}
