use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
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
    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert_eq!(comments_json.trim(), "[]");

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"label\": \"default\""));
    assert!(manifest.contains("default/agent.sh"));
    assert!(manifest.contains("default/filter.sh"));

    let response = fs::read_to_string(bundle.join("agent/0001-src-lib.rs.response.txt")).unwrap();
    assert!(response.contains("\"agent_message\""));
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

#[test]
fn repository_code_and_scripts_do_not_invoke_git_cli() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_files(&repo_root.join("src"), "rs", &mut files);
    collect_files(&repo_root.join("tests"), "rs", &mut files);
    files.push(repo_root.join("ci.sh"));

    let double_quote = "\"";
    let single_quote = "'";
    let forbidden_command_patterns = [
        format!("Command::new({double_quote}git{double_quote})"),
        format!("Command::new({single_quote}git{single_quote})"),
        format!("Command::new({double_quote}/usr/bin/git{double_quote})"),
    ];

    for file in files {
        let contents = fs::read_to_string(&file).unwrap();

        for pattern in &forbidden_command_patterns {
            assert!(
                !contents.contains(pattern),
                "forbidden Git CLI invocation pattern `{pattern}` found in {}",
                file.display()
            );
        }

        if file.extension().and_then(|ext| ext.to_str()) == Some("sh") {
            for (index, line) in contents.lines().enumerate() {
                let trimmed = line.trim_start();
                let starts_git = trimmed.starts_with("git ") || trimmed.starts_with("git\t");
                let execs_git =
                    trimmed.starts_with("exec git ") || trimmed.starts_with("exec git\t");
                assert!(
                    !starts_git && !execs_git,
                    "forbidden shell Git CLI invocation in {}:{}",
                    file.display(),
                    index + 1
                );
            }
        }
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

fn codex_script(path: &Path) -> PathBuf {
    let script = path.join("codex");
    write_executable(
        &script,
        "#!/usr/bin/env bash\nset -euo pipefail\n[ \"$1\" = \"exec\" ]\n[ \"$2\" = \"--json\" ]\n[ \"$3\" = \"-\" ]\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"[]\"}}'\n",
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
    'try .item.type // ""': get_path("item", "type"),
    'try .item.text // ""': get_path("item", "text"),
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

fn prepend_path(dir: &Path) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    if current.is_empty() {
        dir.display().to_string()
    } else {
        format!("{}:{}", dir.display(), current)
    }
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

fn collect_files(root: &Path, extension: &str, files: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }

    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            collect_files(&path, extension, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
            files.push(path);
        }
    }
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
