//! Workspace hooks: declarative actions that run before or after a
//! xen verb invocation.
//!
//! A hook has three fields:
//!
//!   - `match` — regex matched against the joined argv after `xen`.
//!     `xen sync main` produces the string `sync main`; the hook's
//!     pattern is matched against that.
//!   - `exec` — shell command run at the workspace root when the
//!     pattern matches and the phase fires.
//!   - `when` — `pre` or `post` (default `post`). Pre-hooks run
//!     *before* the verb dispatches and abort it on non-zero exit
//!     (analogous to rules). Post-hooks run *after* the verb completes
//!     successfully and surface their own non-zero exit as the
//!     command's exit code.
//!
//! Hooks are the symmetric pair to rules:
//!
//!   - rules gate cascade-internal commands at pre-time (deny on failure)
//!   - hooks react to xen verb invocations at pre- or post-time
//!
//! Both share the same regex + shell-predicate shape, both live in the
//! shared layer, both are inspectable via `xen env`. The split is
//! intentional: rules concern *what cascade is allowed to do*, hooks
//! concern *what should happen around xen itself*.
//!
//! Hooks are stored in `.xen/hooks.toml` and managed via
//! `xen env hooks set/unset/get/list`.

use crate::proc;
use crate::store::{HooksConfig, SharedHook};
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::path::Path;

/// Which side of the verb a hook fires on.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Phase {
    /// Runs before the verb dispatches. Non-zero exit aborts the verb.
    Pre,
    /// Runs after the verb completes successfully. Non-zero exit
    /// surfaces as the command's overall exit code.
    Post,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pre => "pre",
            Self::Post => "post",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "pre" => Ok(Self::Pre),
            "post" => Ok(Self::Post),
            other => bail!("hook `when` must be \"pre\" or \"post\", got {other:?}"),
        }
    }
}

/// A hook loaded from `HooksConfig` with its regex compiled. We
/// validate at load-time so a malformed pattern fails loudly once,
/// not silently every time the hook would have fired.
#[derive(Debug)]
pub struct Hook {
    pub name: String,
    pub regex: Regex,
    pub exec: String,
    pub phase: Phase,
}

impl Hook {
    pub fn from_shared(name: &str, raw: &SharedHook) -> Result<Self> {
        if !raw.is_complete() {
            bail!("hook {name:?} is incomplete (need both `match` and `exec`)");
        }
        let pattern = raw.match_pattern.as_ref().unwrap();
        let exec = raw.exec.clone().unwrap();
        let regex = Regex::new(pattern)
            .with_context(|| format!("hook {name:?}: invalid regex {pattern:?}"))?;
        // Default to `post` when `when` is unset — the most common
        // case is "do this after the verb succeeds".
        let phase = match raw.when.as_deref() {
            Some(s) => Phase::parse(s).with_context(|| format!("hook {name:?}: invalid `when`"))?,
            None => Phase::Post,
        };
        Ok(Self {
            name: name.to_string(),
            regex,
            exec,
            phase,
        })
    }
}

/// Compile every hook in the loaded config. Skip hooks that are
/// **not yet complete** (missing `match` or `exec`) — that lets the
/// user configure a hook one field at a time via `xen env hooks set`
/// without each in-between set tripping its own half-built hook.
/// Hooks become active the moment both required fields are present.
///
/// Unlike rules — which are only consulted inside `xen cascade`,
/// where the chicken-and-egg problem doesn't exist — hooks are
/// consulted around *every* verb invocation, including the very
/// commands the user runs to define them. A strict compile() here
/// would lock the user out of `xen env hooks set` between fields.
pub fn compile(cfg: &HooksConfig) -> Result<Vec<Hook>> {
    let mut out = Vec::with_capacity(cfg.hooks.len());
    for (name, raw) in &cfg.hooks {
        if !raw.is_complete() {
            continue;
        }
        out.push(Hook::from_shared(name, raw)?);
    }
    Ok(out)
}

/// Execute every hook in `hooks` whose phase matches `phase` and
/// whose regex matches `command`. The shell command for each runs in
/// `workspace_root`, with `XEN_COMMAND` exported.
///
/// Returns Err on the first failed hook so callers can decide what to
/// do (pre-hook failure → abort the verb; post-hook failure → surface
/// as exit code).
pub async fn run_matching(
    hooks: &[Hook],
    command: &str,
    phase: Phase,
    workspace_root: &Path,
) -> Result<()> {
    for hook in hooks {
        if hook.phase != phase {
            continue;
        }
        if !hook.regex.is_match(command) {
            continue;
        }
        let env = [("XEN_COMMAND", command)];
        let out = proc::sh_env(workspace_root, &hook.exec, &env).await?;
        // Stream the hook's output through xen's own streams so the
        // user sees what happened. Both streams labelled with the
        // hook name + phase so it's obvious where the output came
        // from when several hooks fire.
        if !out.stdout.is_empty() {
            print!("[{} {}]\n{}", hook.phase.as_str(), hook.name, out.stdout);
        }
        if !out.stderr.is_empty() {
            eprint!("[{} {}]\n{}", hook.phase.as_str(), hook.name, out.stderr);
        }
        if !out.ok() {
            bail!(
                "hook {:?} ({}) failed: exit {}",
                hook.name,
                hook.phase.as_str(),
                out.status
            );
        }
    }
    Ok(())
}
