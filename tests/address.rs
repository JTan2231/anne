use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use git2::{Repository, RepositoryInitOptions};

#[allow(dead_code)]
#[path = "../src/json.rs"]
mod fixture_json;

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
    assert!(prompt.contains("- Feature subsections in Anne spec order:"));
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
fn address_prefers_canonical_review_and_writes_canonical_bundle_timestamp_without_date() {
    let repo = TestRepo::new("canonical-review-selection");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"while IFS= read -r _line; do :; done
printf '%s\n' \
  '# Anne' \
  '' \
  '## Feature: Canonical timestamp selection' \
  '' \
  '### Problem' \
  '' \
  'Legacy review timestamps should not outrank canonical ones.' \
  '' \
  '### Goals' \
  '' \
  '- Select the newest canonical review bundle.' \
  '' \
  '### Non-Goals' \
  '' \
  '- Rewriting historical review manifests.' \
  '' \
  '## Proposed Approach' \
  '' \
  'Prefer canonical `generated_at` values and keep address bundle ids canonical too.'
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T12-00-00Z-legacy",
        Some("unix-9999999999"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/legacy.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Legacy timestamp",
            body: "Legacy bundle should not win.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-legacy.rs.patch"),
        }],
    );
    write_review_bundle(
        repo.path(),
        "2026-04-04T11-00-00Z-canonical",
        Some("2026-04-04T11:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/canonical.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Canonical timestamp",
            body: "Canonical bundle should win.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-canonical.rs.patch"),
        }],
    );

    let output = anne_with_path(repo.path(), &["address", "R001"], Some(""));
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"review_id\": \"2026-04-04T11-00-00Z-canonical\""));

    let generated_at = json_string_field(&manifest, "generated_at");
    assert_canonical_timestamp(&generated_at);
    assert!(!generated_at.starts_with("unix-"));

    assert_eq!(
        bundle.file_name().unwrap().to_string_lossy(),
        address_bundle_id(&generated_at, "R001")
    );
}

#[test]
fn address_prefers_newest_completed_review_over_newer_running_bundle() {
    let repo = TestRepo::new("completed-over-running");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Feature: Completed bundle preferred

### Problem

A newer running review should not displace the newest completed usable review.

### Goals

- Prefer the completed bundle when both are readable.

### Non-Goals

- Consuming in-flight running comments.

## Proposed Approach

Keep scanning completed bundles before falling back to running ones.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T10-00-00Z-completed",
        Some("2026-04-04T10:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/completed.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Completed title",
            body: "Use the completed bundle.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-completed.rs.patch"),
        }],
    );
    write_review_bundle(
        repo.path(),
        "2026-04-04T11-00-00Z-running",
        Some("2026-04-04T11:00:00Z"),
        &[],
    );
    let running_manifest_path =
        review_bundle_path(repo.path(), "2026-04-04T11-00-00Z-running").join("manifest.json");
    let running_manifest = fs::read_to_string(&running_manifest_path).unwrap();
    write_file(
        &running_manifest_path,
        &running_manifest.replace("\"status\": \"succeeded\"", "\"status\": \"running\""),
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
    assert!(manifest.contains("\"review_id\": \"2026-04-04T10-00-00Z-completed\""));

    let selected_comments = fs::read_to_string(bundle.join("selected_comments.json")).unwrap();
    assert!(selected_comments.contains("\"path\": \"src/completed.rs\""));
}

#[test]
fn address_falls_back_to_running_bundle_when_no_completed_review_is_usable() {
    let repo = TestRepo::new("running-fallback");
    init_repo(repo.path());
    write_init_template(repo.path());

    write_review_bundle(
        repo.path(),
        "2026-04-04T11-30-00Z-running-only",
        Some("2026-04-04T11:30:00Z"),
        &[],
    );
    let manifest_path =
        review_bundle_path(repo.path(), "2026-04-04T11-30-00Z-running-only").join("manifest.json");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    write_file(
        &manifest_path,
        &manifest.replace("\"status\": \"succeeded\"", "\"status\": \"running\""),
    );

    let output = anne(repo.path(), &["address"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let address_manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(address_manifest.contains("\"review_id\": \"2026-04-04T11-30-00Z-running-only\""));
    assert!(address_manifest.contains("\"status\": \"running\""));

    let summary = fs::read_to_string(bundle.join("summary.md")).unwrap();
    assert!(summary.contains("No comments selected from the source review."));
}

#[test]
fn address_skips_manifest_only_newer_bundle() {
    let repo = TestRepo::new("manifest-only-fallback");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Feature: Manifest-only fallback

### Problem

Manifest-only review bundles are not usable sources for address.

### Goals

- Skip the malformed newer bundle.

### Non-Goals

- Failing on the first incomplete bundle.

## Proposed Approach

Require all advertised public review artifacts before selecting a bundle.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T10-00-00Z-older-complete",
        Some("2026-04-04T10:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/fallback.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Fallback title",
            body: "Use the older complete bundle.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-fallback.rs.patch"),
        }],
    );
    write_review_bundle(
        repo.path(),
        "2026-04-04T11-00-00Z-newer-manifest-only",
        Some("2026-04-04T11:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/newer.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Manifest only title",
            body: "This bundle should be skipped.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-newer.rs.patch"),
        }],
    );
    let manifest_only_bundle =
        review_bundle_path(repo.path(), "2026-04-04T11-00-00Z-newer-manifest-only");
    fs::remove_file(manifest_only_bundle.join("diff.patch")).unwrap();
    fs::remove_file(manifest_only_bundle.join("comments.json")).unwrap();
    fs::remove_file(manifest_only_bundle.join("comments.md")).unwrap();

    let output = anne(repo.path(), &["address", "R001"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"review_id\": \"2026-04-04T10-00-00Z-older-complete\""));
}

#[test]
fn address_falls_back_when_newest_review_bundle_is_missing_comments_json() {
    let repo = TestRepo::new("missing-comments-fallback");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Feature: Missing comments fallback

### Problem

Address should skip incomplete newer review bundles.

### Goals

- Use the newest usable review bundle.

### Non-Goals

- Failing on the first incomplete candidate.

## Proposed Approach

Use the fallback review bundle when comments.json is unavailable.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T10-00-00Z-older-complete",
        Some("2026-04-04T10:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/fallback.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Fallback title",
            body: "Use the older bundle.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-fallback.rs.patch"),
        }],
    );
    write_review_bundle(
        repo.path(),
        "2026-04-04T11-00-00Z-newer-incomplete",
        Some("2026-04-04T11:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/newest.rs",
            side: "new",
            line: 8,
            severity: "warning",
            title: "Newest title",
            body: "This bundle should be skipped.",
            hunk_header: Some("@@ -7,1 +7,2 @@"),
            patch_file: Some("files/0001-src-newest.rs.patch"),
        }],
    );
    fs::remove_file(
        review_bundle_path(repo.path(), "2026-04-04T11-00-00Z-newer-incomplete")
            .join("comments.json"),
    )
    .unwrap();

    let output = anne(repo.path(), &["address", "R001"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"review_id\": \"2026-04-04T10-00-00Z-older-complete\""));

    let selected_comments = fs::read_to_string(bundle.join("selected_comments.json")).unwrap();
    assert!(selected_comments.contains("\"path\": \"src/fallback.rs\""));
    assert!(!selected_comments.contains("\"path\": \"src/newest.rs\""));
}

#[test]
fn address_falls_back_when_newest_review_bundle_has_invalid_comments_json() {
    let repo = TestRepo::new("invalid-comments-fallback");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Feature: Invalid comments fallback

### Problem

Address should skip malformed newer review bundles.

### Goals

- Use the newest parseable review bundle.

### Non-Goals

- Failing on malformed JSON in a newer candidate.

## Proposed Approach

Keep scanning until a usable comments snapshot is found.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T10-00-00Z-older-valid",
        Some("2026-04-04T10:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/older.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Older valid title",
            body: "Use the older valid bundle.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-older.rs.patch"),
        }],
    );
    write_review_bundle(
        repo.path(),
        "2026-04-04T11-00-00Z-newer-invalid",
        Some("2026-04-04T11:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/newer.rs",
            side: "new",
            line: 4,
            severity: "warning",
            title: "Newest invalid title",
            body: "This bundle should be skipped.",
            hunk_header: Some("@@ -3,1 +3,2 @@"),
            patch_file: Some("files/0001-src-newer.rs.patch"),
        }],
    );
    write_file(
        &review_bundle_path(repo.path(), "2026-04-04T11-00-00Z-newer-invalid")
            .join("comments.json"),
        "{\n",
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
    assert!(manifest.contains("\"review_id\": \"2026-04-04T10-00-00Z-older-valid\""));

    let selected_comments = fs::read_to_string(bundle.join("selected_comments.json")).unwrap();
    assert!(selected_comments.contains("\"path\": \"src/older.rs\""));
    assert!(!selected_comments.contains("\"path\": \"src/newer.rs\""));
}

#[test]
fn address_accepts_json_escaped_comment_text_in_synthetic_review_fixtures() {
    let repo = TestRepo::new("escaped-comment-fixture");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Feature: Escaped comment fixture

### Problem

Synthetic review fixtures must preserve JSON-sensitive comment text.

### Goals

- Exercise normal address generation with decoded source-comment text.

### Non-Goals

- Requiring fixture authors to hand-escape JSON strings.

## Proposed Approach

Keep fixture serialization JSON-safe so prompt construction sees the original text.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    let title = r#"Escaped "title""#;
    let body = "First line with a quote: \"\nSecond line with a backslash: \\";
    let hunk_header = r#"@@ -1,"old" +1,2 @@ \\context"#;
    write_review_bundle(
        repo.path(),
        "2026-04-04T11-30-00Z-escaped-comment-fixture",
        Some("2026-04-04T11:30:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/escaped.rs",
            side: "new",
            line: 4,
            severity: "warning",
            title,
            body,
            hunk_header: Some(hunk_header),
            patch_file: Some("files/0001-src-escaped.rs.patch"),
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
    let prompt = fs::read_to_string(bundle.join("agent/R001.prompt.md")).unwrap();
    assert!(prompt.contains(&format!("- Title: {title}\n")));
    assert!(prompt.contains(&format!("- Hunk header: {hunk_header}\n")));
    assert!(prompt.contains(&format!("Comment body:\n{body}\n\n")));
    assert!(bundle.join("specs/R001-Escaped-title.md").exists());
}

#[test]
fn address_selects_review_bundle_with_failed_manifest_status_when_comments_are_valid() {
    let repo = TestRepo::new("failed-status-still-usable");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Feature: Failed status still usable

### Problem

Address should not require review success status to reuse comments.

### Goals

- Keep parseable comments eligible.

### Non-Goals

- Treating manifest status as a hard gate.

## Proposed Approach

Select the bundle when comments.json remains valid.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    let review_id = "2026-04-04T12-00-00Z-failed-status";
    write_review_bundle(
        repo.path(),
        review_id,
        Some("2026-04-04T12:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/failed.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Failed status title",
            body: "This bundle should still be selected.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-failed.rs.patch"),
        }],
    );

    let manifest_path = review_bundle_path(repo.path(), review_id).join("manifest.json");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    write_file(
        &manifest_path,
        &manifest.replace("\"status\": \"succeeded\"", "\"status\": \"failed\""),
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
    assert!(manifest.contains(&format!("\"review_id\": \"{review_id}\"")));
    assert!(manifest.contains("\"status\": \"failed\""));
}

#[test]
fn address_fails_when_review_bundles_exist_but_none_are_usable() {
    let repo = TestRepo::new("no-usable-review-bundle");
    init_repo(repo.path());
    write_init_template(repo.path());

    write_review_bundle(
        repo.path(),
        "2026-04-04T10-00-00Z-older-invalid",
        Some("2026-04-04T10:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/older.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Older title",
            body: "Older body.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-older.rs.patch"),
        }],
    );
    write_file(
        &review_bundle_path(repo.path(), "2026-04-04T10-00-00Z-older-invalid")
            .join("comments.json"),
        "{\n",
    );

    write_review_bundle(
        repo.path(),
        "2026-04-04T11-00-00Z-newer-missing",
        Some("2026-04-04T11:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/newer.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Newest title",
            body: "Newest body.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-newer.rs.patch"),
        }],
    );
    fs::remove_file(
        review_bundle_path(repo.path(), "2026-04-04T11-00-00Z-newer-missing").join("comments.json"),
    )
    .unwrap();

    let output = anne(repo.path(), &["address"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no usable review bundle was found"));
    assert!(stderr.contains("2026-04-04T11-00-00Z-newer-missing"));
    assert!(stderr.contains("comments.json was not readable"));
    assert!(stderr.contains("2026-04-04T10-00-00Z-older-invalid"));
    assert!(stderr.contains("comments.json was not valid"));
    assert!(!repo.path().join(".anne/address").exists());
}

#[test]
fn address_reports_missing_review_outputs_before_bundle_creation() {
    let repo = TestRepo::new("no-review-outputs");
    init_repo(repo.path());

    let output = anne(repo.path(), &["address"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no review outputs discovered under `.anne/reviews/`"));
    assert!(!repo.path().join(".anne/address").exists());
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
fn address_preserves_explicit_text_output_with_custom_filter() {
    let repo = TestRepo::new("explicit-text-custom-filter");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
printf 'thinking\n'
cat <<'EOF'
FINAL-BEGIN
# Anne

## Feature: Explicit text runtime

### Problem

Anne must preserve the declared text runtime when a custom filter is configured.

### Goals

- Keep manifest runtime metadata aligned with the explicit config.

### Non-Goals

- Inferring wrapped-json from filter presence alone.

## Proposed Approach

Generate the spec through the custom filter without rewriting the output mode.
FINAL-END
EOF
"#,
    );
    let filter = markdown_filter_script(repo.path());
    write_agent_config(repo.path(), "text", &agent, Some(&filter));

    write_review_bundle(
        repo.path(),
        "2026-04-04T12-45-00Z-explicit-text-custom-filter",
        Some("2026-04-04T12:45:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/lib.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Text title",
            body: "Text body.",
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
    assert!(manifest.contains("\"output\": \"text\""));
    assert!(manifest.contains("\"effective_output\": \"text\""));
    assert!(manifest.contains("\"progress_filter_source\": \"explicit\""));
    assert!(manifest.contains("\"assistant_text_source\": \"progress-filter\""));
    assert!(!manifest.contains("\"output\": \"wrapped-json\""));

    let response = fs::read_to_string(bundle.join("agent/R001.response.txt")).unwrap();
    assert!(response.contains("FINAL-BEGIN"));

    let spec = fs::read_to_string(bundle.join("specs/R001-Text-title.md")).unwrap();
    assert!(spec.contains("## Feature: Explicit text runtime"));
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
fn address_generates_specs_without_referring_to_init() {
    let repo = TestRepo::new("missing-init");
    init_repo(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Feature: No init dependency

### Problem

Address should not depend on INIT.md being present.

### Goals

- Generate a focused spec from the review comment alone.

### Non-Goals

- Threading INIT.md through the address workflow.

## Proposed Approach

Use the built-in Anne spec shape.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

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
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = address_bundle_path(repo.path(), &output.stdout);
    let prompt = fs::read_to_string(bundle.join("agent/R001.prompt.md")).unwrap();
    assert!(!prompt.contains("INIT.md"));
    assert!(prompt.contains("- Feature subsections in Anne spec order:"));

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(!manifest.contains("\"init_path\""));
    assert!(!manifest.contains("\"init_headings\""));
    assert!(manifest.contains("\"spec_headings\""));
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
fn address_reserves_unique_bundle_ids_for_same_second_runs() {
    let repo = TestRepo::new("bundle-id-collision");
    init_repo(repo.path());
    write_init_template(repo.path());

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Anne

## Feature: Collision handling

### Problem

Multiple same-second address runs need separate bundles.

### Goals

- Keep each run isolated.

### Non-Goals

- Reusing an older address bundle.

## Proposed Approach

Reserve a unique bundle root before writing run artifacts.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    write_review_bundle(
        repo.path(),
        "2026-04-04T17-00-00Z-collision",
        Some("2026-04-04T17:00:00Z"),
        &[ReviewCommentSpec {
            id: "R001",
            path: "src/lib.rs",
            side: "new",
            line: 2,
            severity: "warning",
            title: "Collision title",
            body: "Reserve a unique bundle.",
            hunk_header: Some("@@ -1,1 +1,2 @@"),
            patch_file: Some("files/0001-src-lib.rs.patch"),
        }],
    );

    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fixed_date_script(&bin_dir, "2026-04-04T17:15:29Z");
    let path_env = prepend_path(&bin_dir);

    let first_output = anne_with_path(repo.path(), &["address", "R001"], Some(&path_env));
    assert!(
        first_output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&first_output.stdout),
        String::from_utf8_lossy(&first_output.stderr)
    );

    let second_output = anne_with_path(repo.path(), &["address", "R001"], Some(&path_env));
    assert!(
        second_output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&second_output.stdout),
        String::from_utf8_lossy(&second_output.stderr)
    );

    let base_id = address_bundle_id("2026-04-04T17:15:29Z", "R001");
    let first_bundle = address_bundle_path(repo.path(), &first_output.stdout);
    let second_bundle = address_bundle_path(repo.path(), &second_output.stdout);
    let second_id = format!("{base_id}-2");

    assert_eq!(
        first_bundle,
        repo.path().join(".anne/address").join(&base_id)
    );
    assert_eq!(
        second_bundle,
        repo.path().join(".anne/address").join(&second_id)
    );
    assert_ne!(first_bundle, second_bundle);

    let first_manifest = fs::read_to_string(first_bundle.join("manifest.json")).unwrap();
    assert!(first_manifest.contains(&format!("\"address_id\": \"{base_id}\"")));
    assert!(first_manifest.contains(&format!("\"bundle_path\": \".anne/address/{base_id}\"")));
    assert!(first_manifest.contains("\"selection_filter\": \"R001\""));

    let second_manifest = fs::read_to_string(second_bundle.join("manifest.json")).unwrap();
    assert!(second_manifest.contains(&format!("\"address_id\": \"{second_id}\"")));
    assert!(second_manifest.contains(&format!("\"bundle_path\": \".anne/address/{second_id}\"")));
    assert!(second_manifest.contains("\"selection_filter\": \"R001\""));

    assert!(first_bundle.join("specs/R001-Collision-title.md").exists());
    assert!(second_bundle.join("specs/R001-Collision-title.md").exists());
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
        ",\n  \"range\": \"main...feature\",\n  \"merge_base\": \"abc1234\",\n  \"status\": \"succeeded\",\n  \"diff_patch\": \"diff.patch\",\n  \"comments_json\": \"comments.json\",\n  \"comments_markdown\": \"comments.md\"\n}\n",
    );
    write_file(&bundle.join("manifest.json"), &manifest);
    write_file(
        &bundle.join("diff.patch"),
        "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1,1 +1,2 @@\n-old\n+new\n",
    );
    write_file(
        &bundle.join("comments.md"),
        "# Review: main...feature\n\n- Status: succeeded\n",
    );

    let mut comments_json = String::from("[\n");
    for (index, comment) in comments.iter().enumerate() {
        comments_json.push_str(&render_review_comment_json(comment));
        if let Some(patch_file) = comment.patch_file {
            write_file(
                &bundle.join(patch_file),
                &format!(
                    "diff --git a/{path} b/{path}\n@@ -1,1 +1,2 @@\n-old\n+new for {id}\n",
                    path = comment.path,
                    id = comment.id
                ),
            );
        }
        if index + 1 != comments.len() {
            comments_json.push(',');
        }
        comments_json.push('\n');
    }
    comments_json.push_str("]\n");
    write_file(&bundle.join("comments.json"), &comments_json);
    assert_valid_comments_json(&comments_json);
}

fn render_review_comment_json(comment: &ReviewCommentSpec<'_>) -> String {
    let mut output = String::from("  {\n");
    output.push_str(&format!(
        "    \"id\": {},\n",
        render_json_string(comment.id)
    ));
    output.push_str(&format!(
        "    \"path\": {},\n",
        render_json_string(comment.path)
    ));
    output.push_str(&format!(
        "    \"side\": {},\n",
        render_json_string(comment.side)
    ));
    output.push_str(&format!("    \"line\": {},\n", comment.line));
    output.push_str(&format!(
        "    \"severity\": {},\n",
        render_json_string(comment.severity)
    ));
    output.push_str(&format!(
        "    \"title\": {},\n",
        render_json_string(comment.title)
    ));
    output.push_str(&format!(
        "    \"body\": {}",
        render_json_string(comment.body)
    ));
    if let Some(hunk_header) = comment.hunk_header {
        output.push_str(&format!(
            ",\n    \"hunk_header\": {}",
            render_json_string(hunk_header)
        ));
    }
    if let Some(patch_file) = comment.patch_file {
        output.push_str(&format!(
            ",\n    \"patch_file\": {}",
            render_json_string(patch_file)
        ));
    }
    output.push_str("\n  }");
    output
}

fn render_json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            ch if ch.is_control() => output.push_str(&format!("\\u{:04x}", ch as u32)),
            _ => output.push(ch),
        }
    }
    output.push('"');
    output
}

fn assert_valid_comments_json(text: &str) {
    let parsed = fixture_json::parse(text).unwrap_or_else(|error| {
        panic!("write_review_bundle emitted invalid comments.json: {error}")
    });
    assert!(
        parsed.as_array().is_some(),
        "write_review_bundle must emit a comments.json array"
    );
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

fn fixed_date_script(path: &Path, timestamp: &str) -> PathBuf {
    let script = path.join("date");
    write_executable(
        &script,
        &format!("#!/bin/sh\nset -eu\nprintf '%s\\n' '{}'\n", timestamp),
    );
    script
}

fn address_bundle_id(timestamp: &str, selection_filter: &str) -> String {
    format!(
        "{}-latest-{}",
        timestamp.replace(':', "-"),
        slugify_for_address_id(selection_filter),
    )
}

fn slugify_for_address_id(text: &str) -> String {
    let mut output = String::new();
    let mut last_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' {
            output.push(ch);
            last_dash = false;
        } else if !last_dash {
            output.push('-');
            last_dash = true;
        }
    }

    let output = output.trim_matches('-').to_string();
    if output.is_empty() {
        "file".to_string()
    } else {
        output
    }
}

fn json_string_field(text: &str, field: &str) -> String {
    let prefix = format!("\"{field}\": \"");
    let start = text.find(&prefix).unwrap() + prefix.len();
    let value = &text[start..];
    value[..value.find('"').unwrap()].to_string()
}

fn assert_canonical_timestamp(timestamp: &str) {
    let bytes = timestamp.as_bytes();
    assert_eq!(bytes.len(), 20, "unexpected timestamp length: {timestamp}");
    assert_eq!(bytes[4], b'-', "unexpected timestamp shape: {timestamp}");
    assert_eq!(bytes[7], b'-', "unexpected timestamp shape: {timestamp}");
    assert_eq!(bytes[10], b'T', "unexpected timestamp shape: {timestamp}");
    assert_eq!(bytes[13], b':', "unexpected timestamp shape: {timestamp}");
    assert_eq!(bytes[16], b':', "unexpected timestamp shape: {timestamp}");
    assert_eq!(bytes[19], b'Z', "unexpected timestamp shape: {timestamp}");
    for index in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
        assert!(
            bytes[index].is_ascii_digit(),
            "unexpected timestamp shape: {timestamp}"
        );
    }
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
    anne_with_path(path, args, None)
}

fn anne_with_path(path: &Path, args: &[&str], path_env: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anne"));
    command.args(args).current_dir(path);
    if let Some(path_env) = path_env {
        command.env("PATH", path_env);
    }
    command.output().unwrap()
}

fn prepend_path(dir: &Path) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    if current.is_empty() {
        dir.display().to_string()
    } else {
        format!("{}:{}", dir.display(), current)
    }
}

fn address_bundle_path(repo: &Path, stdout: &[u8]) -> PathBuf {
    let stdout = String::from_utf8_lossy(stdout);
    let bundle = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Address bundle: "))
        .expect("missing bundle path in stdout");
    repo.join(bundle)
}

fn review_bundle_path(repo: &Path, review_id: &str) -> PathBuf {
    repo.join(".anne/reviews").join(review_id)
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
