//! Entry point — defers to the CLI module.

fn main() {
    if let Err(err) = hash98_miner::cli::run() {
        eprintln!("error: {err:?}");
        std::process::exit(1);
    }
}
