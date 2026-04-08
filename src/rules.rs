//! Workspace rules: declarative gates on cascade commands.
//!
//! A rule has three fields:
//!
//!   - `pattern` — regex matched against the joined cascade command.
//!   - `predicate` — shell command run in each candidate repo. Exit
//!     0 → allow, non-zero → deny. Gets `XEN_REPO` and `XEN_COMMAND`
//!     env vars.
//!   - `message` — surfaced in the error when the predicate denies.
//!
//! Rules fire in `verbs::cascade`, before any user command spawns. If
//! *any* rule's predicate denies in *any* repo, the **entire** cascade
//! is aborted before doing anything. Matches the spec's "fail fast on
//! ambiguity" principle: silently skipping repos that fail a rule is
//! exactly the wrong default.
//!
//! Default rules ship with xen and are seeded into a fresh workspace
//! the first time `portal add` runs against it. They're stored in
//! `.xen/rules.toml` like any other rule — visible, editable, and
//! committed to git so teammates inherit them.

use crate::proc;
use crate::store::{RulesConfig, SharedRule};
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::collections::BTreeMap;
use std::path::Path;

/// A rule loaded from `RulesConfig` with its regex compiled. We
/// validate at load-time so a malformed pattern fails loudly once,
/// not silently for every cascade.
#[derive(Debug)]
pub struct Rule {
    pub name: String,
    pub regex: Regex,
    pub predicate: String,
    pub message: String,
}

impl Rule {
    pub fn from_shared(name: &str, raw: &SharedRule) -> Result<Self> {
        if !raw.is_complete() {
            bail!("rule {name:?} is incomplete (need both `pattern` and `predicate`)");
        }
        let pattern = raw.pattern.as_ref().unwrap();
        let predicate = raw.predicate.clone().unwrap();
        let regex = Regex::new(pattern)
            .with_context(|| format!("rule {name:?}: invalid regex {pattern:?}"))?;
        Ok(Self {
            name: name.to_string(),
            regex,
            predicate,
            message: raw
                .message
                .clone()
                .unwrap_or_else(|| format!("rule {name:?} denied this command")),
        })
    }
}

/// Compile every rule in the loaded config. Skip rules that are
/// incomplete *and* have neither pattern nor predicate (a
/// half-defined rule the user is in the middle of editing).
pub fn compile(cfg: &RulesConfig) -> Result<Vec<Rule>> {
    let mut out = Vec::with_capacity(cfg.rules.len());
    for (name, raw) in &cfg.rules {
        if raw.is_empty() {
            continue;
        }
        out.push(Rule::from_shared(name, raw)?);
    }
    Ok(out)
}

/// Check every rule against `command`. For each rule whose pattern
/// matches, run the predicate in `repo_dir` (with `XEN_REPO` and
/// `XEN_COMMAND` exported). Return Err on the **first** failed
/// predicate so the cascade aborts before any user command runs.
pub async fn check(rules: &[Rule], command: &str, repo_name: &str, repo_dir: &Path) -> Result<()> {
    for rule in rules {
        if !rule.regex.is_match(command) {
            continue;
        }
        let env = [("XEN_REPO", repo_name), ("XEN_COMMAND", command)];
        // Predicate runs through plain proc::sh_env (no rule
        // recursion) so its own git invocations are unrestricted.
        let out = proc::sh_env(repo_dir, &rule.predicate, &env).await?;
        if !out.ok() {
            bail!(
                "rule {:?} denied: {} (in {repo_name})",
                rule.name,
                rule.message
            );
        }
    }
    Ok(())
}

// --- defaults -------------------------------------------------------------

/// Default rules seeded into a fresh workspace by `portal add`. Stored
/// in `.xen/rules.toml`, fully visible, editable via `xen env`,
/// removable via `xen env unset`. They are **not** hidden built-ins.
pub fn defaults() -> BTreeMap<String, SharedRule> {
    let mut out = BTreeMap::new();

    out.insert(
        "protect-main-push".to_string(),
        SharedRule {
            pattern: Some(r"^\s*git\s+push".to_string()),
            predicate: Some(PROTECT_MAIN_PUSH_PREDICATE.to_string()),
            message: Some(
                "would push to or from main/master, or use --force/--force-with-lease".to_string(),
            ),
        },
    );

    out.insert(
        "protect-main-commit".to_string(),
        SharedRule {
            pattern: Some(r"^\s*git\s+commit".to_string()),
            predicate: Some(PROTECT_MAIN_COMMIT_PREDICATE.to_string()),
            message: Some(
                "would commit while on main/master; create a feature branch first".to_string(),
            ),
        },
    );

    out
}

const PROTECT_MAIN_PUSH_PREDICATE: &str = r#"
# Deny: any push that mentions main/master as a refspec word, any
# force/force-with-lease push, any delete of main/master, and any
# bare `git push` while HEAD is on main/master.
case " $XEN_COMMAND " in
  *' main '*|*' main:'*|*':main '*|*':main:'*) exit 1 ;;
  *' master '*|*' master:'*|*':master '*|*':master:'*) exit 1 ;;
  *' --force '*|*' -f '*|*' --force-with-lease '*|*' --force-with-lease='*) exit 1 ;;
  *' --delete '*' main'*|*' --delete '*' master'*) exit 1 ;;
  *' -d '*' main'*|*' -d '*' master'*) exit 1 ;;
esac
# `git push` (no refspec) pushes the current branch — deny if that's main/master.
case "$(git symbolic-ref --short HEAD 2>/dev/null)" in
  main|master)
    case "$XEN_COMMAND" in
      'git push'|'git push '*origin|'git push origin'|'git push -u origin')
        exit 1 ;;
    esac
    ;;
esac
exit 0
"#;

const PROTECT_MAIN_COMMIT_PREDICATE: &str = r#"
case "$(git symbolic-ref --short HEAD 2>/dev/null)" in
  main|master) exit 1 ;;
esac
exit 0
"#;
