# Anne

## Feature: Filesystem-Native Diff Review

### Problem

`anne review` should review the code changes between two branches and emit comments tied to specific changes without depending on GitHub or another forge API.

The output should live on disk as a durable review bundle:

- a patch file for the exact diff that was reviewed
- a human-readable markdown summary of findings
- enough structured metadata to keep comments anchored to specific files/hunks/lines

This should feel like:

```bash
anne review --base main --head feature/login
# or
anne review main...feature/login
```

and produce a local artifact bundle that can be inspected, versioned, regenerated, or later translated into hosted review comments.

### Goals

- Review a branch diff using merge-base semantics, not raw `HEAD..HEAD` range math.
- Produce comments that point at specific changed lines or hunks.
- Keep the primary artifact local and inspectable.
- Invoke agents through an external command contract modeled after `~/rust/vizier`.
- Preserve enough raw I/O to debug agent behavior when the review looks wrong.

### Non-Goals

- Posting comments to GitHub, GitLab, or Gerrit in v1.
- Auto-fixing findings.
- Reviewing binary files or large generated/vendor artifacts by default.
- Solving cross-file architectural review perfectly in v1.

## Command Shape

Primary command:

```bash
anne review --base <branch> --head <branch>
```

Shorthand:

```bash
anne review <base>...<head>
```

Behavior:

- Resolve the merge base between `<base>` and `<head>` via `libgit2`.
- Review the textual diff from `merge_base` to `<head>`.
- Generate a rename-aware unified diff with binary signaling and three lines of context.
- Treat `<base>...<head>` as the canonical user-facing meaning because it matches pull-request style review semantics.

Implementation note:

- Anne performs repository discovery, ref resolution, merge-base lookup, and diff generation through the Rust `git2` crate backed by `libgit2`; it does not require the system `git` executable.

Reasoning:

- Two-dot diffs are easy to misuse when the base branch has moved.
- Triple-dot semantics give the changes introduced by the head branch since divergence, which is what review usually wants.

## Review Bundle

Each run writes a bundle under:

```text
.anne/reviews/<review-id>/
```

Suggested `<review-id>` format:

```text
2026-04-03T18-42-10Z-main...feature-login-1a2b3c4
```

Review phases:

- Preflight: repository discovery, config loading, ref resolution, merge-base lookup, diff generation/splitting, and review-id construction. No path under `.anne/reviews/` is guaranteed in this phase.
- Materialized run: once Anne starts creating `.anne/reviews/<review-id>/`, later operational failures keep the partial bundle on disk and continue reporting that bundle path.

Bundle contents:

```text
.anne/reviews/<review-id>/
  manifest.json
  diff.patch
  comments.md
  comments.json
  files/
    0001-src-auth.rs.patch
    0002-src-session.rs.patch
  agent/
    0001-src-auth.rs.prompt.md
    0001-src-auth.rs.response.txt
    0002-src-session.rs.prompt.md
    0002-src-session.rs.response.txt
```

File meanings:

- `manifest.json`: run metadata, base/head refs, merge base, skipped files, counts, runtime settings.
- `diff.patch`: the full patch reviewed by anne.
- `comments.md`: the readable review output.
- `comments.json`: structured canonical findings for future automation.
- `files/*.patch`: per-file review units actually sent to the agent.
- `agent/*`: raw prompts and raw agent responses for audit/debugging.

`comments.md` is the user-facing artifact. `comments.json` is the machine-facing artifact.

## Review Unit

V1 review unit: one changed text file.

For each changed file:

- materialize a per-file patch under `files/`
- send that patch to the agent
- ask for zero or more findings tied to changed lines

Why file-level first:

- simpler than hunk fan-out
- easier to reason about prompt size
- still precise enough for line-anchored comments

Fallback behavior for large files:

- if a per-file patch exceeds a configurable size budget, anne may split it by hunk or mark it as skipped
- skipped files must be recorded in `manifest.json` and surfaced in `comments.md`

Files skipped by default:

- binary files
- pure rename-only entries with no textual delta
- vendored/generated files matched by configurable ignore rules

## Comment Model

Anne should ask the agent for structured findings, then render markdown from that structure.

Suggested `comments.json` finding shape:

```json
{
  "id": "R001",
  "path": "src/auth.rs",
  "side": "new",
  "line": 87,
  "severity": "warning",
  "title": "Possible panic on anonymous request",
  "body": "This unwrap can panic when the request does not carry a session.",
  "hunk_header": "@@ -80,6 +87,8 @@",
  "patch_file": "files/0001-src-auth.rs.patch"
}
```

Required fields:

- `path`
- `side`: `old` or `new`
- `line`
- `severity`: `note`, `warning`, or `error`
- `title`
- `body`

Recommended fields:

- `id`
- `hunk_header`
- `confidence`
- `patch_file`

Anchoring rules:

- Additions and modified lines should anchor to `side = "new"`.
- Deletions should anchor to `side = "old"`.
- If a comment cannot be tied to one exact line, anchor it to the first changed line in the relevant hunk and keep the `hunk_header`.

## Markdown Rendering

`comments.md` should be deterministic and easy to skim.

Suggested shape:

```md
# Review: main...feature/login

- Generated: 2026-04-03T18:42:10Z
- Merge base: 1a2b3c4d
- Files reviewed: 12
- Files skipped: 2
- Findings: 3
- Patch: diff.patch

## src/auth.rs

### R001 warning new:87
Possible panic on anonymous request

This unwrap can panic when the request does not carry a session.

Hunk: @@ -80,6 +87,8 @@

## src/session.rs

No findings.
```

Rules:

- list files in stable order
- include explicit "No findings" sections only when useful for auditability
- mention skipped files in a dedicated summary section

## Agent Invocation

Anne should copy the `vizier` runtime model, not invent a bespoke one.

The important contract from `~/rust/vizier`:

- the prompt is written to the agent command's stdin
- the agent command writes its primary stream to stdout
- an optional progress filter can transform that stdout stream
- stderr is treated as progress/logging, not canonical result data
- the final assistant text is read from stdout after filtering

Relevant `vizier` references:

- `examples/agents/codex/agent.sh`
- `examples/agents/codex/filter.sh`
- `vizier-core/src/agent.rs`

Anne should adopt the same runtime keys where practical:

- `label`
- `command`
- `progress_filter`
- `output`
- `enable_script_wrapper`

Suggested config sketch:

```toml
[agent]
label = "codex"
command = ["codex", "exec", "--json", "-"]
progress_filter = ["./agents/codex/filter.sh"]
output = "wrapped-json"
enable_script_wrapper = false
```

Execution model:

1. Anne builds a review prompt for one file patch.
2. Anne spawns the configured `command`.
3. Anne writes the prompt to stdin.
4. If `progress_filter` is configured, anne pipes agent stdout into that filter.
5. Anne surfaces filter stderr or agent stderr as progress to the terminal.
6. Anne captures final assistant text from filter stdout, or directly from agent stdout when no filter is configured.
7. Anne stores raw prompt and raw response in the bundle.

This is intentionally close to `vizier` so the same shims can be reused with minimal adaptation.

## Prompt Contract

Anne should instruct the agent to return structured findings, not freeform prose.

V1 response format:

- raw JSON array on stdout
- empty array when there are no findings

Why JSON first:

- line comments need stable anchors
- markdown is better as a rendered report than a parsing format
- future integrations can translate `comments.json` directly into forge review APIs

Prompt requirements:

- include base branch, head branch, merge base, and path
- include the per-file patch
- tell the agent to comment only on concrete issues in changed lines
- ask it to avoid style nits unless they imply correctness, maintainability, or regression risk
- require every finding to cite a changed line anchor
- require `[]` when there are no findings

## Execution Flow

1. Resolve the repository, load config, resolve base/head refs, compute the merge base, render the review diff, split it into per-file sections, and build the review id.
2. Materialize the bundle directory.
3. Write the initial `manifest.json` plus full `diff.patch`.
4. For each reviewable file:
   - write `files/<n>-<path>.patch`
   - build prompt
   - invoke agent using the configured runtime
   - persist raw prompt/response
   - parse JSON findings
5. Aggregate all findings into `comments.json`.
6. Render `comments.md`.
7. Print bundle path and summary counts to stdout only after materialization has begun.

Suggested terminal summary:

```text
Review bundle: .anne/reviews/2026-04-03T18-42-10Z-main...feature-login-1a2b3c4
Files reviewed: 12
Files skipped: 2
Findings: 3
```

## Failure Handling

Operational failures should be distinct from "agent found issues".

- Agent invocation failure: command exits non-zero, bundle still written if possible, run exits non-zero.
- Parse failure: raw response preserved under `agent/`, file marked failed in `manifest.json`, run exits non-zero.
- Repository or ref resolution failure: preflight error, no review bundle is created or reported.
- Bundle or artifact write failure after materialization starts: keep any partial bundle on disk, report its path, and exit non-zero.
- Findings present: run exits 0 by default in v1.

Possible later flag:

```bash
anne review --fail-on-findings
```

## Implementation Notes

Important v1 constraints:

- start serial, not parallel
- review per file, not per hunk
- keep the bundle format stable from the start
- prefer explicit artifacts over hidden in-memory behavior

Good first milestones:

1. Build bundle generation with `diff.patch` and `files/*.patch`, no agent yet.
2. Add `vizier`-style agent runtime and persist raw prompt/response files.
3. Require JSON findings and render `comments.md`.

This gives anne a narrow, durable core: local diff review as a reproducible filesystem artifact, with agent execution modeled after the proven `vizier` shim/filter pattern.
