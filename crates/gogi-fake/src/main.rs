//! `gogi-fake [--listen ADDR] [--state-dir DIR] [--context-tokens N]`

use gogi_fake::{Config, Server};

fn main() {
    let mut cfg = Config { addr: "127.0.0.1:8091".into(), ..Config::default() };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().unwrap_or_else(|| usage(&format!("{} needs a value", name)));
        match a.as_str() {
            "--listen" => cfg.addr = value("--listen"),
            "--state-dir" => cfg.state_dir = Some(value("--state-dir").into()),
            "--context-tokens" => cfg.context_tokens = value("--context-tokens").parse().unwrap_or_else(|_| usage("--context-tokens is a number")),
            _ => usage(&format!("unknown argument {}", a)),
        }
    }
    let server = Server::start(cfg).unwrap_or_else(|e| usage(&e));
    println!("gogi-fake listening on {}", server.url());
    loop {
        std::thread::park();
    }
}

fn usage(msg: &str) -> ! {
    eprintln!("gogi-fake: {}\nusage: gogi-fake [--listen ADDR] [--state-dir DIR] [--context-tokens N]", msg);
    std::process::exit(2)
}
