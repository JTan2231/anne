# Glossary

- Review bundle: The on-disk artifact directory for one `anne review` run at `.anne/reviews/<review-id>/`, including the reviewed diff, manifest, rendered comments, structured findings, per-file patches, and raw agent I/O.
- Address bundle: The on-disk artifact directory for one `anne address` run at `.anne/address/<address-id>/`, including the selected source comments, manifest, readable summary, per-comment agent I/O, and generated feature specs.
- Merge-base review: Anne’s canonical review scope. The command resolves the merge base between `<base>` and `<head>` through `libgit2` and reviews the diff from that merge base to the head ref rather than accepting two-dot range math.
- Source comment: A persisted structured review finding from `comments.json` that `anne address` uses as the unit of planning work, including required `id`, `path`, `side`, `line`, `severity`, `title`, and `body` fields plus any recorded `hunk_header`, `patch_file`, or `confidence`; human-readable anchor text such as `new:6` is derived later from `side` and `line`.
- Reviewable file: A changed text file with textual hunks that is not excluded by binary detection, rename-only detection, ignore prefixes, or the per-file patch size limit.
- Skipped file: A changed path that Anne records but does not send to the agent, along with an explicit reason such as `binary diff`, `pure rename without textual changes`, or `ignored by review.ignore_prefixes`.
- Agent workers: The `[agent].workers` configuration value that bounds concurrent per-file review jobs and per-comment address jobs. Anne defaults it to `4`, and `1` disables parallel fan-out.
- Progress filter: An optional command configured under `[agent].progress_filter` that receives agent stdout on stdin and emits the final assistant text on stdout while using stderr for progress/logging. `review` uses that text as findings JSON; `address` uses it as markdown spec text.
- Validated finding: A structured review comment whose `path`, `side`, and `line` match an actual changed line in the reviewed patch and therefore can be safely published into `comments.json` and `comments.md`.
