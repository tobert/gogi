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
                    Some(id) => {
                        let v = r.json();
                        if r.status >= 400 && v.get("error").is_some() && v["request_id"] != json!(id) {
                            self.request_id_faults.push(format!("{} {} returned {} with request-id {} and an error body whose request_id is {}", method, path, r.status, id, v["request_id"]));
                        }
                        self.request_ids.push(id.to_string());
                    }
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
            self.r.check("an object reads back with its content-length", g.header("content-length") == Some(body.len().to_string().as_str()), g.header("content-length"));
            self.r.check("an object reads back with its media type", g.header("content-type") == Some("application/octet-stream"), g.header("content-type"));
        }
        if let Some(h) = self.send_bytes("HEAD names the size", "HEAD", &path, None, &[]) {
            self.r.check("HEAD names the size and sends no body", h.status == 200 && h.header("content-length") == Some(body.len().to_string().as_str()) && h.body.is_empty(), (h.status, h.header("content-length")));
        }
        let len = body.len();
        for (what, range, want) in [("a range reads its bytes", "bytes=2-5", 2..6), ("a range from a start reads to the end", &*format!("bytes={}-", len - 4), len - 4..len), ("a suffix range reads the last bytes", "bytes=-3", len - 3..len)] {
            if let Some(g) = self.send_bytes(what, "GET", &path, None, &[("range", range)]) {
                let content_range = format!("bytes {}-{}/{}", want.start, want.end - 1, len);
                let length = (want.end - want.start).to_string();
                let ok = g.status == 206 && g.body == body[want.clone()] && g.header("content-range") == Some(content_range.as_str()) && g.header("content-length") == Some(length.as_str()) && g.header("content-type") == Some("application/octet-stream");
                self.r.check(what, ok, (g.status, g.body.len(), g.header("content-range"), g.header("content-length")));
            }
        }
        if let Some(g) = self.send_bytes("a range past the end is a 416", "GET", &path, None, &[("range", &format!("bytes={}-", len))]) {
            let v = g.json();
            self.r.check("a range past the end is a 416 naming range", g.status == 416 && v["error"]["type"] == json!("invalid_request") && v["error"]["param"] == json!("range"), (g.status, &v));
        }
        if let Some(g) = self.send_bytes("a range that ends before it starts reads the whole object", "GET", &path, None, &[("range", &format!("bytes={}-1", len + 5))]) {
            self.r.check("a range that ends before it starts reads the whole object", g.status == 200 && g.body == body, g.status);
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
        let hex = content_id(format!("objects-bad-{}", self.nonce).as_bytes()).trim_start_matches("blake3:").to_string();
        for (what, method, bad) in [("a short object id", "GET", format!("blake3:{}", &hex[..16])), ("a sha256 object id", "GET", format!("sha256:{}", hex)), ("a put at a short object id", "PUT", format!("blake3:{}", &hex[..16])), ("a put at a sha256 object id", "PUT", format!("sha256:{}", hex))] {
            let what = format!("{} is a 400 naming it", what);
            let sent = if method == "PUT" { Some(body.as_slice()) } else { None };
            if let Some(g) = self.send_bytes(&what, method, &format!("/council/v2/objects/{}", bad), sent, &[("content-type", "application/octet-stream")]) {
                let v = g.json();
                self.r.check(&what, g.status == 400 && v["error"]["param"] == json!("id") && v["error"]["message"].as_str().unwrap_or("").contains(&bad), &v);
            }
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
                if let Some(h) = self.send_bytes("an object past object_bytes is not held", "HEAD", &format!("/council/v2/objects/{}", content_id(&big)), None, &[]) {
                    self.r.check("an object past object_bytes is not held", h.status == 404, h.status);
                }
            }
            Some(_) => self.r.skip("an object past object_bytes is a 413", "object_bytes is more than the suite sends"),
            None => self.r.fail("identity names object_bytes", "a server with objects names object_bytes in limits"),
        }
        self.keeps(&id);
        self.keep_bytes();
    }

    /// Puts objects past `keep_bytes` and keeps them in one set: a `413` naming `keep_bytes` that
    /// keeps nothing.
    fn keep_bytes(&mut self) {
        let limits = self.identity.as_ref().map(|i| (i.limits.object_bytes, i.limits.keep_bytes));
        let Some((Some(object), Some(keep))) = limits else {
            self.r.fail("identity names keep_bytes", "a server with objects names object_bytes and keep_bytes in limits");
            return;
        };
        if keep >= MAX_PROBE_BYTES {
            self.r.skip("a keep set past keep_bytes is a 413", "keep_bytes is more than the suite sends");
            return;
        }
        let size = object.min(keep).max(1);
        let mut ids = Vec::new();
        for i in 0..keep / size + 1 {
            let mut bytes = self.object_bytes(&format!("keep-bytes-{}", i));
            bytes.resize(size as usize, i as u8);
            match self.put_object("keep_bytes: object put succeeds", &bytes, "application/octet-stream") {
                Some(id) => ids.push(id),
                None => return,
            }
        }
        let path = format!("/council/v2/keeps/gogi-conformance-keep-bytes-{}", self.nonce);
        if let Some((status, v)) = self.send("a keep set past keep_bytes is a 413", "PUT", &path, Some(json!({"objects": ids, "ttl_s": 60}))) {
            self.r.check("a keep set past keep_bytes is a 413", status == 413 && v["error"]["type"] == json!("too_large") && v["error"]["param"] == json!("keep_bytes"), format!("returned {}: {}", status, v));
        }
        self.refused("a keep set past keep_bytes keeps nothing", "GET", &path, None, 404);
    }

    fn keeps(&mut self, id: &str) {
        let owner = format!("gogi-conformance-{}", self.nonce);
        let path = format!("/council/v2/keeps/{}", owner);
        let unheld = content_id(b"never put either");
        self.refused("an owner that keeps nothing is a 404", "GET", &path, None, 404);
        let long = "k".repeat(129);
        for method in ["GET", "DELETE"] {
            self.refused_naming(&format!("{} of a keep owner of 129 characters is a 400 naming owner", method), method, &format!("/council/v2/keeps/{}", long), None, "owner", "");
        }
        self.refused_naming("a keep owner of 129 characters is a 400 naming owner", "PUT", &format!("/council/v2/keeps/{}", long), Some(json!({"objects": [], "ttl_s": 5})), "owner", "");
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
        let hex = content_id(format!("keeps-bad-{}", self.nonce).as_bytes()).trim_start_matches("blake3:").to_string();
        for (what, bad) in [("a short object id", format!("blake3:{}", &hex[..8])), ("a sha256 object id", format!("sha256:{}", hex)), ("an object id in capitals", format!("blake3:{}", hex.to_ascii_uppercase()))] {
            self.refused_naming(&format!("a keep on {} is a 400 naming it", what), "PUT", &path, Some(json!({"objects": [bad], "ttl_s": 60})), "objects", &bad);
        }
        if let Some(l) = self.expect("a refused keep set changes nothing", "GET", &path, None, 200) {
            self.r.check("a refused keep set changes nothing", l["objects"] == json!([id]), &l);
        }
        let other = self.object_bytes("keeps-other");
        if let Some(b) = self.put_object("keeps: second object put succeeds", &other, "application/octet-stream")
            && self.expect("a keep set is replaced", "PUT", &path, Some(json!({"objects": [b], "ttl_s": 60})), 200).is_some()
            && let Some(l) = self.expect("a replaced keep list succeeds", "GET", &path, None, 200)
        {
            self.r.check("a PUT replaces the owner's whole keep set", l["objects"] == json!([b]), &l);
        }
        self.expect("a keep set is deleted", "DELETE", &path, None, 204);
        self.refused("a deleted keep set is a 404", "GET", &path, None, 404);
    }
}
