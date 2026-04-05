use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn draft_workflow_defaults_file_provenance_and_rewrites_plan_state() {
    let draft = fs::read_to_string(repo_root().join(".vizier/workflows/draft.hcl"))
        .expect("read draft workflow");

    assert!(draft.contains("spec_source = \"file\""));
    assert!(draft.contains("id = \"validate_provenance\""));
    assert!(draft.contains("id = \"rewrite_plan_state_provenance\""));
    assert!(draft.contains("Path(\".vizier/prompts/DRAFT_PROMPTS.md\").read_text()"));
    assert!(draft.contains("record[\"source_path\"] = source_path"));
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
