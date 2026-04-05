use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn draft_workflow_uses_shared_plan_state_rewrite_helper() {
    let draft = fs::read_to_string(repo_root().join(".vizier/workflows/draft.hcl"))
        .expect("read draft workflow");

    assert!(draft.contains("spec_source = \"file\""));
    assert!(draft.contains("id = \"validate_provenance\""));
    assert!(draft.contains("id = \"rewrite_plan_state_provenance\""));
    assert!(draft.contains("Path(\".vizier/prompts/DRAFT_PROMPTS.md\").read_text()"));
    assert!(draft.contains("python3 .vizier/scripts/rewrite_plan_state.py"));
}

#[test]
fn plan_state_rewrite_prefers_overview_prose_for_summary() {
    let repo = TestRepo::new("overview-summary");
    write_file(
        &repo
            .path()
            .join(".vizier/implementation-plans/demo-overview-summary.md"),
        r#"---
plan_id: pln_demo_overview
plan: demo-overview-summary
branch: draft/demo-overview-summary
---

## Operator Spec
# Anne

## Feature: Distinct summaries

### Problem

The plan record should not keep the shared placeholder heading.

## Implementation Plan
## Overview

Keep `record.summary` specific to the generated implementation plan.

## Execution Plan

1. Recompute the persisted summary after plan persistence.
"#,
    );
    write_file(
        &repo
            .path()
            .join(".vizier/state/plans/pln_demo_overview.json"),
        r##"{
  "plan_id": "pln_demo_overview",
  "slug": "demo-overview-summary",
  "branch": "draft/demo-overview-summary",
  "source": "inline",
  "summary": "# Anne"
}
"##,
    );

    let output = run_plan_state_rewrite(repo.path(), "demo-overview-summary", "", "inline");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = fs::read_to_string(
        repo.path()
            .join(".vizier/state/plans/pln_demo_overview.json"),
    )
    .expect("read rewritten plan record");
    assert_eq!(
        json_string_field(&raw, "summary").as_deref(),
        Some("Keep record.summary specific to the generated implementation plan.")
    );
    assert_eq!(json_string_field(&raw, "source").as_deref(), Some("inline"));
    assert!(!raw.contains("\"source_path\""));
}

#[test]
fn plan_state_rewrite_falls_back_to_first_plan_body_line_without_overview() {
    let repo = TestRepo::new("plan-body-fallback");
    write_file(
        &repo
            .path()
            .join(".vizier/implementation-plans/demo-plan-body-fallback.md"),
        r#"---
plan_id: pln_demo_plan_body
plan: demo-plan-body-fallback
branch: draft/demo-plan-body-fallback
---

## Operator Spec
# Anne

## Feature: Plan body fallback

### Problem

The generated plan body should still produce a summary without an Overview section.

## Implementation Plan
## Execution Plan

Explain the execution order without relying on a dedicated Overview section.

## Risks & Unknowns

Keep the fallback deterministic.
"#,
    );
    write_file(
        &repo
            .path()
            .join(".vizier/state/plans/pln_demo_plan_body.json"),
        r##"{
  "plan_id": "pln_demo_plan_body",
  "slug": "demo-plan-body-fallback",
  "branch": "draft/demo-plan-body-fallback",
  "source": "inline",
  "summary": "# Anne"
}
"##,
    );

    let output = run_plan_state_rewrite(repo.path(), "demo-plan-body-fallback", "", "inline");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = fs::read_to_string(
        repo.path()
            .join(".vizier/state/plans/pln_demo_plan_body.json"),
    )
    .expect("read rewritten plan record");
    assert_eq!(
        json_string_field(&raw, "summary").as_deref(),
        Some("Explain the execution order without relying on a dedicated Overview section.")
    );
}

#[test]
fn plan_state_rewrite_falls_back_to_slug_when_only_boilerplate_exists() {
    let repo = TestRepo::new("slug-fallback");
    write_file(
        &repo
            .path()
            .join(".vizier/implementation-plans/demo-slug-fallback.md"),
        r#"---
plan_id: pln_demo_slug
plan: demo-slug-fallback
branch: draft/demo-slug-fallback
---

## Operator Spec
# Anne

## Source Comment

- Review: `R001`

## Implementation Plan
## Overview

## Execution Plan
"#,
    );
    write_file(
        &repo.path().join(".vizier/state/plans/pln_demo_slug.json"),
        r##"{
  "plan_id": "pln_demo_slug",
  "slug": "demo-slug-fallback",
  "branch": "draft/demo-slug-fallback",
  "source": "file",
  "source_path": "SPEC.md",
  "summary": "# Anne"
}
"##,
    );

    let output = run_plan_state_rewrite(repo.path(), "demo-slug-fallback", "SPEC.md", "file");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = fs::read_to_string(repo.path().join(".vizier/state/plans/pln_demo_slug.json"))
        .expect("read rewritten plan record");
    assert_eq!(
        json_string_field(&raw, "summary").as_deref(),
        Some("Plan demo-slug-fallback")
    );
    assert_eq!(json_string_field(&raw, "source").as_deref(), Some("file"));
    assert_eq!(
        json_string_field(&raw, "source_path").as_deref(),
        Some("SPEC.md")
    );
}

#[test]
fn plan_state_rewrite_normalizes_markdown_and_clips_after_normalization() {
    let repo = TestRepo::new("normalized-summary");
    let long_overview = format!(
        "This keeps [`record.summary`](https://example.invalid/docs) stable for `PlanSlug` consumers while preserving {}",
        "0123456789 ".repeat(12)
    );
    write_file(
        &repo
            .path()
            .join(".vizier/implementation-plans/demo-normalized-summary.md"),
        &format!(
            r#"---
plan_id: pln_demo_normalized
plan: demo-normalized-summary
branch: draft/demo-normalized-summary
---

## Operator Spec
# Anne

## Feature: Normalized summary

### Problem

The summary should strip markdown punctuation before clipping.

## Implementation Plan
## Overview

{}

## Execution Plan

1. Persist the normalized summary.
"#,
            long_overview
        ),
    );
    write_file(
        &repo
            .path()
            .join(".vizier/state/plans/pln_demo_normalized.json"),
        r##"{
  "plan_id": "pln_demo_normalized",
  "slug": "demo-normalized-summary",
  "branch": "draft/demo-normalized-summary",
  "source": "inline",
  "summary": "# Anne"
}
"##,
    );

    let output = run_plan_state_rewrite(repo.path(), "demo-normalized-summary", "", "inline");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = fs::read_to_string(
        repo.path()
            .join(".vizier/state/plans/pln_demo_normalized.json"),
    )
    .expect("read rewritten plan record");
    let summary = json_string_field(&raw, "summary").expect("summary field");
    assert!(summary.starts_with("This keeps record.summary stable for PlanSlug consumers"));
    assert!(!summary.contains('`'));
    assert!(!summary.contains('['));
    assert!(summary.ends_with("..."));
    assert!(summary.len() <= 120);
    assert!(summary.len() >= 100);
}

#[test]
fn audited_plan_record_preserves_file_backed_source_path() {
    let raw = fs::read_to_string(
        repo_root().join(".vizier/state/plans/pln_79a7128e082c43e6a572a9dc659653a1.json"),
    )
    .expect("read audited plan record");

    assert!(raw.contains("\"source\": \"file\""));
    assert!(raw.contains("\"source_path\": \"PAR.md\""));
}

#[test]
fn checked_in_plan_records_have_non_placeholder_distinguishable_summaries() {
    let plans_dir = repo_root().join(".vizier/state/plans");
    let mut summaries = BTreeSet::new();
    let mut record_count = 0usize;

    for entry in fs::read_dir(plans_dir).expect("read plan directory") {
        let entry = entry.expect("read plan entry");
        let raw = fs::read_to_string(entry.path()).expect("read plan record");
        let summary = json_string_field(&raw, "summary").expect("plan summary");

        assert!(
            !summary.trim().is_empty(),
            "empty summary in {}",
            entry.path().display()
        );
        assert_ne!(
            summary,
            "# Anne",
            "placeholder summary in {}",
            entry.path().display()
        );
        assert!(
            summaries.insert(summary),
            "duplicate summary in {}",
            entry.path().display()
        );
        record_count += 1;
    }

    assert!(record_count > 0, "expected checked-in plan records");
}

#[test]
fn legacy_plan_records_without_audited_provenance_remain_unbackfilled() {
    let plans_dir = repo_root().join(".vizier/state/plans");

    for entry in fs::read_dir(plans_dir).expect("read plan directory") {
        let entry = entry.expect("read plan entry");
        let file_name = entry.file_name();
        if file_name == "pln_79a7128e082c43e6a572a9dc659653a1.json" {
            continue;
        }

        let raw = fs::read_to_string(entry.path()).expect("read plan record");
        assert!(
            !raw.contains("\"source_path\""),
            "unexpected source_path backfill in {}",
            entry.path().display()
        );
    }
}

fn run_plan_state_rewrite(path: &Path, slug: &str, spec_file: &str, spec_source: &str) -> Output {
    Command::new("python3")
        .arg(repo_root().join(".vizier/scripts/rewrite_plan_state.py"))
        .arg("--slug")
        .arg(slug)
        .arg("--spec-file")
        .arg(spec_file)
        .arg("--spec-source")
        .arg(spec_source)
        .current_dir(path)
        .output()
        .expect("run plan-state rewrite helper")
}

fn json_string_field(raw: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\": ");

    raw.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with(&needle) {
            return None;
        }
        let value = line[needle.len()..].trim_end_matches(',');
        value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .map(ToOwned::to_owned)
    })
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, contents).expect("write file");
}

struct TestRepo {
    path: PathBuf,
}

impl TestRepo {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "anne-vizier-test-{}-{}-{}",
            label,
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&path).expect("create temp repo");
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
