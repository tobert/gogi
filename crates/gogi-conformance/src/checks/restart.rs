use std::time::{Duration, Instant};

use gogi_proto::spec_id_of;
use serde_json::json;

use crate::{Restart, Suite, num};

impl Suite {
    /// Restarts the server and checks that what it was told to keep came back.
    pub(crate) fn restart(&mut self, restart: &mut Restart) {
        let name = "specs and pins survive a restart";
        let spec = self.gate_spec("restart");
        let Some(held) = self.expect("restart: spec post answers", "POST", "/council/v2/specs", Some(spec.clone()), 200) else { return };
        let spec_id = held["spec_id"].as_str().unwrap_or("").to_string();
        let ctx = self.ctx(self.opening("restart"));
        let Some(b) = self.build("restart: build answers", &ctx, false) else { return };
        let head = b["head"].as_str().unwrap_or("").to_string();
        let owner = format!("gogi-conformance-restart-{}", self.nonce);
        let pinned = self.has(gogi_proto::identity::Capability::Pins);
        if pinned && self.expect("restart: pin answers", "PUT", &format!("/council/v2/pins/{}", owner), Some(json!({"snapshots": [head], "ttl_s": 600})), 200).is_none() {
            return;
        }
        let req = json!({"spec_id": spec_id, "state": "git push origin main", "contexts": [ctx]});
        let before = self.expect("restart: decision answers", "POST", "/council/v2/decisions", Some(req.clone()), 200);
        let identity_before = self.identity.as_ref().and_then(|i| serde_json::to_value(i).ok());

        let result = match restart {
            Restart::Command(cmd) => match std::process::Command::new("sh").arg("-c").arg(&*cmd).status() {
                Ok(s) if s.success() => Ok(()),
                Ok(s) => Err(format!("`{}` exited with {}", cmd, s)),
                Err(e) => Err(format!("`{}`: {}", cmd, e)),
            },
            Restart::Hook(f) => f(),
        };
        if let Err(e) = result {
            self.r.fail(name, format!("the restart command failed: {}", e));
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(300);
        let back = loop {
            if let Ok((200, v)) = self.http.call("GET", "/council/v2/identity", None) {
                break Some(v);
            }
            if Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        let Some(identity_after) = back else {
            self.r.fail(name, "the server did not answer identity within 300 seconds of the restart");
            return;
        };
        self.r.ok_with("the server answers again after the restart", "");
        self.r.check("identity is unchanged by a restart", identity_before.as_ref() == Some(&identity_after), (&identity_before, &identity_after));

        if let Some(g) = self.expect("restart: spec get answers", "GET", &format!("/council/v2/specs/{}", spec_id), None, 200) {
            self.r.check("a held spec survives a restart", g["spec"] == spec && Some(g["spec_id"].as_str().unwrap_or("")) == spec_id_of(&spec).ok().as_deref(), &g);
        }
        if pinned {
            if let Some(p) = self.expect("restart: pins list answers", "GET", &format!("/council/v2/pins/{}", owner), None, 200) {
                let ids: Vec<&str> = p["snapshots"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().or_else(|| e["id"].as_str())).collect()).unwrap_or_default();
                self.r.check("an owner's pins survive a restart", ids == [head.as_str()] && num(&p["expires_in_s"]) > 0, &p);
            }
            if let Some(s) = self.expect("restart: snapshot info answers", "GET", &format!("/council/v2/snapshots/{}", head), None, 200) {
                self.r.check("a pinned snapshot survives a restart", num(&s["tokens"]) == num(&b["tokens"]), &s);
            }
            if let Some(again) = self.build("restart: build again answers", &self.ctx(self.opening("restart")), false) {
                self.r.check("a pinned context needs no feeding after a restart", num(&again["fed"]) == 0 && again["head"] == b["head"], &again);
            }
        } else {
            self.r.skip("pins survive a restart", "the server does not declare the pins capability");
        }
        if let (Some(before), Some(after)) = (before, self.expect("restart: decision again answers", "POST", "/council/v2/decisions", Some(req), 200)) {
            self.r.check("a decision replays bit for bit across a restart", before["reads"][0]["answers"] == after["reads"][0]["answers"] && before["answers"] == after["answers"], (&before["answers"], &after["answers"]));
        }
        if pinned {
            self.expect("restart: unpin answers", "DELETE", &format!("/council/v2/pins/{}", owner), None, 204);
        }
        self.expect("restart: spec cleanup", "DELETE", &format!("/council/v2/specs/{}", spec_id), None, 204);
    }
}
