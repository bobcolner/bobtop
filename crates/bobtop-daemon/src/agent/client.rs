//! `bobtop agent <subcommand>` — CLI glue around the engine's agent
//! client primitives. Argv parsing lives here; the wire protocol and
//! socket I/O live in `bobtop_engine::agent::client`.

use std::time::Duration;

use bobtop_engine::agent::client::{auto_query, socket_path_buf};
use serde_json::json;

/// Top-level dispatch for `bobtop agent ...`. Called from `main.rs`
/// before the TUI is initialized when the first positional arg is
/// `agent`. Returns the process exit code.
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
    let request: Result<serde_json::Value, String> = match args[0].as_str() {
        "snapshot" => Ok(json!({ "q": "snapshot" })),
        "top" => parse_top(&args[1..]),
        "window" => parse_history(&args[1..], "window"),
        "peak" => parse_history(&args[1..], "peak"),
        "summary" => parse_summary(&args[1..]),
        "pid_inspect" | "inspect" => parse_pid_inspect(&args[1..]),
        "responsible_for" | "who" => parse_responsible_for(&args[1..]),
        "help" | "-h" | "--help" => {
            print_help();
            return 0;
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            return 2;
        }
    };
    let request = match request {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("bobtop agent {}: {msg}", args[0]);
            return 2;
        }
    };
    send(request)
}

fn print_help() {
    println!("bobtop agent — query the running daemon over its Unix socket");
    println!();
    println!("subcommands:");
    println!("  snapshot                                   latest host-level summary");
    println!("  top --by <metric> [--n N] [--group G]       ranked processes");
    println!("       [--match PATTERN]");
    println!("  window --metric <m> --window <w>            avg/peak/p95 over a window");
    println!("  peak --metric <m> --window <w>              peak value + responsible pids");
    println!("  summary [--match PAT | --pid N]             host or scoped rollup");
    println!("  pid_inspect (--pid N | --match PAT)         full detail for one pid");
    println!("  responsible_for --metric <m> --at <off>     who owned <m> at <off> ago");
    println!();
    println!("metrics: cpu | mem | net.tx | net.rx | disk.r | disk.w");
    println!("groups:  flat (default) | exec | cgroup | tree");
    println!("windows: 1s..30m (e.g. 30s, 1m, 5m, 30m)");
    println!();
    println!("examples:");
    println!("  bobtop agent snapshot");
    println!("  bobtop agent top --by cpu --n 5");
    println!("  bobtop agent top --by mem --group exec --match '*chrome*'");
    println!("  bobtop agent top --by cpu --group cgroup");
    println!("  bobtop agent top --by cpu --group tree --n 10");
    println!("  bobtop agent window --metric cpu --window 5m");
    println!("  bobtop agent peak --metric net.tx --window 1m");
    println!("  bobtop agent summary --match 'python*'");
    println!("  bobtop agent pid_inspect --pid 1234");
    println!("  bobtop agent responsible_for --metric cpu --at 30s");
}

/// Send a request through the engine's auto-spawn-aware client. Auto-
/// spawn forks `bobtop --daemon` if no daemon is currently listening.
fn send(request: serde_json::Value) -> i32 {
    let path = socket_path_buf();
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bobtop agent: cannot locate own binary ({e})");
            return 4;
        }
    };
    match auto_query(&path, &exe, &["--daemon"], &request) {
        Ok(resp) => {
            println!("{}", resp.line);
            if resp.is_error {
                1
            } else {
                0
            }
        }
        Err(msg) => {
            eprintln!(
                "bobtop agent: {msg}. Try `bobtop --daemon &` or run a TUI."
            );
            3
        }
    }
}

// ---- argv parsers ---------------------------------------------------------

fn parse_top(args: &[String]) -> Result<serde_json::Value, String> {
    let mut by: Option<String> = None;
    let mut n: Option<usize> = None;
    let mut group: Option<String> = None;
    let mut match_: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let take = || -> Result<&String, String> {
            args.get(i + 1).ok_or_else(|| format!("`{a}` expects a value"))
        };
        match a.as_str() {
            "--by" => {
                by = Some(take()?.clone());
                i += 2;
            }
            "--n" | "-n" => {
                let v = take()?;
                n = Some(
                    v.parse()
                        .map_err(|_| format!("`--n` expects an integer, got `{v}`"))?,
                );
                i += 2;
            }
            "--group" => {
                group = Some(take()?.clone());
                i += 2;
            }
            "--match" => {
                match_ = Some(take()?.clone());
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

fn parse_summary(args: &[String]) -> Result<serde_json::Value, String> {
    let mut match_: Option<String> = None;
    let mut pid: Option<u32> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let take = || -> Result<&String, String> {
            args.get(i + 1).ok_or_else(|| format!("`{a}` expects a value"))
        };
        match a.as_str() {
            "--match" => {
                match_ = Some(take()?.clone());
                i += 2;
            }
            "--pid" => {
                let v = take()?;
                pid = Some(v.parse().map_err(|_| format!("--pid expects integer, got '{v}'"))?);
                i += 2;
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    let mut req = json!({ "q": "summary" });
    if let Some(m) = match_ {
        req["match"] = json!(m);
    }
    if let Some(p) = pid {
        req["pid"] = json!(p);
    }
    Ok(req)
}

fn parse_pid_inspect(args: &[String]) -> Result<serde_json::Value, String> {
    let mut match_: Option<String> = None;
    let mut pid: Option<u32> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let take = || -> Result<&String, String> {
            args.get(i + 1).ok_or_else(|| format!("`{a}` expects a value"))
        };
        match a.as_str() {
            "--match" => {
                match_ = Some(take()?.clone());
                i += 2;
            }
            "--pid" => {
                let v = take()?;
                pid = Some(v.parse().map_err(|_| format!("--pid expects integer, got '{v}'"))?);
                i += 2;
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    if pid.is_none() && match_.is_none() {
        return Err("requires --pid <n> or --match <pattern>".into());
    }
    let mut req = json!({ "q": "pid_inspect" });
    if let Some(m) = match_ {
        req["match"] = json!(m);
    }
    if let Some(p) = pid {
        req["pid"] = json!(p);
    }
    Ok(req)
}

fn parse_responsible_for(args: &[String]) -> Result<serde_json::Value, String> {
    let mut metric: Option<String> = None;
    let mut at: Option<String> = None;
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
            "--at" => {
                at = Some(take()?.clone());
                i += 2;
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    let metric = metric.ok_or_else(|| "`--metric <m>` is required".to_string())?;
    let at = at.ok_or_else(|| "`--at <offset>` is required (e.g. 30s, 5m)".to_string())?;
    Ok(json!({ "q": "responsible_for", "metric": metric, "at": at }))
}

// Suppress unused-import warning for Duration when nothing in the
// thin client pulls it directly — kept around because future polish
// (custom timeouts, retry windows) will use it without re-importing.
#[allow(dead_code)]
fn _keep_duration_in_scope() -> Duration {
    Duration::from_secs(0)
}
