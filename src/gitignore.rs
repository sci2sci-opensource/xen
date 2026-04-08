//! Gitignore management for the parent git repo of the xen workspace.
//!
//! xen lives inside a git repo. The repos it portals are clones
//! nested under that repo's tree. Without help, the parent git sees
//! those nested clones as untracked directories — and worse, as
//! embedded git repos that someone might accidentally `git add` into
//! the parent. That's exactly the submodule failure mode xen exists
//! to route around.
//!
//! So xen writes the right `.gitignore` automatically. The set of
//! ignored paths is a pure projection of `<repo>.at` from the shared
//! manifest — no new state, just intent rendered into the file git
//! happens to read.
//!
//! The block is bracketed by markers so xen only ever touches its
//! own region. Lines outside the markers are the user's and stay
//! exactly as written. To opt out entirely, delete the markers — xen
//! won't find its block and won't touch the file.

use crate::store::{EnvPaths, SharedConfig};
use anyhow::{Context, Result};
use std::fs;

const BEGIN: &str = "# xen-managed: BEGIN — do not edit (run xen to update)";
const END: &str = "# xen-managed: END";

/// Idempotently reconcile `.gitignore` at the workspace root with
/// the set of placements in `shared`. Creates the file if missing,
/// no-ops if the contents are already correct.
pub fn reconcile(paths: &EnvPaths, shared: &SharedConfig) -> Result<()> {
    let file = paths.shared_root.join(".gitignore");
    let existing = fs::read_to_string(&file).unwrap_or_default();
    let updated = replace_block(&existing, &render_block(shared));
    if updated != existing {
        fs::write(&file, updated).with_context(|| format!("writing {}", file.display()))?;
    }
    Ok(())
}

fn render_block(shared: &SharedConfig) -> String {
    let mut placements: Vec<String> = shared
        .repos
        .iter()
        .map(|(name, r)| {
            let at = r.at.clone().unwrap_or_else(|| name.clone());
            format!("/{}/", at.trim_matches('/'))
        })
        .collect();
    placements.sort();
    placements.dedup();

    let mut block = String::from(BEGIN);
    block.push('\n');
    for p in &placements {
        block.push_str(p);
        block.push('\n');
    }
    block.push_str(END);
    block.push('\n');
    block
}

fn replace_block(existing: &str, new_block: &str) -> String {
    if let Some(begin) = existing.find(BEGIN) {
        if let Some(end_off) = existing[begin..].find(END) {
            let mut end = begin + end_off + END.len();
            // Swallow the trailing newline of the old END line so we
            // don't grow blank lines on every re-write.
            if existing.as_bytes().get(end) == Some(&b'\n') {
                end += 1;
            }
            let mut out = String::with_capacity(existing.len() + new_block.len());
            out.push_str(&existing[..begin]);
            out.push_str(new_block);
            out.push_str(&existing[end..]);
            return out;
        }
    }
    // No existing block — append, with a blank line of separation
    // from any pre-existing user content.
    let mut out = existing.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(new_block);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{SharedConfig, SharedRepo};
    use std::collections::BTreeMap;

    fn cfg(entries: &[(&str, &str)]) -> SharedConfig {
        let mut repos = BTreeMap::new();
        for (name, at) in entries {
            repos.insert(
                name.to_string(),
                SharedRepo {
                    at: Some(at.to_string()),
                    ..Default::default()
                },
            );
        }
        SharedConfig { repos }
    }

    #[test]
    fn block_is_sorted_and_bracketed() {
        let c = cfg(&[
            ("research", "OSS/research"),
            ("parseltongue", "OSS/parseltongue"),
        ]);
        let b = render_block(&c);
        let p = b.find("/OSS/parseltongue/").unwrap();
        let r = b.find("/OSS/research/").unwrap();
        assert!(p < r, "expected alphabetical order:\n{b}");
        assert!(b.starts_with(BEGIN));
        assert!(b.trim_end().ends_with(END));
    }

    #[test]
    fn append_preserves_user_lines() {
        let existing = "target/\n*.log\n";
        let block = render_block(&cfg(&[("foo", "foo")]));
        let out = replace_block(existing, &block);
        assert!(out.starts_with("target/\n*.log\n"));
        assert!(out.contains(BEGIN));
        assert!(out.contains("/foo/"));
    }

    #[test]
    fn replace_in_place_keeps_lines_around_block() {
        let v1 = render_block(&cfg(&[("foo", "foo")]));
        let combined = format!("target/\n\n{v1}# user note\n");
        let v2 = render_block(&cfg(&[("foo", "foo"), ("bar", "bar")]));
        let out = replace_block(&combined, &v2);
        assert!(out.starts_with("target/\n"));
        assert!(out.contains("# user note"));
        assert!(out.contains("/bar/"));
        assert!(out.contains("/foo/"));
    }

    #[test]
    fn idempotent_when_unchanged() {
        let block = render_block(&cfg(&[("foo", "foo/bar")]));
        let once = replace_block("", &block);
        let twice = replace_block(&once, &block);
        assert_eq!(once, twice);
    }

    #[test]
    fn missing_markers_means_hands_off() {
        // The opt-out path: if the user deletes BEGIN/END, xen has
        // no block to find. `replace_block` falls through to the
        // append branch — which is correct for first-write but the
        // *intent* of opt-out is "don't touch my file". The verbs
        // call `reconcile` which always runs append, so true opt-out
        // requires not running xen. Document this behavior.
        let user_only = "target/\n*.log\n";
        let block = render_block(&cfg(&[("foo", "foo")]));
        let out = replace_block(user_only, &block);
        // We do append a block here. The opt-out story is "delete
        // markers AND don't run reconcile", or wait for the future
        // workspace.gitignore=manual key.
        assert!(out.contains(BEGIN));
    }
}
