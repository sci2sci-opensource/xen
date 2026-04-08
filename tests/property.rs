//! Property-style tests against synthetic multi-repo layouts.
//!
//! For each seed in a fixed list we generate a deterministic-but-
//! random layout (N repos, random branch sets, random placements,
//! random file trees), materialize it into a set of real bare
//! upstream repos, run xen against it, then use **git itself as
//! ground truth** to verify the result.
//!
//! What's actually being tested here, beyond what `tests/e2e.rs`
//! covers with hand-written cases:
//!
//!   - the cross-product of placement / branch-set / branchtype-
//!     coverage shapes catches things a small fixture won't
//!   - the "hard-fail leaves no mutation" guarantee for `xen sync`
//!     when one or more repos lack the branchtype mapping (we
//!     snapshot HEADs before, run sync, and assert HEADs are
//!     unchanged after the failure)
//!   - the cascade output is parseable and the SHAs it reports
//!     match what `git rev-parse HEAD` says directly
//!   - `summon` is idempotent
//!
//! Seeds are SplitMix64 — deterministic, no extra dep needed. Adding
//! a seed to `SEEDS` is the only knob.

use assert_cmd::Command;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command as Std;
use tempfile::TempDir;

// --- prng -----------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// inclusive `lo..=hi`
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next_u64() as usize) % (hi - lo + 1)
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next_u64() as usize) % xs.len()]
    }
    fn chance(&mut self, num: u32, den: u32) -> bool {
        (self.next_u64() as u32) % den < num
    }
}

// --- layout types ---------------------------------------------------------

#[derive(Debug, Clone)]
struct Layout {
    repos: Vec<RepoSpec>,
}

#[derive(Debug, Clone)]
struct RepoSpec {
    name: String,
    placement: String,
    branches: Vec<String>,
    branchtypes: BTreeMap<String, String>, // alias -> branch
    files: Vec<String>,
}

const REPO_NAMES: &[&str] = &[
    "api",
    "web",
    "worker",
    "gateway",
    "billing",
    "search",
    "cache",
    "queue",
    "scheduler",
    "notifier",
    "frontend",
    "backend",
    "shared",
    "metrics",
    "logger",
    "router",
    "proxy",
    "ingest",
];

const PLACEMENT_PREFIXES: &[&str] = &["", "services/", "libs/", "apps/", "modules/"];
const FILE_DIRS: &[&str] = &["src", "tests", "docs", "lib", "internal"];

fn gen_layout(seed: u64) -> Layout {
    let mut rng = Rng::new(seed);
    let n = rng.range(2, 6);

    let mut names: Vec<String> = REPO_NAMES.iter().map(|s| s.to_string()).collect();
    for i in (1..names.len()).rev() {
        let j = (rng.next_u64() as usize) % (i + 1);
        names.swap(i, j);
    }
    names.truncate(n);

    let mut repos: Vec<RepoSpec> = Vec::new();
    for name in &names {
        let prefix = rng.pick(PLACEMENT_PREFIXES);
        let placement = format!("{prefix}{name}");

        // Mirror xen's overlap rule. With our prefix pool overlap is
        // rare but possible (e.g. one repo at root named the same as
        // a prefix segment of another). Skip the colliding entry.
        if repos
            .iter()
            .any(|r| paths_overlap(&placement, &r.placement))
        {
            continue;
        }

        let mut branches = vec!["main".to_string()];
        if rng.chance(60, 100) {
            branches.push("develop".to_string());
        }
        if rng.chance(40, 100) {
            branches.push("release".to_string());
        }
        if rng.chance(20, 100) {
            branches.push("trunk".to_string());
        }

        let mut branchtypes = BTreeMap::new();
        // `main` is always mapped — gives us at least one universal
        // branchtype to test the "all repos covered" code path.
        branchtypes.insert("main".to_string(), "main".to_string());
        if branches.iter().any(|b| b == "develop") {
            branchtypes.insert("dev".to_string(), "develop".to_string());
        }
        if branches.iter().any(|b| b == "release") {
            branchtypes.insert("release".to_string(), "release".to_string());
        }
        if branches.iter().any(|b| b == "trunk") {
            branchtypes.insert("trunk".to_string(), "trunk".to_string());
        }

        // Random file tree.
        let blocks = rng.range(1, 4);
        let mut files = Vec::new();
        for _ in 0..blocks {
            let d = rng.pick(FILE_DIRS);
            let count = rng.range(1, 3);
            for k in 0..count {
                files.push(format!("{d}/file{k}.txt"));
            }
        }
        files.sort();
        files.dedup();

        repos.push(RepoSpec {
            name: name.clone(),
            placement,
            branches,
            branchtypes,
            files,
        });
    }

    assert!(!repos.is_empty(), "seed {seed}: empty layout");
    Layout { repos }
}

fn paths_overlap(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

// --- materialize ----------------------------------------------------------

#[derive(Debug)]
struct Materialized {
    repos: BTreeMap<String, MatRepo>,
    _dir: TempDir,
}

#[derive(Debug)]
struct MatRepo {
    url: String,
    /// Branch name → SHA in the bare. The ground truth.
    heads: BTreeMap<String, String>,
}

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

fn git_capture(cwd: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn materialize(layout: &Layout) -> Materialized {
    let dir = tempfile::tempdir().unwrap();
    let mut repos = BTreeMap::new();
    for spec in &layout.repos {
        let bare = dir.path().join(format!("{}.git", spec.name));
        std::fs::create_dir(&bare).unwrap();
        git(&bare, &["init", "--bare", "--initial-branch=main"]);
        git(&bare, &["config", "uploadpack.allowFilter", "true"]);

        let work = dir.path().join(format!("work-{}", spec.name));
        std::fs::create_dir(&work).unwrap();
        git(&work, &["init", "--initial-branch=main"]);
        git(&work, &["config", "user.email", "t@t"]);
        git(&work, &["config", "user.name", "t"]);

        // Initial main commit with the layout's files.
        std::fs::write(work.join("README.md"), format!("# {}\n", spec.name)).unwrap();
        for f in &spec.files {
            let p = work.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, format!("{}: {f}\n", spec.name)).unwrap();
        }
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "init"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["push", "origin", "main"]);

        let mut heads = BTreeMap::new();
        heads.insert(
            "main".to_string(),
            git_capture(&work, &["rev-parse", "main"]),
        );

        // Each extra branch gets a unique commit so its HEAD differs
        // from main's. Branch off of main to keep histories simple.
        for branch in &spec.branches {
            if branch == "main" {
                continue;
            }
            git(&work, &["checkout", "-B", branch, "main"]);
            std::fs::write(
                work.join(format!("{branch}.marker")),
                format!("on {branch}\n"),
            )
            .unwrap();
            git(&work, &["add", "."]);
            git(&work, &["commit", "-m", &format!("on {branch}")]);
            git(&work, &["push", "origin", branch]);
            heads.insert(branch.clone(), git_capture(&work, &["rev-parse", branch]));
        }

        // Forward-slash form so file:// URLs round-trip on Windows.
        // See the matching comment in tests/e2e.rs make_remote_named.
        let url = format!("file://{}", bare.display().to_string().replace('\\', "/"));
        repos.insert(spec.name.clone(), MatRepo { url, heads });
    }
    Materialized { repos, _dir: dir }
}

// --- xen drivers ----------------------------------------------------------

fn xen(root: &Path, priv_dir: &Path) -> Command {
    let mut c = Command::cargo_bin("xen").unwrap();
    c.env("XEN_ROOT", root).env("XEN_PRIVATE_DIR", priv_dir);
    c
}

fn portal_all(root: &Path, priv_dir: &Path, layout: &Layout, mat: &Materialized) {
    for spec in &layout.repos {
        let url = &mat.repos[&spec.name].url;
        let mut c = xen(root, priv_dir);
        c.args(["portal", "add", url, "--at", &spec.placement]);
        for (k, v) in &spec.branchtypes {
            c.args(["--alias", &format!("{k}={v}")]);
        }
        c.assert().success();
    }
}

// --- truth checks ---------------------------------------------------------

fn assert_portal_truth(seed: u64, root: &Path, layout: &Layout, mat: &Materialized) {
    for spec in &layout.repos {
        let dir = root.join(&spec.placement);
        assert!(
            dir.join(".git").exists(),
            "seed {seed}: portal didn't create {}",
            dir.display()
        );
        let origin = git_capture(&dir, &["remote", "get-url", "origin"]);
        let expected = &mat.repos[&spec.name].url;
        assert_eq!(
            origin.trim_end_matches('/'),
            expected.trim_end_matches('/'),
            "seed {seed}: origin mismatch for {}",
            spec.name
        );
    }
}

fn assert_sync_truth(
    seed: u64,
    root: &Path,
    layout: &Layout,
    mat: &Materialized,
    branchtype: &str,
) {
    for spec in &layout.repos {
        let branch = spec
            .branchtypes
            .get(branchtype)
            .expect("caller verified mapping exists");
        let dir = root.join(&spec.placement);
        let head = git_capture(&dir, &["rev-parse", "HEAD"]);
        let expected = &mat.repos[&spec.name].heads[branch];
        assert_eq!(
            &head, expected,
            "seed {seed}: sync {branchtype}: {} HEAD mismatch",
            spec.name
        );
    }
}

fn capture_heads(root: &Path, layout: &Layout) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for spec in &layout.repos {
        let dir = root.join(&spec.placement);
        out.insert(spec.name.clone(), git_capture(&dir, &["rev-parse", "HEAD"]));
    }
    out
}

// --- cascade output parsing ----------------------------------------------

/// Cascade prints `=== <name> (ok) ===\n<stdout>` per repo. Order is
/// non-deterministic (parallel JoinSet), so we collect into a map.
fn parse_cascade_output(s: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut buf = String::new();
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("=== ") {
            if let Some(name) = current.take() {
                out.insert(name, std::mem::take(&mut buf));
            }
            let name_end = rest.find(" (").unwrap_or(rest.len());
            current = Some(rest[..name_end].to_string());
        } else if current.is_some() {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(line);
        }
    }
    if let Some(name) = current.take() {
        out.insert(name, buf);
    }
    out
}

// --- scenarios ------------------------------------------------------------

struct Bed {
    layout: Layout,
    mat: Materialized,
    root: TempDir,
    priv_dir: TempDir,
}

fn setup(seed: u64) -> Bed {
    let layout = gen_layout(seed);
    let mat = materialize(&layout);
    let root = tempfile::tempdir().unwrap();
    let priv_dir = tempfile::tempdir().unwrap();
    Bed {
        layout,
        mat,
        root,
        priv_dir,
    }
}

fn scenario_portal_then_verify(seed: u64) {
    let bed = setup(seed);
    portal_all(bed.root.path(), bed.priv_dir.path(), &bed.layout, &bed.mat);
    assert_portal_truth(seed, bed.root.path(), &bed.layout, &bed.mat);
}

fn scenario_sync_main(seed: u64) {
    let bed = setup(seed);
    portal_all(bed.root.path(), bed.priv_dir.path(), &bed.layout, &bed.mat);
    xen(bed.root.path(), bed.priv_dir.path())
        .args(["sync", "main"])
        .assert()
        .success();
    assert_sync_truth(seed, bed.root.path(), &bed.layout, &bed.mat, "main");
}

/// Counters returned from a single seed run, so the top-level test
/// can confirm the seed list actually exercises both code paths.
#[derive(Default)]
struct CoverageCounts {
    universal_succeeded: usize,
    nonuniversal_hardfailed: usize,
}

fn scenario_sync_branchtype_coverage(seed: u64) -> CoverageCounts {
    let bed = setup(seed);
    portal_all(bed.root.path(), bed.priv_dir.path(), &bed.layout, &bed.mat);

    let mut all_bts: BTreeSet<String> = BTreeSet::new();
    for r in &bed.layout.repos {
        for k in r.branchtypes.keys() {
            all_bts.insert(k.clone());
        }
    }

    let mut counts = CoverageCounts::default();
    for bt in &all_bts {
        let universal = bed
            .layout
            .repos
            .iter()
            .all(|r| r.branchtypes.contains_key(bt));
        if universal {
            xen(bed.root.path(), bed.priv_dir.path())
                .args(["sync", bt])
                .assert()
                .success();
            assert_sync_truth(seed, bed.root.path(), &bed.layout, &bed.mat, bt);
            counts.universal_succeeded += 1;
        } else {
            let before = capture_heads(bed.root.path(), &bed.layout);
            xen(bed.root.path(), bed.priv_dir.path())
                .args(["sync", bt])
                .assert()
                .failure()
                .stderr(predicates::str::contains("no mapping for branchtype"));
            let after = capture_heads(bed.root.path(), &bed.layout);
            assert_eq!(
                before, after,
                "seed {seed}: sync {bt} hard-failed but mutated HEADs"
            );
            counts.nonuniversal_hardfailed += 1;
        }
    }
    counts
}

fn scenario_cascade_matches_git_truth(seed: u64) {
    let bed = setup(seed);
    portal_all(bed.root.path(), bed.priv_dir.path(), &bed.layout, &bed.mat);

    let assert_out = xen(bed.root.path(), bed.priv_dir.path())
        .args(["cascade", "git", "rev-parse", "HEAD"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert_out.get_output().stdout.clone()).unwrap();
    let parsed = parse_cascade_output(&stdout);

    for spec in &bed.layout.repos {
        let dir: PathBuf = bed.root.path().join(&spec.placement);
        let truth = git_capture(&dir, &["rev-parse", "HEAD"]);
        let xen_says = parsed.get(&spec.name).unwrap_or_else(|| {
            panic!(
                "seed {seed}: cascade missing repo {} in stdout:\n{stdout}",
                spec.name
            )
        });
        assert_eq!(
            xen_says.trim(),
            truth,
            "seed {seed}: cascade HEAD for {} disagrees with git ground truth",
            spec.name
        );
    }
}

fn scenario_summon_idempotent(seed: u64) {
    let bed = setup(seed);
    portal_all(bed.root.path(), bed.priv_dir.path(), &bed.layout, &bed.mat);

    xen(bed.root.path(), bed.priv_dir.path())
        .arg("summon")
        .assert()
        .success();
    let a = capture_heads(bed.root.path(), &bed.layout);
    xen(bed.root.path(), bed.priv_dir.path())
        .arg("summon")
        .assert()
        .success();
    let b = capture_heads(bed.root.path(), &bed.layout);
    assert_eq!(a, b, "seed {seed}: summon not idempotent");
}

// --- the seed list --------------------------------------------------------

const SEEDS: &[u64] = &[1, 2, 3, 5, 7, 11, 13, 17, 19, 23];

#[test]
fn property_portal_then_verify() {
    for &s in SEEDS {
        scenario_portal_then_verify(s);
    }
}

#[test]
fn property_sync_main_universal() {
    for &s in SEEDS {
        scenario_sync_main(s);
    }
}

#[test]
fn property_sync_branchtype_coverage_or_hardfail() {
    let mut total = CoverageCounts::default();
    for &s in SEEDS {
        let c = scenario_sync_branchtype_coverage(s);
        total.universal_succeeded += c.universal_succeeded;
        total.nonuniversal_hardfailed += c.nonuniversal_hardfailed;
    }
    // Confirm the seed list isn't accidentally one-sided. If this
    // fires, the generator distribution drifted and the test stopped
    // exercising one of the two paths it claims to cover.
    assert!(
        total.universal_succeeded > 0,
        "no universal-branchtype sync was exercised across SEEDS"
    );
    assert!(
        total.nonuniversal_hardfailed > 0,
        "no missing-mapping hard-fail was exercised across SEEDS"
    );
    eprintln!(
        "branchtype coverage: {} universal/success, {} nonuniversal/hardfail",
        total.universal_succeeded, total.nonuniversal_hardfailed
    );
}

#[test]
fn property_cascade_matches_git_truth() {
    for &s in SEEDS {
        scenario_cascade_matches_git_truth(s);
    }
}

#[test]
fn property_summon_idempotent() {
    for &s in SEEDS {
        scenario_summon_idempotent(s);
    }
}
