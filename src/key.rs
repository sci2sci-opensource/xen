//! The env keyspace.
//!
//! Per the spec: *"the pre-design that has to happen before any code"*.
//!
//! Every legal key in xen's config is enumerated here, tagged at the
//! type level with shared-vs-private. The load-bearing invariants —
//!
//!   - auth never written to the shared layer
//!   - URL never written to the private layer
//!   - branchtype hard-fail on missing alias (enforced in `verbs::sync`)
//!
//! become *function signatures*, not runtime checks. `write_shared`
//! only accepts a `SharedKey`; `write_private` only accepts a
//! `PrivateKey`. "Auth in shared" is therefore unrepresentable, not
//! caught by a runtime guard.
//!
//! Three of the categories — `at`, `paths`, `branchtypes` — are
//! shared-by-default but may be *overridden* per-machine in the private
//! layer. Each gets a sibling variant on `PrivateKey`, so the override
//! still flows through `write_private` and the same type-level rules
//! apply.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A repo alias. Newtyped so a stray `String` can't be passed where a
/// repo name is expected. Names live in dotted-path keys, so `.`,
/// slashes, and whitespace are rejected at construction.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd, Serialize, Deserialize)]
pub struct RepoName(String);

impl RepoName {
    pub fn new(s: impl Into<String>) -> Result<Self, KeyError> {
        let s = s.into();
        if s.is_empty() || s.chars().any(|c| c == '.' || c == '/' || c.is_whitespace()) {
            return Err(KeyError::InvalidRepoName(s));
        }
        Ok(Self(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RepoName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A branchtype name. The *value* it maps to is a half-refspec like
/// `develop`; the branchtype is the identifier on the LHS.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Branchtype(String);

impl Branchtype {
    pub fn new(s: impl Into<String>) -> Result<Self, KeyError> {
        let s = s.into();
        if s.is_empty() || s.chars().any(|c| c == '.' || c == '/' || c.is_whitespace()) {
            return Err(KeyError::InvalidBranchtype(s));
        }
        Ok(Self(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Branchtype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A workspace-level rule name. Same character constraints as
/// `RepoName` so the dotted-path keyspace stays uniform.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd, Serialize, Deserialize)]
pub struct RuleName(String);

impl RuleName {
    pub fn new(s: impl Into<String>) -> Result<Self, KeyError> {
        let s = s.into();
        if s.is_empty() || s.chars().any(|c| c == '.' || c == '/' || c.is_whitespace()) {
            return Err(KeyError::InvalidRuleName(s));
        }
        Ok(Self(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One field on a rule. Adding a new rule field is a single variant
/// here plus a match arm in `apply_rule` — by design.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum RuleField {
    Pattern,
    Predicate,
    Message,
}

impl RuleField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
            Self::Predicate => "predicate",
            Self::Message => "message",
        }
    }
}

/// Auth field. Each variant is a thing the spec calls out by name.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum AuthField {
    SshKeyPath,
    CredentialHelper,
    TokenRef,
}

impl AuthField {
    #[allow(dead_code)] // used by Key::dotted, which is used in tests
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SshKeyPath => "key",
            Self::CredentialHelper => "helper",
            Self::TokenRef => "token",
        }
    }
}

/// Which env layer a write targets. The user steers this with
/// `--shared` (default) or `--private`.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Layer {
    Shared,
    Private,
}

impl Layer {
    pub fn name(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Private => "private",
        }
    }
}

/// Keys that live in the **shared** layer (`.xen/`, committed to git).
///
/// Two on-disk files back this enum: per-repo variants live in
/// `.xen/repos.toml`, the `Rule` variant lives in `.xen/rules.toml`.
/// `store::write_shared` dispatches by variant.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum SharedKey {
    Url(RepoName),
    At(RepoName),
    Branchtype(RepoName, Branchtype),
    Paths(RepoName),
    Rule(RuleName, RuleField),
}

/// Keys that live in the **private** layer (`~/.xen/`, never committed).
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum PrivateKey {
    Auth(RepoName, AuthField),
    AtOverride(RepoName),
    BranchtypeOverride(RepoName, Branchtype),
    PathsOverride(RepoName),
}

/// Layer-tagged key. Built by `parse`, which the verbs call with the
/// user's chosen target layer. Construction is the *only* place
/// shared-vs-private gets decided.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Key {
    Shared(SharedKey),
    Private(PrivateKey),
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("invalid repo name: {0:?}")]
    InvalidRepoName(String),
    #[error("invalid branchtype: {0:?}")]
    InvalidBranchtype(String),
    #[error("invalid rule name: {0:?}")]
    InvalidRuleName(String),
    #[error("unknown key: {0:?}")]
    Unknown(String),
    #[error("key {0:?} cannot be written to the {1} layer")]
    WrongLayer(String, &'static str),
}

impl Key {
    /// Parse a **repo-shaped** dotted key, given the layer the user
    /// is writing to. `Layer::Shared` is the default at the call
    /// site.
    ///
    /// This is the parser used by `xen env set/get/unset`. Rules
    /// have their own parser (`parse_rule`) and live under the
    /// `xen env rules` sub-namespace; they never go through here.
    pub fn parse(dotted: &str, layer: Layer) -> Result<Self, KeyError> {
        let parts: Vec<&str> = dotted.split('.').collect();
        let here = || dotted.to_string();
        match (parts.as_slice(), layer) {
            // shared-only
            ([repo, "url"], Layer::Shared) => {
                Ok(Key::Shared(SharedKey::Url(RepoName::new(*repo)?)))
            }
            ([_, "url"], Layer::Private) => Err(KeyError::WrongLayer(here(), "private")),

            // private-only (auth)
            ([repo, "key"], Layer::Private) => Ok(Key::Private(PrivateKey::Auth(
                RepoName::new(*repo)?,
                AuthField::SshKeyPath,
            ))),
            ([repo, "helper"], Layer::Private) => Ok(Key::Private(PrivateKey::Auth(
                RepoName::new(*repo)?,
                AuthField::CredentialHelper,
            ))),
            ([repo, "token"], Layer::Private) => Ok(Key::Private(PrivateKey::Auth(
                RepoName::new(*repo)?,
                AuthField::TokenRef,
            ))),
            ([_, "key" | "helper" | "token"], Layer::Shared) => {
                Err(KeyError::WrongLayer(here(), "shared"))
            }

            // overlapping (default shared, can be overridden private)
            ([repo, "at"], Layer::Shared) => Ok(Key::Shared(SharedKey::At(RepoName::new(*repo)?))),
            ([repo, "at"], Layer::Private) => {
                Ok(Key::Private(PrivateKey::AtOverride(RepoName::new(*repo)?)))
            }
            ([repo, "paths"], Layer::Shared) => {
                Ok(Key::Shared(SharedKey::Paths(RepoName::new(*repo)?)))
            }
            ([repo, "paths"], Layer::Private) => Ok(Key::Private(PrivateKey::PathsOverride(
                RepoName::new(*repo)?,
            ))),
            ([repo, "branchtypes", bt], Layer::Shared) => Ok(Key::Shared(SharedKey::Branchtype(
                RepoName::new(*repo)?,
                Branchtype::new(*bt)?,
            ))),
            ([repo, "branchtypes", bt], Layer::Private) => Ok(Key::Private(
                PrivateKey::BranchtypeOverride(RepoName::new(*repo)?, Branchtype::new(*bt)?),
            )),

            _ => Err(KeyError::Unknown(here())),
        }
    }

    #[allow(dead_code)] // round-trip helper, used by tests
    pub fn dotted(&self) -> String {
        match self {
            Key::Shared(SharedKey::Url(r)) => format!("{r}.url"),
            Key::Shared(SharedKey::At(r)) => format!("{r}.at"),
            Key::Shared(SharedKey::Branchtype(r, b)) => format!("{r}.branchtypes.{b}"),
            Key::Shared(SharedKey::Paths(r)) => format!("{r}.paths"),
            Key::Shared(SharedKey::Rule(name, f)) => format!("{name}.{}", f.as_str()),
            Key::Private(PrivateKey::Auth(r, f)) => format!("{r}.{}", f.as_str()),
            Key::Private(PrivateKey::AtOverride(r)) => format!("{r}.at"),
            Key::Private(PrivateKey::BranchtypeOverride(r, b)) => format!("{r}.branchtypes.{b}"),
            Key::Private(PrivateKey::PathsOverride(r)) => format!("{r}.paths"),
        }
    }

    /// Parse a **rule-shaped** dotted key. Rules live under
    /// `xen env rules` and never overlap with the repo keyspace.
    ///
    /// Returns the (name, field) pair on success. The verb layer
    /// turns it into `SharedKey::Rule(name, field)` for storage.
    pub fn parse_rule(dotted: &str) -> Result<(RuleName, RuleField), KeyError> {
        let parts: Vec<&str> = dotted.split('.').collect();
        match parts.as_slice() {
            [name, "pattern"] => Ok((RuleName::new(*name)?, RuleField::Pattern)),
            [name, "predicate"] => Ok((RuleName::new(*name)?, RuleField::Predicate)),
            [name, "message"] => Ok((RuleName::new(*name)?, RuleField::Message)),
            _ => Err(KeyError::Unknown(dotted.to_string())),
        }
    }

    /// Parse a **bare rule name** with no field — used by
    /// `xen env rules unset <name>` to drop a whole rule at once.
    pub fn parse_rule_name(dotted: &str) -> Result<RuleName, KeyError> {
        if dotted.contains('.') {
            return Err(KeyError::Unknown(dotted.to_string()));
        }
        RuleName::new(dotted)
    }

    #[allow(dead_code)] // used by tests
    pub fn layer(&self) -> Layer {
        match self {
            Key::Shared(_) => Layer::Shared,
            Key::Private(_) => Layer::Private,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_shared(dotted: &str) {
        let k = Key::parse(dotted, Layer::Shared).expect("parse shared");
        assert_eq!(k.dotted(), dotted);
        assert_eq!(k.layer(), Layer::Shared);
    }
    fn rt_private(dotted: &str) {
        let k = Key::parse(dotted, Layer::Private).expect("parse private");
        assert_eq!(k.dotted(), dotted);
        assert_eq!(k.layer(), Layer::Private);
    }

    #[test]
    fn shared_only_keys() {
        rt_shared("foo.url");
        assert!(matches!(
            Key::parse("foo.url", Layer::Private),
            Err(KeyError::WrongLayer(_, "private"))
        ));
    }

    #[test]
    fn private_only_keys() {
        rt_private("foo.key");
        rt_private("foo.helper");
        rt_private("foo.token");
        for k in ["foo.key", "foo.helper", "foo.token"] {
            assert!(matches!(
                Key::parse(k, Layer::Shared),
                Err(KeyError::WrongLayer(_, "shared"))
            ));
        }
    }

    #[test]
    fn overlapping_keys_can_target_either_layer() {
        rt_shared("foo.at");
        rt_private("foo.at");
        rt_shared("foo.paths");
        rt_private("foo.paths");
        rt_shared("foo.branchtypes.dev");
        rt_private("foo.branchtypes.dev");
    }

    #[test]
    fn unknown_key_rejected() {
        assert!(Key::parse("foo.bogus", Layer::Shared).is_err());
        assert!(Key::parse("foo", Layer::Shared).is_err());
        assert!(Key::parse("", Layer::Shared).is_err());
    }

    #[test]
    fn rule_field_keys_parse_via_parse_rule() {
        let (name, field) = Key::parse_rule("protect-main-push.pattern").unwrap();
        assert_eq!(name.as_str(), "protect-main-push");
        assert_eq!(field, RuleField::Pattern);

        for f in ["pattern", "predicate", "message"] {
            assert!(Key::parse_rule(&format!("foo.{f}")).is_ok());
        }
    }

    #[test]
    fn parse_rule_rejects_malformed() {
        assert!(Key::parse_rule("").is_err());
        assert!(Key::parse_rule("foo").is_err()); // no field
        assert!(Key::parse_rule("foo.bogus").is_err());
        assert!(Key::parse_rule("foo.pattern.extra").is_err());
    }

    #[test]
    fn parse_rule_name_accepts_bare_name() {
        assert_eq!(Key::parse_rule_name("foo").unwrap().as_str(), "foo");
        assert!(Key::parse_rule_name("foo.pattern").is_err());
        assert!(Key::parse_rule_name("").is_err());
    }

    #[test]
    fn repo_and_rule_with_same_name_use_different_parsers() {
        // The CLI dispatches: `xen env set foo.url=...` uses
        // Key::parse, `xen env rules set foo.pattern=...` uses
        // Key::parse_rule. Both succeed for the same name `foo`,
        // each producing a key in its own keyspace.
        assert!(Key::parse("foo.url", Layer::Shared).is_ok());
        assert!(Key::parse_rule("foo.pattern").is_ok());
    }

    #[test]
    fn invalid_repo_name_rejected() {
        assert!(RepoName::new("").is_err());
        assert!(RepoName::new("a/b").is_err());
        assert!(RepoName::new("a.b").is_err());
        assert!(RepoName::new("a b").is_err());
    }
}
