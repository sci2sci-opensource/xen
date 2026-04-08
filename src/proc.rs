//! The subprocess primitive.
//!
//! There is no typed git wrapper in xen. Verbs that need to talk to
//! git compose this primitive with literal argv at the call site —
//! exactly the same shape `cascade` uses to fan user commands out
//! across repos. Cascade is both the user-facing escape hatch *and*
//! the internal implementation pattern; the verbs are "the cases
//! where xen happens to know the right argv".
//!
//! Two entry points, one for each shape we actually use:
//!
//!   - `run`/`check` — direct exec with explicit argv. The verbs use
//!     this so spaces, quoting, and the user's `$SHELL` aren't in
//!     the loop.
//!   - `sh`         — `sh -c <string>`. Cascade uses this so the user's
//!     command can be a real shell line.
//!
//! Each shape has an `_env` sibling that takes per-spawn environment
//! variables on top of the inherited shell env. Per-repo auth lives
//! there: `verbs::merge` builds a `GIT_SSH_COMMAND` from a repo's
//! private `key` (and friends) and the spawn site passes it through.
//!
//! `GIT_TERMINAL_PROMPT=0` is set on every spawn so auth failures
//! surface as non-zero exits rather than hangs. This blocks the
//! terminal prompt path; ssh-agent and OS keyring integrations work
//! exactly as they do for any other git call.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

/// Direct exec with no extra env. The common case.
pub async fn run(cwd: &Path, argv: &[&str]) -> Result<Output> {
    run_env(cwd, argv, &[]).await
}

/// Run-and-bail-on-non-zero with no extra env.
pub async fn check(cwd: &Path, argv: &[&str]) -> Result<String> {
    check_env(cwd, argv, &[]).await
}

/// Direct exec inside `cwd` with the supplied env vars layered on
/// top of inherited ones.
pub async fn run_env(cwd: &Path, argv: &[&str], env: &[(&str, &str)]) -> Result<Output> {
    let (prog, rest) = argv.split_first().context("empty argv")?;
    let mut cmd = Command::new(prog);
    cmd.args(rest)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd
        .output()
        .await
        .with_context(|| format!("spawn {prog} in {}", cwd.display()))?;
    Ok(Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

pub async fn check_env(cwd: &Path, argv: &[&str], env: &[(&str, &str)]) -> Result<String> {
    let o = run_env(cwd, argv, env).await?;
    if !o.ok() {
        bail!("{:?} failed: {}", argv, o.stderr.trim());
    }
    Ok(o.stdout)
}

pub async fn sh_env(cwd: &Path, command: &str, env: &[(&str, &str)]) -> Result<Output> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd
        .output()
        .await
        .with_context(|| format!("spawn sh -c {command:?} in {}", cwd.display()))?;
    Ok(Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}
