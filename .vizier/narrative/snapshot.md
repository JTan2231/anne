# Running Snapshot

Narrative theme
- Anne is now a filesystem-native diff review CLI. The `review` command compares branches with merge-base semantics, invokes an external agent per reviewable file, and writes a durable local review bundle under `.anne/reviews/`.

Code state (behaviors that matter)
- `anne review --base <base> --head <head>` and `anne review <base>...<head>` both normalize to triple-dot review semantics. The command resolves the merge base between the two refs through `git2`/`libgit2` and reviews the diff from that merge base to `<head>`.
- Each review run writes `.anne/reviews/<review-id>/manifest.json`, `diff.patch`, `comments.md`, `comments.json`, per-file patch artifacts under `files/`, and raw prompt/response artifacts under `agent/`.
- Review units are individual changed text files processed serially. Binary diffs, pure rename-only changes, metadata-only changes, ignored prefixes from `.anne/config.toml`, and oversized per-file patches are skipped and recorded with explicit reasons.
- Agent runtime configuration lives in `.anne/config.toml` under `[agent]` and `[review]`. The runtime writes the prompt to agent stdin, treats raw agent stdout as the preserved response artifact, and optionally pipes stdout through `progress_filter` to obtain the final JSON findings stream.
- Agent responses must be JSON arrays of findings with `path`, `side`, `line`, `severity`, `title`, and `body`. Anne validates every anchor against changed old/new lines before publishing findings.
- Findings themselves do not make the command fail in v1. Operational problems such as repository/ref resolution failures, agent failures, invalid JSON, or invalid anchors mark the run failed while preserving the bundle for audit/debugging.
- Automated coverage exists for CLI parsing, hunk-anchor parsing, merge-base review behavior, plain-text agent execution, progress-filter execution with invalid-anchor failure handling, rename-only review skipping from real `git2` diff data, and a guard test that blocks reintroduced Git CLI subprocess usage in tracked code and scripts.
