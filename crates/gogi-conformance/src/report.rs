//! What a run reports: one line per check.

use std::fmt::Debug;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    Skip,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
}

pub struct Report {
    pub checks: Vec<Check>,
    pub print: bool,
}

fn clip(s: String) -> String {
    if s.chars().count() > 400 { format!("{}...", s.chars().take(400).collect::<String>()) } else { s }
}

impl Report {
    pub fn new(print: bool) -> Report {
        Report { checks: Vec::new(), print }
    }

    fn push(&mut self, name: &str, status: Status, detail: String) {
        if self.print {
            match status {
                Status::Ok if detail.is_empty() => println!("ok   {}", name),
                Status::Ok => println!("ok   {}  ({})", name, detail),
                Status::Fail => println!("FAIL {}  {}", name, detail),
                Status::Skip => println!("skip {}  {}", name, detail),
            }
        }
        self.checks.push(Check { name: name.to_string(), status, detail });
    }

    /// Records `ok` or `FAIL`; `detail` explains a failure.
    pub fn check(&mut self, name: &str, ok: bool, detail: impl Debug) -> bool {
        if ok {
            self.push(name, Status::Ok, String::new());
        } else {
            self.push(name, Status::Fail, clip(format!("{:?}", detail)));
        }
        ok
    }

    /// Records `ok` with a note on what the server did.
    pub fn ok_with(&mut self, name: &str, note: impl Into<String>) {
        self.push(name, Status::Ok, note.into());
    }

    pub fn fail(&mut self, name: &str, detail: impl Into<String>) {
        self.push(name, Status::Fail, clip(detail.into()));
    }

    pub fn skip(&mut self, name: &str, why: impl Into<String>) {
        self.push(name, Status::Skip, why.into());
    }

    pub fn failures(&self) -> usize {
        self.checks.iter().filter(|c| c.status == Status::Fail).count()
    }

    pub fn summary(&self) -> String {
        let n = |s| self.checks.iter().filter(|c| c.status == s).count();
        format!("{} ok, {} FAIL, {} skip", n(Status::Ok), n(Status::Fail), n(Status::Skip))
    }
}
