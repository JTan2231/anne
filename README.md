# Anne

Anne is a local, agent-driven review tool for Git repositories. It reviews a `<base>...<head>` diff, persists findings under `.anne/reviews/`, lets you prune those findings interactively, and can turn selected findings into Markdown feature specs under `.anne/address/`.

## Commands

- `anne review <base>...<head>` reviews the diff from the merge base to `<head>` and writes a review bundle.
- `anne address [<comment-id>]` generates one spec per selected review comment from the newest usable review bundle.
- `anne filter` pages through the current persisted review comments and lets you delete them in place.

Run `anne --help` or `anne <command> --help` for the built-in command help.

## Requirements

- Rust 1.85 or newer to build from source.
- Run Anne from inside a Git worktree.
- An agent command configured in `.anne/config.toml`, or a discoverable bundled agent shim.
- If you use the bundled default agent, `codex` must be on `PATH`.
- `jq` is recommended when using the bundled default filter so you get human-friendly progress output. If `jq` is missing, Anne can still recover the final wrapped-json response directly.

## Build And Install

Build from the repo:

```bash
cargo build --release
cargo run -- --help
```

Install from a clone:

```bash
./install.sh
```

Install into a user prefix:

```bash
PREFIX="$HOME/.local" ./install.sh
```

Other install helpers:

```bash
./install.sh --dry-run
./install.sh --uninstall
```

`install.sh` installs:

- `anne` under `$PREFIX/bin`
- bundled agent shims under `$PREFIX/share/anne/agents`
- an install manifest at `$PREFIX/share/anne/install-manifest.txt`

## Quick Start

1. Run a review against the branch or commit range you care about:

```bash
anne review origin/main...HEAD
```

2. Inspect the bundle Anne prints on stdout. The bundle will be under `.anne/reviews/`.

3. Optionally prune noisy findings:

```bash
anne filter
```

4. Generate specs for all remaining comments or for one specific comment:

```bash
anne address
anne address R003
```

If you are running from a source checkout instead of an installed binary, use `cargo run --`:

```bash
cargo run -- review origin/main...HEAD
```

## Typical Workflow

Review a feature branch:

```bash
anne review main...feature/login
```

Review the current branch against `origin/main`:

```bash
anne review origin/main...HEAD
```

Keep only the findings you want to act on:

```bash
anne filter
```

Generate specs for every remaining finding:

```bash
anne address
```

Generate a spec for a single finding:

```bash
anne address R001
```

## Configuration

Anne looks for `.anne/config.toml` at the repo root.

If `.anne/config.toml` is missing, Anne tries to use bundled defaults:

- in a source checkout, from `examples/agents/default/`
- in an installed layout, from the bundled shared-data directory discovered relative to the binary
- from `ANNE_AGENT_SHIMS_DIR` if you want to override shim discovery explicitly

Example config for a custom agent:

```toml
[agent]
command = ["./scripts/anne-agent.sh"]
progress_filter = ["./scripts/anne-filter.sh"]
output = "wrapped-json"
workers = 4

[review]
max_patch_bytes = 131072
ignore_prefixes = [
  "vendor/",
  "third_party/",
  "node_modules/",
  "dist/",
  "build/",
  "target/",
]
```

Settings Anne currently uses:

- `agent.command`: program and arguments Anne will execute for both `review` and `address`
- `agent.progress_filter`: optional program that turns wrapped-json agent output into progress plus a final reply
- `agent.output`: `text` or `wrapped-json`
- `agent.workers`: parallelism for file reviews and address generation
- `review.max_patch_bytes`: skip patches larger than this byte limit
- `review.ignore_prefixes`: skip paths with these prefixes

Bundled default behavior:

- the bundled default agent shells out to `codex exec --json -`
- the bundled default filter expects `jq`
- if the bundled filter cannot run because `jq` is unavailable, Anne falls back to decoding the final wrapped-json `agent_message` itself

## Command Notes

### `anne review`

- Triple-dot syntax is the canonical user-facing form. Anne resolves the merge base between `<base>` and `<head>` and reviews the diff from that merge base to `<head>`.
- Two-dot positional syntax is intentionally not accepted.
- Each reviewable file is processed independently, then findings are sorted into a stable order and assigned ids like `R001`, `R002`, and so on.
- Anne skips files that are ignored, binary, pure renames, metadata-only changes, or larger than `review.max_patch_bytes`.

The review prompt expects the agent to return only a JSON array. A minimal valid response looks like:

```json
[
  {
    "path": "src/lib.rs",
    "side": "new",
    "line": 42,
    "severity": "warning",
    "title": "Short finding title",
    "body": "One concise explanation.",
    "hunk_header": "@@ -40,6 +40,9 @@"
  }
]
```

Return `[]` when there are no findings.

### `anne filter`

- `anne filter` operates on the newest usable review bundle under `.anne/reviews/`.
- Completed bundles are preferred over running bundles.
- In a terminal, Anne shows a scrollable patch view. If stdout is not a terminal, it falls back to a plain prompt flow.
- Every delete rewrites `comments.json`, `comments.md`, and manifest counts immediately.

Keys:

- `Up` / `Down`: scroll one line
- `Left` / `Right`: scroll horizontally
- `PgUp` / `PgDn`: scroll by one page
- `n`: keep the current comment and move on
- `d`: delete the current comment from the selected bundle
- `q`: quit immediately and keep the remaining comments unchanged

### `anne address`

- `anne address` also works from the newest usable review bundle under `.anne/reviews/`.
- Without a comment id, Anne processes all comments from that bundle in stable id order.
- With a comment id, Anne generates exactly one spec for that finding.
- The agent must return Markdown with at least one heading.
- If the returned Markdown does not include `## Source Comment`, Anne injects that section automatically.

## Bundle Layout

Review bundles live under `.anne/reviews/<timestamp>-<base>...<head>/`.

Typical review bundle contents:

```text
.anne/reviews/<review-id>/
  manifest.json
  comments.json
  comments.md
  diff.patch
  files/
    0001-<path>.patch
  agent/
    0001-<path>.prompt.md
    0001-<path>.response.txt
```

Address bundles live under `.anne/address/<timestamp>-latest-<selection>/`.

Typical address bundle contents:

```text
.anne/address/<address-id>/
  manifest.json
  summary.md
  selected_comments.json
  specs/
    R001-<slug>.md
  agent/
    R001.prompt.md
    R001.response.txt
```

The Markdown outputs are meant to be human-readable:

- `comments.md` summarizes the review and findings
- `summary.md` summarizes the address run and links each generated spec

The JSON outputs are meant to be machine-readable:

- `comments.json` contains the final stable review findings
- `selected_comments.json` captures the comments selected for an address run
- `manifest.json` captures run status, counts, runtime details, and artifact paths

## Environment Notes

- `ANNE_AGENT_SHIMS_DIR` can point Anne at a custom bundled-shim root.
- `NO_COLOR` and `ANNE_NO_ANSI` disable ANSI color in the bundled default progress filter.

