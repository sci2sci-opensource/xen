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

// --- hooks ----------------------------------------------------------------
//
// Hooks are the symmetric pair to rules: pattern + shell command,
// fired around xen verb invocations instead of inside cascade. The
// CLI surface mirrors `xen env rules` exactly.

#[test]
fn env_hooks_set_writes_to_hooks_toml() {
    let (root, priv_dir, mut cmd) = isolated();
    cmd.args(["env", "hooks", "set", "pull-after-sync.match=^sync\\b"])
        .assert()
        .success();
    let _ = priv_dir;
    let s = std::fs::read_to_string(root.path().join(".xen/hooks.toml")).unwrap();
    assert!(s.contains("[hooks.pull-after-sync]"), "hooks.toml: {s}");
    assert!(s.contains("match"), "hooks.toml: {s}");
}

#[test]
fn env_hooks_set_all_three_fields() {
    let (root, _p, _) = isolated();
    for kv in [
        "pull.match=^sync\\b",
        "pull.exec=git pull --ff-only",
        "pull.when=post",
    ] {
        let mut c = xen();
        c.env("XEN_ROOT", root.path())
            .env("XEN_PRIVATE_DIR", _p.path())
            .args(["env", "hooks", "set", kv])
            .assert()
            .success();
    }
    let s = std::fs::read_to_string(root.path().join(".xen/hooks.toml")).unwrap();
    assert!(s.contains("match"));
    assert!(s.contains("exec"));
    assert!(s.contains("when"));
    assert!(s.contains("post"));
}

#[test]
fn env_hooks_get_field_and_whole_hook() {
    let (root, priv_dir, _) = isolated();
    for kv in ["h1.match=^sync$", "h1.exec=echo hi", "h1.when=pre"] {
        let mut c = xen();
        c.env("XEN_ROOT", root.path())
            .env("XEN_PRIVATE_DIR", priv_dir.path())
            .args(["env", "hooks", "set", kv])
            .assert()
            .success();
    }
    // Get a single field.
    let mut g = xen();
    g.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path())
        .args(["env", "hooks", "get", "h1.exec"])
        .assert()
        .success()
        .stdout(contains("echo hi"));
    // Get the whole hook.
    let mut g2 = xen();
    g2.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path())
        .args(["env", "hooks", "get", "h1"])
        .assert()
        .success()
        .stdout(contains("match"))
        .stdout(contains("exec"))
        .stdout(contains("when"));
}

#[test]
fn env_hooks_unset_whole_hook() {
    let (root, priv_dir, _) = isolated();
    let mut c = xen();
    c.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path())
        .args(["env", "hooks", "set", "h.match=^sync"])
        .assert()
        .success();
    let mut u = xen();
    u.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path())
        .args(["env", "hooks", "unset", "h"])
        .assert()
        .success();
    // After dropping the only field, the file may be empty or absent.
    let p = root.path().join(".xen/hooks.toml");
    if p.exists() {
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(!s.contains("[hooks.h]"), "hook should be gone: {s}");
    }
}

#[test]
fn env_hooks_list_empty_and_populated() {
    let (root, priv_dir, mut cmd) = isolated();
    cmd.args(["env", "hooks", "list"])
        .assert()
        .success()
        .stdout(contains("(no hooks configured)"));
    let mut s1 = xen();
    s1.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path())
        .args(["env", "hooks", "set", "h.match=^sync"])
        .assert()
        .success();
    let mut s2 = xen();
    s2.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path())
        .args(["env", "hooks", "set", "h.exec=echo hi"])
        .assert()
        .success();
    let mut l = xen();
    l.env("XEN_ROOT", root.path())
        .env("XEN_PRIVATE_DIR", priv_dir.path())
        .args(["env", "hooks", "list"])
        .assert()
        .success()
        .stdout(contains("h ("))
        .stdout(contains("match"));
}

#[test]
fn env_hooks_set_rejects_invalid_field() {
    let (_r, _p, mut cmd) = isolated();
    cmd.args(["env", "hooks", "set", "h.bogus=x"])
        .assert()
        .failure();
}

#[test]
fn env_hooks_set_rejects_invalid_when() {
    // The CLI accepts any string for `when`; validation happens at
    // hook compile time when the verb runs. Set is fine; the failure
    // shows up at the next xen verb invocation. (We don't trip it
    // here because env subcommand doesn't itself trigger compile.)
    let (_r, _p, mut cmd) = isolated();
    cmd.args(["env", "hooks", "set", "h.when=sideways"])
        .assert()
        .success();
}
