//! End-to-end CLI smoke tests for the parser surface and the env
//! verb. The full stack tests (portal/summon/sync against a real
//! local bare repo) live in `tests/e2e.rs`.

use assert_cmd::Command;
use predicates::str::contains;

fn xen() -> Command {
    Command::cargo_bin("xen").unwrap()
}

fn isolated() -> (tempfile::TempDir, tempfile::TempDir, Command) {
    let root = tempfile::tempdir().unwrap();
    let priv_dir = tempfile::tempdir().unwrap();
    let mut cmd = xen();
    cmd.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path());
    (root, priv_dir, cmd)
}

#[test]
fn version_runs() {
    xen().arg("--version").assert().success();
}

#[test]
fn help_lists_all_verbs() {
    let out = xen().arg("--help").assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    for v in ["portal", "summon", "sync", "cascade", "env", "resonate"] {
        assert!(stdout.contains(v), "help missing verb {v}: {stdout}");
    }
}

#[test]
fn env_set_writes_shared_layer() {
    let (root, _p, mut cmd) = isolated();
    cmd.args(["env", "set", "foo.url=git@github.com:x/y.git"])
        .assert()
        .success();
    let s = std::fs::read_to_string(root.path().join(".xen/repos.toml")).unwrap();
    assert!(s.contains("[repos.foo]"), "config: {s}");
    assert!(s.contains("y.git"), "config: {s}");
}

#[test]
fn env_set_auth_routes_to_private() {
    let (root, priv_dir, mut cmd) = isolated();
    cmd.args(["env", "set", "foo.key=~/.ssh/id_x", "--private"])
        .assert()
        .success();
    let s = std::fs::read_to_string(priv_dir.path().join("config.toml")).unwrap();
    assert!(s.contains("[repos.foo]"));
    assert!(s.contains("id_x"));
    assert!(!root.path().join(".xen/repos.toml").exists());
}

#[test]
fn env_refuses_auth_in_shared() {
    let (_r, _p, mut cmd) = isolated();
    cmd.args(["env", "set", "foo.key=x"])
        .assert()
        .failure()
        .stderr(contains("cannot be written to the shared"));
}

#[test]
fn env_refuses_url_in_private() {
    let (_r, _p, mut cmd) = isolated();
    cmd.args(["env", "set", "foo.url=x", "--private"])
        .assert()
        .failure()
        .stderr(contains("cannot be written to the private"));
}

#[test]
fn env_set_rejects_unknown_key() {
    let (_r, _p, mut cmd) = isolated();
    cmd.args(["env", "set", "foo.bogus=x"])
        .assert()
        .failure()
        .stderr(contains("unknown key"));
}

#[test]
fn env_unset_removes_key() {
    // Use isolated() so BOTH XEN_ROOT and XEN_PRIVATE_DIR are set —
    // otherwise the bench falls back to ~/.xen for the private dir,
    // which on Windows requires resolving HOME (or USERPROFILE) and
    // tripped CI on the windows-latest runner before the home()
    // fallback was added.
    let (root, priv_dir, _) = isolated();
    let mut a = xen();
    a.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path())
        .args(["env", "set", "foo.url=x"])
        .assert()
        .success();
    let mut b = xen();
    b.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path())
        .args(["env", "unset", "foo.url"])
        .assert()
        .success();
    // After unsetting the only field, the repos file may not exist
    // anymore (empty repo entry → removed → empty config → no save).
    let p = root.path().join(".xen/repos.toml");
    if p.exists() {
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(!s.contains("foo"), "expected foo gone from {s}");
    }
}

#[test]
fn env_status_reports_empty() {
    let (_r, _p, mut cmd) = isolated();
    cmd.arg("env")
        .assert()
        .success()
        .stdout(contains("0 repos registered"));
}

#[test]
fn cascade_requires_command() {
    xen().arg("cascade").assert().failure();
}
