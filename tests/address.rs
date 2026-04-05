use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use git2::{Repository, RepositoryInitOptions};

#[test]
fn address_selects_newest_review_and_generates_specs_for_all_comments() {
    let repo = TestRepo::new("all-comments");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"prompt=$(cat)
printf 'thinking\n'
case "$prompt" in
  *"Comment: R001"*)
    cat <<'EOF'
FINAL-BEGIN
# Anne

## Feature: Address first finding

### Problem

The first review finding needs a concrete implementation spec.

### Goals

- Produce a focused spec for the first finding.

### Non-Goals

- Solving unrelated review findings.

## Proposed Approach

Explain the narrow implementation steps needed for the first finding.
FINAL-END
EOF
    ;;
  *"Comment: R002"*)
    cat <<'EOF'
FINAL-BEGIN
# Anne

## Feature: Address second finding

### Problem

The second review finding needs a concrete implementation spec.

### Goals

- Produce a focused spec for the second finding.

### Non-Goals

- Solving unrelated review findings.

## Proposed Approach

Explain the narrow implementation steps needed for the second finding.
FINAL-END
EOF
    ;;
  *)
    printf 'unexpected prompt\n' >&2
    exit 1
    ;;
esac
"#,
    );
    let filter = markdown_filter_script(repo.path());
    write_agent_config(repo.path(), "wrapped-json", &agent, Some(&filter));

    write_review_bundle(
        repo.path(),
        "2026-04-04T09-00-00Z-older",
        Some("2026-04-04T09:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/old.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Older comment",
            body: "This should not be selected.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-old.rs.patch"),
        }],
    );

    write_review_bundle(
        repo.path(),
        "2026-04-04T11-00-00Z-newer",
        Some("2026-04-04T11:00:00Z"),
        &[
            ReviewCommentSpec {
                id: "R002",
                path: "src/lib.rs",
                side: "new",
                line: 6,
                severity: "warning",
                title: "Second title",
                body: "Second comment body.",
                hunk_header: Some("@@ -3,3 +3,4 @@"),
                patch_file: Some("files/0001-src-lib.rs.patch"),
            },
            ReviewCommentSpec {
                id: "R001",
                path: "src/lib.rs",
                side: "new",
                line: 2,
                severity: "error",
                title: "First title",
                body: "First comment body.",
                hunk_header: Some("@@ -1,1 +1,2 @@"),
                patch_file: Some("files/0001-src-lib.rs.patch"),
            },
        ],
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"review_id\": \"2026-04-04T11-00-00Z-newer\""));
    assert!(manifest.contains("\"comments_selected\": 2"));
    assert!(manifest.contains("\"specs_generated\": 2"));

    let selected_comments = fs::read_to_string(bundle.join("selected_comments.json")).unwrap();
    let first = selected_comments.find("\"id\": \"R001\"").unwrap();
    let second = selected_comments.find("\"id\": \"R002\"").unwrap();
    assert!(first < second, "selected comments were not sorted by id");
    assert!(!selected_comments.contains("\"anchor\""));

    let summary = fs::read_to_string(bundle.join("summary.md")).unwrap();
    assert!(summary.contains("Comments selected: 2"));
    assert!(summary.contains("Specs generated: 2"));
    assert!(summary.contains("## R001 src/lib.rs new:2"));
    assert!(summary.contains("## R002 src/lib.rs new:6"));
    assert!(summary.contains("specs/R001-First-title.md"));
    assert!(summary.contains("specs/R002-Second-title.md"));

    let prompt = fs::read_to_string(bundle.join("agent/R001.prompt.md")).unwrap();
    assert!(prompt.contains("- Anchor: new:2"));

    let spec = fs::read_to_string(bundle.join("specs/R001-First-title.md")).unwrap();
    assert!(spec.contains("## Source Comment"));
    assert_eq!(spec.matches("## Source Comment").count(), 1);
    assert!(spec.contains("- Comment: R001"));
    assert!(spec.contains("- Anchor: new:2"));
    assert!(spec.ends_with('\n'));

    let response = fs::read_to_string(bundle.join("agent/R001.response.txt")).unwrap();
    assert!(response.contains("FINAL-BEGIN"));

    let prompt = fs::read_to_string(bundle.join("agent/R001.prompt.md")).unwrap();
    assert!(prompt.contains("- Core feature heading pattern: ## Feature: <short feature name>"));
    assert!(prompt.contains("- Feature subsections in INIT.md order:"));
    assert!(prompt.contains("  - ### Problem"));
    assert!(
        prompt.contains("Include a `## Source Comment` section near the top for traceability.")
    );
}

#[test]
fn address_falls_back_to_lexicographic_bundle_id_when_timestamps_are_missing() {
    let repo = TestRepo::new("lexicographic-fallback");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Feature: Fallback selection

### Problem

Missing timestamps should still produce deterministic review selection.

### Goals

- Pick the lexicographically newest review id.

### Non-Goals

- Depending on filesystem iteration order.

## Proposed Approach

Use the newest bundle id when generated_at is unavailable.
EOF
"#,
    );
    write_agent_config_with_workers(repo.path(), "text", &agent, None, 4);

    write_review_bundle(
        repo.path(),
        "2026-04-04T10-00-00Z-alpha",
        None,
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/alpha.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Alpha title",
            body: "Alpha body.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-alpha.rs.patch"),
        }],
    );
    write_review_bundle(
        repo.path(),
        "2026-04-04T10-00-00Z-zeta",
        None,
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/zeta.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Zeta title",
            body: "Zeta body.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-zeta.rs.patch"),
        }],
    );

    let output = anne(repo.path(), &["address", "R001"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"review_id\": \"2026-04-04T10-00-00Z-zeta\""));
}

#[test]
fn address_preserves_partial_results_when_one_comment_fails() {
    let repo = TestRepo::new("partial-failure");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"prompt=$(cat)
case "$prompt" in
  *"Comment: R001"*)
    cat <<'EOF'
# Anne

## Feature: First spec

### Problem

The first comment should succeed.

### Goals

- Produce one valid spec.

### Non-Goals

- Failing the whole run immediately.

## Proposed Approach

Write the spec and continue.
EOF
    ;;
  *"Comment: R002"*)
    printf 'partial raw output before failure\n'
    printf 'agent failed intentionally\n' >&2
    exit 9
    ;;
  *)
    printf 'unexpected prompt\n' >&2
    exit 1
    ;;
esac
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T12-00-00Z-partial",
        Some("2026-04-04T12:00:00Z"),
        &[
            ReviewCommentSpec {
                id: "R001",
                path: "src/lib.rs",
                side: "new",
                line: 2,
                severity: "warning",
                title: "First title",
                body: "First body.",
                hunk_header: Some("@@ -1,1 +1,2 @@"),
                patch_file: Some("files/0001-src-lib.rs.patch"),
            },
            ReviewCommentSpec {
                id: "R002",
                path: "src/lib.rs",
                side: "new",
                line: 6,
                severity: "warning",
                title: "Second title",
                body: "Second body.",
                hunk_header: Some("@@ -3,3 +3,4 @@"),
                patch_file: Some("files/0001-src-lib.rs.patch"),
            },
        ],
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"comments_selected\": 2"));
    assert!(manifest.contains("\"specs_generated\": 1"));
    assert!(manifest.contains("\"specs_failed\": 1"));
    assert!(manifest.contains("\"status\": \"failed\""));

    let summary = fs::read_to_string(bundle.join("summary.md")).unwrap();
    assert!(summary.contains("## Failed"));
    assert!(summary.contains("R002: agent exited with status 9"));

    let response = fs::read_to_string(bundle.join("agent/R002.response.txt")).unwrap();
    assert!(response.contains("partial raw output before failure"));

    let spec = fs::read_to_string(bundle.join("specs/R001-First-title.md")).unwrap();
    assert!(spec.contains("## Source Comment"));
}

#[test]
fn address_rejects_empty_filtered_response() {
    let repo = TestRepo::new("empty-response");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
printf 'thinking\n'
cat <<'EOF'
FINAL-BEGIN

FINAL-END
EOF
"#,
    );
    let filter = markdown_filter_script(repo.path());
    write_agent_config(repo.path(), "wrapped-json", &agent, Some(&filter));

    write_review_bundle(
        repo.path(),
        "2026-04-04T12-30-00Z-empty-response",
        Some("2026-04-04T12:30:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/lib.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Empty title",
            body: "Empty body.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-lib.rs.patch"),
        }],
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"comments_selected\": 1"));
    assert!(manifest.contains("\"specs_generated\": 0"));
    assert!(manifest.contains("\"specs_failed\": 1"));
    assert!(manifest.contains("\"status\": \"failed\""));

    let summary = fs::read_to_string(bundle.join("summary.md")).unwrap();
    assert!(summary.contains("## Failed"));
    assert!(summary.contains("R001: agent response was empty"));

    let response = fs::read_to_string(bundle.join("agent/R001.response.txt")).unwrap();
    assert!(response.contains("FINAL-BEGIN"));
    assert!(!bundle.join("specs/R001-Empty-title.md").exists());
}

#[test]
fn address_recovers_wrapped_json_output_without_progress_filter() {
    let repo = TestRepo::new("wrapped-json-no-filter");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r##"cat >/dev/null
printf '%s\n' '{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}'
cat <<'EOF'
{"type":"item.completed","item":{"type":"agent_message","text":"# Anne\n\n## Feature: Wrapped-json fallback\n\n### Problem\n\nAnne should recover the final spec without a shell filter.\n\n### Goals\n\n- Decode the final assistant message directly.\n\n### Non-Goals\n\n- Requiring jq for address generation.\n\n## Proposed Approach\n\nUse Anne's wrapped-json decoder.\n"}}
EOF
"##,
    );
    write_agent_config(repo.path(), "wrapped-json", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T12-40-00Z-wrapped-json-no-filter",
        Some("2026-04-04T12:40:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/lib.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Wrapped title",
            body: "Wrapped body.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-lib.rs.patch"),
        }],
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"succeeded\""));
    assert!(manifest.contains("\"assistant_text_source\": \"wrapped-json-decoder\""));
    assert!(manifest.contains("\"progress_filter_ran\": false"));

    let response = fs::read_to_string(bundle.join("agent/R001.response.txt")).unwrap();
    assert!(response.contains("\"agent_message\""));

    let spec = fs::read_to_string(bundle.join("specs/R001-Wrapped-title.md")).unwrap();
    assert!(spec.contains("## Source Comment"));
    assert!(spec.contains("## Feature: Wrapped-json fallback"));
}

#[test]
fn address_rejects_headingless_response_but_continues_later_comments() {
    let repo = TestRepo::new("headingless-response");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"prompt=$(cat)
case "$prompt" in
  *"Comment: R001"*)
    cat <<'EOF'
FINAL-BEGIN
This prose looks like markdown.

- It still has no heading.
FINAL-END
EOF
    ;;
  *"Comment: R002"*)
    cat <<'EOF'
FINAL-BEGIN
# Anne

## Feature: Second spec

### Problem

The later comment should still generate a spec.

### Goals

- Keep successful comments moving after validation failures.

### Non-Goals

- Rescuing heading-less output by insertion.

## Proposed Approach

Write the valid later spec and keep the bundle ordering stable.
FINAL-END
EOF
    ;;
  *)
    printf 'unexpected prompt\n' >&2
    exit 1
    ;;
esac
"#,
    );
    let filter = markdown_filter_script(repo.path());
    write_agent_config_with_workers(repo.path(), "wrapped-json", &agent, Some(&filter), 1);

    write_review_bundle(
        repo.path(),
        "2026-04-04T12-45-00Z-headingless-response",
        Some("2026-04-04T12:45:00Z"),
        &[
            ReviewCommentSpec {
                id: "R001",
                path: "src/lib.rs",
                side: "new",
                line: 2,
                severity: "warning",
                title: "First title",
                body: "First body.",
                hunk_header: Some("@@ -1,1 +1,2 @@"),
                patch_file: Some("files/0001-src-lib.rs.patch"),
            },
            ReviewCommentSpec {
                id: "R002",
                path: "src/lib.rs",
                side: "new",
                line: 6,
                severity: "warning",
                title: "Second title",
                body: "Second body.",
                hunk_header: Some("@@ -3,3 +3,4 @@"),
                patch_file: Some("files/0001-src-lib.rs.patch"),
            },
        ],
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"comments_selected\": 2"));
    assert!(manifest.contains("\"specs_generated\": 1"));
    assert!(manifest.contains("\"specs_failed\": 1"));
    assert!(manifest.contains("\"workers\": 1"));

    let summary = fs::read_to_string(bundle.join("summary.md")).unwrap();
    assert!(
        summary
            .contains("R001: agent response must be a markdown document with at least one heading")
    );

    let response = fs::read_to_string(bundle.join("agent/R001.response.txt")).unwrap();
    assert!(response.contains("FINAL-BEGIN"));
    assert!(!bundle.join("specs/R001-First-title.md").exists());

    let spec = fs::read_to_string(bundle.join("specs/R002-Second-title.md")).unwrap();
    assert!(spec.contains("## Source Comment"));
}

#[test]
fn address_parallel_workers_preserve_stable_comment_order() {
    let repo = TestRepo::new("parallel-order");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"prompt=$(cat)
mkdir -p .anne-test
case "$prompt" in
  *"Comment: R001"*)
    : > .anne-test/r001-started
    attempts=0
    while [ ! -f .anne-test/r002-started ]; do
      attempts=$((attempts + 1))
      if [ "$attempts" -ge 10 ]; then
        printf 'R002 never started\n' >&2
        exit 91
      fi
      sleep 0.1
    done
    cat <<'EOF'
# Anne

## Feature: Address first finding

### Problem

The first review finding needs a concrete implementation spec.

### Goals

- Produce a focused spec for the first finding.

### Non-Goals

- Solving unrelated review findings.

## Proposed Approach

Explain the narrow implementation steps needed for the first finding.
EOF
    ;;
  *"Comment: R002"*)
    : > .anne-test/r002-started
    cat <<'EOF'
# Anne

## Feature: Address second finding

### Problem

The second review finding needs a concrete implementation spec.

### Goals

- Produce a focused spec for the second finding.

### Non-Goals

- Solving unrelated review findings.

## Proposed Approach

Explain the narrow implementation steps needed for the second finding.
EOF
    ;;
  *)
    printf 'unexpected prompt\n' >&2
    exit 1
    ;;
esac
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T16-00-00Z-parallel",
        Some("2026-04-04T16:00:00Z"),
        &[
            ReviewCommentSpec {
                id: "R002",
                path: "src/lib.rs",
                side: "new",
                line: 6,
                severity: "warning",
                title: "Second title",
                body: "Second body.",
                hunk_header: Some("@@ -3,3 +3,4 @@"),
                patch_file: Some("files/0001-src-lib.rs.patch"),
            },
            ReviewCommentSpec {
                id: "R001",
                path: "src/lib.rs",
                side: "new",
                line: 2,
                severity: "warning",
                title: "First title",
                body: "First body.",
                hunk_header: Some("@@ -1,1 +1,2 @@"),
                patch_file: Some("files/0001-src-lib.rs.patch"),
            },
        ],
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"workers\": 4"));
    assert!(
        manifest.find("\"comment_id\": \"R001\"").unwrap()
            < manifest.find("\"comment_id\": \"R002\"").unwrap()
    );

    let selected_comments = fs::read_to_string(bundle.join("selected_comments.json")).unwrap();
    assert!(
        selected_comments.find("\"id\": \"R001\"").unwrap()
            < selected_comments.find("\"id\": \"R002\"").unwrap()
    );

    let summary = fs::read_to_string(bundle.join("summary.md")).unwrap();
    assert!(summary.contains("Specs generated: 2"));
    assert!(
        summary.find("## R001 src/lib.rs new:2").unwrap()
            < summary.find("## R002 src/lib.rs new:6").unwrap()
    );

    let spec = fs::read_to_string(bundle.join("specs/R001-First-title.md")).unwrap();
    assert!(spec.contains("## Source Comment"));
}

#[test]
fn address_rejects_unknown_comment_before_bundle_creation() {
    let repo = TestRepo::new("unknown-comment");
    init_repo(repo.path());
    write_init_template(repo.path());

    write_review_bundle(
        repo.path(),
        "2026-04-04T13-00-00Z-review",
        Some("2026-04-04T13:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/lib.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Only title",
            body: "Only body.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-lib.rs.patch"),
        }],
    );

    let output = anne(repo.path(), &["address", "R999"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("comment `R999` was not found"));
    assert!(!repo.path().join(".anne/address").exists());
}

#[test]
fn address_requires_readable_init_before_bundle_creation() {
    let repo = TestRepo::new("missing-init");
    init_repo(repo.path());

    write_review_bundle(
        repo.path(),
        "2026-04-04T14-00-00Z-review",
        Some("2026-04-04T14:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/lib.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Only title",
            body: "Only body.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-lib.rs.patch"),
        }],
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("INIT.md"));
    assert!(!repo.path().join(".anne/address").exists());
}

#[test]
fn address_records_empty_selection_when_review_has_no_comments() {
    let repo = TestRepo::new("empty-selection");
    init_repo(repo.path());
    write_init_template(repo.path());

    write_review_bundle(
        repo.path(),
        "2026-04-04T15-00-00Z-empty",
        Some("2026-04-04T15:00:00Z"),
        &[],
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let summary = fs::read_to_string(bundle.join("summary.md")).unwrap();
    assert!(summary.contains("Comments selected: 0"));
    assert!(summary.contains("Specs generated: 0"));
    assert!(summary.contains("No comments selected from the source review."));
}

#[test]
fn address_does_not_duplicate_existing_source_comment_section() {
    let repo = TestRepo::new("existing-source-comment");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Source Comment

- Review: already-present

## Feature: Existing source comment

### Problem

The generated spec already includes traceability metadata.

### Goals

- Preserve the authored section as-is.

### Non-Goals

- Inserting a duplicate section.

## Proposed Approach

Leave the existing source comment section intact.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T15-30-00Z-existing-source-comment",
        Some("2026-04-04T15:30:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/lib.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Existing title",
            body: "Existing body.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-lib.rs.patch"),
        }],
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let spec = fs::read_to_string(bundle.join("specs/R001-Existing-title.md")).unwrap();
    assert_eq!(spec.matches("## Source Comment").count(), 1);
    assert!(spec.contains("- Review: already-present"));
    assert!(!spec.contains("- Comment: R001"));
    assert!(spec.ends_with('\n'));
}

struct ReviewCommentSpec<'a> {
    id: &'a str,
    path: &'a str,
    side: &'a str,
    line: usize,
    severity: &'a str,
    title: &'a str,
    body: &'a str,
    hunk_header: Option<&'a str>,
    patch_file: Option<&'a str>,
}

fn init_repo(path: &Path) {
    let mut options = RepositoryInitOptions::new();
    options.initial_head("main");
    let repo = Repository::init_opts(path, &options).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Anne Test").unwrap();
    config.set_str("user.email", "anne@example.com").unwrap();
}

fn write_init_template(path: &Path) {
    write_file(
        &path.join("INIT.md"),
        "\
# Anne

## Feature: Template

### Problem

Describe the problem clearly.

### Goals

- Capture the desired outcomes.

### Non-Goals

- Avoid unrelated work.

## Proposed Approach

Describe the concrete implementation path.

## Execution Flow

Describe how the feature runs.
",
    );
}

fn write_review_bundle(
    path: &Path,
    review_id: &str,
    generated_at: Option<&str>,
    comments: &[ReviewCommentSpec<'_>],
) {
    let bundle = path.join(".anne/reviews").join(review_id);
    fs::create_dir_all(bundle.join("files")).unwrap();

    let mut manifest = String::from("{\n");
    manifest.push_str(&format!("  \"review_id\": \"{review_id}\""));
    if let Some(generated_at) = generated_at {
        manifest.push_str(&format!(",\n  \"generated_at\": \"{generated_at}\""));
    }
    manifest.push_str(
        ",\n  \"range\": \"main...feature\",\n  \"merge_base\": \"abc1234\",\n  \"status\": \"succeeded\",\n  \"diff_patch\": \"diff.patch\"\n}\n",
    );
    write_file(&bundle.join("manifest.json"), &manifest);
    write_file(
        &bundle.join("diff.patch"),
        "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1,1 +1,2 @@\n-old\n+new\n",
    );

    let mut comments_json = String::from("[\n");
    for (index, comment) in comments.iter().enumerate() {
        comments_json.push_str("  {\n");
        comments_json.push_str(&format!("    \"id\": \"{}\",\n", comment.id));
        comments_json.push_str(&format!("    \"path\": \"{}\",\n", comment.path));
        comments_json.push_str(&format!("    \"side\": \"{}\",\n", comment.side));
        comments_json.push_str(&format!("    \"line\": {},\n", comment.line));
        comments_json.push_str(&format!("    \"severity\": \"{}\",\n", comment.severity));
        comments_json.push_str(&format!("    \"title\": \"{}\",\n", comment.title));
        comments_json.push_str(&format!("    \"body\": \"{}\"", comment.body));
        if let Some(hunk_header) = comment.hunk_header {
            comments_json.push_str(&format!(",\n    \"hunk_header\": \"{}\"", hunk_header));
        }
        if let Some(patch_file) = comment.patch_file {
            comments_json.push_str(&format!(",\n    \"patch_file\": \"{}\"", patch_file));
            write_file(
                &bundle.join(patch_file),
                &format!(
                    "diff --git a/{path} b/{path}\n@@ -1,1 +1,2 @@\n-old\n+new for {id}\n",
                    path = comment.path,
                    id = comment.id
                ),
            );
        }
        comments_json.push_str("\n  }");
        if index + 1 != comments.len() {
            comments_json.push(',');
        }
        comments_json.push('\n');
    }
    comments_json.push_str("]\n");
    write_file(&bundle.join("comments.json"), &comments_json);
}

fn write_agent_config(path: &Path, output: &str, agent: &Path, filter: Option<&Path>) {
    write_agent_config_with_workers(path, output, agent, filter, 4);
}

fn write_agent_config_with_workers(
    path: &Path,
    output: &str,
    agent: &Path,
    filter: Option<&Path>,
    workers: usize,
) {
    let mut text = String::new();
    text.push_str("[agent]\n");
    text.push_str("label = \"test-agent\"\n");
    text.push_str(&format!("command = [\"{}\"]\n", agent.display()));
    if let Some(filter) = filter {
        text.push_str(&format!("progress_filter = [\"{}\"]\n", filter.display()));
    } else {
        text.push_str("progress_filter = []\n");
    }
    text.push_str(&format!("output = \"{}\"\n", output));
    text.push_str("enable_script_wrapper = false\n");
    text.push_str(&format!("workers = {}\n", workers));

    write_file(&path.join(".anne/config.toml"), &text);
}

fn agent_script(path: &Path, body: &str) -> PathBuf {
    let script = path.join("agent.sh");
    write_executable(&script, &format!("#!/bin/sh\nset -eu\n{}\n", body));
    script
}

fn markdown_filter_script(path: &Path) -> PathBuf {
    let script = path.join("markdown-filter.sh");
    write_executable(
        &script,
        "#!/bin/sh\nset -eu\ncapture=0\nwhile IFS= read -r line; do\n  case \"$line\" in\n    FINAL-BEGIN) capture=1 ;;\n    FINAL-END) capture=0 ;;\n    *)\n      if [ \"$capture\" -eq 1 ]; then\n        printf '%s\\n' \"$line\"\n      else\n        printf '%s\\n' \"$line\" >&2\n      fi\n      ;;\n  esac\ndone\n",
    );
    script
}

fn anne(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anne"))
        .args(args)
        .current_dir(path)
        .output()
        .unwrap()
}

fn address_bundle_path(repo: &Path, stdout: &[u8]) -> PathBuf {
    let stdout = String::from_utf8_lossy(stdout);
    let bundle = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Address bundle: "))
        .expect("missing bundle path in stdout");
    repo.join(bundle)
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_executable(path: &Path, contents: &str) {
    write_file(path, contents);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}

struct TestRepo {
    path: PathBuf,
}

impl TestRepo {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "anne-address-test-{}-{}-{}",
            label,
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
