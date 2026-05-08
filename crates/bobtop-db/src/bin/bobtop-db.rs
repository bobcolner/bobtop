//! Thin entry point — argv → `cli::run`.

fn main() -> anyhow::Result<()> {
    bobtop_db::cli::run(std::env::args().collect())
}
