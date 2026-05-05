//! Client primitives for talking to the agent socket.
//!
//! Argv parsing and the daemon-binary auto-spawn live in the consuming
//! crate (the bobtop daemon today, future MCP shims tomorrow); this
//! module is the protocol-only half: connect, send one line of JSON,
//! read one line back.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use super::server::socket_path;

/// Result of a send: the response line (already trimmed of the
/// trailing newline) plus a flag indicating whether the response is
/// an error envelope. Clients use the flag to set their exit code.
#[derive(Debug, Clone)]
pub struct AgentResponse {
    pub line: String,
    pub is_error: bool,
}

/// Connect to `path` and send `request` as a single line of JSON,
/// returning the response line. Reads have a 5-second timeout — the
/// daemon answers in microseconds, so anything past that is a stalled
/// host. Errors carry a human-readable message suitable for printing
/// directly to stderr.
pub fn send(path: &Path, request: &serde_json::Value) -> Result<AgentResponse, String> {
    let mut stream = UnixStream::connect(path)
        .map_err(|e| format!("connect {}: {e}", path.display()))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let line = serde_json::to_string(request).map_err(|e| format!("serialize: {e}"))?;
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.flush())
        .map_err(|e| format!("write: {e}"))?;
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
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    let line = std::str::from_utf8(&buf)
        .map_err(|_| "non-utf8 response".to_string())?
        .trim_end()
        .to_string();
    let is_error = line.contains("\"error\":");
    Ok(AgentResponse { line, is_error })
}

/// Connect to an existing socket, or — when missing — spawn `binary
/// args...` (detached via `setsid`), poll the path for up to
/// `timeout`, and connect once it appears. Honors
/// `BOBTOP_NO_AUTOSPAWN=1` so callers (CI, security-sensitive contexts)
/// can opt out by env.
///
/// Errors carry a human-readable reason. The caller is expected to
/// surface them to the user with appropriate context (e.g. "is bobtop
/// running?").
pub fn connect_or_spawn(
    path: &Path,
    binary: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<UnixStream, String> {
    if let Ok(s) = UnixStream::connect(path) {
        return Ok(s);
    }
    if std::env::var("BOBTOP_NO_AUTOSPAWN")
        .ok()
        .filter(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .is_some()
    {
        return Err("socket missing and BOBTOP_NO_AUTOSPAWN=1 is set".into());
    }
    spawn_detached(binary, args).map_err(|e| format!("spawn {}: {e}", binary.display()))?;
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(s) = UnixStream::connect(path) {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(75));
    }
    Err(format!(
        "spawned {} but socket {} did not appear within {:?}",
        binary.display(),
        path.display(),
        timeout
    ))
}

/// Best-effort detached spawn: stdio = /dev/null, new session via
/// `setsid` so the child outlives the caller.
fn spawn_detached(binary: &Path, args: &[&str]) -> Result<(), std::io::Error> {
    let mut cmd = std::process::Command::new(binary);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; pre-exec runs in the
        // child between fork and execve where only such functions are
        // permitted.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    cmd.spawn().map(|_| ())
}

/// Convenience: full client cycle — auto-spawn `binary` if needed,
/// send the request, parse the response. Returns the wire response or
/// a human-readable failure reason.
pub fn auto_query(
    path: &Path,
    binary: &Path,
    binary_args: &[&str],
    request: &serde_json::Value,
) -> Result<AgentResponse, String> {
    let stream = connect_or_spawn(path, binary, binary_args, Duration::from_secs(3))?;
    send_on(stream, request)
}

/// Like [`send`] but takes an already-connected stream — used by
/// [`auto_query`] to avoid a redundant `connect` after `connect_or_spawn`
/// already gave us a live stream.
fn send_on(mut stream: UnixStream, request: &serde_json::Value) -> Result<AgentResponse, String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let line = serde_json::to_string(request).map_err(|e| format!("serialize: {e}"))?;
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.flush())
        .map_err(|e| format!("write: {e}"))?;
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
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    let line = std::str::from_utf8(&buf)
        .map_err(|_| "non-utf8 response".to_string())?
        .trim_end()
        .to_string();
    let is_error = line.contains("\"error\":");
    Ok(AgentResponse { line, is_error })
}

/// Re-export `PathBuf` so callers don't have to also depend on `std`.
/// (Almost every Rust caller will, but it removes an awkward
/// "where do I import this from?" moment.)
pub fn socket_path_buf() -> PathBuf {
    socket_path()
}
