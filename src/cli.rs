//! CLI surface. The six verbs and their args, expressed as types so
//! `clap` derives the parser, help text, and shell completions for free.

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "xen",
    version,
    about = "Intent-only multi-repo orchestration",
    long_about = "xen stores intent: which repos exist, what `dev` means in each one, \
                  which paths should be visible. Git stores facts. They never overlap."
)]
pub struct Cli {
    #[command(subcommand)]
    pub verb: Verb,
}

#[derive(Subcommand, Debug)]
pub enum Verb {
    /// Register a repo (clones metainfo only; `summon` materializes).
    Portal {
        #[command(subcommand)]
        op: PortalOp,
    },
    /// Reconcile on-disk state to match env intent.
    Summon(SummonArgs),
    /// Resolve a branchtype/tag and fast-forward each repo to it.
    Sync(SyncArgs),
    /// Run a shell command across a selected set of repos.
    Cascade(CascadeArgs),
    /// xen's only config surface.
    Env(EnvArgs),
    /// Copy files from a local source into an already-registered portal.
    /// Pure filesystem op — no git, no manifest writes. The portal must
    /// exist (`xen portal add` first); resonate just delivers files into it.
    Resonate(ResonateArgs),
}

#[derive(Subcommand, Debug)]
pub enum PortalOp {
    /// Add a repo to the manifest and clone its metainfo.
    Add {
        url: String,
        /// Where to materialize, relative to the xen root.
        /// Default: basename of the URL.
        #[arg(long)]
        at: Option<String>,
        /// Branchtype aliases, e.g. `--alias dev=develop`.
        #[arg(long = "alias", value_parser = parse_alias)]
        alias: Vec<(String, String)>,
    },
    /// Remove a repo from the manifest. Leaves the working tree in place.
    Remove { repo: String },
    /// List registered repos.
    List,
}

fn parse_alias(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("expected key=value, got {s:?}"))?;
    Ok((k.trim().to_string(), v.trim().to_string()))
}

#[derive(Args, Debug)]
pub struct SummonArgs {
    /// Narrow to these repos. Empty = everything xen knows about.
    pub repos: Vec<String>,
    /// Sparse paths. Sugar for `env set <repo>.paths=...` then reconcile.
    /// Requires exactly one repo.
    #[arg(long, value_delimiter = ',')]
    pub paths: Vec<String>,
}

#[derive(Args, Debug)]
pub struct SyncArgs {
    /// Branchtype, branch, or tag. Use `refs/heads/` or `refs/tags/`
    /// prefixes to disambiguate.
    pub ref_name: String,
}

#[derive(Args, Debug)]
pub struct CascadeArgs {
    /// The shell command to run inside each selected repo.
    #[arg(trailing_var_arg = true, required = true)]
    pub command: Vec<String>,
    /// Only repos with a dirty tree or unpushed commits.
    #[arg(long)]
    pub changed_only: bool,
    /// Only repos currently on a branch matching this glob.
    #[arg(long)]
    pub branch_pattern: Option<String>,
}

#[derive(Args, Debug)]
pub struct ResonateArgs {
    /// Name of an already-registered portal (from `xen portal add`).
    pub portal: String,
    /// Local file or directory to copy from.
    #[arg(long)]
    pub source: std::path::PathBuf,
    /// Subpath inside the portal placement to drop the source into.
    /// Absent → portal placement root. Example: `--at docs/notes` puts
    /// the source's contents under `<placement>/docs/notes/`.
    #[arg(long)]
    pub at: Option<String>,
    /// Overwrite existing files at the destination. By default resonate
    /// refuses if any destination file already exists.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct EnvArgs {
    #[command(subcommand)]
    pub op: Option<EnvOp>,
    /// Dump the raw shared layer (only when no subcommand is given).
    #[arg(long, conflicts_with = "private")]
    pub shared: bool,
    /// Dump the raw private layer (only when no subcommand is given).
    #[arg(long)]
    pub private: bool,
}

#[derive(Subcommand, Debug)]
pub enum EnvOp {
    /// Set a key. Layer is determined by the key's shape; `--private`
    /// targets the private layer for keys that allow either.
    Set {
        assignment: String,
        #[arg(long)]
        private: bool,
    },
    /// Unset a key.
    Unset {
        key: String,
        #[arg(long)]
        private: bool,
    },
    /// Get a single key's merged value.
    Get { key: String },
    /// Workspace rules — gates that fire on `xen cascade` commands.
    Rules {
        #[command(subcommand)]
        op: RulesOp,
    },
}

#[derive(Subcommand, Debug)]
pub enum RulesOp {
    /// Set a rule field, e.g. `set protect-main.pattern='^git push'`.
    Set { assignment: String },
    /// Unset a rule field, or a whole rule by bare name.
    Unset { key: String },
    /// Get a rule field, or all fields of a rule by bare name.
    Get { key: String },
    /// List all configured rules.
    List,
}
