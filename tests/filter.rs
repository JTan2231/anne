use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use git2::{Repository, RepositoryInitOptions, StatusOptions, build::CheckoutBuilder};

#[allow(dead_code)]
#[path = "../src/json.rs"]
mod fixture_json;

#[test]
fn filter_deletes_comments_and_rewrites_bundle_outputs() {
    let repo = TestRepo::new("delete-and-persist");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    let value = 2;\n    value + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf '[{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":2,\"severity\":\"warning\",\"title\":\"Temporary local\",\"body\":\"This should be deleted during filtering.\"},{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":3,\"severity\":\"error\",\"title\":\"Keep this\",\"body\":\"This should remain after filtering.\"}]\\n'\n",
    );
    write_agent_config(repo.path(), "text", &agent);

    let review_output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        review_output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&review_output.stdout),
        String::from_utf8_lossy(&review_output.stderr)
    );

    let bundle = review_bundle_path(repo.path(), &review_output.stdout);
    let filter_output = anne_with_stdin(repo.path(), &["filter"], "dn");
    assert!(
        filter_output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&filter_output.stdout),
        String::from_utf8_lossy(&filter_output.stderr)
    );

    let comments_json = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert!(!comments_json.contains("\"id\": \"R001\""));
    assert!(comments_json.contains("\"id\": \"R002\""));
    assert!(comments_json.contains("\"title\": \"Keep this\""));

    let comments_md = fs::read_to_string(bundle.join("comments.md")).unwrap();
    assert!(comments_md.contains("- Findings: 1"));
    assert!(!comments_md.contains("### R001"));
    assert!(comments_md.contains("### R002 error new:3"));

    let manifest = fs::read_to_string(bundle.join("manifest.json")).unwrap();
    let manifest = fixture_json::parse(&manifest).unwrap();
    let object = manifest.as_object().unwrap();
    let counts = object.get("counts").unwrap().as_object().unwrap();
    assert_eq!(
        counts
            .get("findings")
            .and_then(fixture_json::JsonValue::as_i64),
        Some(1)
    );

    let files = object.get("files").unwrap().as_array().unwrap();
    assert_eq!(files.len(), 1);
    let file = files[0].as_object().unwrap();
    assert_eq!(
        file.get("findings")
            .and_then(fixture_json::JsonValue::as_i64),
        Some(1)
    );

    let stdout = String::from_utf8_lossy(&filter_output.stdout);
    assert!(stdout.contains("Comments loaded: 2"));
    assert!(stdout.contains("Comments deleted: 1"));
    assert!(stdout.contains("Comments remaining: 1"));
    assert!(stdout.contains("Filter status: completed"));
}

#[test]
fn filter_quit_keeps_remaining_comments_unchanged() {
    let repo = TestRepo::new("quit-without-delete");
    init_repo(repo.path());

    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    1\n}\n",
    );
    commit_all(repo.path(), "initial");

    create_and_checkout_branch(repo.path(), "feature");
    write_file(
        &repo.path().join("src/lib.rs"),
        "pub fn value() -> i32 {\n    let value = 2;\n    value + 1\n}\n",
    );
    commit_all(repo.path(), "feature changes");

    checkout_branch(repo.path(), "main");

    let agent = agent_script(
        repo.path(),
        "while IFS= read -r _line; do :; done\nprintf '[{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":2,\"severity\":\"warning\",\"title\":\"Keep first\",\"body\":\"This should survive quitting.\"},{\"path\":\"src/lib.rs\",\"side\":\"new\",\"line\":3,\"severity\":\"warning\",\"title\":\"Keep second\",\"body\":\"This should also survive quitting.\"}]\\n'\n",
    );
    write_agent_config(repo.path(), "text", &agent);

    let review_output = anne(repo.path(), &["review", "main...feature"]);
    assert!(
        review_output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&review_output.stdout),
        String::from_utf8_lossy(&review_output.stderr)
    );

    let bundle = review_bundle_path(repo.path(), &review_output.stdout);
    let before = fs::read_to_string(bundle.join("comments.json")).unwrap();

    let filter_output = anne_with_stdin(repo.path(), &["filter"], "q");
    assert!(
        filter_output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&filter_output.stdout),
        String::from_utf8_lossy(&filter_output.stderr)
    );

    let after = fs::read_to_string(bundle.join("comments.json")).unwrap();
    assert_eq!(after, before);

    let stdout = String::from_utf8_lossy(&filter_output.stdout);
    assert!(stdout.contains("Comments deleted: 0"));
    assert!(stdout.contains("Comments remaining: 2"));
    assert!(stdout.contains("Filter status: quit"));
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

fn write_agent_config(path: &Path, output: &str, agent: &Path) {
    let mut text = String::new();
    text.push_str("[agent]\n");
    text.push_str("label = \"test-agent\"\n");
    text.push_str(&format!("command = [\"{}\"]\n", agent.display()));
    text.push_str("progress_filter = []\n");
    text.push_str(&format!("output = \"{}\"\n", output));
    text.push_str("enable_script_wrapper = false\n\n");
    text.push_str("workers = 4\n\n");
    text.push_str("[review]\n");
    text.push_str("max_patch_bytes = 4096\n");
    text.push_str("ignore_prefixes = []\n");

    write_file(&path.join(".anne/config.toml"), &text);
}

fn agent_script(path: &Path, body: &str) -> PathBuf {
    let script = path.join("agent.sh");
    write_executable(&script, &format!("#!/bin/sh\nset -eu\n{}\n", body));
    script
}

fn anne(path: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anne"));
    command.args(args).current_dir(path);
    command.output().unwrap()
}

fn anne_with_stdin(path: &Path, args: &[&str], input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anne"));
    command
        .args(args)
        .current_dir(path)
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

fn review_bundle_path(repo: &Path, stdout: &[u8]) -> PathBuf {
    let stdout = String::from_utf8_lossy(stdout);
    let bundle = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Review bundle: "))
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
            "anne-filter-test-{}-{}-{}",
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
