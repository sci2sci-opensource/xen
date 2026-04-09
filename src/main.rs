//! xen — intent-only multi-repo orchestration.
//!
//! See README.md for the design. The verbs live in `verbs.rs`; the
//! load-bearing piece is `key.rs`.

mod cli;
mod gitignore;
mod hooks;
mod key;
mod proc;
mod rules;
mod store;
mod verbs;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Verb};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    // Capture the joined argv after `xen` so hooks can match against
    // it. `xen sync main` becomes `sync main`; `xen cascade git push`
    // becomes `cascade git push`. This is the user's intent in
    // string form, before clap re-shapes it into the typed Verb enum.
    let cmd_string = std::env::args().skip(1).collect::<Vec<_>>().join(" ");

    let result = run(cli, &cmd_string).await;
    if let Err(e) = result {
        eprintln!("xen: {e:#}");
        std::process::exit(1);
    }
}

/// Run a verb with hook execution wrapped around it. Pre-hooks run
/// before the verb dispatches; if any pre-hook fails, the verb does
/// not run. Post-hooks run after the verb completes successfully; if
/// any post-hook fails, its non-zero exit becomes the command's
/// overall failure.
async fn run(cli: Cli, cmd_string: &str) -> Result<()> {
    // Hook discovery is best-effort: if `.xen/` doesn't exist (e.g.
    // running `xen --help` outside any workspace) load_hooks returns
    // an empty config and compile returns an empty Vec. No surprises.
    let paths = store::EnvPaths::discover()?;
    let hooks_cfg = store::load_hooks(&paths)?;
    let compiled = hooks::compile(&hooks_cfg)?;

    hooks::run_matching(&compiled, cmd_string, hooks::Phase::Pre, &paths.shared_root).await?;

    match cli.verb {
        Verb::Portal { op } => verbs::portal(op).await?,
        Verb::Summon(a) => verbs::summon(a).await?,
        Verb::Sync(a) => verbs::sync(a).await?,
        Verb::Cascade(a) => verbs::cascade(a).await?,
        Verb::Env(a) => verbs::env(a).await?,
        Verb::Resonate(a) => verbs::resonate(a).await?,
    }

    hooks::run_matching(
        &compiled,
        cmd_string,
        hooks::Phase::Post,
        &paths.shared_root,
    )
    .await?;

    Ok(())
}
