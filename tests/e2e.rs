//! End-to-end tests against a real local bare repo. These exercise
//! the verbs that actually shell out to git.
//!
//! Each test sets up its own fake "remote" (a bare repo with one
//! commit on `main`) and its own fresh xen root + private dir, so
//! tests are hermetic and parallelizable.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as Std;
use tempfile::TempDir;

fn git(cwd: &Path, args: &[&str]) {
    let out = Std::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Make a bare upstream with one commit on `main`. Returns the temp
/// dir (kept alive for lifetime), and a `file://` URL pointing at it.
/// `name` becomes the bare repo's basename, which is what xen will
/// derive the repo alias from.
fn make_remote_named(name: &str) -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let bare = dir.path().join(format!("{name}.git"));
    std::fs::create_dir(&bare).unwrap();
    git(&bare, &["init", "--bare", "--initial-branch=main"]);
    // Local file:// remotes need this to honor `--filter=blob:none`.
    git(&bare, &["config", "uploadpack.allowFilter", "true"]);

    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    git(&work, &["init", "--initial-branch=main"]);
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("README.md"), "hi\n").unwrap();
    std::fs::create_dir(work.join("src")).unwrap();
    std::fs::write(work.join("src/lib.rs"), "// hi\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-m", "init"]);
    git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&work, &["push", "origin", "main"]);

    // file:// URLs are conventionally posix-style even on Windows.
    // Path::display() emits backslashes on Windows, which aren't valid
    // URL chars and which git rejects in URLs — convert to forward
    // slashes for cross-platform test stability. The result on Windows
    // is `file://C:/Users/.../upstream.git`, which both git and the
    // xen URL parser accept.
    let url = format!("file://{}", bare.display().to_string().replace('\\', "/"));
    (dir, url)
}

fn make_remote() -> (TempDir, String) {
    make_remote_named("upstream")
}

fn xen() -> Command {
    Command::cargo_bin("xen").unwrap()
}

fn isolated() -> (TempDir, TempDir) {
    (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
}

fn cmd(root: &Path, priv_dir: &Path) -> Command {
    let mut c = xen();
    c.env("XEN_ROOT", root).env("XEN_PRIVATE_DIR", priv_dir);
    c
}

fn portal_add(root: &Path, priv_dir: &Path, url: &str, extra: &[&str]) {
    let mut a = cmd(root, priv_dir);
    a.args(["portal", "add", url]);
    for e in extra {
        a.arg(e);
    }
    a.assert().success();
}

#[test]
fn portal_add_clones_at_default_basename() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);
    assert!(root.path().join("upstream").join(".git").exists());
}

#[test]
fn portal_add_does_not_materialize_working_tree() {
    // The spec promise: `portal add` clones metadata only. Anything
    // visible in the working tree (other than `.git/`) violates the
    // contract that summon is the *only* verb that puts files on
    // disk. This test pins it.
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let upstream_dir = root.path().join("upstream");
    assert!(upstream_dir.join(".git").exists());

    let leaked: Vec<String> = std::fs::read_dir(&upstream_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".git")
        .collect();
    assert!(
        leaked.is_empty(),
        "post-portal working tree should be empty except for .git/, leaked: {leaked:?}"
    );
}

#[test]
fn summon_after_portal_populates_working_tree() {
    // Counterpart to the test above: portal leaves the tree empty,
    // summon (with no paths spec) widens to the full HEAD tree.
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);
    cmd(root.path(), priv_dir.path())
        .arg("summon")
        .assert()
        .success();

    // The fixture `make_remote` commits README.md and src/lib.rs.
    assert!(root.path().join("upstream/README.md").exists());
    assert!(root.path().join("upstream/src/lib.rs").exists());
}

#[test]
fn portal_add_with_at_places_repo() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(
        root.path(),
        priv_dir.path(),
        &url,
        &["--at", "services/api"],
    );
    assert!(root.path().join("services/api/.git").exists());
}

#[test]
fn portal_overlap_is_rejected() {
    let (_remote, url) = make_remote_named("api");
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &["--at", "services"]);

    // A second, *distinctly named* repo can't claim a path nested
    // inside the first one's tree.
    let (_remote2, url2) = make_remote_named("web");
    let mut c = cmd(root.path(), priv_dir.path());
    c.args(["portal", "add", &url2, "--at", "services/web"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("overlaps"));
}

#[test]
fn sync_to_branchtype_when_mapped() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(
        root.path(),
        priv_dir.path(),
        &url,
        &["--alias", "trunk=main"],
    );
    cmd(root.path(), priv_dir.path())
        .args(["sync", "trunk"])
        .assert()
        .success();
}

#[test]
fn sync_branchtype_hard_fails_when_unmapped() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);
    cmd(root.path(), priv_dir.path())
        .args(["sync", "dev"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no mapping for branchtype"));
}

#[test]
fn summon_paths_writes_sparse_spec_and_reconciles() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    cmd(root.path(), priv_dir.path())
        .args(["summon", "upstream", "--paths", "src/"])
        .assert()
        .success();

    // env should now show a sparse spec for this repo.
    cmd(root.path(), priv_dir.path())
        .arg("env")
        .assert()
        .success()
        .stdout(predicates::str::contains("sparse specs"));

    // And src/ is checked out.
    assert!(root.path().join("upstream/src/lib.rs").exists());
}

#[test]
fn cascade_emits_both_stdout_and_stderr_on_success() {
    // Regression: git push and friends write their useful output to
    // stderr. A previous version of cascade only printed stdout on
    // success, so users couldn't see push results without `2>&1`
    // inside the cascade command. This test runs a tiny shell line
    // that writes one marker to each stream and asserts both arrive
    // at xen's corresponding stream.
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let assert_out = cmd(root.path(), priv_dir.path())
        .args(["cascade", "echo XEN_OUT_MARKER; echo XEN_ERR_MARKER 1>&2"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert_out.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert_out.get_output().stderr.clone()).unwrap();

    assert!(
        stdout.contains("XEN_OUT_MARKER"),
        "stdout should carry the stdout marker:\n{stdout}"
    );
    assert!(
        stderr.contains("XEN_ERR_MARKER"),
        "stderr should carry the stderr marker (this is the bug regression):\n{stderr}"
    );
    // Both blocks should be labelled with the repo name.
    assert!(stdout.contains("=== upstream (ok) ==="));
    assert!(stderr.contains("=== upstream (ok) ==="));
}

#[test]
fn cascade_runs_command_in_each_repo() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let out = cmd(root.path(), priv_dir.path())
        .args(["cascade", "git", "rev-parse", "HEAD"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("=== upstream (ok) ==="), "got: {stdout}");
}

#[test]
fn portal_list_shows_registered_repos() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(
        root.path(),
        priv_dir.path(),
        &url,
        &["--at", "services/api"],
    );
    cmd(root.path(), priv_dir.path())
        .args(["portal", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("upstream"))
        .stdout(predicates::str::contains("services/api"));
}

#[test]
fn cascade_propagates_git_ssh_command_when_key_is_set() {
    // The integration we agreed on: setting `<repo>.key` in private
    // env makes xen pass `GIT_SSH_COMMAND="ssh -i <path> -o
    // IdentitiesOnly=yes"` to every per-repo git spawn for that
    // repo. ssh-agent + OS keychain hold the unlocked material; xen
    // just steers which loaded identity git asks for.
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    cmd(root.path(), priv_dir.path())
        .args(["env", "set", "upstream.key=/tmp/fake_id_rsa", "--private"])
        .assert()
        .success();

    // `cascade env` runs `sh -c env` in each repo and prints every
    // env var the spawn inherited. We expect GIT_SSH_COMMAND with
    // the configured key path inside it.
    let assert_out = cmd(root.path(), priv_dir.path())
        .args(["cascade", "env"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert_out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("GIT_SSH_COMMAND="),
        "no GIT_SSH_COMMAND in cascade output:\n{stdout}"
    );
    assert!(
        stdout.contains("/tmp/fake_id_rsa"),
        "key path missing from GIT_SSH_COMMAND:\n{stdout}"
    );
    assert!(
        stdout.contains("IdentitiesOnly=yes"),
        "IdentitiesOnly missing from GIT_SSH_COMMAND:\n{stdout}"
    );
}

#[test]
fn cascade_omits_git_ssh_command_when_no_key() {
    // Without `<repo>.key`, xen leaves the env alone — ssh-agent
    // and the user's shell handle everything as they always did.
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let assert_out = cmd(root.path(), priv_dir.path())
        .args(["cascade", "env"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert_out.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("GIT_SSH_COMMAND="),
        "GIT_SSH_COMMAND was set without a `key`:\n{stdout}"
    );
}

#[test]
fn cascade_expands_tilde_in_key_path() {
    // Single-quoting the path in GIT_SSH_COMMAND blocks tilde
    // expansion at git's shell, so xen has to expand `~/` against
    // $HOME itself before quoting.
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    cmd(root.path(), priv_dir.path())
        .args(["env", "set", "upstream.key=~/.ssh/id_xen", "--private"])
        .assert()
        .success();

    // Mirror src/verbs.rs::expand_home — HOME on POSIX, USERPROFILE on
    // Windows. The Windows runner does not set HOME.
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .expect("HOME or USERPROFILE must be set");
    let assert_out = cmd(root.path(), priv_dir.path())
        .args(["cascade", "env"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert_out.get_output().stdout.clone()).unwrap();
    let expected = format!("{home}/.ssh/id_xen");
    assert!(
        stdout.contains(&expected),
        "expected expanded path {expected:?} in:\n{stdout}"
    );
    assert!(
        !stdout.contains("~/.ssh/id_xen"),
        "tilde was passed through unexpanded:\n{stdout}"
    );
}

#[test]
fn portal_add_failure_leaves_no_partial_state() {
    // Regression: a previous version of portal_add wrote the
    // manifest *before* attempting the clone, so any clone failure
    // (auth, network, bad URL) left a half-registered repo in
    // .xen/config.toml and .gitignore that the user had to clean up
    // by hand. The fix is "clone first, manifest second" — this
    // test pins it.
    let (root, priv_dir) = isolated();
    let bogus = "file:///tmp/xen-test-definitely-not-a-repo-99999.git";

    cmd(root.path(), priv_dir.path())
        .args(["portal", "add", bogus, "--at", "subdir/foo"])
        .assert()
        .failure();

    // Manifest must not contain the failed entry.
    let cfg_path = root.path().join(".xen/config.toml");
    if cfg_path.exists() {
        let cfg = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(
            !cfg.contains("subdir/foo"),
            "manifest leaked failed entry:\n{cfg}"
        );
        assert!(
            !cfg.contains("[repos.foo]"),
            "manifest leaked failed entry:\n{cfg}"
        );
    }
    // Gitignore must not list it.
    let gi_path = root.path().join(".gitignore");
    if gi_path.exists() {
        let gi = std::fs::read_to_string(&gi_path).unwrap();
        assert!(
            !gi.contains("subdir/foo"),
            "gitignore leaked failed entry:\n{gi}"
        );
    }
    // Disk: nothing materialized.
    assert!(!root.path().join("subdir/foo").exists());
}

#[test]
fn portal_add_writes_gitignore_block() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(
        root.path(),
        priv_dir.path(),
        &url,
        &["--at", "OSS/upstream"],
    );

    let gi = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
    assert!(gi.contains("/OSS/upstream/"), "got: {gi}");
    assert!(gi.contains("xen-managed"), "got: {gi}");
}

#[test]
fn portal_remove_drops_entry_from_gitignore() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(
        root.path(),
        priv_dir.path(),
        &url,
        &["--at", "OSS/upstream"],
    );

    cmd(root.path(), priv_dir.path())
        .args(["portal", "remove", "upstream"])
        .assert()
        .success();

    let gi = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
    assert!(
        !gi.contains("/OSS/upstream/"),
        "entry should be gone:\n{gi}"
    );
    // The block markers stay (now empty between them) — that's fine.
    assert!(gi.contains("xen-managed"));
}

#[test]
fn portal_preserves_user_lines_in_gitignore() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();

    // Pre-existing gitignore with user content.
    std::fs::write(root.path().join(".gitignore"), "target/\n*.log\n").unwrap();

    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let gi = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
    assert!(gi.contains("target/"), "user line dropped:\n{gi}");
    assert!(gi.contains("*.log"), "user line dropped:\n{gi}");
    assert!(gi.contains("xen-managed"));
}

#[test]
fn summon_reconciles_gitignore_on_demand() {
    // Useful for the case where a teammate clones the xen repo and
    // there's no .gitignore yet — `xen summon` should put one in
    // place even before any portal verbs run.
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    // Nuke the file and re-summon — reconcile should rebuild it.
    std::fs::remove_file(root.path().join(".gitignore")).unwrap();
    cmd(root.path(), priv_dir.path())
        .arg("summon")
        .assert()
        .success();
    let gi = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
    assert!(gi.contains("xen-managed"));
    assert!(gi.contains("/upstream/"));
}

// --- rules ----------------------------------------------------------------

#[test]
fn env_rules_set_writes_to_rules_toml() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "no-x.pattern=^git x"])
        .assert()
        .success();

    let s = std::fs::read_to_string(root.path().join(".xen/rules.toml")).unwrap();
    assert!(s.contains("[rules.no-x]"), "rules.toml: {s}");
    assert!(s.contains("^git x"), "rules.toml: {s}");
    // Repo manifest is untouched.
    let r = std::fs::read_to_string(root.path().join(".xen/repos.toml")).unwrap();
    assert!(!r.contains("no-x"), "repos.toml leaked: {r}");
}

#[test]
fn env_rules_get_field_and_whole_rule() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "guard.pattern=foo"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "guard.predicate=exit 0"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "guard.message=nope"])
        .assert()
        .success();

    // Single field
    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "get", "guard.pattern"])
        .assert()
        .success()
        .stdout(predicates::str::contains("foo"));

    // Whole rule (bare name)
    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "get", "guard"])
        .assert()
        .success()
        .stdout(predicates::str::contains("pattern"))
        .stdout(predicates::str::contains("predicate"))
        .stdout(predicates::str::contains("nope"));
}

#[test]
fn env_rules_unset_whole_rule() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "guard.pattern=foo"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "guard.predicate=exit 0"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "unset", "guard"])
        .assert()
        .success()
        .stdout(predicates::str::contains("removed rule guard"));

    // `guard` is gone but the seeded default rules are still around.
    let assert_out = cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert_out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("protect-main-push"),
        "default rule should still be listed:\n{stdout}"
    );
    assert!(
        !stdout.contains("guard"),
        "guard should have been removed:\n{stdout}"
    );
}

#[test]
fn cascade_aborts_on_rule_denial_before_running_user_command() {
    // Set a rule that denies any cascade command containing the
    // word "danger". Then run a cascade that contains that word
    // and assert it never ran the actual command.
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);
    // Materialize first so cascade has a worktree to run in.
    cmd(root.path(), priv_dir.path())
        .arg("summon")
        .assert()
        .success();

    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "no-danger.pattern=danger"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "no-danger.predicate=exit 1"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "no-danger.message=danger word"])
        .assert()
        .success();

    // The cascade command itself would create a sentinel file if it
    // ran. We use that as the "did it actually execute?" probe.
    let sentinel = root.path().join("upstream/SENTINEL_danger");
    cmd(root.path(), priv_dir.path())
        .args(["cascade", "touch SENTINEL_danger && echo danger word"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no-danger"))
        .stderr(predicates::str::contains("denied"));

    assert!(
        !sentinel.exists(),
        "rule denial should have prevented the user command from running"
    );
}

#[test]
fn cascade_allows_when_no_rule_matches() {
    // Same setup, but the cascade command doesn't contain the word
    // "danger" so the rule's regex doesn't match and the predicate
    // never runs.
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);
    cmd(root.path(), priv_dir.path())
        .arg("summon")
        .assert()
        .success();

    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "no-danger.pattern=danger"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "rules", "set", "no-danger.predicate=exit 1"])
        .assert()
        .success();

    cmd(root.path(), priv_dir.path())
        .args(["cascade", "echo safe"])
        .assert()
        .success();
}

#[test]
fn portal_seeds_default_rules_in_fresh_workspace() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    // First portal in a fresh workspace should drop default rules.
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let rules_path = root.path().join(".xen/rules.toml");
    assert!(rules_path.exists(), "rules.toml not seeded");
    let s = std::fs::read_to_string(&rules_path).unwrap();
    assert!(
        s.contains("protect-main-push"),
        "default protect-main-push rule missing:\n{s}"
    );
    assert!(
        s.contains("protect-main-commit"),
        "default protect-main-commit rule missing:\n{s}"
    );
}

#[test]
fn default_protect_main_commit_blocks_commit_on_main() {
    let (_remote, url) = make_remote();
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);
    cmd(root.path(), priv_dir.path())
        .arg("summon")
        .assert()
        .success();

    // Make a stray file so there's something to commit.
    std::fs::write(root.path().join("upstream/garbage.txt"), "x").unwrap();
    // Configure git in the fixture so commit doesn't fail on missing
    // user.email/user.name (it shouldn't even get that far).
    git(
        &root.path().join("upstream"),
        &["config", "user.email", "t@t"],
    );
    git(&root.path().join("upstream"), &["config", "user.name", "t"]);
    git(&root.path().join("upstream"), &["add", "garbage.txt"]);

    cmd(root.path(), priv_dir.path())
        .args(["cascade", "git commit -m wip"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("protect-main-commit"));
}

// --- resonate -------------------------------------------------------------
//
// Resonate is a pure filesystem copy that operates on an already-registered
// portal. It does not touch git, the manifest, or the workspace gitignore.
// These tests pin all four corners: directory copy, subpath copy, single
// file, overwrite refusal, and unknown-portal failure. They use a real
// `portal add` to set up the manifest entry — no shortcuts — so the test
// also exercises the lookup path resonate uses.

/// Build a self-contained "scratch" source directory with a small mixed
/// tree: top-level files, a nested subdirectory, multiple files per dir.
/// Returns the temp dir (kept alive) and the source path inside it.
fn make_scratch_source() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("scratch");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("README.md"), "scratch readme\n").unwrap();
    std::fs::write(src.join("Cargo.toml"), "[package]\nname = \"scratch\"\n").unwrap();
    std::fs::create_dir(src.join("src")).unwrap();
    std::fs::write(src.join("src/lib.rs"), "// lib\n").unwrap();
    std::fs::write(src.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::create_dir(src.join("docs")).unwrap();
    std::fs::write(src.join("docs/guide.md"), "guide\n").unwrap();
    (dir, src)
}

#[test]
fn resonate_copies_directory_into_placement_root() {
    let (_remote, url) = make_remote_named("scratch");
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let (_src_dir, src) = make_scratch_source();
    cmd(root.path(), priv_dir.path())
        .args(["resonate", "scratch", "--source"])
        .arg(&src)
        .assert()
        .success();

    // Files appear inside the placement (placement root = "scratch"
    // because portal add derived the name from the URL basename).
    let placement = root.path().join("scratch");
    assert!(placement.join("README.md").exists());
    assert!(placement.join("Cargo.toml").exists());
    assert!(placement.join("src/lib.rs").exists());
    assert!(placement.join("src/main.rs").exists());
    assert!(placement.join("docs/guide.md").exists());

    // .git/ from the original portal add must remain untouched —
    // resonate is filesystem-only and never reads or writes git state.
    assert!(placement.join(".git").exists());
}

#[test]
fn resonate_with_at_drops_into_subpath() {
    let (_remote, url) = make_remote_named("scratch");
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let (_src_dir, src) = make_scratch_source();
    cmd(root.path(), priv_dir.path())
        .args(["resonate", "scratch", "--at", "vendor/scratch", "--source"])
        .arg(&src)
        .assert()
        .success();

    // Source contents land under <placement>/vendor/scratch/, NOT at
    // the placement root.
    let placement = root.path().join("scratch");
    assert!(placement.join("vendor/scratch/README.md").exists());
    assert!(placement.join("vendor/scratch/src/lib.rs").exists());
    // The placement root must not have leaked top-level source files.
    assert!(!placement.join("README.md").exists());
    assert!(!placement.join("Cargo.toml").exists());
}

#[test]
fn resonate_single_file_lands_inside_target_dir() {
    let (_remote, url) = make_remote_named("scratch");
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let src_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(src_file.path(), "single file content\n").unwrap();

    cmd(root.path(), priv_dir.path())
        .args(["resonate", "scratch", "--at", "scratch_drop", "--source"])
        .arg(src_file.path())
        .assert()
        .success();

    let placement = root.path().join("scratch");
    let basename = src_file.path().file_name().unwrap();
    let dest = placement.join("scratch_drop").join(basename);
    assert!(dest.exists());
    let body = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(body, "single file content\n");
}

#[test]
fn resonate_refuses_to_overwrite_existing_files_without_force() {
    let (_remote, url) = make_remote_named("scratch");
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let (_src_dir, src) = make_scratch_source();
    // Pre-populate one of the destination files so the second resonate
    // sees a conflict on a real, unrelated file.
    let placement = root.path().join("scratch");
    std::fs::create_dir_all(placement.join("src")).unwrap();
    std::fs::write(placement.join("src/lib.rs"), "stale\n").unwrap();

    cmd(root.path(), priv_dir.path())
        .args(["resonate", "scratch", "--source"])
        .arg(&src)
        .assert()
        .failure()
        .stderr(predicates::str::contains("already exists"));

    // The pre-existing file is unchanged after the failed run.
    let body = std::fs::read_to_string(placement.join("src/lib.rs")).unwrap();
    assert_eq!(body, "stale\n");

    // --force makes it through and overwrites everything.
    cmd(root.path(), priv_dir.path())
        .args(["resonate", "scratch", "--force", "--source"])
        .arg(&src)
        .assert()
        .success();
    let body = std::fs::read_to_string(placement.join("src/lib.rs")).unwrap();
    assert_eq!(body, "// lib\n");
}

#[test]
fn resonate_unknown_portal_errors_clearly() {
    let (root, priv_dir) = isolated();
    let (_src_dir, src) = make_scratch_source();
    cmd(root.path(), priv_dir.path())
        .args(["resonate", "nonexistent", "--source"])
        .arg(&src)
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown portal"));
}

#[test]
fn resonate_missing_source_errors_clearly() {
    let (_remote, url) = make_remote_named("scratch");
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    cmd(root.path(), priv_dir.path())
        .args(["resonate", "scratch", "--source", "/nonexistent/path/xyzzy"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("does not exist"));
}

#[test]
fn resonate_does_not_touch_manifest_or_gitignore() {
    // Pin: resonate is filesystem-only. After the call, repos.toml and
    // the workspace .gitignore must be byte-identical to before.
    let (_remote, url) = make_remote_named("scratch");
    let (root, priv_dir) = isolated();
    portal_add(root.path(), priv_dir.path(), &url, &[]);

    let xen_dir = root.path().join(".xen");
    let repos_toml_before = std::fs::read(xen_dir.join("repos.toml")).unwrap();
    let gi_path = root.path().join(".gitignore");
    let gi_before = if gi_path.exists() {
        Some(std::fs::read(&gi_path).unwrap())
    } else {
        None
    };

    let (_src_dir, src) = make_scratch_source();
    cmd(root.path(), priv_dir.path())
        .args(["resonate", "scratch", "--source"])
        .arg(&src)
        .assert()
        .success();

    let repos_toml_after = std::fs::read(xen_dir.join("repos.toml")).unwrap();
    assert_eq!(
        repos_toml_before, repos_toml_after,
        "resonate must not touch the manifest"
    );
    let gi_after = if gi_path.exists() {
        Some(std::fs::read(&gi_path).unwrap())
    } else {
        None
    };
    assert_eq!(
        gi_before, gi_after,
        "resonate must not touch the workspace .gitignore"
    );
}

// --- hooks ----------------------------------------------------------------
//
// End-to-end: configure a hook via `xen env hooks set`, then run a
// xen verb whose joined argv matches the hook's regex, and verify the
// hook's `exec` shell command actually fired in the workspace root.
//
// All these tests use `^env$` as the hook pattern — a strict match
// for the bare `xen env` invocation only. That way the setup commands
// (`env hooks set ...`) don't self-trigger the hook during their own
// runs, which would otherwise create a chicken-and-egg situation
// where setting field N+1 fires the half-defined hook from field N.

#[test]
fn post_hook_fires_after_matching_verb() {
    let (root, priv_dir) = isolated();
    let marker = root.path().join(".post-hook-marker");
    // Forward-slash form so the path round-trips through `sh -c` on
    // Windows. `Path::display()` emits backslashes there, which bash
    // would silently treat as escape characters and write the file
    // to a garbage relative location. The Path-level `marker.exists()`
    // check below still uses the original Path and resolves either form.
    let marker_str = marker.display().to_string().replace('\\', "/");

    // Configure a complete hook: matches bare `xen env`, executes
    // `touch <marker>` at workspace root, fires post (default).
    cmd(root.path(), priv_dir.path())
        .args(["env", "hooks", "set", "marker.match=^env$"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args([
            "env",
            "hooks",
            "set",
            &format!("marker.exec=touch {}", marker_str),
        ])
        .assert()
        .success();

    assert!(!marker.exists(), "marker should not exist before trigger");

    // Bare `xen env` matches `^env$` → post-hook fires → marker created.
    cmd(root.path(), priv_dir.path())
        .arg("env")
        .assert()
        .success();

    assert!(
        marker.exists(),
        "post-hook should have created {}",
        marker.display()
    );
}

#[test]
fn pre_hook_fires_before_verb_and_can_abort() {
    let (root, priv_dir) = isolated();

    // Pre-hook on `^env$` that always fails. Configure all three
    // fields explicitly so phase = pre is set deliberately.
    cmd(root.path(), priv_dir.path())
        .args(["env", "hooks", "set", "blocker.match=^env$"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "hooks", "set", "blocker.exec=exit 7"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "hooks", "set", "blocker.when=pre"])
        .assert()
        .success();

    // Bare `xen env` should fail because the pre-hook exits non-zero.
    // The error message should mention the hook name so the user can
    // tell where the abort came from.
    cmd(root.path(), priv_dir.path())
        .arg("env")
        .assert()
        .failure()
        .stderr(predicates::str::contains("blocker"));
}

#[test]
fn hook_does_not_fire_when_pattern_does_not_match() {
    let (root, priv_dir) = isolated();
    let marker = root.path().join(".should-not-exist");
    // Same forward-slash normalization as `post_hook_fires_after_matching_verb`
    // — see that test for the rationale.
    let marker_str = marker.display().to_string().replace('\\', "/");

    // Hook that matches `^sync$` only — bare `xen env` shouldn't fire it.
    cmd(root.path(), priv_dir.path())
        .args(["env", "hooks", "set", "h.match=^sync$"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args([
            "env",
            "hooks",
            "set",
            &format!("h.exec=touch {}", marker_str),
        ])
        .assert()
        .success();

    cmd(root.path(), priv_dir.path())
        .arg("env")
        .assert()
        .success();
    assert!(!marker.exists());
}

#[test]
fn post_hook_failure_fails_the_command() {
    let (root, priv_dir) = isolated();
    cmd(root.path(), priv_dir.path())
        .args(["env", "hooks", "set", "fail-after.match=^env$"])
        .assert()
        .success();
    cmd(root.path(), priv_dir.path())
        .args(["env", "hooks", "set", "fail-after.exec=exit 3"])
        .assert()
        .success();

    // The verb itself succeeds (env prints status), but the failing
    // post-hook surfaces as the overall non-zero exit.
    cmd(root.path(), priv_dir.path())
        .arg("env")
        .assert()
        .failure()
        .stderr(predicates::str::contains("fail-after"));
}

#[test]
fn incomplete_hook_is_inactive_and_does_not_break_xen() {
    // A half-defined hook (only `match` set, no `exec`) must be
    // silently inactive — running any other xen verb must still work.
    // This is the property that lets `xen env hooks set` configure a
    // hook one field at a time without locking the user out.
    let (root, priv_dir) = isolated();
    cmd(root.path(), priv_dir.path())
        .args(["env", "hooks", "set", "halfdone.match=^.*$"])
        .assert()
        .success();
    // No `exec` set yet — hook is incomplete. Running any verb must
    // still succeed. `xen env` is a no-state-required verb here.
    cmd(root.path(), priv_dir.path())
        .arg("env")
        .assert()
        .success();
}

// Suppress unused-warning for the helper if a future test removes it.
#[allow(dead_code)]
fn _force_use(_: PathBuf) {}
