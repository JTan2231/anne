# Glossary

- Review bundle: The on-disk artifact directory for one `anne review` run at `.anne/reviews/<review-id>/`, including the reviewed diff, manifest, rendered comments, structured findings, per-file patches, and raw agent I/O.
- Merge-base review: Anne’s canonical review scope. The command resolves `git merge-base <base> <head>` and reviews the diff from that merge base to the head ref rather than accepting two-dot range math.
- Reviewable file: A changed text file with textual hunks that is not excluded by binary detection, rename-only detection, ignore prefixes, or the per-file patch size limit.
- Skipped file: A changed path that Anne records but does not send to the agent, along with an explicit reason such as `binary diff`, `pure rename without textual changes`, or `ignored by review.ignore_prefixes`.
- Progress filter: An optional command configured under `[agent].progress_filter` that receives agent stdout on stdin and emits the final findings JSON on stdout while using stderr for progress/logging.
- Validated finding: A structured review comment whose `path`, `side`, and `line` match an actual changed line in the reviewed patch and therefore can be safely published into `comments.json` and `comments.md`.
