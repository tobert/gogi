use std::sync::{Arc, Mutex};

use gogi_conformance::{Opts, Restart, Status, run};
use gogi_fake::{Config, FakeModel, ModelStatus, Server};

fn failures(r: &gogi_conformance::Report) -> Vec<String> {
    r.checks.iter().filter(|c| c.status == Status::Fail).map(|c| format!("{}: {}", c.name, c.detail)).collect()
}

#[test]
fn the_fake_passes_every_check() {
    let server = Server::start(Config::default()).unwrap();
    let r = run(Opts { url: server.url(), restart: None, model: None }, false);
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
    let r = run(Opts { url, restart: Some(Restart::Hook(Box::new(move || hook.lock().unwrap().restart()))), model: None }, false);
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
    let r = run(Opts { url, restart: Some(Restart::Hook(Box::new(move || hook.lock().unwrap().restart()))), model: None }, false);
    let failed = failures(&r);
    assert!(failed.iter().any(|f| f.starts_with("restart: spec get succeeds")), "{failed:#?}");
    assert!(failed.iter().any(|f| f.starts_with("restart: pins list succeeds")), "{failed:#?}");
}

#[test]
fn a_server_that_is_not_there_fails_loudly() {
    let r = run(Opts { url: "http://127.0.0.1:1".into(), restart: None, model: None }, false);
    assert_eq!(r.failures(), 1);
}

#[test]
fn the_fake_passes_with_three_served_models_in_three_statuses() {
    let models = vec![
        FakeModel::ready("gogi-toy-day", 0),
        FakeModel { aliases: vec!["night".into()], ..FakeModel::ready("gogi-toy-night", 1) },
        FakeModel { status: ModelStatus::Loading, ..FakeModel::ready("gogi-toy-big", 2) },
    ];
    let server = Server::start(Config { models, ..Config::default() }).unwrap();
    let r = run(Opts { url: server.url(), restart: None, model: Some("night".into()) }, false);
    assert!(failures(&r).is_empty(), "{:#?}", failures(&r));
    for name in [
        "a build without model is a 400 naming model",
        "gogi-toy-big, Loading in identity, is a 503 model_unavailable naming it and its status",
        "gogi-toy-day builds the same context into a snapshot of its own",
    ] {
        assert!(r.checks.iter().any(|c| c.name == name && c.status == Status::Ok), "{name}");
    }
}

#[test]
fn a_model_that_is_not_ready_cannot_be_checked() {
    let server = Server::start(Config { models: vec![FakeModel { status: ModelStatus::Unloaded, ..FakeModel::ready("gogi-toy-1", 0) }], ..Config::default() }).unwrap();
    let r = run(Opts { url: server.url(), restart: None, model: None }, false);
    assert_eq!(failures(&r).len(), 1, "{:#?}", failures(&r));
    assert!(failures(&r)[0].starts_with("a served model to check is ready"));
}
