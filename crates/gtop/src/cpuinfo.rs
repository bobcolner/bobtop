//! `read_cpu_model` — parse `/proc/cpuinfo` for the CPU model name shown
//! in the cores subpanel title.
//!
//! Strips the noisy AMD/Intel marketing tail (`(R)`, `(TM)`, `Processor`,
//! `CPU @ 3.40GHz`, `XX-Core Processor`) so the panel title doesn't blow
//! past 30 chars on the wide-name SKUs.

pub fn read_cpu_model() -> Option<String> {
    let body = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let raw = body
        .lines()
        .find_map(|l| l.strip_prefix("model name").and_then(|r| r.split_once(':')))
        .map(|(_, v)| v.trim().to_string())?;
    let cleaned = raw
        .replace("(R)", "")
        .replace("(TM)", "")
        .replace("CPU ", "")
        .split('@')
        .next()
        .unwrap_or(&raw)
        .split("-Core")
        .next()
        .unwrap_or(&raw)
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned = cleaned.trim_end_matches(char::is_whitespace).to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}
