//! Smoke tests for the `gtop` binary's multi-call dispatch.
//!
//! Each test shells out to the just-built `gtop` binary (via the
//! `CARGO_BIN_EXE_gtop` env var that cargo populates for integration
//! tests) and checks that an obviously-fast command exits 0. The point
//! is to catch *binary*-level regressions that crate-level unit tests
//! can't see — `main.rs` and the multi-call entry are invisible to
//! `cargo test --lib`.
//!
//! The original sin these tests are guarding against: an outer
//! `#[tokio::main]` runtime around `main()` caused every dispatched
//! subcommand (`gtop fb`, `gtop agent`) to construct its own inner
//! runtime, then drop it inside the outer's async context — which
//! tokio panics on. The bug shipped because no test ever invoked the
//! actual binary.
//!
//! These tests run on every `cargo test --workspace`, so the next
//! regression of this shape fails CI loudly instead of silently
//! crashing the file browser.

use std::process::Command;
use std::time::Duration;

/// Path to the freshly-compiled `gtop` binary. Cargo populates this
/// env var at compile time for integration tests.
const GTOP: &str = env!("CARGO_BIN_EXE_gtop");

/// Run `gtop <args>` with a hard wall-clock cap so a hung subcommand
/// can't block the test suite. Returns the exit status + captured
/// stdout/stderr so tests can assert against output text when useful.
fn run_gtop(args: &[&str]) -> (std::process::ExitStatus, String, String) {
    let mut cmd = Command::new(GTOP);
    cmd.args(args);
    // Strip env vars that would change behavior between local and CI.
    // RUST_LOG would otherwise spam the captured stderr; HOME / XDG_*
    // would route persisted-state writes to real user dirs.
    cmd.env_remove("RUST_LOG");
    cmd.env("HOME", "/tmp");
    cmd.env("XDG_CONFIG_HOME", "/tmp");
    cmd.env("XDG_STATE_HOME", "/tmp");
    cmd.env("XDG_RUNTIME_DIR", "/tmp");
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn gtop");
    // 10 s is plenty for any --help / --version / --list-themes path.
    // The TUI path doesn't run here — none of these args open a screen.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait().expect("wait gtop") {
            Some(status) => {
                let out = child.stdout.take().map(read_pipe).unwrap_or_default();
                let err = child.stderr.take().map(read_pipe).unwrap_or_default();
                return (status, out, err);
            }
            None => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("gtop {:?} hung past 10s deadline", args);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn read_pipe<R: std::io::Read>(mut r: R) -> String {
    let mut s = String::new();
    let _ = r.read_to_string(&mut s);
    s
}

#[test]
fn help_exits_clean() {
    let (status, stdout, stderr) = run_gtop(&["--help"]);
    assert!(
        status.success(),
        "gtop --help exited {status:?}; stderr={stderr:?}"
    );
    // clap writes help text to stdout, so absence of stdout content
    // would mean the binary didn't get far enough to print help.
    assert!(stdout.contains("gtop"), "no help text in stdout");
}

#[test]
fn version_exits_clean() {
    let (status, _stdout, stderr) = run_gtop(&["--version"]);
    assert!(
        status.success(),
        "gtop --version exited {status:?}; stderr={stderr:?}"
    );
}

#[test]
fn list_themes_exits_clean() {
    let (status, stdout, stderr) = run_gtop(&["--list-themes"]);
    assert!(
        status.success(),
        "gtop --list-themes exited {status:?}; stderr={stderr:?}"
    );
    // Should print at least one bundled theme name.
    assert!(
        !stdout.trim().is_empty(),
        "--list-themes produced no output"
    );
}

#[test]
fn agent_help_exits_clean() {
    let (status, _stdout, stderr) = run_gtop(&["agent", "--help"]);
    assert!(
        status.success(),
        "gtop agent --help exited {status:?}; stderr={stderr:?}"
    );
}

/// Sanity: `--help` short-circuits inside clap before any App is
/// built, so this only checks the dispatch wiring + clap surface
/// reaches the file browser. The real regression test is
/// `fb_constructs_and_drops_runtime_cleanly` below.
#[cfg(feature = "fb")]
#[test]
fn fb_help_exits_clean() {
    let (status, stdout, stderr) = run_gtop(&["fb", "--help"]);
    assert!(
        status.success(),
        "gtop fb --help exited {status:?}; stderr={stderr:?}"
    );
    assert!(
        stdout.contains("gfb") || stdout.contains("file browser"),
        "fb --help output missing expected text: {stdout:?}"
    );
}

/// Regression for the nested-tokio-runtime panic.
///
/// Invokes `gtop fb /tmp` against stdin/stdout pipes (no TTY). Path:
///
///   1. clap parses successfully (so we get past `--help`'s early exit).
///   2. `App::new_with` builds an inner `tokio::runtime::Runtime`.
///   3. `enable_raw_mode` fails because stdin isn't a TTY.
///   4. `cli::run` propagates the error; App drops; inner Runtime drops.
///
/// With the bug present (outer `#[tokio::main]` wrapping the dispatch),
/// step 4 dropped the inner Runtime inside the outer Runtime's async
/// context and tokio panicked:
///
///     "Cannot drop a runtime in a context where blocking is not allowed."
///
/// Panic = non-zero exit. The fix runs the dispatch in plain sync
/// context, so the inner Runtime drops cleanly and the binary exits
/// with a non-zero status code carrying a "not a tty" error message
/// instead of a panic. We accept any non-success exit here — the
/// distinguishing signal is *whether* the panic message appears.
#[cfg(feature = "fb")]
#[test]
fn fb_constructs_and_drops_runtime_cleanly() {
    let (status, _stdout, stderr) = run_gtop(&["fb", "/tmp"]);
    // Either way the binary exits — what matters is HOW. With the bug
    // the error is a tokio panic; without it the error is the expected
    // raw-mode failure. So we assert on the *content* of stderr, not
    // the exit status.
    assert!(
        !stderr.contains("Cannot drop a runtime"),
        "regression: outer tokio runtime is wrapping the fb dispatch.\n\
         stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("blocking is not allowed"),
        "regression: nested tokio runtime detected.\n\
         stderr:\n{stderr}"
    );
    let _ = status;
}
