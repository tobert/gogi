use std::sync::{Arc, Mutex};

use gogi_conformance::{Opts, Restart, Status, run};
use gogi_fake::{Config, Server};

fn failures(r: &gogi_conformance::Report) -> Vec<String> {
    r.checks.iter().filter(|c| c.status == Status::Fail).map(|c| format!("{}: {}", c.name, c.detail)).collect()
}

#[test]
fn the_fake_passes_every_check() {
    let server = Server::start(Config::default()).unwrap();
    let r = run(Opts { url: server.url(), restart: None }, false);
    assert!(failures(&r).is_empty(), "{:#?}", failures(&r));
    assert!(r.checks.len() > 150, "{}", r.checks.len());
}

#[test]
fn the_fake_passes_the_restart_checks_when_it_keeps_its_state() {
    let dir = std::env::temp_dir().join(format!("gogi-conf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Arc::new(Mutex::new(Server::start(Config { state_dir: Some(dir.clone()), ..Config::default() }).unwrap()));
    let url = server.lock().unwrap().url();
    let hook = server.clone();
    let r = run(Opts { url, restart: Some(Restart::Hook(Box::new(move || hook.lock().unwrap().restart()))) }, false);
    assert!(failures(&r).is_empty(), "{:#?}", failures(&r));
    assert!(r.checks.iter().any(|c| c.name == "a pinned snapshot survives a restart" && c.status == Status::Ok));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_restart_that_loses_state_fails_the_restart_checks() {
    // No state directory: specs and pins are gone after the restart.
    let server = Arc::new(Mutex::new(Server::start(Config::default()).unwrap()));
    let url = server.lock().unwrap().url();
    let hook = server.clone();
    let r = run(Opts { url, restart: Some(Restart::Hook(Box::new(move || hook.lock().unwrap().restart()))) }, false);
    let failed = failures(&r);
    assert!(failed.iter().any(|f| f.starts_with("restart: spec get answers")), "{failed:#?}");
    assert!(failed.iter().any(|f| f.starts_with("restart: pins list answers")), "{failed:#?}");
}

#[test]
fn a_server_that_is_not_there_fails_loudly() {
    let r = run(Opts { url: "http://127.0.0.1:1".into(), restart: None }, false);
    assert_eq!(r.failures(), 1);
}
