//! TOML-backed env storage.
//!
//! The shared layer (`<root>/.xen/`) is **two files**, one per
//! concept:
//!
//!   - `repos.toml` — per-repo manifest (URL, placement, branchtype
//!     aliases, sparse spec)
//!   - `rules.toml` — workspace-level cascade rules
//!
//! The private layer (`~/.xen/private.toml` by default) stays as one
//! file because it's small and entirely per-repo.
//!
//! Splitting the shared layer into separate files keeps two unrelated
//! namespaces — repos and rules — from colliding in a single flat
//! TOML table. The dotted-path keyspace dispatches by first segment:
//! `rules.<name>.<field>` lives in `rules.toml`, anything else lives
//! in `repos.toml`. There is no reserved-names list.
//!
//! Locations are discovered with these env vars (mainly for tests and
//! for users who want explicit control):
//!
//!   - `XEN_ROOT`         — overrides the shared root (default: cwd).
//!   - `XEN_PRIVATE_DIR`  — overrides the private dir (default: `~/.xen`).
//!
//! The typed mutators below — `write_shared`, `unset_shared`,
//! `write_shared_rule`, `unset_shared_rule`, `write_private`,
//! `unset_private` — are the only paths that touch env. Their
//! signatures enforce the load-bearing invariant: a `SharedKey::Rule`
//! variant routes to rules.toml, repo variants route to repos.toml,
//! and "auth in shared" remains unrepresentable.

use crate::key::{AuthField, PrivateKey, RuleField, SharedKey};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct EnvPaths {
    pub shared_root: PathBuf,
    pub shared_dir: PathBuf,
    pub private_dir: PathBuf,
}

impl EnvPaths {
    pub fn discover() -> Result<Self> {
        let shared_root = match std::env::var_os("XEN_ROOT") {
            Some(v) => PathBuf::from(v),
            None => std::env::current_dir().context("cwd")?,
        };
        let shared_dir = shared_root.join(".xen");
        let private_dir = match std::env::var_os("XEN_PRIVATE_DIR") {
            Some(v) => PathBuf::from(v),
            None => home()?.join(".xen"),
        };
        Ok(Self {
            shared_root,
            shared_dir,
            private_dir,
        })
    }

    pub fn repos_file(&self) -> PathBuf {
        self.shared_dir.join("repos.toml")
    }
    pub fn rules_file(&self) -> PathBuf {
        self.shared_dir.join("rules.toml")
    }
    pub fn private_file(&self) -> PathBuf {
        self.private_dir.join("config.toml")
    }

    /// Pre-split workspaces stored everything in `.xen/config.toml`.
    /// On load we transparently fall back to it; on next save we
    /// switch to `repos.toml` and the legacy file becomes orphaned.
    pub fn legacy_shared_file(&self) -> PathBuf {
        self.shared_dir.join("config.toml")
    }
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))
}

// --- on-disk types --------------------------------------------------------

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct SharedConfig {
    #[serde(default)]
    pub repos: BTreeMap<String, SharedRepo>,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct SharedRepo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub branchtypes: BTreeMap<String, String>,
}

impl SharedRepo {
    fn is_empty(&self) -> bool {
        self.url.is_none()
            && self.at.is_none()
            && self.paths.is_empty()
            && self.branchtypes.is_empty()
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct RulesConfig {
    #[serde(default)]
    pub rules: BTreeMap<String, SharedRule>,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct SharedRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl SharedRule {
    pub fn is_empty(&self) -> bool {
        self.pattern.is_none() && self.predicate.is_none() && self.message.is_none()
    }
    pub fn is_complete(&self) -> bool {
        self.pattern.is_some() && self.predicate.is_some()
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct PrivateConfig {
    #[serde(default)]
    pub repos: BTreeMap<String, PrivateRepo>,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct PrivateRepo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helper: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub branchtypes: BTreeMap<String, String>,
}

impl PrivateRepo {
    fn is_empty(&self) -> bool {
        self.key.is_none()
            && self.helper.is_none()
            && self.token.is_none()
            && self.at.is_none()
            && self.paths.is_empty()
            && self.branchtypes.is_empty()
    }
}

// --- io -------------------------------------------------------------------

pub fn load_shared(paths: &EnvPaths) -> Result<SharedConfig> {
    // Prefer the new file. Fall back to the legacy single-file shape
    // so workspaces created before the split keep loading.
    let repos = paths.repos_file();
    if repos.exists() {
        return load(&repos);
    }
    let legacy = paths.legacy_shared_file();
    if legacy.exists() {
        return load(&legacy);
    }
    Ok(SharedConfig::default())
}

pub fn load_rules(paths: &EnvPaths) -> Result<RulesConfig> {
    load(&paths.rules_file())
}

pub fn load_private(paths: &EnvPaths) -> Result<PrivateConfig> {
    load(&paths.private_file())
}

fn load<T: Default + serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let s = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))
}

fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let s = toml::to_string_pretty(value)?;
    fs::write(path, s).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn save_shared(paths: &EnvPaths, cfg: &SharedConfig) -> Result<()> {
    save(&paths.repos_file(), cfg)?;
    // One-shot migration: if the legacy file is still around after we
    // wrote the new one, drop it. Idempotent — only fires once.
    let legacy = paths.legacy_shared_file();
    if legacy.exists() {
        let _ = fs::remove_file(&legacy);
    }
    Ok(())
}

pub fn save_rules(paths: &EnvPaths, cfg: &RulesConfig) -> Result<()> {
    save(&paths.rules_file(), cfg)
}

pub fn save_private(paths: &EnvPaths, cfg: &PrivateConfig) -> Result<()> {
    save(&paths.private_file(), cfg)
}

// --- typed mutators -------------------------------------------------------

pub fn write_shared(paths: &EnvPaths, key: SharedKey, value: String) -> Result<()> {
    match key {
        SharedKey::Rule(name, field) => {
            let mut cfg = load_rules(paths)?;
            apply_rule(&mut cfg, name.as_str(), field, Some(value));
            save_rules(paths, &cfg)
        }
        repo_key => {
            let mut cfg = load_shared(paths)?;
            apply_shared(&mut cfg, repo_key, Some(value));
            save_shared(paths, &cfg)
        }
    }
}

pub fn unset_shared(paths: &EnvPaths, key: SharedKey) -> Result<()> {
    match key {
        SharedKey::Rule(name, field) => {
            let mut cfg = load_rules(paths)?;
            apply_rule(&mut cfg, name.as_str(), field, None);
            save_rules(paths, &cfg)
        }
        repo_key => {
            let mut cfg = load_shared(paths)?;
            apply_shared(&mut cfg, repo_key, None);
            save_shared(paths, &cfg)
        }
    }
}

/// Drop a whole rule by name. Used by `xen env rules unset <name>`
/// when no field is given.
pub fn unset_rule_by_name(paths: &EnvPaths, name: &str) -> Result<bool> {
    let mut cfg = load_rules(paths)?;
    let removed = cfg.rules.remove(name).is_some();
    if removed {
        save_rules(paths, &cfg)?;
    }
    Ok(removed)
}

pub fn write_private(paths: &EnvPaths, key: PrivateKey, value: String) -> Result<()> {
    let mut cfg = load_private(paths)?;
    apply_private(&mut cfg, key, Some(value));
    save_private(paths, &cfg)
}

pub fn unset_private(paths: &EnvPaths, key: PrivateKey) -> Result<()> {
    let mut cfg = load_private(paths)?;
    apply_private(&mut cfg, key, None);
    save_private(paths, &cfg)
}

fn apply_shared(cfg: &mut SharedConfig, key: SharedKey, value: Option<String>) {
    let repo_name = match &key {
        SharedKey::Url(r)
        | SharedKey::At(r)
        | SharedKey::Paths(r)
        | SharedKey::Branchtype(r, _) => r.as_str().to_string(),
        SharedKey::Rule(_, _) => unreachable!("Rule routed to apply_rule above"),
    };
    let entry = cfg.repos.entry(repo_name.clone()).or_default();
    match key {
        SharedKey::Url(_) => entry.url = value,
        SharedKey::At(_) => entry.at = value,
        SharedKey::Paths(_) => entry.paths = value.map(parse_paths_value).unwrap_or_default(),
        SharedKey::Branchtype(_, b) => match value {
            Some(v) => {
                entry.branchtypes.insert(b.as_str().to_string(), v);
            }
            None => {
                entry.branchtypes.remove(b.as_str());
            }
        },
        SharedKey::Rule(_, _) => unreachable!(),
    }
    if entry.is_empty() {
        cfg.repos.remove(&repo_name);
    }
}

fn apply_rule(cfg: &mut RulesConfig, name: &str, field: RuleField, value: Option<String>) {
    let entry = cfg.rules.entry(name.to_string()).or_default();
    match field {
        RuleField::Pattern => entry.pattern = value,
        RuleField::Predicate => entry.predicate = value,
        RuleField::Message => entry.message = value,
    }
    if entry.is_empty() {
        cfg.rules.remove(name);
    }
}

fn apply_private(cfg: &mut PrivateConfig, key: PrivateKey, value: Option<String>) {
    let repo_name = match &key {
        PrivateKey::Auth(r, _)
        | PrivateKey::AtOverride(r)
        | PrivateKey::BranchtypeOverride(r, _)
        | PrivateKey::PathsOverride(r) => r.as_str().to_string(),
    };
    let entry = cfg.repos.entry(repo_name.clone()).or_default();
    match key {
        PrivateKey::Auth(_, AuthField::SshKeyPath) => entry.key = value,
        PrivateKey::Auth(_, AuthField::CredentialHelper) => entry.helper = value,
        PrivateKey::Auth(_, AuthField::TokenRef) => entry.token = value,
        PrivateKey::AtOverride(_) => entry.at = value,
        PrivateKey::PathsOverride(_) => {
            entry.paths = value.map(parse_paths_value).unwrap_or_default()
        }
        PrivateKey::BranchtypeOverride(_, b) => match value {
            Some(v) => {
                entry.branchtypes.insert(b.as_str().to_string(), v);
            }
            None => {
                entry.branchtypes.remove(b.as_str());
            }
        },
    }
    if entry.is_empty() {
        cfg.repos.remove(&repo_name);
    }
}

/// Comma-separated paths, trimmed. The spec leaves the array syntax
/// open; this is the placeholder until something better lands.
pub fn parse_paths_value(s: String) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}
