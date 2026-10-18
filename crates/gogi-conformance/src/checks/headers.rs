//! What travels in headers: the `request-id` a server gives each response, and the `traceparent`
//! a client sends.

use gogi_proto::identity::Capability;
use gogi_proto::ids::TRACEPARENT_HEADER;
use serde_json::{Value, json};

use crate::Suite;

impl Suite {
    /// A `traceparent` header joins the server's spans to the client's trace. It changes no number
    /// and never appears in a body.
    pub(crate) fn tracing(&mut self) {
        // A trace id unique to this run: 32 lowercase hex digits.
        let trace = format!("{:0>32}", &self.nonce[self.nonce.len().saturating_sub(32)..]);
        let traceparent = format!("00-{}-00f067aa0ba902b7-01", trace);
        let ctx = self.ctx(self.opening("trace"));
        let (path, body) = if self.has(Capability::Reads) {
            ("/council/v2/reads", json!({"contexts": [ctx], "suffixes": ["\nAnswer: "], "letters": ["A", "B"], "top": 4}))
        } else {
            ("/council/v2/contexts", json!({"context": ctx}))
        };
        let Some(body) = self.routed("POST", path, Some(body)) else { return };
        let Some((_, plain)) = self.send_as_written("a request without traceparent succeeds", "POST", path, Some(&body)) else { return };
        match self.http.raw_with("POST", path, Some(&body), &[(TRACEPARENT_HEADER, &traceparent)]) {
            Err(e) => self.r.fail("a request with traceparent succeeds", e),
            Ok(r) => {
                self.note("POST", path, &r);
                let traced = r.json();
                self.r.check("a request with traceparent succeeds", r.status == 200, (r.status, &traced));
                let numbers = |v: &Value| if path.ends_with("reads") { v["reads"].clone() } else { json!([v["head"], v["snapshots"]]) };
                self.r.check("a traceparent header changes no number", numbers(&plain) == numbers(&traced), (numbers(&plain), numbers(&traced)));
                self.r.check("a traceparent stays out of the response body", !r.text.contains(&trace), &r.text);
            }
        }
    }

    /// Every response carries a `request-id` header, no two the same, and an error body repeats
    /// it as `request_id`.
    pub(crate) fn request_ids(&mut self) {
        let faults = std::mem::take(&mut self.request_id_faults);
        self.r.check("every response carries a request-id, and every error body repeats it", faults.is_empty(), &faults);
        let mut ids = self.request_ids.clone();
        let all = ids.len();
        ids.sort();
        ids.dedup();
        self.r.check("request ids differ between requests", ids.len() == all && all > 0, format!("{} responses, {} distinct request ids", all, ids.len()));
    }
}
