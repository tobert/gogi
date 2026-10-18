//! `gogi-conformance <url> [--model <name>] [--allow-restart <cmd>]`

use gogi_conformance::{Opts, Restart, run};

fn main() {
    let mut url = None;
    let mut restart = None;
    let mut model = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--allow-restart" => match args.next() {
                Some(cmd) => restart = Some(Restart::Command(cmd)),
                None => usage("--allow-restart needs a command"),
            },
            "--model" => match args.next() {
                Some(m) => model = Some(m),
                None => usage("--model needs a name"),
            },
            "-h" | "--help" => usage(""),
            s if s.starts_with("--") => usage(&format!("unknown flag {}", s)),
            _ if url.is_none() => url = Some(a),
            _ => usage("one url"),
        }
    }
    let Some(url) = url else { usage("a url is required") };
    let report = run(Opts { url, restart, model }, true);
    println!("{}", report.summary());
    std::process::exit(if report.failures() > 0 { 1 } else { 0 });
}

fn usage(msg: &str) -> ! {
    if !msg.is_empty() {
        eprintln!("gogi-conformance: {}", msg);
    }
    eprintln!("usage: gogi-conformance <url> [--model <name>] [--allow-restart <cmd>]\n\n  <url>                  base URL of a council API server, such as http://127.0.0.1:8091\n  --model <name>         check this served model; it must be ready. Default: the first ready one in identity\n  --allow-restart <cmd>  run <cmd> (through sh -c) to restart the server, then check that specs and pins survive");
    std::process::exit(2)
}
