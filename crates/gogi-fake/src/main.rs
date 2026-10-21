//! `gogi-fake [--listen ADDR] [--state-dir DIR] [--context-tokens N] [--seal-cut-heads] [--model NAME]...`

use gogi_fake::{Config, FakeModel, Server};

fn main() {
    let mut cfg = Config { addr: "127.0.0.1:8091".into(), ..Config::default() };
    let mut models = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().unwrap_or_else(|| usage(&format!("{} needs a value", name)));
        match a.as_str() {
            "--listen" => cfg.addr = value("--listen"),
            "--state-dir" => cfg.state_dir = Some(value("--state-dir").into()),
            "--context-tokens" => cfg.context_tokens = value("--context-tokens").parse().unwrap_or_else(|_| usage("--context-tokens is a number")),
            "--seal-cut-heads" => cfg.seal_cut_heads = true,
            "--model" => {
                let name = value("--model");
                models.push(FakeModel::ready(&name, models.len() as u64));
            }
            _ => usage(&format!("unknown argument {}", a)),
        }
    }
    if !models.is_empty() {
        cfg.models = models;
    }
    let server = Server::start(cfg).unwrap_or_else(|e| usage(&e));
    println!("gogi-fake listening on {}", server.url());
    loop {
        std::thread::park();
    }
}

fn usage(msg: &str) -> ! {
    eprintln!("gogi-fake: {}\nusage: gogi-fake [--listen ADDR] [--state-dir DIR] [--context-tokens N] [--seal-cut-heads] [--model NAME]...\n\n  --model NAME      serve a model named NAME, ready; repeat it to serve more than one. Default: gogi-toy-1, alias toy\n  --seal-cut-heads  hold a cut reply's head sealed, so a continuation from it reads a rounded model state", msg);
    std::process::exit(2)
}
