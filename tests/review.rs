use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use git2::{Repository, RepositoryInitOptions, StatusOptions, build::CheckoutBuilder};

#[test]
fn review_uses_merge_base_semantics_and_records_skips() {
    let repo = TestRepo::new("merge-base");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    2\n}\n",
    );
    write_file(&repo.path().join("vendor/generated.txt"), "skip me\n");
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");
    write_file(
        &repo.path().join("src/main_only.rs"),
        "pub const MAIN: i32 = 42;\n",
    );
    commit_all(repo.path(), "main changes");

    write_agent_config(
        repo.path(),
        "text",
        &agent_script(
            repo.path(),
            "while IFS= read -r _line; do :; done\nprintf '[]\\n'\n",
        ),
        None,
    );

    let output = anne_with_path(repo.path(), &["review", "main...feature"], Some(""));
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let diff = fs::read_to_string(bundle.join("diff.patch")).unwrap();
    assert!(diff.contains("+    2"));
    assert!(!diff.contains("MAIN: i32 = 42"));

    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert_eq!(comments_json.trim(), "[]");

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("No findings."));
    assert!(comments_md.contains("## Skipped Files"));
    assert!(comments_md.contains("vendor/generated.txt"));
}

#[test]
fn review_uses_canonical_timestamp_without_date_on_path() {
    let repo = TestRepo::new("canonical-fallback-timestamp");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf '[]\\n'\n",
    );
    write_agent_config(repo.path(), "text", &agent, None);

    let output = anne_with_path(repo.path(), &["review", "main...feature"], Some(""));
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    let generated_at = json_string_field(&manifest, "generated_at");
    assert_canonical_timestamp(&generated_at);
    assert!(!generated_at.starts_with("unix-"));

    let expected_review_id = review_bundle_id(repo.path(), "main", "feature", &generated_at);
    assert_eq!(
        bundle.file_name().unwrap().to_string_lossy(),
        expected_review_id
    );
}

#[test]
fn review_accepts_plain_agent_findings() {
    let repo = TestRepo::new("plain-agent");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf '[{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":2,\"severity\":\"warning\",\"title\":\"Off-by-one result\",\"body\":\"The function now adds an unexpected extra 1.\"}]\\n'\n",
    );
    write_agent_config(repo.path(), "text", &agent, None);

    let output = anne(
        repo.path(),
        &["review", "--base", "main", "--head", "feature"],
    );
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert!(comments_json.contains("\"id\": \"R001\""));
    assert!(comments_json.contains("\"path\": \"src/lib.rs\""));
    assert!(comments_json.contains("\"side\": \"new\""));
    assert!(comments_json.contains("\"line\": 2"));
    assert!(comments_json.contains("\"patch_file\": \"files/0001-src-lib.rs.patch\""));
    assert!(!comments_json.contains("\"anchor\""));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("### R001 warning new:2"));
    assert!(comments_md.contains("Off-by-one result"));
}

#[test]
fn review_bad_ref_writes_preflight_failure_bundle() {
    let repo = TestRepo::new("bad-ref-preflight");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    let output = anne(repo.path(), &["review", "main...missing"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("Review bundle:"));
    assert!(stderr.contains("failed resolving head ref `missing`"));

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"stage\": \"preflight\""));
    assert!(manifest.contains("\"repository_opened\": true"));
    assert!(manifest.contains("\"base_resolved\": true"));
    assert!(manifest.contains("\"head_resolved\": false"));
    assert!(manifest.contains("\"merge_base\": null"));
    assert!(manifest.contains("\"diff_patch\": null"));
    assert!(manifest.contains("\"files\": []"));

    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert_eq!(comments_json.trim(), "[]");

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("- Head resolved: no"));
    assert!(comments_md.contains("No file review occurred."));
    assert!(comments_md.contains("Anne never reached file analysis."));

    assert!(!bundle.join("diff.patch").exists());
    assert!(!bundle.join("files").exists());
    assert!(!bundle.join("agent").exists());
}

#[test]
fn review_merge_base_failure_writes_preflight_failure_bundle() {
    let repo = TestRepo::new("merge-base-preflight");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    2\n}\n",
    );
    commit_all_without_parents_to_ref(repo.path(), "refs/heads/feature", "orphan feature");
    checkout_branch(repo.path(), "main");

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("Review bundle:"));
    assert!(stderr.contains("failed resolving merge base for `main` and `feature`"));

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"stage\": \"preflight\""));
    assert!(manifest.contains("\"base_resolved\": true"));
    assert!(manifest.contains("\"head_resolved\": true"));
    assert!(manifest.contains("\"merge_base_resolved\": false"));
    assert!(manifest.contains("\"merge_base\": null"));
    assert!(manifest.contains("\"diff_patch\": null"));

    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert_eq!(comments_json.trim(), "[]");

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("- Merge base resolved: no"));
    assert!(comments_md.contains("No file review occurred."));
    assert!(!bundle.join("diff.patch").exists());
}

#[test]
fn review_outside_repo_reports_no_bundle_path() {
    let repo = TestRepo::new("outside-repo");

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains("Review bundle:"));
    assert!(stderr.contains("failed discovering repository"));
}

#[test]
fn review_uses_bundled_default_agent_shim_when_config_is_missing() {
    let repo = TestRepo::new("bundled-default");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    codex_script(&bin_dir);
    jq_script(&bin_dir);

    let path_env = prepend_path(&bin_dir);
    let output = anne_with_path(repo.path(), &["review", "main...feature"], Some(&path_env));
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let captured_prompt = fs::read(repo.path().join(".anne-test/codex-stdin.bin")).unwrap();
    let persisted_prompt = fs::read(bundle.join("agent/0001-src-lib.rs.prompt.md")).unwrap();
    assert!(
        persisted_prompt.ends_with(b"\n"),
        "review prompt artifact unexpectedly omitted a trailing newline"
    );
    assert_eq!(captured_prompt, persisted_prompt);

    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert_eq!(comments_json.trim(), "[]");

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"label\": \"default\""));
    assert!(manifest.contains("\"command_source\": \"bundled-default\""));
    assert!(manifest.contains("default/agent.sh"));
    assert!(manifest.contains("default/filter.sh"));
    assert!(manifest.contains("\"progress_filter_source\": \"bundled-default\""));
    assert!(manifest.contains("\"bundled_filter_requested\": true"));
    assert!(manifest.contains("\"progress_filter_ran\": true"));
    assert!(manifest.contains("\"assistant_text_source\": \"progress-filter\""));

    assert!(bundle.join("agent/0001-src-lib.rs.response.txt").exists());
}

#[test]
fn review_surfaces_nested_turn_failed_message_from_bundled_default_filter() {
    let repo = TestRepo::new("bundled-default-turn-failed");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    codex_script_with_body(
        &bin_dir,
        "mkdir -p .anne-test\ncat >.anne-test/codex-stdin.bin\nprintf '%s\\n' '{\"type\":\"turn.failed\",\"error\":{\"message\":\"model refused\\nwith nested details\"}}'\nexit 23\n",
    );
    jq_script(&bin_dir);

    let path_env = prepend_path(&bin_dir);
    let output = anne_with_path(repo.path(), &["review", "main...feature"], Some(&path_env));
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("turn failed: model refused with nested details"));
    assert!(!stderr.contains("event: turn.failed"));

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"failed\""));
    assert!(manifest.contains("\"progress_filter_ran\": true"));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("turn failed: model refused with nested details"));

    let response = fs::read_to_string(bundle.join("agent/0001-src-lib.rs.response.txt")).unwrap();
    assert!(response.contains("\"type\":\"turn.failed\""));
    assert!(response.contains("model refused\\nwith nested details"));
}

#[test]
fn review_skips_bundled_default_filter_when_jq_is_missing() {
    let repo = TestRepo::new("bundled-default-no-jq");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    bundled_default_runtime_tools(&bin_dir);
    codex_script(&bin_dir);

    let path_env = bin_dir.display().to_string();
    let output = anne_with_path(repo.path(), &["review", "main...feature"], Some(&path_env));
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Bundled default progress filter skipped"));

    let bundle = bundle_path(repo.path(), &output.stdout);
    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert_eq!(comments_json.trim(), "[]");

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("Runtime Notes"));
    assert!(comments_md.contains("Bundled default progress filter skipped"));

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"succeeded\""));
    assert!(manifest.contains("\"errors\": []"));
    assert!(manifest.contains("\"bundled_filter_requested\": true"));
    assert!(manifest.contains("\"progress_filter_ran\": false"));
    assert!(manifest.contains("\"progress_filter_skip_reason\": \"jq was not found on PATH\""));
    assert!(manifest.contains("\"assistant_text_source\": \"wrapped-json-decoder\""));

    assert!(bundle.join("agent/0001-src-lib.rs.response.txt").exists());
}

#[test]
fn review_allows_default_label_with_custom_command_and_explicit_empty_filter() {
    let repo = TestRepo::new("default-label-custom-command-no-filter");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf '[{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":2,\"severity\":\"warning\",\"title\":\"Explicit filter disable respected\",\"body\":\"Anne kept the explicit custom command and did not re-enable the bundled filter.\"}]\\n'\n",
    );

    write_file(
        &repo.path().join(".anne/config.toml"),
        &format!(
            "\
[agent]
label = \"default\"
command = [\"{}\"]
progress_filter = []
output = \"text\"
enable_script_wrapper = false
workers = 4

[review]
max_patch_bytes = 4096
ignore_prefixes = [\"vendor/\"]
",
            agent.display()
        ),
    );

    let output = anne_with_path(repo.path(), &["review", "main...feature"], Some(""));
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr)
            .contains("Bundled default progress filter skipped")
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert!(comments_json.contains("\"title\": \"Explicit filter disable respected\""));

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"label\": \"default\""));
    assert!(manifest.contains(&agent.display().to_string()));
    assert!(manifest.contains("\"command_source\": \"explicit\""));
    assert!(manifest.contains("\"progress_filter\": []"));
    assert!(manifest.contains("\"progress_filter_source\": \"explicit\""));
    assert!(manifest.contains("\"bundled_filter_requested\": false"));
    assert!(manifest.contains("\"progress_filter_ran\": false"));
    assert!(manifest.contains("\"assistant_text_source\": \"raw-stdout\""));
    assert!(!manifest.contains("default/filter.sh"));
    assert!(!manifest.contains("jq was not found on PATH"));
}

#[test]
fn review_preserves_explicit_text_output_with_custom_filter() {
    let repo = TestRepo::new("explicit-text-custom-filter");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf 'thinking\\n'\nprintf 'FINAL:[{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":2,\"severity\":\"warning\",\"title\":\"Text output preserved\",\"body\":\"Anne should keep the explicit text runtime when a custom filter is present.\"}]\\n'\n",
    );
    let filter = filter_script(repo.path());
    write_agent_config(repo.path(), "text", &agent, Some(&filter));

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert!(comments_json.contains("\"title\": \"Text output preserved\""));

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"output\": \"text\""));
    assert!(manifest.contains("\"effective_output\": \"text\""));
    assert!(manifest.contains("\"progress_filter_source\": \"explicit\""));
    assert!(manifest.contains("\"assistant_text_source\": \"progress-filter\""));
    assert!(!manifest.contains("\"output\": \"wrapped-json\""));

    let response = fs::read_to_string(bundle.join("agent/0001-src-lib.rs.response.txt")).unwrap();
    assert!(response.contains("FINAL:[{"));
}

#[test]
fn review_preserves_bundle_path_when_initial_snapshot_publish_fails() {
    let repo = TestRepo::new("output-write-failure");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf '[]\\n'\n",
    );
    write_agent_config(repo.path(), "text", &agent, None);

    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fixed_date_script(&bin_dir, "2026-04-04T00:00:00Z");

    let review_id = review_bundle_id(repo.path(), "main", "feature", "2026-04-04T00:00:00Z");
    let bundle = repo.path().join(".anne").join("reviews").join(&review_id);
    fs::create_dir_all(bundle.join("comments.md")).unwrap();

    let path_env = prepend_path(&bin_dir);
    let output = anne_with_path(repo.path(), &["review", "main...feature"], Some(&path_env));
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_bundle = bundle_path(repo.path(), &output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stdout_bundle, bundle);
    assert!(stderr.contains("failed writing comments.md"));

    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert_eq!(comments_json.trim(), "[]");
    assert!(!bundle.join("manifest.json").exists());
    assert!(bundle.join("comments.md").is_dir());
}

#[test]
fn review_publishes_readable_running_snapshot_before_completion() {
    let repo = TestRepo::new("running-snapshot");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/a.rs"),
        "pub fn alpha() -> i32 {\n    1\n}\n",
    );
    write_file(
        &repo.path().join("src/b.rs"),
        "pub fn beta() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/a.rs"),
        "pub fn alpha() -> i32 {\n    2\n}\n",
    );
    write_file(
        &repo.path().join("src/b.rs"),
        "pub fn beta() -> i32 {\n    2\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        r#"prompt=$(cat)
mkdir -p .anne-test
case "$prompt" in
  *"Path: src/a.rs"*)
    : > .anne-test/a-started
    attempts=0
    while [ ! -f .anne-test/continue ]; do
      attempts=$((attempts + 1))
      if [ "$attempts" -ge 100 ]; then
        printf 'timed out waiting for continue\n' >&2
        exit 91
      fi
      sleep 0.1
    done
    printf '[]\n'
    ;;
  *"Path: src/b.rs"*)
    : > .anne-test/b-finished
    printf '[]\n'
    ;;
  *)
    printf 'unexpected prompt\n' >&2
    exit 1
    ;;
esac
"#,
    );
    write_agent_config_with_workers(repo.path(), "text", &agent, None, 2);

    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fixed_date_script(&bin_dir, "2026-04-04T18:00:00Z");
    let path_env = prepend_path(&bin_dir);

    let bundle = repo.path().join(".anne/reviews").join(review_bundle_id(
        repo.path(),
        "main",
        "feature",
        "2026-04-04T18:00:00Z",
    ));
    let child = spawn_anne_with_path(repo.path(), &["review", "main...feature"], Some(&path_env));

    wait_for("running snapshot publication", || {
        repo.path().join(".anne-test/a-started").exists()
            && repo.path().join(".anne-test/b-finished").exists()
            && bundle.join("manifest.json").is_file()
            && bundle.join("comments.json").is_file()
            && bundle.join("comments.md").exists()
    });

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"running\""));

    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert_eq!(comments_json.trim(), "[]");

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("This review is still running."));
    assert!(comments_md.contains("## src/a.rs"));
    assert!(comments_md.contains("Review running."));
    assert!(comments_md.contains("## src/b.rs"));
    assert!(comments_md.contains("Review complete for this file."));

    write_file(&repo.path().join(".anne-test/continue"), "continue\n");
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn review_uses_progress_filter_and_fails_on_invalid_anchor() {
    let repo = TestRepo::new("filter-failure");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf 'thinking\\n'\nprintf 'FINAL:[{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":99,\"severity\":\"warning\",\"title\":\"Bad anchor\",\"body\":\"This should fail anchor validation.\"}]\\n'\n",
    );
    let filter = filter_script(repo.path());
    write_agent_config(repo.path(), "wrapped-json", &agent, Some(&filter));

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"failed\""));
    assert!(manifest.contains("points at unchanged new line 99"));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("Review failed."));

    let response = fs::read_to_string(bundle.join("agent/0001-src-lib.rs.response.txt")).unwrap();
    assert!(response.contains("FINAL:[{"));
}

#[test]
fn review_explicit_progress_filter_dependency_failure_is_still_fatal() {
    let repo = TestRepo::new("explicit-filter-missing-dependency");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"[]\"}}'\n",
    );
    let filter = filter_requires_jq_script(repo.path());
    write_agent_config(repo.path(), "wrapped-json", &agent, Some(&filter));

    let output = anne_with_path(repo.path(), &["review", "main...feature"], Some(""));
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"failed\""));
    assert!(manifest.contains("\"progress_filter_ran\": true"));

    assert!(bundle.join("agent/0001-src-lib.rs.response.txt").exists());
}

#[test]
fn review_wrapped_json_protocol_failure_preserves_raw_response() {
    let repo = TestRepo::new("wrapped-json-protocol-failure");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"reasoning\",\"text\":\"thinking\"}}'\n",
    );
    write_agent_config(repo.path(), "wrapped-json", &agent, None);

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"failed\""));
    assert!(
        manifest.contains("failed recovering final assistant message from wrapped-json stdout")
    );
    assert!(manifest.contains("\"assistant_text_source\": \"wrapped-json-decoder\""));

    let response = fs::read_to_string(bundle.join("agent/0001-src-lib.rs.response.txt")).unwrap();
    assert!(response.contains("\"reasoning\""));
}

#[test]
fn review_parallel_workers_preserve_stable_output_order() {
    let repo = TestRepo::new("parallel-order");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/a.rs"),
        "pub fn alpha() -> i32 {\n    1\n}\n",
    );
    write_file(
        &repo.path().join("src/b.rs"),
        "pub fn beta() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/a.rs"),
        "pub fn alpha() -> i32 {\n    2\n}\n",
    );
    write_file(
        &repo.path().join("src/b.rs"),
        "pub fn beta() -> i32 {\n    2\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        r#"prompt=$(cat)
mkdir -p .anne-test
case "$prompt" in
  *"Path: src/a.rs"*)
    : > .anne-test/a-started
    attempts=0
    while [ ! -f .anne-test/b-started ]; do
      attempts=$((attempts + 1))
      if [ "$attempts" -ge 10 ]; then
        printf 'b never started\n' >&2
        exit 91
      fi
      sleep 0.1
    done
    printf '[{"path":"src/a.rs","side":"new","line":2,"severity":"warning","title":"A finding","body":"A body."}]\n'
    ;;
  *"Path: src/b.rs"*)
    : > .anne-test/b-started
    printf '[{"path":"src/b.rs","side":"new","line":2,"severity":"warning","title":"B finding","body":"B body."}]\n'
    ;;
  *)
    printf 'unexpected prompt\n' >&2
    exit 1
    ;;
esac
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"workers\": 4"));

    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert!(comments_json.contains("\"id\": \"R001\""));
    assert!(comments_json.contains("\"id\": \"R002\""));
    assert!(
        comments_json.find("\"title\": \"A finding\"").unwrap()
            < comments_json.find("\"title\": \"B finding\"").unwrap()
    );
    assert!(
        comments_json
            .find("\"patch_file\": \"files/0001-src-a.rs.patch\"")
            .unwrap()
            < comments_json
                .find("\"patch_file\": \"files/0002-src-b.rs.patch\"")
                .unwrap()
    );

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(
        comments_md
            .find("### R001 warning new:2\nA finding")
            .unwrap()
            < comments_md
                .find("### R002 warning new:2\nB finding")
                .unwrap()
    );
}

#[test]
fn review_preserves_partial_results_when_one_parallel_file_fails() {
    let repo = TestRepo::new("parallel-partial-failure");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/a.rs"),
        "pub fn alpha() -> i32 {\n    1\n}\n",
    );
    write_file(
        &repo.path().join("src/b.rs"),
        "pub fn beta() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/a.rs"),
        "pub fn alpha() -> i32 {\n    2\n}\n",
    );
    write_file(
        &repo.path().join("src/b.rs"),
        "pub fn beta() -> i32 {\n    2\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        r#"prompt=$(cat)
case "$prompt" in
  *"Path: src/a.rs"*)
    printf '[{"path":"src/a.rs","side":"new","line":2,"severity":"warning","title":"A finding","body":"A body."}]\n'
    ;;
  *"Path: src/b.rs"*)
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

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"failed\""));
    assert!(manifest.contains("\"files_failed\": 1"));
    assert!(manifest.contains("\"findings\": 1"));

    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert!(comments_json.contains("\"path\": \"src/a.rs\""));
    assert!(!comments_json.contains("\"path\": \"src/b.rs\""));

    let failed_response =
        fs::read_to_string(bundle.join("agent/0002-src-b.rs.response.txt")).unwrap();
    assert!(failed_response.contains("partial raw output before failure"));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("### R001 warning new:2"));
    assert!(comments_md.contains("src/b.rs"));
    assert!(comments_md.contains("Review failed."));
}

#[test]
fn review_workers_one_disables_parallel_fan_out() {
    let repo = TestRepo::new("workers-one");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/a.rs"),
        "pub fn alpha() -> i32 {\n    1\n}\n",
    );
    write_file(
        &repo.path().join("src/b.rs"),
        "pub fn beta() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/a.rs"),
        "pub fn alpha() -> i32 {\n    2\n}\n",
    );
    write_file(
        &repo.path().join("src/b.rs"),
        "pub fn beta() -> i32 {\n    2\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        r#"prompt=$(cat)
mkdir -p .anne-test
case "$prompt" in
  *"Path: src/a.rs"*)
    attempts=0
    while [ ! -f .anne-test/b-started ]; do
      attempts=$((attempts + 1))
      if [ "$attempts" -ge 10 ]; then
        printf 'b never started\n' >&2
        exit 91
      fi
      sleep 0.1
    done
    printf '[{"path":"src/a.rs","side":"new","line":2,"severity":"warning","title":"A finding","body":"A body."}]\n'
    ;;
  *"Path: src/b.rs"*)
    : > .anne-test/b-started
    printf '[{"path":"src/b.rs","side":"new","line":2,"severity":"warning","title":"B finding","body":"B body."}]\n'
    ;;
  *)
    printf 'unexpected prompt\n' >&2
    exit 1
    ;;
esac
"#,
    );
    write_agent_config_with_workers(repo.path(), "text", &agent, None, 1);

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"workers\": 1"));
    assert!(manifest.contains("b never started"));
    assert!(manifest.contains("\"status\": \"failed\""));
}

#[test]
fn review_skips_pure_renames_from_git2_diff_data() {
    let repo = TestRepo::new("rename-only");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/old.rs"),
        "pub fn value() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    fs::rename(
        repo.path().join("src/old.rs"),
        repo.path().join("src/new.rs"),
    )
    .unwrap();
    commit_all(repo.path(), "rename file");

    checkout_branch(repo.path(), "main");
    write_agent_config(
        repo.path(),
        "text",
        &agent_script(
            repo.path(),
            "while IFS= read -r _line; do :; done\nprintf '[]\\n'\n",
        ),
        None,
    );

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let diff = fs::read_to_string(bundle.join("diff.patch")).unwrap();
    assert!(diff.contains("rename from src/old.rs"));
    assert!(diff.contains("rename to src/new.rs"));
    assert!(!diff.contains("@@"));

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"old_path\": \"src/old.rs\""));
    assert!(manifest.contains("\"new_path\": \"src/new.rs\""));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("src/new.rs: pure rename without textual changes"));
}

#[cfg(unix)]
#[test]
fn review_expands_file_to_symlink_typechanges_into_one_reviewed_file() {
    let repo = TestRepo::new("file-to-symlink");
    init_repo(repo.path());

    write_file(&repo.path().join("target/path"), "target file\n");
    write_file(&repo.path().join("src/link"), "line1\nline2\n");
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    fs::remove_file(repo.path().join("src/link")).unwrap();
    write_symlink("target/path", &repo.path().join("src/link"));
    commit_all(repo.path(), "convert to symlink");

    fs::remove_file(repo.path().join("src/link")).unwrap();
    checkout_branch(repo.path(), "main");
    write_agent_config(
        repo.path(),
        "text",
        &agent_script(
            repo.path(),
            "while IFS= read -r _line; do :; done\nprintf '[]\\n'\n",
        ),
        None,
    );

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let diff = fs::read_to_string(bundle.join("diff.patch")).unwrap();
    let file_patch = fs::read_to_string(bundle.join("files/0001-src-link.patch")).unwrap();
    assert_eq!(diff, file_patch);
    assert!(diff.contains("deleted file mode 100644"));
    assert!(diff.contains("new file mode 120000"));
    assert!(diff.contains("@@ -1,2 +0,0 @@"));
    assert!(diff.contains("@@ -0,0 +1,1 @@"));
    assert!(diff.contains("-line1"));
    assert!(diff.contains("-line2"));
    assert!(diff.contains("+target/path"));

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"patch_file\": \"files/0001-src-link.patch\""));
    assert!(!manifest.contains("metadata-only change without textual diff"));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert_eq!(comments_md.matches("## src/link\n").count(), 1);
    assert!(comments_md.contains("No findings."));
    assert!(!comments_md.contains("## Skipped Files"));
}

#[cfg(unix)]
#[test]
fn review_expands_symlink_to_file_typechanges_into_one_reviewed_file() {
    let repo = TestRepo::new("symlink-to-file");
    init_repo(repo.path());

    write_file(&repo.path().join("target/path"), "target file\n");
    write_symlink("target/path", &repo.path().join("src/link"));
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    fs::remove_file(repo.path().join("src/link")).unwrap();
    write_file(&repo.path().join("src/link"), "line1\nline2\n");
    commit_all(repo.path(), "convert to file");

    checkout_branch(repo.path(), "main");
    write_agent_config(
        repo.path(),
        "text",
        &agent_script(
            repo.path(),
            "while IFS= read -r _line; do :; done\nprintf '[]\\n'\n",
        ),
        None,
    );

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let diff = fs::read_to_string(bundle.join("diff.patch")).unwrap();
    let file_patch = fs::read_to_string(bundle.join("files/0001-src-link.patch")).unwrap();
    assert_eq!(diff, file_patch);
    assert!(diff.contains("deleted file mode 120000"));
    assert!(diff.contains("new file mode 100644"));
    assert!(diff.contains("@@ -1,1 +0,0 @@"));
    assert!(diff.contains("@@ -0,0 +1,2 @@"));
    assert!(diff.contains("-target/path"));
    assert!(diff.contains("+line1"));
    assert!(diff.contains("+line2"));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert_eq!(comments_md.matches("## src/link\n").count(), 1);
    assert!(comments_md.contains("No findings."));
    assert!(!comments_md.contains("## Skipped Files"));
}

#[cfg(unix)]
#[test]
fn review_keeps_symlink_target_edits_as_modified_file_reviews() {
    let repo = TestRepo::new("symlink-target-edit");
    init_repo(repo.path());

    write_file(&repo.path().join("old/target"), "old target\n");
    write_file(&repo.path().join("new/target"), "new target\n");
    write_symlink("old/target", &repo.path().join("src/link"));
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    fs::remove_file(repo.path().join("src/link")).unwrap();
    write_symlink("new/target", &repo.path().join("src/link"));
    commit_all(repo.path(), "change target");

    checkout_branch(repo.path(), "main");
    write_agent_config(
        repo.path(),
        "text",
        &agent_script(
            repo.path(),
            "while IFS= read -r _line; do :; done\nprintf '[]\\n'\n",
        ),
        None,
    );

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let diff = fs::read_to_string(bundle.join("diff.patch")).unwrap();
    assert!(diff.contains("index"));
    assert!(diff.contains("120000"));
    assert!(diff.contains("@@ -1 +1 @@"));
    assert!(diff.contains("-old/target"));
    assert!(diff.contains("+new/target"));
    assert!(!diff.contains("deleted file mode"));
    assert!(!diff.contains("new file mode"));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("## src/link"));
    assert!(comments_md.contains("No findings."));
}

#[cfg(unix)]
#[test]
fn review_keeps_permission_only_mode_changes_as_metadata_only_skips() {
    let repo = TestRepo::new("permission-only");
    init_repo(repo.path());

    write_file(&repo.path().join("src/tool"), "echo hi\n");
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    set_mode(&repo.path().join("src/tool"), 0o755);
    commit_all(repo.path(), "make executable");

    checkout_branch(repo.path(), "main");
    write_agent_config(
        repo.path(),
        "text",
        &agent_script(
            repo.path(),
            "while IFS= read -r _line; do :; done\nprintf '[]\\n'\n",
        ),
        None,
    );

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let diff = fs::read_to_string(bundle.join("diff.patch")).unwrap();
    assert!(diff.contains("old mode 100644"));
    assert!(diff.contains("new mode 100755"));
    assert!(!bundle.join("files/0001-src-tool.patch").exists());

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("## Skipped Files"));
    assert!(comments_md.contains("src/tool: metadata-only change without textual diff"));
}

#[cfg(unix)]
#[test]
fn review_skips_binary_file_to_symlink_typechanges_with_explicit_reason() {
    let repo = TestRepo::new("binary-to-symlink");
    init_repo(repo.path());

    write_file(&repo.path().join("target/path"), "target file\n");
    write_bytes(&repo.path().join("src/blob.bin"), b"\x00\x01\x02binary");
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    fs::remove_file(repo.path().join("src/blob.bin")).unwrap();
    write_symlink("target/path", &repo.path().join("src/blob.bin"));
    commit_all(repo.path(), "convert binary to symlink");

    fs::remove_file(repo.path().join("src/blob.bin")).unwrap();
    checkout_branch(repo.path(), "main");
    write_agent_config(
        repo.path(),
        "text",
        &agent_script(
            repo.path(),
            "while IFS= read -r _line; do :; done\nprintf '[]\\n'\n",
        ),
        None,
    );

    let output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = bundle_path(repo.path(), &output.stdout);
    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("src/blob.bin: binary file/symlink typechange"));
    assert!(!comments_md.contains("src/blob.bin: metadata-only change without textual diff"));
    assert!(!bundle.join("files/0001-src-blob.bin.patch").exists());
}

#[test]
fn default_filter_surfaces_nested_turn_failed_message() {
    let repo = TestRepo::new("default-filter-turn-failed");
    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    jq_script(&bin_dir);

    let input = "{\"type\":\"turn.failed\",\"error\":{\"message\":\"model refused\\nwith nested details\"}}\n";
    let output = run_executable_with_stdin(
        &default_filter_script(),
        input,
        Some(&prepend_path(&bin_dir)),
    );
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(String::from_utf8_lossy(&output.stdout), "");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("turn failed: model refused with nested details"));
    assert!(!stderr.contains("event: turn.failed"));
}

#[test]
fn default_filter_uses_item_message_for_warning_fallback_and_preserves_last_payload() {
    let repo = TestRepo::new("default-filter-warning-fallback");
    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    jq_script(&bin_dir);

    let input = concat!(
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"error\",\"message\":\"warning payload\\nfrom item.message\"}}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"error\"}}\n",
    );
    let output = run_executable_with_stdin(
        &default_filter_script(),
        input,
        Some(&prepend_path(&bin_dir)),
    );
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "warning payload from item.message\n"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error: warning payload from item.message"));
    assert!(stderr.contains("error update"));
}

#[test]
fn default_filter_keeps_final_agent_message_over_warning_fallback() {
    let repo = TestRepo::new("default-filter-agent-message-precedence");
    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    jq_script(&bin_dir);

    let input = concat!(
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"error\",\"message\":\"warning payload\"}}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"[]\"}}\n",
    );
    let output = run_executable_with_stdin(
        &default_filter_script(),
        input,
        Some(&prepend_path(&bin_dir)),
    );
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(String::from_utf8_lossy(&output.stdout), "[]\n");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error: warning payload"));
    assert!(stderr.contains("agent reply received"));
}

#[test]
fn repo_layout_rust_and_shell_files_do_not_invoke_git_cli() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = repo_layout_git_cli_guard_files(repo_root).unwrap();
    let shell_files = files
        .iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sh"))
        .collect::<Vec<_>>();

    for expected in [
        Path::new("ci.sh"),
        Path::new("examples/agents/default/agent.sh"),
        Path::new("examples/agents/default/filter.sh"),
    ] {
        assert!(
            shell_files.iter().any(|path| *path == expected),
            "repo-layout shell guard unexpectedly omitted {}",
            expected.display()
        );
    }

    if let Err(error) = check_repo_layout_git_cli_guard(repo_root) {
        panic!("{error}");
    }
}

#[test]
fn repo_layout_git_cli_guard_discovers_new_shell_scripts_and_skips_excluded_dirs() {
    let repo = TestRepo::new("git-cli-guard-discovery");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    1\n}\n",
    );
    write_file(
        &repo.path().join("tests/review_helper.rs"),
        "#[test]\nfn helper() {}\n",
    );
    write_executable(&repo.path().join("ci.sh"), "#!/bin/sh\nexit 0\n");
    write_executable(
        &repo.path().join("scripts/new-tool.sh"),
        "#!/bin/sh\nexit 0\n",
    );
    write_executable(
        &repo.path().join("examples/agents/custom/helper.sh"),
        "#!/bin/sh\nexit 0\n",
    );
    write_executable(
        &repo.path().join(".anne/reviews/ignored.sh"),
        "#!/bin/sh\nexec git status\n",
    );
    write_executable(
        &repo.path().join(".git/hooks/pre-commit.sh"),
        "#!/bin/sh\nexec git status\n",
    );
    write_executable(
        &repo.path().join("target/generated/ignored.sh"),
        "#!/bin/sh\nexec git status\n",
    );

    let files = repo_layout_git_cli_guard_files(repo.path()).unwrap();
    assert_eq!(
        files,
        vec![
            PathBuf::from("ci.sh"),
            PathBuf::from("examples/agents/custom/helper.sh"),
            PathBuf::from("scripts/new-tool.sh"),
            PathBuf::from("src/lib.rs"),
            PathBuf::from("tests/review_helper.rs"),
        ]
    );
}

#[test]
fn repo_layout_git_cli_guard_treats_missing_optional_directories_as_empty() {
    let repo = TestRepo::new("git-cli-guard-empty");

    let files = repo_layout_git_cli_guard_files(repo.path()).unwrap();
    assert!(files.is_empty(), "unexpected guard files: {files:?}");
    check_repo_layout_git_cli_guard(repo.path()).unwrap();
}

#[test]
fn repo_layout_git_cli_guard_reports_new_shell_violations_with_line_numbers() {
    let repo = TestRepo::new("git-cli-guard-shell-violation");
    write_executable(
        &repo.path().join("scripts/new-tool.sh"),
        "#!/bin/sh\nprintf 'ok\\n'\nexec git status\n",
    );

    let error = check_repo_layout_git_cli_guard(repo.path()).unwrap_err();
    assert!(
        error.contains("scripts/new-tool.sh:3"),
        "unexpected error: {error}"
    );
}

#[test]
fn repo_layout_git_cli_guard_reports_rust_violations_with_patterns() {
    let repo = TestRepo::new("git-cli-guard-rust-violation");
    write_file(
        &repo.path().join("src/lib.rs"),
        "use std::process::Command;\n\npub fn run() {\n    let _ = Command::new(\"git\");\n}\n",
    );

    let error = check_repo_layout_git_cli_guard(repo.path()).unwrap_err();
    assert!(error.contains("src/lib.rs"), "unexpected error: {error}");
    assert!(
        error.contains("Command::new(\"git\")"),
        "unexpected error: {error}"
    );
}

#[cfg(unix)]
#[test]
fn repo_layout_git_cli_guard_fails_when_a_matched_file_is_unreadable() {
    let repo = TestRepo::new("git-cli-guard-unreadable");
    let blocked = repo.path().join("scripts/blocked.sh");
    write_executable(&blocked, "#!/bin/sh\nexit 0\n");

    set_mode(&blocked, 0o000);
    let result = check_repo_layout_git_cli_guard(repo.path());
    set_mode(&blocked, 0o755);

    let error = result.unwrap_err();
    assert!(
        error.contains("failed reading guard file scripts/blocked.sh"),
        "unexpected error: {error}"
    );
}

#[test]
fn default_dependency_graph_excludes_remote_transport_native_crates() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tree = cargo_stdout(
        repo_root,
        &[
            "tree",
            "--locked",
            "--offline",
            "-e",
            "all",
            "--prefix",
            "none",
        ],
    );

    assert!(
        !tree.contains("openssl-sys v"),
        "default dependency graph unexpectedly contains openssl-sys:\n{tree}"
    );
    assert!(
        !tree.contains("libssh2-sys v"),
        "default dependency graph unexpectedly contains libssh2-sys:\n{tree}"
    );
}

#[test]
fn default_git2_feature_surface_stays_local_only() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let outgoing_tree = cargo_stdout(
        repo_root,
        &[
            "tree",
            "--locked",
            "--offline",
            "-e",
            "features",
            "-p",
            "git2",
        ],
    );
    let incoming_tree = cargo_stdout(
        repo_root,
        &[
            "tree",
            "--locked",
            "--offline",
            "-e",
            "features",
            "-i",
            "git2",
        ],
    );

    assert!(
        incoming_tree.contains("git2 feature \"vendored-libgit2\""),
        "default git2 feature graph lost vendored-libgit2:\n{incoming_tree}"
    );
    for forbidden in [
        "git2 feature \"default\"",
        "git2 feature \"https\"",
        "git2 feature \"ssh\"",
    ] {
        assert!(
            !outgoing_tree.contains(forbidden),
            "default git2 feature graph unexpectedly contains {forbidden}:\n{outgoing_tree}"
        );
    }
}

fn init_repo(path: &Path) {
    let mut options = RepositoryInitOptions::new();
    options.initial_head("main");
    let repo = Repository::init_opts(path, &options).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Anne Test").unwrap();
    config.set_str("user.email", "anne@example.com").unwrap();
}

fn create_and_checkout_branch(path: &Path, name: &str) {
    let repo = open_repo(path);
    let head = head_commit(&repo);
    repo.branch(name, &head, false).unwrap();
    drop(head);
    checkout_branch(path, name);
}

fn checkout_branch(path: &Path, name: &str) {
    let repo = open_repo(path);
    repo.set_head(&format!("refs/heads/{name}")).unwrap();
    let mut checkout = CheckoutBuilder::new();
    checkout.force();
    repo.checkout_head(Some(&mut checkout)).unwrap();
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
    text.push_str("enable_script_wrapper = false\n\n");
    text.push_str(&format!("workers = {}\n\n", workers));
    text.push_str("[review]\n");
    text.push_str("max_patch_bytes = 4096\n");
    text.push_str("ignore_prefixes = [\"vendor/\"]\n");

    write_file(&path.join(".anne/config.toml"), &text);
}

fn agent_script(path: &Path, body: &str) -> PathBuf {
    let script = path.join("agent.sh");
    write_executable(&script, &format!("#!/bin/sh\nset -eu\n{}\n", body));
    script
}

fn filter_script(path: &Path) -> PathBuf {
    let script = path.join("filter.sh");
    write_executable(
        &script,
        "#!/bin/sh\nset -eu\nwhile IFS= read -r line; do\n  case \"$line\" in\n    FINAL:*) printf '%s\\n' \"${line#FINAL:}\" ;;\n    *) printf '%s\\n' \"$line\" >&2 ;;\n  esac\ndone\n",
    );
    script
}

fn filter_requires_jq_script(path: &Path) -> PathBuf {
    let script = path.join("filter-requires-jq.sh");
    write_executable(
        &script,
        "#!/bin/sh\nset -eu\nif ! command -v jq >/dev/null 2>&1; then\n  printf 'custom filter requires jq\\n' >&2\n  exit 7\nfi\ncat >/dev/null\n",
    );
    script
}

fn codex_script(path: &Path) -> PathBuf {
    codex_script_with_body(
        path,
        "mkdir -p .anne-test\ncat >.anne-test/codex-stdin.bin\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"[]\"}}'\n",
    )
}

fn codex_script_with_body(path: &Path, body: &str) -> PathBuf {
    let script = path.join("codex");
    write_executable(
        &script,
        &format!(
            "#!/usr/bin/env bash\nset -euo pipefail\n[ \"$1\" = \"exec\" ]\n[ \"$2\" = \"--json\" ]\n[ \"$3\" = \"-\" ]\n{}",
            body
        ),
    );
    script
}

fn bundled_default_runtime_tools(path: &Path) {
    for name in ["bash", "cat", "mkdir", "mktemp", "rm"] {
        passthrough_script(path, name);
    }
}

fn passthrough_script(path: &Path, name: &str) -> PathBuf {
    let script = path.join(name);
    let target = [
        Path::new("/bin").join(name),
        Path::new("/usr/bin").join(name),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
    .unwrap_or_else(|| panic!("missing passthrough target for {name}"));
    write_executable(
        &script,
        &format!("#!/bin/sh\nexec {} \"$@\"\n", target.display()),
    );
    script
}

fn jq_script(path: &Path) -> PathBuf {
    let script = path.join("jq");
    write_executable(
        &script,
        r#"#!/usr/bin/env bash
set -euo pipefail
[ "$1" = "-r" ]
expr="$2"
input=$(cat)
export ANNE_TEST_JQ_INPUT="$input"
python3 - "$expr" <<'PY'
import json
import os
import sys

expr = sys.argv[1]
data = json.loads(os.environ["ANNE_TEST_JQ_INPUT"])

def get_path(*parts):
    value = data
    for part in parts:
        if not isinstance(value, dict) or part not in value:
            return None
        value = value[part]
    return value

def coalesce(*values):
    for value in values:
        if value is not None:
            return value
    return ""

value = {
    'try .type // ""': get_path("type"),
    'try .thread_id // ""': get_path("thread_id"),
    'try .error.message // ""': get_path("error", "message"),
    'try .item.type // ""': get_path("item", "type"),
    'try .item.text // ""': get_path("item", "text"),
    'try .item.message // ""': get_path("item", "message"),
    'try .item.status // ""': get_path("item", "status"),
    'try (.item.progress // .progress) // ""': coalesce(
        get_path("item", "progress"),
        get_path("progress"),
    ),
    'try .item.command // ""': get_path("item", "command"),
    'try .item.exit_code // ""': get_path("item", "exit_code"),
    'try (.usage.input_tokens // .usage.prompt_tokens) // ""': coalesce(
        get_path("usage", "input_tokens"),
        get_path("usage", "prompt_tokens"),
    ),
    'try .usage.cached_input_tokens // ""': get_path("usage", "cached_input_tokens"),
    'try (.usage.output_tokens // .usage.completion_tokens) // ""': coalesce(
        get_path("usage", "output_tokens"),
        get_path("usage", "completion_tokens"),
    ),
    'try .usage.reasoning_output_tokens // ""': get_path("usage", "reasoning_output_tokens"),
    'try (.usage.total_tokens // .usage.total) // ""': coalesce(
        get_path("usage", "total_tokens"),
        get_path("usage", "total"),
    ),
    'try (.phase // .label) // ""': coalesce(get_path("phase"), get_path("label")),
    'try .message // ""': get_path("message"),
    'try .detail // .path // ""': coalesce(get_path("detail"), get_path("path")),
}.get(expr, "")

if value is None:
    value = ""

if isinstance(value, bool):
    print("true" if value else "false")
else:
    print(value)
PY
"#,
    );
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

fn review_bundle_id(_repo: &Path, base: &str, head: &str, timestamp: &str) -> String {
    format!(
        "{}-{}...{}",
        timestamp.replace(':', "-"),
        slugify_for_review_id(base),
        slugify_for_review_id(head),
    )
}

fn slugify_for_review_id(text: &str) -> String {
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

fn prepend_path(dir: &Path) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    if current.is_empty() {
        dir.display().to_string()
    } else {
        format!("{}:{}", dir.display(), current)
    }
}

fn default_filter_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/agents/default/filter.sh")
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

fn spawn_anne_with_path(path: &Path, args: &[&str], path_env: Option<&str>) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anne"));
    command.args(args).current_dir(path);
    if let Some(path_env) = path_env {
        command.env("PATH", path_env);
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn run_executable_with_stdin(executable: &Path, input: &str, path_env: Option<&str>) -> Output {
    let mut command = Command::new(executable);
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    if let Some(path_env) = path_env {
        command.env("PATH", path_env);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn wait_for(label: &str, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }

    panic!("timed out waiting for {label}");
}

fn cargo_stdout(repo_root: &Path, args: &[&str]) -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args(args)
        .current_dir(repo_root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cargo {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn commit_all(path: &Path, message: &str) {
    let repo = open_repo(path);
    let removed_paths = collect_removed_paths(&repo);
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    for path in removed_paths {
        index.remove_path(&path).unwrap();
    }
    index.write().unwrap();

    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = repo.signature().unwrap();
    let parent = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .map(|oid| repo.find_commit(oid).unwrap());
    let parents = parent.iter().collect::<Vec<_>>();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )
    .unwrap();
}

fn commit_all_without_parents_to_ref(path: &Path, reference: &str, message: &str) {
    let repo = open_repo(path);
    let removed_paths = collect_removed_paths(&repo);
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    for path in removed_paths {
        index.remove_path(&path).unwrap();
    }
    index.write().unwrap();

    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = repo.signature().unwrap();
    repo.commit(Some(reference), &signature, &signature, message, &tree, &[])
        .unwrap();
}

fn collect_removed_paths(repo: &Repository) -> Vec<PathBuf> {
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);

    let statuses = repo.statuses(Some(&mut options)).unwrap();
    let mut removed_paths = Vec::new();

    for entry in statuses.iter() {
        let status = entry.status();
        if status.is_wt_deleted() || status.is_index_deleted() {
            removed_paths.push(PathBuf::from(entry.path().unwrap()));
        }
    }

    removed_paths
}

fn open_repo(path: &Path) -> Repository {
    Repository::open(path).unwrap()
}

fn head_commit(repo: &Repository) -> git2::Commit<'_> {
    let oid = repo.head().unwrap().target().unwrap();
    repo.find_commit(oid).unwrap()
}

const GIT_CLI_GUARD_EXCLUDED_DIRS: &[&str] = &[".anne", ".git", "target"];

fn repo_layout_git_cli_guard_files(repo_root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = collect_guard_files_in_subtree(repo_root, Path::new("src"), "rs")?;
    files.extend(collect_guard_files_in_subtree(
        repo_root,
        Path::new("tests"),
        "rs",
    )?);
    files.extend(collect_shell_guard_files(repo_root)?);
    files.sort();
    files.dedup();
    Ok(files)
}

fn collect_guard_files_in_subtree(
    repo_root: &Path,
    relative_dir: &Path,
    extension: &str,
) -> Result<Vec<PathBuf>, String> {
    let root = repo_root.join(relative_dir);
    if !root.exists() {
        return Ok(Vec::new());
    }
    if !root.is_dir() {
        return Err(format!(
            "guard subtree {} is not a directory",
            relative_dir.display()
        ));
    }

    let mut files = Vec::new();
    collect_guard_files(&root, relative_dir, extension, false, &mut files)?;
    Ok(files)
}

fn collect_shell_guard_files(repo_root: &Path) -> Result<Vec<PathBuf>, String> {
    if !repo_root.is_dir() {
        return Err(format!(
            "guard root {} is not a directory",
            repo_root.display()
        ));
    }

    let mut files = Vec::new();
    collect_guard_files(repo_root, Path::new(""), "sh", true, &mut files)?;
    Ok(files)
}

fn collect_guard_files(
    current_dir: &Path,
    current_relative: &Path,
    extension: &str,
    exclude_guard_dirs: bool,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(current_dir).map_err(|error| {
        format!(
            "failed reading guard directory {}: {error}",
            current_dir.display()
        )
    })?;

    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed iterating guard directory {}: {error}",
                current_dir.display()
            )
        })?;
        let file_type = entry.file_type().map_err(|error| {
            format!(
                "failed reading guard entry type {}: {error}",
                entry.path().display()
            )
        })?;
        let file_name = entry.file_name();
        let relative_path = if current_relative.as_os_str().is_empty() {
            PathBuf::from(&file_name)
        } else {
            current_relative.join(&file_name)
        };

        if file_type.is_dir() {
            let excluded = exclude_guard_dirs
                && file_name
                    .to_str()
                    .is_some_and(|name| GIT_CLI_GUARD_EXCLUDED_DIRS.contains(&name));
            if excluded {
                continue;
            }
            collect_guard_files(
                &entry.path(),
                &relative_path,
                extension,
                exclude_guard_dirs,
                files,
            )?;
            continue;
        }

        if file_type.is_file()
            && relative_path.extension().and_then(|ext| ext.to_str()) == Some(extension)
        {
            files.push(relative_path);
        }
    }

    Ok(())
}

fn check_repo_layout_git_cli_guard(repo_root: &Path) -> Result<(), String> {
    let files = repo_layout_git_cli_guard_files(repo_root)?;
    let double_quote = "\"";
    let single_quote = "'";
    let forbidden_command_patterns = [
        format!("Command::new({double_quote}git{double_quote})"),
        format!("Command::new({single_quote}git{single_quote})"),
        format!("Command::new({double_quote}/usr/bin/git{double_quote})"),
    ];

    for file in files {
        let contents = fs::read_to_string(repo_root.join(&file))
            .map_err(|error| format!("failed reading guard file {}: {error}", file.display()))?;

        match file.extension().and_then(|ext| ext.to_str()) {
            Some("rs") => {
                for pattern in &forbidden_command_patterns {
                    if contents.contains(pattern) {
                        return Err(format!(
                            "forbidden Git CLI invocation pattern `{pattern}` found in Rust guard file {}",
                            file.display()
                        ));
                    }
                }
            }
            Some("sh") => {
                for (index, line) in contents.lines().enumerate() {
                    let trimmed = line.trim_start();
                    let starts_git = trimmed.starts_with("git ") || trimmed.starts_with("git\t");
                    let execs_git =
                        trimmed.starts_with("exec git ") || trimmed.starts_with("exec git\t");
                    if starts_git || execs_git {
                        return Err(format!(
                            "forbidden shell Git CLI invocation found in shell guard file {}:{}",
                            file.display(),
                            index + 1
                        ));
                    }
                }
            }
            Some(other) => {
                return Err(format!(
                    "unexpected guard file extension `{other}` for {}",
                    file.display()
                ));
            }
            None => {
                return Err(format!(
                    "guard file {} is missing an extension",
                    file.display()
                ));
            }
        }
    }

    Ok(())
}

fn write_file(path: &Path, contents: &str) {
    write_bytes(path, contents.as_bytes());
}

fn write_bytes(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[cfg(unix)]
fn write_symlink(target: &str, path: &Path) {
    use std::os::unix::fs::symlink;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    symlink(target, path).unwrap();
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).unwrap();
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

fn bundle_path(repo: &Path, stdout: &[u8]) -> PathBuf {
    let stdout = String::from_utf8_lossy(stdout);
    let bundle = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Review bundle: "))
        .expect("missing bundle path in stdout");
    repo.join(bundle)
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
            "anne-review-test-{}-{}-{}",
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
