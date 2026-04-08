//! The six verbs — five at the git layer (`portal`, `summon`, `sync`,
//! `cascade`, `env`) plus the filesystem-only `resonate`.
//!
//! Verbs that need to talk to git compose `proc::run`/`proc::check`
//! with literal argv at the call site. There is intentionally no
//! typed git wrapper to consult: cascade is the pattern, the verbs
//! just happen to know which argv they want.

use crate::cli::{
    CascadeArgs, EnvArgs, EnvOp, PortalOp, ResonateArgs, RulesOp, SummonArgs, SyncArgs,
};
use crate::gitignore;
use crate::key::{Branchtype, Key, Layer, RepoName, RuleField, SharedKey};
use crate::proc;
use crate::rules;
use crate::store::{self, EnvPaths, PrivateConfig, PrivateRepo, RulesConfig, SharedConfig};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tokio::task::JoinSet;

// --- merged read view -----------------------------------------------------

#[derive(Default, Debug, Clone)]
struct MergedRepo {
    url: Option<String>,
    at: Option<String>,
    paths: Vec<String>,
    branchtypes: BTreeMap<String, String>,
    auth_count: usize,
    /// Per-repo env vars to layer onto every git spawn for this repo.
    /// Built from the private layer's `key` (and friends) at merge
    /// time. Empty if no per-repo auth is recorded — the agent and
    /// the user's shell env handle the common case.
    auth_env: Vec<(String, String)>,
}

impl MergedRepo {
    /// Effective placement: explicit `at` wins, otherwise URL basename,
    /// otherwise the repo name.
    fn placement(&self, name: &str) -> String {
        if let Some(at) = &self.at {
            return at.clone();
        }
        if let Some(url) = &self.url {
            let last = url.rsplit('/').next().unwrap_or(name);
            return last.trim_end_matches(".git").to_string();
        }
        name.to_string()
    }

    /// Reborrow the owned auth env as the slice shape `proc::*_env` wants.
    fn auth_refs(&self) -> Vec<(&str, &str)> {
        self.auth_env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }
}

struct Merged {
    repos: BTreeMap<String, MergedRepo>,
}

fn merge(shared: &SharedConfig, private: &PrivateConfig) -> Merged {
    let mut repos: BTreeMap<String, MergedRepo> = BTreeMap::new();
    for (name, s) in &shared.repos {
        let m = repos.entry(name.clone()).or_default();
        m.url = s.url.clone();
        m.at = s.at.clone();
        m.paths = s.paths.clone();
        m.branchtypes = s.branchtypes.clone();
    }
    for (name, p) in &private.repos {
        let m = repos.entry(name.clone()).or_default();
        if p.at.is_some() {
            m.at = p.at.clone();
        }
        if !p.paths.is_empty() {
            m.paths = p.paths.clone();
        }
        for (k, v) in &p.branchtypes {
            m.branchtypes.insert(k.clone(), v.clone());
        }
        m.auth_count = usize::from(p.key.is_some())
            + usize::from(p.helper.is_some())
            + usize::from(p.token.is_some());
        m.auth_env = build_auth_env(p);
    }
    Merged { repos }
}

/// Translate a `PrivateRepo` into the env vars git needs to honor it.
///
/// Right now we only wire `key` → `GIT_SSH_COMMAND`. The intended
/// composition is: ssh-agent + OS keychain hold the unlocked key
/// material; xen names *which* loaded identity git should use for
/// this particular repo. The agent does the unlock, xen does the
/// per-repo selector.
///
/// `helper` and `token` slots are reserved on `PrivateRepo` for the
/// HTTPS-token flows but aren't translated yet — wait until there's
/// a real use case before turning them on.
fn build_auth_env(p: &PrivateRepo) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(key) = &p.key {
        let path = expand_home(key);
        env.push((
            "GIT_SSH_COMMAND".to_string(),
            format!("ssh -i {} -o IdentitiesOnly=yes", shell_single_quote(&path)),
        ));
    }
    env
}

/// Expand a leading `~/` against the user's home directory. Tilde
/// expansion otherwise happens in unquoted shell context, which is
/// exactly what `shell_single_quote` strips, so we have to do it
/// ourselves before quoting. Tries `HOME` first (POSIX), falls back
/// to `USERPROFILE` (Windows) so the same auth path works on both.
fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return format!("{}/{rest}", home.to_string_lossy());
        }
    }
    p.to_string()
}

/// POSIX single-quote escaping: every `'` becomes `'\''`. Safe for
/// arbitrary contents including spaces, glob chars, `$`, backticks.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// --- portal ---------------------------------------------------------------

pub async fn portal(op: PortalOp) -> Result<()> {
    let paths = EnvPaths::discover()?;
    match op {
        PortalOp::Add { url, at, alias } => portal_add(&paths, url, at, alias).await,
        PortalOp::Remove { repo } => portal_remove(&paths, repo),
        PortalOp::List => portal_list(&paths),
    }
}

async fn portal_add(
    paths: &EnvPaths,
    url: String,
    at: Option<String>,
    alias: Vec<(String, String)>,
) -> Result<()> {
    let name = derive_repo_name(&url)?;
    let mut shared = store::load_shared(paths)?;
    let workspace_was_fresh = shared.repos.is_empty()
        && !paths.repos_file().exists()
        && !paths.legacy_shared_file().exists();

    if shared.repos.contains_key(name.as_str()) {
        bail!("portal: repo {name} already registered");
    }

    let at_value = at.unwrap_or_else(|| name.as_str().to_string());
    check_no_overlap(&shared, &name, &at_value)?;

    // Validate aliases up front so we don't even attempt the clone
    // with bad input.
    for (k, _) in &alias {
        Branchtype::new(k.as_str())?;
    }

    // Clone first, manifest second. This makes `portal add`
    // transactional from the user's POV: if the clone fails (auth,
    // network, wrong URL), the workspace looks exactly as it did
    // before the command. No half-registered entries to clean up.
    //
    // Per spec: clone metadata only (`--no-checkout` is the
    // load-bearing flag — without it git would materialize the
    // sparse cone and we'd be lying about portal not putting files
    // on disk).
    let dest_abs = paths.shared_root.join(&at_value);
    if dest_abs.exists() {
        eprintln!("portal: {at_value} already exists on disk, skipping clone");
    } else {
        let parent = dest_abs
            .parent()
            .unwrap_or(&paths.shared_root)
            .to_path_buf();
        std::fs::create_dir_all(&parent)?;
        let dest_name = dest_abs
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(name.as_str());
        proc::check(
            &parent,
            &[
                "git",
                "clone",
                "--filter=blob:none",
                "--sparse",
                "--no-checkout",
                "--",
                url.as_str(),
                dest_name,
            ],
        )
        .await
        .with_context(|| format!("cloning {url}"))?;
    }

    // Clone succeeded (or already on disk) — now record intent.
    let entry = shared.repos.entry(name.as_str().to_string()).or_default();
    entry.url = Some(url.clone());
    entry.at = Some(at_value.clone());
    for (k, v) in alias {
        entry.branchtypes.insert(k, v);
    }
    store::save_shared(paths, &shared)?;
    gitignore::reconcile(paths, &shared)?;

    // First-portal-in-a-fresh-workspace: seed default rules so a
    // newcomer gets the protect-main-* gates without having to know
    // they exist. Idempotent: only fires when both repos.toml and
    // rules.toml were missing before this command. The seeded rules
    // are visible in `.xen/rules.toml` and can be edited or removed
    // via `xen env rules` like any other rule.
    if workspace_was_fresh && !paths.rules_file().exists() {
        let mut rules_cfg = store::load_rules(paths)?;
        if rules_cfg.rules.is_empty() {
            rules_cfg.rules = rules::defaults();
            store::save_rules(paths, &rules_cfg)?;
            println!(
                "portal: seeded {} default rule(s) into .xen/rules.toml",
                rules_cfg.rules.len()
            );
        }
    }

    println!("portal: registered {name} at {at_value}");
    Ok(())
}

fn portal_remove(paths: &EnvPaths, repo: String) -> Result<()> {
    let name = RepoName::new(repo.as_str())?;
    let mut shared = store::load_shared(paths)?;
    if shared.repos.remove(name.as_str()).is_none() {
        bail!("portal: repo {name} not registered");
    }
    store::save_shared(paths, &shared)?;
    gitignore::reconcile(paths, &shared)?;
    let mut priv_cfg = store::load_private(paths)?;
    priv_cfg.repos.remove(name.as_str());
    store::save_private(paths, &priv_cfg)?;
    println!("portal: removed {name} (working tree left in place)");
    Ok(())
}

fn portal_list(paths: &EnvPaths) -> Result<()> {
    let merged = merge(&store::load_shared(paths)?, &store::load_private(paths)?);
    if merged.repos.is_empty() {
        println!("(no repos registered)");
        return Ok(());
    }
    for (name, m) in &merged.repos {
        let at = m.placement(name);
        let url = m.url.as_deref().unwrap_or("?");
        println!("{name}  ->  {at}  ({url})");
    }
    Ok(())
}

fn derive_repo_name(url: &str) -> Result<RepoName> {
    // Accept both '/' and '\' as path separators so file:// URLs and
    // bare local paths work on Windows. Real Windows users may type
    // either `xen portal add C:\path\to\repo.git` or
    // `xen portal add file://C:\path\to\repo.git`; without backslash
    // handling we'd treat the whole drive-rooted path as the repo
    // name and reject it as invalid.
    let last = url.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(url);
    let stem = last.trim_end_matches(".git");
    if stem.is_empty() {
        bail!("portal: cannot derive repo name from url {url:?}");
    }
    Ok(RepoName::new(stem)?)
}

fn check_no_overlap(shared: &SharedConfig, new_name: &RepoName, new_at: &str) -> Result<()> {
    let new_path = normalize_rel(new_at);
    for (name, r) in &shared.repos {
        if name == new_name.as_str() {
            continue;
        }
        let other = match &r.at {
            Some(a) => normalize_rel(a),
            None => normalize_rel(name),
        };
        if paths_overlap(&new_path, &other) {
            bail!("portal: placement {new_at:?} overlaps with repo {name:?} at {other:?}");
        }
    }
    Ok(())
}

fn normalize_rel(p: &str) -> String {
    p.trim_start_matches("./").trim_end_matches('/').to_string()
}

fn paths_overlap(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

// --- summon ---------------------------------------------------------------

pub async fn summon(args: SummonArgs) -> Result<()> {
    let paths = EnvPaths::discover()?;

    // `--paths` is sugar for an `env set` then a reconcile.
    if !args.paths.is_empty() {
        if args.repos.len() != 1 {
            bail!("summon --paths requires exactly one repo");
        }
        let name = RepoName::new(args.repos[0].as_str())?;
        store::write_shared(&paths, SharedKey::Paths(name), args.paths.join(","))?;
    }

    let shared = store::load_shared(&paths)?;
    gitignore::reconcile(&paths, &shared)?;
    let merged = merge(&shared, &store::load_private(&paths)?);
    let selected: Vec<String> = if args.repos.is_empty() {
        merged.repos.keys().cloned().collect()
    } else {
        for r in &args.repos {
            if !merged.repos.contains_key(r) {
                bail!("summon: unknown repo {r:?}");
            }
        }
        args.repos.clone()
    };

    let mut errs = 0;
    for name in selected {
        let m = &merged.repos[&name];
        match reconcile_one(&paths, &name, m).await {
            Ok(()) => println!("  {name}: ok"),
            Err(e) => {
                eprintln!("  {name}: {e:#}");
                errs += 1;
            }
        }
    }
    if errs > 0 {
        bail!("summon: {errs} repo(s) failed");
    }
    Ok(())
}

async fn reconcile_one(paths: &EnvPaths, name: &str, m: &MergedRepo) -> Result<()> {
    let url = m.url.as_deref().context("no url recorded")?;
    let target = paths.shared_root.join(m.placement(name));

    if !target.exists() {
        let parent = target.parent().unwrap_or(&paths.shared_root).to_path_buf();
        std::fs::create_dir_all(&parent)?;
        let dest_name = target
            .file_name()
            .and_then(|s| s.to_str())
            .context("bad target name")?;
        let auth = m.auth_refs();
        proc::check_env(
            &parent,
            &[
                "git",
                "clone",
                "--filter=blob:none",
                "--sparse",
                "--no-checkout",
                "--",
                url,
                dest_name,
            ],
            &auth,
        )
        .await?;
    }

    // `git clone --no-checkout` leaves both the worktree and the
    // **index** empty — sparse-checkout commands operate on the
    // index, so calling `disable` or `set` on a no-checkout clone is
    // a no-op (nothing populates). The fix is two-step: first do an
    // explicit `git checkout <current-branch>` to read HEAD into
    // index + worktree (under whatever sparse cone is currently
    // configured), then adjust the cone. The checkout is also
    // idempotent for already-populated repos: `Already on branch X`
    // and zero side effects.
    let auth = m.auth_refs();
    let head_branch_raw = proc::check(&target, &["git", "symbolic-ref", "--short", "HEAD"]).await?;
    let head_branch = head_branch_raw.trim();
    proc::check_env(&target, &["git", "checkout", head_branch], &auth).await?;

    if m.paths.is_empty() {
        proc::check(&target, &["git", "sparse-checkout", "disable"]).await?;
    } else {
        let mut argv: Vec<&str> = vec!["git", "sparse-checkout", "set"];
        for p in &m.paths {
            argv.push(p.as_str());
        }
        proc::check(&target, &argv).await?;
    }
    Ok(())
}

// --- sync -----------------------------------------------------------------

enum RefKind {
    Tag(String),
    ExplicitBranch(String),
    Branchtype(String),
}

fn classify_ref(s: &str) -> RefKind {
    if let Some(rest) = s.strip_prefix("refs/tags/") {
        RefKind::Tag(rest.to_string())
    } else if let Some(rest) = s.strip_prefix("refs/heads/") {
        RefKind::ExplicitBranch(rest.to_string())
    } else {
        RefKind::Branchtype(s.to_string())
    }
}

pub async fn sync(args: SyncArgs) -> Result<()> {
    let paths = EnvPaths::discover()?;
    let merged = merge(&store::load_shared(&paths)?, &store::load_private(&paths)?);
    if merged.repos.is_empty() {
        bail!("sync: no repos registered");
    }
    match classify_ref(&args.ref_name) {
        RefKind::Tag(tag) => sync_tag(&paths, &merged, &tag).await,
        RefKind::ExplicitBranch(b) => sync_branch_each(&paths, &merged, |_| Some(b.clone())).await,
        RefKind::Branchtype(bt) => sync_branchtype(&paths, &merged, &bt).await,
    }
}

async fn sync_tag(paths: &EnvPaths, merged: &Merged, tag: &str) -> Result<()> {
    let mut errs = 0;
    let tag_ref = format!("refs/tags/{tag}");
    for (name, m) in &merged.repos {
        let dir = paths.shared_root.join(m.placement(name));
        let auth = m.auth_refs();
        let r = async {
            proc::check_env(&dir, &["git", "fetch", "--tags", "origin"], &auth).await?;
            proc::check_env(&dir, &["git", "checkout", &tag_ref], &auth).await?;
            anyhow::Ok(())
        }
        .await;
        match r {
            Ok(()) => println!("  {name}: {tag}"),
            Err(e) => {
                eprintln!("  {name}: {e:#}");
                errs += 1;
            }
        }
    }
    if errs > 0 {
        bail!("sync: {errs} repo(s) failed");
    }
    println!("ok: {} repos synced to {tag}", merged.repos.len());
    Ok(())
}

async fn sync_branchtype(paths: &EnvPaths, merged: &Merged, bt: &str) -> Result<()> {
    // Per spec: collect missing first, hard-fail with the exact list
    // before doing any work. No silent fallback to defaults.
    struct Resolved {
        name: String,
        branch: String,
        dir: PathBuf,
        auth: Vec<(String, String)>,
    }

    let mut missing: Vec<String> = Vec::new();
    let mut resolved: Vec<Resolved> = Vec::new();
    for (name, m) in &merged.repos {
        match m.branchtypes.get(bt) {
            Some(branch) => resolved.push(Resolved {
                name: name.clone(),
                branch: branch.clone(),
                dir: paths.shared_root.join(m.placement(name)),
                auth: m.auth_env.clone(),
            }),
            None => missing.push(name.clone()),
        }
    }
    if !missing.is_empty() {
        eprintln!(
            "fail: {} repo(s) have no mapping for branchtype {bt:?}:",
            missing.len()
        );
        for name in &missing {
            eprintln!("  {name}  (suggest: xen env set {name}.branchtypes.{bt}=<branch>)");
        }
        bail!("sync: missing branchtype mappings");
    }
    let mut errs = 0;
    for r in &resolved {
        let auth: Vec<(&str, &str)> = r
            .auth
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let res = async {
            proc::check_env(&r.dir, &["git", "fetch", "origin", &r.branch], &auth).await?;
            proc::check_env(
                &r.dir,
                &[
                    "git",
                    "checkout",
                    "-B",
                    &r.branch,
                    &format!("origin/{}", r.branch),
                ],
                &auth,
            )
            .await?;
            anyhow::Ok(())
        }
        .await;
        match res {
            Ok(()) => println!("  {}: {}", r.name, r.branch),
            Err(e) => {
                eprintln!("  {}: {e:#}", r.name);
                errs += 1;
            }
        }
    }
    if errs > 0 {
        bail!("sync: {errs} repo(s) failed");
    }
    println!("ok: {} repos synced to {bt}", resolved.len());
    Ok(())
}

async fn sync_branch_each<F>(paths: &EnvPaths, merged: &Merged, pick: F) -> Result<()>
where
    F: Fn(&str) -> Option<String>,
{
    let mut errs = 0;
    for (name, m) in &merged.repos {
        let Some(branch) = pick(name) else {
            continue;
        };
        let dir = paths.shared_root.join(m.placement(name));
        let auth = m.auth_refs();
        let r = async {
            proc::check_env(&dir, &["git", "fetch", "origin", &branch], &auth).await?;
            proc::check_env(
                &dir,
                &[
                    "git",
                    "checkout",
                    "-B",
                    &branch,
                    &format!("origin/{branch}"),
                ],
                &auth,
            )
            .await?;
            anyhow::Ok(())
        }
        .await;
        match r {
            Ok(()) => println!("  {name}: {branch}"),
            Err(e) => {
                eprintln!("  {name}: {e:#}");
                errs += 1;
            }
        }
    }
    if errs > 0 {
        bail!("sync: {errs} repo(s) failed");
    }
    Ok(())
}

// --- cascade --------------------------------------------------------------

pub async fn cascade(args: CascadeArgs) -> Result<()> {
    let paths = EnvPaths::discover()?;
    let merged = merge(&store::load_shared(&paths)?, &store::load_private(&paths)?);

    let cmd = args.command.join(" ");
    if cmd.trim().is_empty() {
        bail!("cascade: empty command");
    }

    // Each candidate carries its per-repo auth env so the spawn site
    // can pass it to git/sh without re-loading the merged config.
    struct Candidate {
        name: String,
        dir: PathBuf,
        auth: Vec<(String, String)>,
    }

    let mut candidates: Vec<Candidate> = merged
        .repos
        .iter()
        .map(|(name, m)| Candidate {
            name: name.clone(),
            dir: paths.shared_root.join(m.placement(name)),
            auth: m.auth_env.clone(),
        })
        .filter(|c| c.dir.exists())
        .collect();

    if args.changed_only {
        let mut keep = Vec::new();
        for c in candidates.drain(..) {
            let dirty = proc::run(&c.dir, &["git", "status", "--porcelain"])
                .await
                .map(|o| o.ok() && !o.stdout.trim().is_empty())
                .unwrap_or(false);
            let unpushed = proc::run(
                &c.dir,
                &[
                    "git",
                    "log",
                    "--branches",
                    "--not",
                    "--remotes",
                    "--oneline",
                ],
            )
            .await
            .map(|o| o.ok() && !o.stdout.trim().is_empty())
            .unwrap_or(false);
            if dirty || unpushed {
                keep.push(c);
            }
        }
        candidates = keep;
    }
    if let Some(pat) = &args.branch_pattern {
        let mut keep = Vec::new();
        for c in candidates.drain(..) {
            let branch = proc::run(
                &c.dir,
                &["git", "symbolic-ref", "--quiet", "--short", "HEAD"],
            )
            .await
            .ok()
            .and_then(|o| o.ok().then(|| o.stdout.trim().to_string()));
            if let Some(b) = branch {
                if glob_match(pat, &b) {
                    keep.push(c);
                }
            }
        }
        candidates = keep;
    }

    if candidates.is_empty() {
        eprintln!("cascade: no repos matched");
        return Ok(());
    }

    // Rule check phase: load and compile workspace rules, then run
    // matching predicates against the joined command in each
    // surviving candidate. If *any* predicate denies in *any* repo,
    // the entire cascade aborts before a single user command spawns.
    // Matches the spec's "fail fast on ambiguity" principle.
    let compiled_rules = rules::compile(&store::load_rules(&paths)?)?;
    if !compiled_rules.is_empty() {
        for c in &candidates {
            rules::check(&compiled_rules, &cmd, &c.name, &c.dir).await?;
        }
    }

    let mut set = JoinSet::new();
    for c in candidates {
        let cmd = cmd.clone();
        set.spawn(async move {
            let auth_refs: Vec<(&str, &str)> = c
                .auth
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let r = proc::sh_env(&c.dir, &cmd, &auth_refs).await;
            (c.name, r)
        });
    }

    let mut total = 0usize;
    let mut failed = 0usize;
    while let Some(joined) = set.join_next().await {
        total += 1;
        let (name, r) = joined?;
        match r {
            Ok(o) => {
                let label = if o.ok() {
                    "ok".to_string()
                } else {
                    failed += 1;
                    format!("exit {}", o.status)
                };
                // Many git commands (push, fetch, clone, ...) write
                // their progress and per-ref update lines to stderr.
                // We must surface those — otherwise `xen cascade git
                // push` looks like it does nothing. Same header on
                // both streams so terminal users see one labelled
                // block per repo regardless of which stream the
                // content came from, while pipelines that consume
                // only stdout still get clean stdout.
                let any_output = !o.stdout.is_empty() || !o.stderr.is_empty();
                if !o.stdout.is_empty() {
                    print!("=== {name} ({label}) ===\n{}", o.stdout);
                }
                if !o.stderr.is_empty() {
                    eprint!("=== {name} ({label}) ===\n{}", o.stderr);
                }
                if !any_output {
                    println!("=== {name} ({label}) ===");
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("=== {name} (spawn failed) ===\n{e}");
            }
        }
    }
    eprintln!("cascade: {total} repos, {failed} failed");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Tiny `*`/`?` glob matcher. No character classes; if more is ever
/// needed, pull in `globset`.
fn glob_match(pat: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(pc), Some(sc)) if pc == sc => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = s.chars().collect();
    go(&p, &s)
}

// --- env ------------------------------------------------------------------

pub async fn env(args: EnvArgs) -> Result<()> {
    let paths = EnvPaths::discover()?;
    match args.op {
        Some(EnvOp::Set {
            assignment,
            private,
        }) => {
            let layer = if private {
                Layer::Private
            } else {
                Layer::Shared
            };
            let (k, v) = assignment
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("expected key=value, got {assignment:?}"))?;
            let key = Key::parse(k, layer)?;
            match key {
                Key::Shared(sk) => store::write_shared(&paths, sk, v.to_string())?,
                Key::Private(pk) => store::write_private(&paths, pk, v.to_string())?,
            }
            println!("set: {k} ({})", layer.name());
        }
        Some(EnvOp::Unset { key, private }) => {
            let layer = if private {
                Layer::Private
            } else {
                Layer::Shared
            };
            let parsed = Key::parse(&key, layer)?;
            match parsed {
                Key::Shared(sk) => store::unset_shared(&paths, sk)?,
                Key::Private(pk) => store::unset_private(&paths, pk)?,
            }
            println!("unset: {key} ({})", layer.name());
        }
        Some(EnvOp::Get { key }) => {
            let merged = merge(&store::load_shared(&paths)?, &store::load_private(&paths)?);
            match lookup(&merged, &key) {
                Some(v) => println!("{v}"),
                None => bail!("env get: no value for {key:?}"),
            }
        }
        Some(EnvOp::Rules { op }) => env_rules(&paths, op).await?,
        None => {
            let shared = store::load_shared(&paths)?;
            let private = store::load_private(&paths)?;
            let rules_cfg = store::load_rules(&paths)?;
            if args.shared {
                print!("{}", toml::to_string_pretty(&shared)?);
                if !rules_cfg.rules.is_empty() {
                    println!();
                    print!("{}", toml::to_string_pretty(&rules_cfg)?);
                }
            } else if args.private {
                print!("{}", toml::to_string_pretty(&private)?);
            } else {
                print_status(&shared, &private, &rules_cfg);
            }
        }
    }
    Ok(())
}

async fn env_rules(paths: &EnvPaths, op: RulesOp) -> Result<()> {
    match op {
        RulesOp::Set { assignment } => {
            let (k, v) = assignment
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("expected key=value, got {assignment:?}"))?;
            let (name, field) = Key::parse_rule(k)?;
            store::write_shared(paths, SharedKey::Rule(name, field), v.to_string())?;
            println!("rules: set {k}");
        }
        RulesOp::Unset { key } => {
            // Two shapes accepted:
            //   - "name.field"  → unset that one field
            //   - "name"        → drop the whole rule
            if let Ok((name, field)) = Key::parse_rule(&key) {
                store::unset_shared(paths, SharedKey::Rule(name, field))?;
                println!("rules: unset {key}");
            } else if let Ok(name) = Key::parse_rule_name(&key) {
                let removed = store::unset_rule_by_name(paths, name.as_str())?;
                if removed {
                    println!("rules: removed rule {name}");
                } else {
                    bail!("rules: no rule named {name}");
                }
            } else {
                bail!("rules: invalid key {key:?}");
            }
        }
        RulesOp::Get { key } => {
            let cfg = store::load_rules(paths)?;
            // "name.field" → one value, "name" → all fields
            if let Ok((name, field)) = Key::parse_rule(&key) {
                let entry = cfg
                    .rules
                    .get(name.as_str())
                    .ok_or_else(|| anyhow::anyhow!("rules get: no rule named {name}"))?;
                let value = match field {
                    RuleField::Pattern => entry.pattern.as_deref(),
                    RuleField::Predicate => entry.predicate.as_deref(),
                    RuleField::Message => entry.message.as_deref(),
                };
                match value {
                    Some(v) => println!("{v}"),
                    None => bail!("rules get: no value for {key}"),
                }
            } else if let Ok(name) = Key::parse_rule_name(&key) {
                let entry = cfg
                    .rules
                    .get(name.as_str())
                    .ok_or_else(|| anyhow::anyhow!("rules get: no rule named {name}"))?;
                if let Some(p) = &entry.pattern {
                    println!("pattern   = {p}");
                }
                if let Some(p) = &entry.predicate {
                    println!("predicate = {p}");
                }
                if let Some(m) = &entry.message {
                    println!("message   = {m}");
                }
            } else {
                bail!("rules: invalid key {key:?}");
            }
        }
        RulesOp::List => {
            let cfg = store::load_rules(paths)?;
            if cfg.rules.is_empty() {
                println!("(no rules configured)");
                return Ok(());
            }
            for (name, r) in &cfg.rules {
                let pat = r.pattern.as_deref().unwrap_or("(unset)");
                println!("{name}");
                println!("  pattern: {pat}");
                if let Some(m) = &r.message {
                    println!("  message: {m}");
                }
            }
        }
    }
    Ok(())
}

fn lookup(merged: &Merged, dotted: &str) -> Option<String> {
    let parts: Vec<&str> = dotted.split('.').collect();
    let repo = merged.repos.get(parts[0])?;
    match parts.as_slice() {
        [_, "url"] => repo.url.clone(),
        [_, "at"] => Some(
            repo.at
                .clone()
                .unwrap_or_else(|| MergedRepo::placement(repo, parts[0])),
        ),
        [_, "paths"] => (!repo.paths.is_empty()).then(|| repo.paths.join(",")),
        [_, "branchtypes"] => (!repo.branchtypes.is_empty()).then(|| {
            repo.branchtypes
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("\n")
        }),
        [_, "branchtypes", bt] => repo.branchtypes.get(*bt).cloned(),
        _ => None,
    }
}

fn print_status(shared: &SharedConfig, private: &PrivateConfig, rules_cfg: &RulesConfig) {
    let merged = merge(shared, private);
    let total = merged.repos.len();
    println!(
        "{} repo{} registered",
        total,
        if total == 1 { "" } else { "s" }
    );
    if total == 0 {
        return;
    }

    let mut all_bts: BTreeSet<&String> = BTreeSet::new();
    for r in merged.repos.values() {
        for k in r.branchtypes.keys() {
            all_bts.insert(k);
        }
    }
    if !all_bts.is_empty() {
        println!("branchtype coverage:");
        for bt in &all_bts {
            let mut have = 0;
            let mut missing: Vec<&str> = Vec::new();
            for (name, r) in &merged.repos {
                if r.branchtypes.contains_key(bt.as_str()) {
                    have += 1;
                } else {
                    missing.push(name.as_str());
                }
            }
            print!("  {bt:8} {have}/{total}");
            if !missing.is_empty() {
                print!("   missing: {}", missing.join(", "));
            }
            println!();
        }
    }

    let sparse: Vec<&str> = merged
        .repos
        .iter()
        .filter(|(_, r)| !r.paths.is_empty())
        .map(|(n, _)| n.as_str())
        .collect();
    if !sparse.is_empty() {
        println!(
            "sparse specs: {} repo{} ({})",
            sparse.len(),
            if sparse.len() == 1 { "" } else { "s" },
            sparse.join(", "),
        );
    }

    let with_auth = merged.repos.values().filter(|r| r.auth_count > 0).count();
    println!("auth: {with_auth}/{total} configured (private)");

    if !rules_cfg.rules.is_empty() {
        println!(
            "rules: {} active ({})",
            rules_cfg.rules.len(),
            rules_cfg
                .rules
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

// --- resonate -------------------------------------------------------------
//
// Pure filesystem copy. Looks the portal up in the manifest to compute
// the destination, then byte-for-byte copies `--source` into it. No git,
// no manifest writes, no exclusions, no `.gitignore` parsing. The user
// scopes what gets copied via `--source` itself — if they don't want
// `target/` in the placement, they don't point `--source` at a directory
// that contains it.

pub async fn resonate(args: ResonateArgs) -> Result<()> {
    let paths = EnvPaths::discover()?;
    let shared = store::load_shared(&paths)?;

    let entry = shared
        .repos
        .get(args.portal.as_str())
        .ok_or_else(|| anyhow::anyhow!("resonate: unknown portal {:?}", args.portal))?;

    // Effective placement: explicit `at` from manifest, otherwise the
    // portal name. Mirrors `MergedRepo::placement` so we don't drift.
    let placement_rel = entry.at.clone().unwrap_or_else(|| args.portal.clone());
    let placement_root = paths.shared_root.join(&placement_rel);

    // `--at` is interpreted as a *subpath inside* the placement, never
    // an override. Empty / absent → placement root.
    let target_dir = match args.at.as_deref() {
        Some(sub) if !sub.is_empty() => placement_root.join(sub),
        _ => placement_root.clone(),
    };

    if !args.source.exists() {
        bail!("resonate: source {} does not exist", args.source.display());
    }

    let count = if args.source.is_dir() {
        std::fs::create_dir_all(&target_dir)
            .with_context(|| format!("resonate: creating target dir {}", target_dir.display()))?;
        copy_dir_contents(&args.source, &target_dir, args.force)?
    } else if args.source.is_file() {
        std::fs::create_dir_all(&target_dir)
            .with_context(|| format!("resonate: creating target dir {}", target_dir.display()))?;
        let basename = args
            .source
            .file_name()
            .context("resonate: source has no filename")?;
        let dest = target_dir.join(basename);
        if dest.exists() && !args.force {
            bail!(
                "resonate: {} already exists (rerun with --force to overwrite)",
                dest.display()
            );
        }
        std::fs::copy(&args.source, &dest).with_context(|| {
            format!(
                "resonate: copying {} → {}",
                args.source.display(),
                dest.display()
            )
        })?;
        1
    } else {
        bail!(
            "resonate: source {} is neither a file nor a directory",
            args.source.display()
        );
    };

    let suffix = match args.at.as_deref() {
        Some(s) if !s.is_empty() => format!("/{s}"),
        _ => String::new(),
    };
    println!(
        "resonate: {portal} ← {src} ({count} file{plural} into {placement}{suffix})",
        portal = args.portal,
        src = args.source.display(),
        plural = if count == 1 { "" } else { "s" },
        placement = placement_rel,
    );
    Ok(())
}

fn copy_dir_contents(src: &Path, dst: &Path, force: bool) -> Result<usize> {
    let mut count = 0;
    for entry in
        std::fs::read_dir(src).with_context(|| format!("resonate: reading {}", src.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        let name = entry.file_name();
        let to = dst.join(&name);
        let ft = entry.file_type()?;

        if ft.is_dir() {
            std::fs::create_dir_all(&to)
                .with_context(|| format!("resonate: creating {}", to.display()))?;
            count += copy_dir_contents(&from, &to, force)?;
        } else if ft.is_file() {
            if to.exists() && !force {
                bail!(
                    "resonate: {} already exists (rerun with --force to overwrite)",
                    to.display()
                );
            }
            std::fs::copy(&from, &to).with_context(|| {
                format!("resonate: copying {} → {}", from.display(), to.display())
            })?;
            count += 1;
        }
        // Symlinks, devices, fifos: silently skipped — resonate's job
        // is plain content, not faithful filesystem mirroring. If a
        // user needs symlink-preserving copies, they reach for `rsync`.
    }
    Ok(count)
}
