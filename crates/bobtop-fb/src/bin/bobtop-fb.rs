//! `bobtop-fb` standalone binary entry point.
//!
//! Thin wrapper around [`bobtop_fb::cli::run`] — same function the
//! bundled `bobtop fb …` subcommand uses, so behavior between the
//! two install paths is identical.

use anyhow::Result;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    bobtop_fb::cli::run(args)
}
