use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn review_uses_merge_base_semantics_and_records_skips() {
    let repo = TestRepo::new("merge-base");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    git(repo.path(), &["checkout", "-b", "feature"]);
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    2\n}\n",
    );
    write_file(&repo.path().join("vendor/generated.txt"), "skip me\n");
    commit_all(repo.path(), "feature changes");

    git(repo.path(), &["checkout", "main"]);
    write_file(
        &repo.path().join("src/main_only.rs"),
        "pub const MAIN: i32 = 42;\n",
    );
    commit_all(repo.path(), "main changes");

    write_agent_config(
        repo.path(),
        "text",
        &agent_script(repo.path(), "cat >/dev/null\nprintf '[]\\n'\n"),
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

    git(repo.path(), &["checkout", "-b", "feature"]);
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    git(repo.path(), &["checkout", "main"]);

    let agent = agent_script(
        repo.path(),
        "cat >/dev/null\nprintf '[{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":2,\"severity\":\"warning\",\"title\":\"Off-by-one result\",\"body\":\"The function now adds an unexpected extra 1.\"}]\\n'\n",
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
    assert!(comments_json.contains("\"patch_file\": \"files/0001-src-lib.rs.patch\""));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("### R001 warning new:2"));
    assert!(comments_md.contains("Off-by-one result"));
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

    git(repo.path(), &["checkout", "-b", "feature"]);
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    git(repo.path(), &["checkout", "main"]);

    let agent = agent_script(
        repo.path(),
        "cat >/dev/null\nprintf 'thinking\\n'\nprintf 'FINAL:[{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":99,\"severity\":\"warning\",\"title\":\"Bad anchor\",\"body\":\"This should fail anchor validation.\"}]\\n'\n",
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

fn init_repo(path: &Path) {
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.name", "Anne Test"]);
    git(path, &["config", "user.email", "anne@example.com"]);
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
    text.push_str("enable_script_wrapper = false\n\n");
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

fn anne(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anne"))
        .args(args)
        .current_dir(path)
        .output()
        .unwrap()
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_all(path: &Path, message: &str) {
    git(path, &["add", "."]);
    git(path, &["commit", "-m", message]);
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
