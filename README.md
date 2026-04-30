# xen

**xen is a virtual-monorepo layer that materializes independent git repos into one queryable workspace without collapsing their gitflows.**

Each child repo keeps its own remote, its own history, its own branches, its own CI, its own merge model. xen never holds commits about other repos' commits. From the workspace root you get one queryable layout — sparse where you want, full where you don't, dirty / clean / drifted visible across the fleet — and from inside any child you get a normal git repo with its normal upstream and its normal PR flow.

The mechanism is small: xen stores **intent** — which repos exist, where they materialize, what `dev` means in each, which paths are visible. Git stores **facts** — what's fetched, what SHA you're on, what's dirty. The two layers never overlap, and **there are no SHAs in xen's manifest, ever**. That's what makes the children stay independent and the workspace stay live.

This is the load-bearing rule. `repo`, `git-meta`, and submodules all blur the line by recording SHAs in the intent layer, which is why they all grow an update treadmill. xen does not pin, does not snapshot, does not remember what it last fetched. A teammate cloning your xen workspace gets the same *description* of the stack you have; what they materialize from it is between them and git.

## Install

### One-liner

```bash
curl -fsSL https://raw.githubusercontent.com/sci2sci-opensource/xen/master/install.sh | sh
```

### Homebrew (macOS / Linux)

```bash
brew install sci2sci-opensource/xen/xen
```

### Cargo

```bash
cargo install xen-space
```

### Ubuntu / Debian (.deb)

```bash
VERSION=$(curl -sSL https://api.github.com/repos/sci2sci-opensource/xen/releases/latest \
  | grep '"tag_name"' | cut -d'"' -f4 | sed 's/^v//')
ARCH=$(dpkg --print-architecture)
curl -sSL -o xen.deb \
  "https://github.com/sci2sci-opensource/xen/releases/download/v${VERSION}/xen_${VERSION}_${ARCH}.deb"
sudo dpkg -i xen.deb
```

### Build from source

```bash
git clone https://github.com/sci2sci-opensource/xen.git
cd xen
cargo install --path .
```

### Verify a download

```bash
curl -sSL https://github.com/sci2sci-opensource/xen/releases/latest/download/SHA256SUMS \
  | shasum -a 256 -c --ignore-missing
```

The one-liner detects OS + arch, downloads the matching static binary from the corresponding GitHub Release, drops it in `~/.local/bin`. No prerequisites beyond `curl`, `tar`, and a POSIX shell. Override the version with `XEN_VERSION=v0.1.0 sh install.sh`, the install location with `XEN_INSTALL_DIR=...`.

## Quickstart

```sh
# Bootstrap a workspace
cd ~/work/my-stack
xen portal add git@github.com:org/api.git --at services/api
xen portal add git@github.com:org/web.git --at services/web
xen portal add git@github.com:org/shared-lib.git --at libs/shared
xen summon                                 # materialize all working trees
xen sync main                              # everyone on main, fast-forwarded

# Day-to-day
xen cascade --changed-only git status -sb              # show dirty repos
xen cascade --changed-only git checkout -b feature/x   # branch only where you touched
xen cascade --branch-pattern='feature/x' \
  'git add -A && git commit -m "..." && git push -u origin feature/x'

# Selective materialization
xen summon api --paths=src/,tests/                     # sparse spec, recorded in env
xen env unset api.paths && xen summon api              # widen back out
```

## The verbs

Every feature request gets tested against two questions: (1) is it intent or fact? If fact, refuse — git owns it. (2) If intent, is it an action or an env mutation? Anything that doesn't fit one of those slots — or sit cleanly at the filesystem layer below them — doesn't belong in xen.

| Plane           | Verb       | What it does                                                  |
|-----------------|------------|---------------------------------------------------------------|
| Git-level       | `portal`   | Register / unregister / list repos. Clones metainfo only.     |
| Git-level       | `summon`   | Reconcile on-disk state to match env intent. Idempotent.      |
| Git-level       | `sync`     | Resolve a branchtype/tag and fast-forward each repo to it.    |
| Git-level       | `cascade`  | `git -C <path> <cmd>` over a selected set of repos.           |
| Filesystem      | `resonate` | Copy local files into an already-registered portal placement. |
| xen config      | `env`      | xen's only config surface (interpreted status by default).    |

### `xen portal` — register a repo

```sh
xen portal add <url> [--at=<path>] [--alias dev=develop] [--alias main=master]
xen portal remove <repo>
xen portal list
```

Records the existence of a repo and enough information to later sync it. Clones metainfo only (`git clone --filter=blob:none --sparse --no-checkout`) — no working tree files appear until `summon`. Transactional: a failed clone leaves no manifest residue.

`--at=<path>` records where the repo materializes on disk, relative to the workspace root. Defaults to the repo's basename. Explicit placement is what makes layouts like `services/api`, `services/web`, `libs/shared`, `infra/terraform` possible — the directory shape is part of the workspace's description, and teammates cloning the xen workspace get the same shape automatically.

Two repos cannot be portaled into overlapping paths — `portal add` checks this and fails immediately with the conflict named.

### `xen summon` — make reality match intent

```sh
xen summon                                 # everything, full trees
xen summon <repo> [<repo>...]              # narrow to these repos
xen summon <repo> --paths=src/,docs/       # narrow + write a sparse spec
```

Idempotent reconciliation between env intent and what's on disk. The `--paths` form is the subtle one: paths are intent — they describe what you want visible — so the flag is shorthand for "write this sparse spec to env, then reconcile." A subsequent bare `xen summon` produces the same tree, because the spec lives in env, not in the invocation.

Changing a repo's placement works the same way: `xen env set foo.at=services/foo` followed by `xen summon` moves the working tree from wherever it is to where env now says it should be. If the tree is dirty, summon refuses and names the repo — you resolve it in git and re-run. **xen never silently relocates uncommitted work.**

### `xen sync` — converge to a named ref

```sh
xen sync main
xen sync dev
xen sync refs/heads/develop
xen sync refs/tags/v1.2.0
```

Resolves a branchtype, branch, or tag across the stack and fast-forwards each repo to it. Branchtype aliases live in env: `foo.branchtypes.dev = develop` means "in repo foo, the name `dev` resolves to `refs/heads/develop`". The `refs/heads/...` and `refs/tags/...` forms exist for when you need to be unambiguous.

If a branchtype is unmapped for one or more repos, **sync fails**, names exactly which repos are missing the mapping, and prints the command to fix each one. No silent fallback to default branches. Ever.

```
$ xen sync dev
fail: 2 repos have no mapping for branchtype "dev":
  foo  (suggest: xen env set foo.branchtypes.dev=<branch>)
  bar  (suggest: xen env set bar.branchtypes.dev=<branch>)
```

### `xen cascade` — `git -C` over a set

```sh
xen cascade git status -sb                              # everywhere
xen cascade --changed-only git status -sb               # dirty / unpushed only
xen cascade --branch-pattern='feature/x' git push       # by current branch
xen cascade pwd | xargs -I{} my-thing {}                # use as path source
```

Two filters, both optional, both selectors:
- `--changed-only` — repos with a dirty tree or unpushed commits.
- `--branch-pattern=<glob>` — repos currently on a matching branch.

Execution: parallel via a `JoinSet`, keep-going, per-repo exit codes summarized at the end. `GIT_TERMINAL_PROMPT=0` so auth failures surface as failures rather than hangs. The command isn't restricted to git — it's any shell — but the verb is named for its dominant use.

There is deliberately no separate plumbing verb. Cascade is already thin enough to be its own escape hatch.

#### Workspace rules

Cascade composes with workspace **rules** — pattern + predicate gates that fire on matching commands. The default rules `protect-main-push` and `protect-main-commit` ship pre-seeded; `xen env rules` lets you add, remove, or inspect them. If any rule denies in any repo, the **entire cascade aborts before a single user command spawns**.

### `xen resonate` — drop local files into a portal

```sh
xen resonate <portal> --source <local-path> [--at <subpath>] [--force]
```

The pre-git escape hatch. `resonate` copies files from a local directory into an already-registered portal's placement — pure filesystem op, no git, no manifest writes. The portal must exist (`xen portal add` first); resonate just delivers content into it.

Use it when you have a working directory of files that should become a portal but isn't yet a published repo. The flow:

```sh
# Register a portal — the URL can be a placeholder or a local path
xen portal add /path/to/local/scratch --at services/new-thing

# Materialize files into it from wherever they live
xen resonate new-thing --source ~/scratch/new-thing
xen resonate new-thing --source ~/notes/api-design --at docs/design

# Now it's a normal placement. Init git, commit, set a real upstream
# whenever you're ready
cd services/new-thing
git init && git add . && git commit -m "initial"
git remote add origin git@github.com:org/new-thing.git && git push -u
xen env set new-thing.url=git@github.com:org/new-thing.git
```

`--at` is interpreted as a *subpath inside* the placement, never an override. Absent → copy into the placement root. `--force` is required to overwrite existing files at the destination — by default resonate refuses on any conflict so you can't accidentally clobber work.

Resonate is a pure recursive copy with no built-in exclusions. It does not read `.gitignore`, does not skip `target/` or `node_modules/`, does not have an opinion about what is or isn't "source". If you point it at a directory full of build artifacts, it copies them. The user controls scope by where they point `--source` — there is no "common floor" to be wrong about.

### `xen env` — the only config surface

Two layers, modeled directly on `git config`'s scopes:

- **Shared** — `.xen/` at the workspace root, committed to git. Manifest of repos, remote URLs, branchtype aliases, sparse specs. Anything a teammate needs to reproduce the same description of the stack.
- **Private** — `~/.xen/`, never committed, per-machine. Auth, local-only branchtype overrides, machine-specific sparse overrides.

Reads merge both layers with private overriding shared. Writes go to whichever layer the key belongs to — xen knows which keys are shared vs private and refuses to write auth into shared or repo manifests into private.

The default output of `xen env` is an **interpreted status report**, modeled on `git status`: what's converged, what's drifted, what's missing.

```
$ xen env
7 repos registered
branchtype coverage:
  dev    6/7   missing: bar
  main   7/7
  trunk  3/7   missing: bar, baz, qux, quux
sparse specs: 2 repos (foo, frontend)
auth: 7/7 configured (private)
rules: 2 active (protect-main-commit, protect-main-push)

$ xen env --shared                            # raw shared layer
$ xen env --private                           # raw private layer
$ xen env get foo.branchtypes.dev             # one merged value
$ xen env set foo.branchtypes.dev=develop     # → shared
$ xen env set foo.at=services/foo             # → shared
$ xen env set foo.key=~/.ssh/id_work --private
$ xen env unset foo.branchtypes.dev
```

All xen config mutations go through this verb. Action verbs **never** mutate env as a side effect. The one apparent exception — `summon --paths` — is sugar for an `env set` followed by a reconcile, and is documented as such.

## Auth

xen does not store secrets. It records *which identity* each repo should authenticate with; the actual passphrase lives wherever the OS already has a vault.

The recommended setup, for the common case of "many SSH keys, all guarded by my OS login":

```sh
# macOS — once per key, prompts for that key's passphrase, stores in
# Keychain. After this, opening a fresh shell costs zero prompts.
ssh-add --apple-use-keychain ~/.ssh/key_repo_a
ssh-add --apple-use-keychain ~/.ssh/key_repo_b

# In ~/.ssh/config:
Host *
  UseKeychain yes
  AddKeysToAgent yes
  IgnoreUnknown UseKeychain
```

Linux: gnome-keyring, kwallet, or [`keychain`](https://www.funtoo.org/Keychain) play the same role.

If a particular repo needs a *specific* identity (you have ten keys loaded and this repo must use exactly one of them), record the path in private env:

```sh
xen env set foo.key=~/.ssh/key_foo --private
```

xen will set `GIT_SSH_COMMAND="ssh -i <path> -o IdentitiesOnly=yes"` on every git spawn for that repo. The agent still does the unlock; xen just steers which loaded identity git asks for. The path is stored in your private layer, never in shared.

## Principles

- **Intent and facts are disjoint.** No SHAs in env. Ever.
- **Children are real repos.** They evolve through plain git. xen has no opinion about their history.
- **Action verbs are pure.** They read env, act on git, report. They never write env.
- **`env` is the only read/write surface for xen's own config.**
- **Fail fast on ambiguity.** Missing aliases, drifted intent, unresolved refs — surfaced immediately, never papered over.
- **Lazy by default.** Partial clone + sparse checkout. Materialize on demand.
- **Mirror git's UX where possible.** Users already have it in their fingers.

## Non-goals

- **No lockfile, no snapshot, no SHA pinning.** A lockfile is facts pretending to be intent. If you want reproducibility, `xen cascade git rev-parse HEAD` and check the result into your own repo.
- **No cross-forge PR creation.** Use `gh`, `glab`, etc. via cascade.
- **No build/test verb.** `xen cascade '<your build command>'` is the answer.
- **No cross-repo grep verb.** `xen cascade 'grep -r foo .'` is the answer.
- **No attempt to make cross-repo changes atomic.** They aren't. xen makes the ceremony cheap; it doesn't fake the semantics.
- **No submodule-style anything.** The whole point is routing around the failure mode where a parent repo holds commits about other repos' commits.
- **No plugin system.** The fixed verb set is the extensibility story.

## Status

Pre-alpha. All six verbs work end-to-end against real repos and are covered by an integration test suite including property tests against synthetic multi-repo layouts. The keyspace types, the verbs, and the design principles above are the load-bearing pieces.

## Implementation

Rust. Not for performance — xen is I/O-bound on `git fetch` and the language doesn't matter for that — but because the spec is a list of invariants, not conventions. Intent and facts disjoint. Auth never written to shared. Repo manifests never written to private. Action verbs never mutate env. Branchtype hard-fail on missing alias. Every one of these is something the *compiler* should enforce, not something a code reviewer should remember to check.

The stack is small: `clap` for the CLI surface, `tokio` + `tokio::process` for the parallel cascade, `serde` + `toml` for env serialization, `cargo-dist` for releases. Target size is ~1k lines of source — if it grows past 2k, something has been smuggled in that doesn't belong.

The pre-design that has to happen before any code is the env keyspace as a Rust type: a single `enum` enumerating every legal key in xen's config, tagged at the type level with shared-vs-private. `env set` becomes a `match`, "refuse to write auth into shared" becomes a function signature, adding a new key category later becomes a build error at every call site that needs updating.

## CI

CI matrix is Linux / macOS / Windows. `cargo test --all-features --locked` is what gets run on every PR.

## License

Apache-2.0. See [LICENSE](LICENSE).
