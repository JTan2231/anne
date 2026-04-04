use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone)]
pub struct ChangeRecord {
    pub status: String,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
}

impl ChangeRecord {
    pub fn display_path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }
}

pub fn repo_root(cwd: &Path) -> Result<PathBuf, String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("failed to run git rev-parse: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "git rev-parse --show-toplevel failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

pub fn merge_base(repo_root: &Path, base: &str, head: &str) -> Result<String, String> {
    run_git_string(repo_root, &["merge-base", base, head]).map(|value| value.trim().to_string())
}

pub fn full_diff(repo_root: &Path, merge_base: &str, head: &str) -> Result<String, String> {
    run_git_string(
        repo_root,
        &[
            "diff",
            "--find-renames",
            "--binary",
            "--unified=3",
            merge_base,
            head,
        ],
    )
}

pub fn name_status(
    repo_root: &Path,
    merge_base: &str,
    head: &str,
) -> Result<Vec<ChangeRecord>, String> {
    let output = run_git_bytes(
        repo_root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            merge_base,
            head,
        ],
    )?;
    parse_name_status(&output)
}

fn run_git_string(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git_bytes(repo_root, args)?;
    Ok(String::from_utf8_lossy(&output).to_string())
}

fn run_git_bytes(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|error| format!("failed to run git {}: {error}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(output.stdout)
}

fn parse_name_status(bytes: &[u8]) -> Result<Vec<ChangeRecord>, String> {
    let fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).to_string())
        .collect::<Vec<_>>();

    let mut index = 0;
    let mut records = Vec::new();
    while index < fields.len() {
        let status = fields[index].clone();
        index += 1;

        if status.starts_with('R') || status.starts_with('C') {
            let old_path = fields
                .get(index)
                .cloned()
                .ok_or_else(|| "malformed git --name-status output".to_string())?;
            let new_path = fields
                .get(index + 1)
                .cloned()
                .ok_or_else(|| "malformed git --name-status output".to_string())?;
            index += 2;
            records.push(ChangeRecord {
                status,
                old_path: Some(old_path),
                new_path: Some(new_path),
            });
        } else {
            let path = fields
                .get(index)
                .cloned()
                .ok_or_else(|| "malformed git --name-status output".to_string())?;
            index += 1;
            let (old_path, new_path) = if status.starts_with('D') {
                (Some(path), None)
            } else {
                (None, Some(path))
            };
            records.push(ChangeRecord {
                status,
                old_path,
                new_path,
            });
        }
    }

    Ok(records)
}
