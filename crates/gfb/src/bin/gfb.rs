//! `gfb` standalone binary entry point.
//!
//! Thin wrapper around [`gfb::cli::run`] — same function the
//! bundled `gtop fb …` subcommand uses, so behavior between the
//! two install paths is identical.

use anyhow::Result;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    gfb::cli::run(args)
}
