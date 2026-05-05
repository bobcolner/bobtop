//! `bobtop agent <subcommand>` — thin Unix-socket client.
//!
//! The client never starts collectors of its own. It connects to the
//! running daemon's socket, sends a single line-delimited JSON request,
//! prints the response to stdout, and exits.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::json;

use super::server::socket_path;

/// Top-level dispatch for `bobtop agent ...`. Called from `main.rs` before
/// the TUI is initialized when the first positional arg is `agent`.
///
/// Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("usage: bobtop agent <subcommand>");
        eprintln!();
        eprintln!("subcommands:");
        eprintln!("  snapshot                       latest host-level summary");
        eprintln!("  top --by <metric> [opts]       ranked processes / groups");
        eprintln!();
        eprintln!("see docs/agent-schema.md for the full wire schema");
        return 2;
    }
    match args[0].as_str() {
        "snapshot" => send(json!({ "q": "snapshot" })),
        "top" => match parse_top(&args[1..]) {
            Ok(req) => send(req),
            Err(msg) => {
                eprintln!("bobtop agent top: {msg}");
                2
            }
        },
        "window" => match parse_history(&args[1..], "window") {
            Ok(req) => send(req),
            Err(msg) => {
                eprintln!("bobtop agent window: {msg}");
                2
            }
        },
        "peak" => match parse_history(&args[1..], "peak") {
            Ok(req) => send(req),
            Err(msg) => {
                eprintln!("bobtop agent peak: {msg}");
                2
            }
        },
        "help" | "-h" | "--help" => {
            println!("bobtop agent — query the running daemon over its Unix socket");
            println!();
            println!("subcommands:");
            println!("  snapshot                                   latest host-level summary");
            println!("  top --by <metric> [--n N] [--group G]       ranked processes");
            println!("       [--match PATTERN]");
            println!("  window --metric <m> --window <w>            avg/peak/p95 over a window");
            println!("  peak --metric <m> --window <w>              peak value + responsible pids");
            println!();
            println!("metrics: cpu | mem | net.tx | net.rx | disk.r | disk.w");
            println!("groups:  flat (default) | exec");
            println!("windows: 1s..30m (e.g. 30s, 1m, 5m, 30m)");
            println!();
            println!("examples:");
            println!("  bobtop agent snapshot");
            println!("  bobtop agent top --by cpu --n 5");
            println!("  bobtop agent top --by mem --group exec --match '*chrome*'");
            println!("  bobtop agent window --metric cpu --window 5m");
            println!("  bobtop agent peak --metric net.tx --window 1m");
            0
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            2
        }
    }
}

/// Parse arg list for `window` / `peak`. Both take the same `--metric`
/// + `--window` flags; the verb name is bundled into the resulting JSON.
fn parse_history(args: &[String], verb: &str) -> Result<serde_json::Value, String> {
    let mut metric: Option<String> = None;
    let mut window: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let take = || -> Result<&String, String> {
            args.get(i + 1).ok_or_else(|| format!("`{a}` expects a value"))
        };
        match a.as_str() {
            "--metric" => {
                metric = Some(take()?.clone());
                i += 2;
            }
            "--window" | "-w" => {
                window = Some(take()?.clone());
                i += 2;
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    let metric = metric.ok_or_else(|| "`--metric <m>` is required".to_string())?;
    let window = window.ok_or_else(|| "`--window <w>` is required".to_string())?;
    Ok(json!({ "q": verb, "metric": metric, "window": window }))
}

/// Parse `top` arg list into a JSON request body. Hand-rolled because
/// pulling clap's full subcommand machinery just for the client is
/// disproportionate, and the verb's argument set is small and fixed.
fn parse_top(args: &[String]) -> Result<serde_json::Value, String> {
    let mut by: Option<String> = None;
    let mut n: Option<usize> = None;
    let mut group: Option<String> = None;
    let mut match_: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let take_value = || -> Result<&String, String> {
            args.get(i + 1).ok_or_else(|| format!("`{a}` expects a value"))
        };
        match a.as_str() {
            "--by" => {
                by = Some(take_value()?.clone());
                i += 2;
            }
            "--n" | "-n" => {
                let v = take_value()?;
                n = Some(
                    v.parse()
                        .map_err(|_| format!("`--n` expects an integer, got `{v}`"))?,
                );
                i += 2;
            }
            "--group" => {
                group = Some(take_value()?.clone());
                i += 2;
            }
            "--match" => {
                match_ = Some(take_value()?.clone());
                i += 2;
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    let by = by.ok_or_else(|| "`--by <metric>` is required".to_string())?;
    let mut req = json!({ "q": "top", "by": by });
    if let Some(n) = n {
        req["n"] = json!(n);
    }
    if let Some(g) = group {
        req["group"] = json!(g);
    }
    if let Some(m) = match_ {
        req["match"] = json!(m);
    }
    Ok(req)
}

fn send(request: serde_json::Value) -> i32 {
    let path = socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "bobtop agent: cannot connect to {} ({e}). Is bobtop running?",
                path.display()
            );
            return 3;
        }
    };
    // Generous read timeout — the daemon answers in microseconds, but a
    // stalled host shouldn't hang the client forever.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let line = serde_json::to_string(&request).expect("serialize request");
    if let Err(e) = stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.flush())
    {
        eprintln!("bobtop agent: write failed: {e}");
        return 4;
    }
    // Single response line. Read until newline or EOF.
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') {
                    break;
                }
            }
            Err(e) => {
                eprintln!("bobtop agent: read failed: {e}");
                return 4;
            }
        }
    }
    let line = match std::str::from_utf8(&buf) {
        Ok(s) => s.trim_end(),
        Err(_) => {
            eprintln!("bobtop agent: non-utf8 response");
            return 4;
        }
    };
    println!("{line}");
    if line.contains("\"error\":") {
        1
    } else {
        0
    }
}
