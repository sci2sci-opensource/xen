//! xen — intent-only multi-repo orchestration.
//!
//! See README.md for the design. The verbs live in `verbs.rs`; the
//! load-bearing piece is `key.rs`.

mod cli;
mod gitignore;
mod key;
mod proc;
mod rules;
mod store;
mod verbs;

use clap::Parser;
use cli::{Cli, Verb};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.verb {
        Verb::Portal { op } => verbs::portal(op).await,
        Verb::Summon(a) => verbs::summon(a).await,
        Verb::Sync(a) => verbs::sync(a).await,
        Verb::Cascade(a) => verbs::cascade(a).await,
        Verb::Env(a) => verbs::env(a).await,
        Verb::Resonate(a) => verbs::resonate(a).await,
    };
    if let Err(e) = result {
        eprintln!("xen: {e:#}");
        std::process::exit(1);
    }
}
