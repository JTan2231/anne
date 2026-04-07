use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use git2::{Repository, RepositoryInitOptions};

#[test]
fn investigate_generates_report_from_plain_text_agent() {
    let repo = TestRepo::new("plain-text-success");
    init_repo(repo.path());
    write_file(&repo.path().join("src/lib.rs"), "pub fn hello() {}\n");
    commit_all(repo.path(), "initial");

    let agent = agent_script(
        repo.path(),
        r#"prompt=$(cat)
case "$prompt" in
  *"my requests keep hanging"*) : ;;
  *)
    printf 'missing investigate prompt\n' >&2
    exit 1
    ;;
esac
cat <<'EOF'
# Investigation Report

## Findings

The request path appears to block while waiting on a timeout-free call chain.

## Evidence

- Inspected the repository and local Anne artifacts.

## Likely Causes

- Timeout propagation is incomplete in the hanging path.

## Recommended Next Steps

- Trace the blocking call chain and add an explicit timeout.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    let output = anne(
        repo.path(),
        &["investigate", "my", "requests", "keep", "hanging"],
    );
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Investigation bundle: .anne/investigations/"));
    assert!(stdout.contains("Report: report.md"));

    let bundle = investigation_bundle_path(repo.path(), &output.stdout);
    let prompt = fs::read_to_string(bundle.join("prompt.txt")).unwrap();
    assert_eq!(prompt, "my requests keep hanging");

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"succeeded\""));
    assert!(manifest.contains("\"prompt_file\": \"prompt.txt\""));
    assert!(manifest.contains("\"report_markdown\": \"report.md\""));
    assert!(manifest.contains("\"agent_prompt_file\": \"agent/prompt.md\""));
    assert!(manifest.contains("\"agent_response_file\": \"agent/response.txt\""));
    assert!(manifest.contains("\"assistant_text_source\": \"raw-stdout\""));
    assert!(manifest.contains("\"branch\": \"main\""));
    assert!(manifest.contains("\"worktree_dirty\": true"));
    assert!(manifest.contains("\"repo_root\":"));

    let rendered_prompt = fs::read_to_string(bundle.join("agent/prompt.md")).unwrap();
    assert!(rendered_prompt.contains("Reported problem (verbatim):"));
    assert!(rendered_prompt.contains("my requests keep hanging"));
    assert!(rendered_prompt.contains("Anne artifacts may already exist under `.anne/`."));
    assert!(rendered_prompt.contains(&format!("- Repo root: {}", repo.path().display())));
    assert!(rendered_prompt.contains("- Branch: main"));
    assert!(rendered_prompt.contains("- Worktree dirty: yes"));

    let report = fs::read_to_string(bundle.join("report.md")).unwrap();
    assert!(report.contains("## Prompt"));
    assert!(report.contains("> my requests keep hanging"));
    assert!(report.contains("## Findings"));
    assert!(report.ends_with('\n'));
}

#[test]
fn investigate_recovers_wrapped_json_output_without_progress_filter_on_detached_head() {
    let repo = TestRepo::new("wrapped-json-detached");
    init_repo(repo.path());
    write_file(&repo.path().join("src/lib.rs"), "pub fn hello() {}\n");
    commit_all(repo.path(), "initial");
    detach_head(repo.path());

    let agent = agent_script(
        repo.path(),
        r##"cat >/dev/null
printf '%s\n' '{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}'
cat <<'EOF'
{"type":"item.completed","item":{"type":"agent_message","text":"# Investigation Report\n\n## Prompt\n\n> detached investigation\n\n## Findings\n\nThe hang is reproducible from the detached-head worktree.\n\n## Evidence\n\n- Captured the repository state from the current checkout.\n\n## Likely Causes\n\n- The issue is independent of branch metadata.\n\n## Recommended Next Steps\n\n- Continue tracing the hanging path from the detached checkout.\n"}}
EOF
"##,
    );
    write_agent_config(repo.path(), "wrapped-json", &agent, None);

    let output = anne(repo.path(), &["investigate", "detached", "investigation"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = investigation_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"succeeded\""));
    assert!(manifest.contains("\"assistant_text_source\": \"wrapped-json-decoder\""));
    assert!(manifest.contains("\"progress_filter_ran\": false"));
    assert!(manifest.contains("\"branch\": null"));

    let rendered_prompt = fs::read_to_string(bundle.join("agent/prompt.md")).unwrap();
    assert!(rendered_prompt.contains("- Branch: detached or unavailable"));

    let response = fs::read_to_string(bundle.join("agent/response.txt")).unwrap();
    assert!(response.contains("\"agent_message\""));

    let report = fs::read_to_string(bundle.join("report.md")).unwrap();
    assert_eq!(report.matches("## Prompt").count(), 1);
    assert!(report.contains("> detached investigation"));
}

#[test]
fn investigate_persists_missing_agent_failure_after_bundle_reservation() {
    let repo = TestRepo::new("missing-agent");
    init_repo(repo.path());
    write_file(&repo.path().join("src/lib.rs"), "pub fn hello() {}\n");
    commit_all(repo.path(), "initial");
    write_missing_agent_config(repo.path());

    let output = anne(repo.path(), &["investigate", "requests", "hang"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Investigation bundle: .anne/investigations/"));
    assert!(!stdout.contains("Report: report.md"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("agent.command is not configured"));

    let bundle = investigation_bundle_path(repo.path(), &output.stdout);
    assert!(bundle.join("prompt.txt").exists());
    assert!(bundle.join("agent/prompt.md").exists());
    assert!(bundle.join("agent/response.txt").exists());
    assert!(bundle.join("report.md").exists());

    let response = fs::read_to_string(bundle.join("agent/response.txt")).unwrap();
    assert_eq!(response, "");

    let report = fs::read_to_string(bundle.join("report.md")).unwrap();
    assert!(report.contains("## Failure"));
    assert!(report.contains("agent.command is not configured"));
    assert!(report.contains("- Agent prompt: agent/prompt.md"));
    assert!(report.contains("- Raw agent response: agent/response.txt"));

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"failed\""));
    assert!(manifest.contains("\"prompt_file\": \"prompt.txt\""));
    assert!(manifest.contains("\"report_markdown\": \"report.md\""));
    assert!(manifest.contains("\"agent_prompt_file\": \"agent/prompt.md\""));
    assert!(manifest.contains("\"agent_response_file\": \"agent/response.txt\""));
    assert!(manifest.contains("agent.command is not configured"));
}

#[test]
fn investigate_persists_empty_response_failure() {
    let repo = TestRepo::new("empty-response");
    init_repo(repo.path());
    write_file(&repo.path().join("src/lib.rs"), "pub fn hello() {}\n");
    commit_all(repo.path(), "initial");

    let agent = agent_script(repo.path(), "cat >/dev/null\n: > /dev/stdout\n");
    write_agent_config(repo.path(), "text", &agent, None);

    let output = anne(repo.path(), &["investigate", "empty", "response"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = investigation_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"failed\""));

    let report = fs::read_to_string(bundle.join("report.md")).unwrap();
    assert!(report.contains("agent response was empty"));

    let response = fs::read_to_string(bundle.join("agent/response.txt")).unwrap();
    assert_eq!(response, "");
}

#[test]
fn investigate_persists_headingless_response_failure() {
    let repo = TestRepo::new("headingless-response");
    init_repo(repo.path());
    write_file(&repo.path().join("src/lib.rs"), "pub fn hello() {}\n");
    commit_all(repo.path(), "initial");

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
The requests keep hanging.

- The output is prose.
- There is no heading.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    let output = anne(repo.path(), &["investigate", "headingless", "response"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = investigation_bundle_path(repo.path(), &output.stdout);
    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"status\": \"failed\""));

    let report = fs::read_to_string(bundle.join("report.md")).unwrap();
    assert!(
        report.contains("agent response must be a markdown document with at least one heading")
    );
    assert!(report.contains("- Raw agent response: agent/response.txt"));

    let response = fs::read_to_string(bundle.join("agent/response.txt")).unwrap();
    assert!(response.contains("The requests keep hanging."));
}

#[test]
fn investigate_reserves_unique_bundle_ids_for_same_second_runs() {
    let repo = TestRepo::new("bundle-id-collision");
    init_repo(repo.path());
    write_file(&repo.path().join("src/lib.rs"), "pub fn hello() {}\n");
    commit_all(repo.path(), "initial");

    let agent = agent_script(
        repo.path(),
        r#"cat >/dev/null
cat <<'EOF'
# Investigation Report

## Findings

Collision handling should reserve a unique bundle root.

## Evidence

- Repeated runs used the same prompt and timestamp.

## Likely Causes

- Same-second ids collide without a retry suffix.

## Recommended Next Steps

- Reserve the bundle root before writing artifacts.
EOF
"#,
    );
    write_agent_config(repo.path(), "text", &agent, None);

    let bin_dir = repo.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fixed_date_script(&bin_dir, "2026-04-04T18:30:00Z");
    let path_env = prepend_path(&bin_dir);

    let first_output = anne_with_path(
        repo.path(),
        &["investigate", "same", "prompt"],
        Some(&path_env),
    );
    assert!(
        first_output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&first_output.stdout),
        String::from_utf8_lossy(&first_output.stderr)
    );

    let second_output = anne_with_path(
        repo.path(),
        &["investigate", "same", "prompt"],
        Some(&path_env),
    );
    assert!(
        second_output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&second_output.stdout),
        String::from_utf8_lossy(&second_output.stderr)
    );

    let base_id = investigation_bundle_id("2026-04-04T18:30:00Z", "same prompt");
    let first_bundle = investigation_bundle_path(repo.path(), &first_output.stdout);
    let second_bundle = investigation_bundle_path(repo.path(), &second_output.stdout);
    let second_id = format!("{base_id}-2");

    assert_eq!(
        first_bundle,
        repo.path().join(".anne/investigations").join(&base_id)
    );
    assert_eq!(
        second_bundle,
        repo.path().join(".anne/investigations").join(&second_id)
    );

    let first_manifest = fs::read_to_string(first_bundle.join("manifest.json")).unwrap();
    assert!(first_manifest.contains(&format!("\"investigation_id\": \"{base_id}\"")));

    let second_manifest = fs::read_to_string(second_bundle.join("manifest.json")).unwrap();
    assert!(second_manifest.contains(&format!("\"investigation_id\": \"{second_id}\"")));
}

#[test]
fn investigate_outside_repo_reports_no_bundle_path() {
    let repo = TestRepo::new("outside-repo");

    let output = anne(repo.path(), &["investigate", "outside", "repo"]);
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains("Investigation bundle: "));
    assert!(stderr.contains("failed discovering repository"));
    assert!(!repo.path().join(".anne/investigations").exists());
}

fn init_repo(path: &Path) {
    let mut options = RepositoryInitOptions::new();
    options.initial_head("main");
    let repo = Repository::init_opts(path, &options).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Anne Test").unwrap();
    config.set_str("user.email", "anne@example.com").unwrap();
}

fn detach_head(path: &Path) {
    let repo = open_repo(path);
    let oid = repo.head().unwrap().target().unwrap();
    repo.set_head_detached(oid).unwrap();
}

fn commit_all(path: &Path, message: &str) {
    let repo = open_repo(path);
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
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

fn open_repo(path: &Path) -> Repository {
    Repository::open(path).unwrap()
}

fn write_agent_config(path: &Path, output: &str, agent: &Path, filter: Option<&Path>) {
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
    text.push_str("workers = 1\n");

    write_file(&path.join(".anne/config.toml"), &text);
}

fn write_missing_agent_config(path: &Path) {
    write_file(
        &path.join(".anne/config.toml"),
        "[agent]\nlabel = \"custom\"\nprogress_filter = []\noutput = \"text\"\nworkers = 1\n",
    );
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

fn investigation_bundle_id(timestamp: &str, prompt: &str) -> String {
    format!(
        "{}-{}",
        timestamp.replace(':', "-"),
        slugify_for_bundle_id(prompt)
    )
}

fn slugify_for_bundle_id(text: &str) -> String {
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

fn investigation_bundle_path(repo: &Path, stdout: &[u8]) -> PathBuf {
    let stdout = String::from_utf8_lossy(stdout);
    let bundle = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Investigation bundle: "))
        .expect("missing investigation bundle path in stdout");
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
            "anne-investigate-test-{}-{}-{}",
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
