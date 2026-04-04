use std::path::{Path, PathBuf};

use git2::{Commit, Delta, DiffFindOptions, DiffFormat, DiffOptions, Repository};

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

#[derive(Debug, Clone)]
pub struct ReviewRange {
    pub merge_base: String,
    pub full_diff: String,
    pub changes: Vec<ChangeRecord>,
}

pub fn repo_root(cwd: &Path) -> Result<PathBuf, String> {
    let repo = Repository::discover(cwd).map_err(|error| {
        format!(
            "failed discovering repository from {}: {error}",
            cwd.display()
        )
    })?;
    Ok(repo
        .workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo.path().to_path_buf()))
}

pub fn review_range(repo_root: &Path, base: &str, head: &str) -> Result<ReviewRange, String> {
    let repo = open_repo(repo_root)?;
    let base_commit = resolve_commit(&repo, base, "base")?;
    let head_commit = resolve_commit(&repo, head, "head")?;
    let merge_base_oid = repo
        .merge_base(base_commit.id(), head_commit.id())
        .map_err(|error| {
            format!("failed resolving merge base for `{base}` and `{head}`: {error}")
        })?;
    let merge_base_commit = repo
        .find_commit(merge_base_oid)
        .map_err(|error| format!("failed loading merge base `{merge_base_oid}`: {error}"))?;

    build_review_range(&repo, &merge_base_commit, &head_commit)
}

fn open_repo(repo_root: &Path) -> Result<Repository, String> {
    Repository::open(repo_root).map_err(|error| {
        format!(
            "failed opening repository at {}: {error}",
            repo_root.display()
        )
    })
}

fn resolve_commit<'repo>(
    repo: &'repo Repository,
    spec: &str,
    label: &str,
) -> Result<Commit<'repo>, String> {
    let object = repo
        .revparse_single(spec)
        .map_err(|error| format!("failed resolving {label} ref `{spec}`: {error}"))?;
    object
        .peel_to_commit()
        .map_err(|error| format!("ref `{spec}` for {label} does not resolve to a commit: {error}"))
}

fn build_review_range(
    repo: &Repository,
    merge_base: &Commit<'_>,
    head: &Commit<'_>,
) -> Result<ReviewRange, String> {
    let merge_base_tree = merge_base.tree().map_err(|error| {
        format!(
            "failed reading merge-base tree `{}`: {error}",
            merge_base.id()
        )
    })?;
    let head_tree = head
        .tree()
        .map_err(|error| format!("failed reading head tree `{}`: {error}", head.id()))?;

    let mut diff_options = DiffOptions::new();
    diff_options
        .context_lines(3)
        .include_typechange(true)
        .show_binary(true);

    let mut diff = repo
        .diff_tree_to_tree(
            Some(&merge_base_tree),
            Some(&head_tree),
            Some(&mut diff_options),
        )
        .map_err(|error| {
            format!(
                "failed generating diff from merge base `{}` to head `{}`: {error}",
                merge_base.id(),
                head.id()
            )
        })?;

    let mut find_options = DiffFindOptions::new();
    find_options.renames(true);
    diff.find_similar(Some(&mut find_options))
        .map_err(|error| format!("failed detecting renamed files: {error}"))?;

    Ok(ReviewRange {
        merge_base: merge_base.id().to_string(),
        full_diff: render_diff(&diff)?,
        changes: diff
            .deltas()
            .map(change_record_from_delta)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn render_diff(diff: &git2::Diff<'_>) -> Result<String, String> {
    let mut bytes = Vec::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        match line.origin() {
            ' ' | '+' | '-' => bytes.push(line.origin() as u8),
            _ => {}
        }
        bytes.extend_from_slice(line.content());
        true
    })
    .map_err(|error| format!("failed rendering diff patch: {error}"))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn change_record_from_delta(delta: git2::DiffDelta<'_>) -> Result<ChangeRecord, String> {
    let old_path = delta.old_file().path().map(path_to_string);
    let new_path = delta.new_file().path().map(path_to_string);

    let (status, old_path, new_path) = match delta.status() {
        Delta::Added => (
            "A".to_string(),
            None,
            Some(required_path(new_path, "added", "new")?),
        ),
        Delta::Deleted => (
            "D".to_string(),
            Some(required_path(old_path, "deleted", "old")?),
            None,
        ),
        Delta::Modified => (
            "M".to_string(),
            None,
            Some(required_path(new_path.or(old_path), "modified", "path")?),
        ),
        Delta::Renamed => (
            "R".to_string(),
            Some(required_path(old_path, "renamed", "old")?),
            Some(required_path(new_path, "renamed", "new")?),
        ),
        Delta::Copied => (
            "C".to_string(),
            Some(required_path(old_path, "copied", "old")?),
            Some(required_path(new_path, "copied", "new")?),
        ),
        Delta::Typechange => (
            "T".to_string(),
            old_path,
            Some(required_path(new_path, "typechanged", "new")?),
        ),
        Delta::Unmodified => {
            return Err("git2 diff unexpectedly included an unmodified entry".to_string());
        }
        other => return Err(format!("unsupported diff delta status `{other:?}`")),
    };

    Ok(ChangeRecord {
        status,
        old_path,
        new_path,
    })
}

fn required_path(path: Option<String>, status: &str, side: &str) -> Result<String, String> {
    path.ok_or_else(|| format!("missing {side} path for {status} diff entry"))
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}
