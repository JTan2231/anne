use std::{
    mem,
    path::{Path, PathBuf},
};

use git2::{Commit, Delta, DiffFindOptions, DiffFormat, DiffOptions, FileMode, Oid, Repository};

#[derive(Debug, Clone)]
pub struct ChangeRecord {
    pub status: String,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub patch_text: String,
    pub skip_reason_hint: Option<String>,
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

pub fn open_repo(repo_root: &Path) -> Result<Repository, String> {
    Repository::open(repo_root).map_err(|error| {
        format!(
            "failed opening repository at {}: {error}",
            repo_root.display()
        )
    })
}

pub fn resolve_commit_oid(repo: &Repository, spec: &str, label: &str) -> Result<Oid, String> {
    let object = repo
        .revparse_single(spec)
        .map_err(|error| format!("failed resolving {label} ref `{spec}`: {error}"))?;
    object
        .peel_to_commit()
        .map(|commit| commit.id())
        .map_err(|error| format!("ref `{spec}` for {label} does not resolve to a commit: {error}"))
}

pub fn resolve_merge_base_oid(
    repo: &Repository,
    base_oid: Oid,
    head_oid: Oid,
    base: &str,
    head: &str,
) -> Result<Oid, String> {
    repo.merge_base(base_oid, head_oid)
        .map_err(|error| format!("failed resolving merge base for `{base}` and `{head}`: {error}"))
}

pub fn build_review_range(
    repo: &Repository,
    merge_base_oid: Oid,
    head_oid: Oid,
) -> Result<ReviewRange, String> {
    let merge_base = repo
        .find_commit(merge_base_oid)
        .map_err(|error| format!("failed loading merge base `{merge_base_oid}`: {error}"))?;
    let head = repo
        .find_commit(head_oid)
        .map_err(|error| format!("failed loading head `{head_oid}`: {error}"))?;

    build_review_range_from_commits(repo, &merge_base, &head)
}

fn build_review_range_from_commits(
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

    let raw_sections = render_diff_sections(&diff)?;
    let deltas = diff.deltas().collect::<Vec<_>>();
    if raw_sections.len() != deltas.len() {
        return Err(format!(
            "failed matching rendered diff sections to logical changes: got {} sections for {} deltas",
            raw_sections.len(),
            deltas.len()
        ));
    }

    let changes = deltas
        .into_iter()
        .zip(raw_sections)
        .map(|(delta, raw_patch_text)| change_record_from_delta(repo, delta, raw_patch_text))
        .collect::<Result<Vec<_>, _>>()?;
    let full_diff = changes
        .iter()
        .map(|change| change.patch_text.as_str())
        .collect::<String>();

    Ok(ReviewRange {
        merge_base: merge_base.id().to_string(),
        full_diff,
        changes,
    })
}

fn render_diff_sections(diff: &git2::Diff<'_>) -> Result<Vec<String>, String> {
    let mut sections = Vec::new();
    let mut current = String::new();
    let mut current_key = None::<DeltaKey>;

    diff.print(DiffFormat::Patch, |delta, _hunk, line| {
        let key = DeltaKey::from_delta(delta);
        if current_key.as_ref() != Some(&key) {
            if !current.is_empty() {
                sections.push(mem::take(&mut current));
            }
            current_key = Some(key);
        }

        if matches!(line.origin(), ' ' | '+' | '-') {
            current.push(line.origin());
        }
        current.push_str(&String::from_utf8_lossy(line.content()));
        true
    })
    .map_err(|error| format!("failed rendering diff patch: {error}"))?;

    if !current.is_empty() {
        sections.push(current);
    }

    Ok(sections)
}

fn change_record_from_delta(
    repo: &Repository,
    delta: git2::DiffDelta<'_>,
    raw_patch_text: String,
) -> Result<ChangeRecord, String> {
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

    let (patch_text, skip_reason_hint) = canonicalize_patch(repo, delta, raw_patch_text)?;

    Ok(ChangeRecord {
        status,
        old_path,
        new_path,
        patch_text,
        skip_reason_hint,
    })
}

fn canonicalize_patch(
    repo: &Repository,
    delta: git2::DiffDelta<'_>,
    raw_patch_text: String,
) -> Result<(String, Option<String>), String> {
    if delta.status() != Delta::Typechange {
        return Ok((raw_patch_text, None));
    }

    let old_mode = delta.old_file().mode();
    let new_mode = delta.new_file().mode();
    if !is_regular_file_symlink_typechange(old_mode, new_mode) {
        return Ok((raw_patch_text, None));
    }

    let old_blob = load_diff_blob(repo, delta.old_file(), "old")?;
    let new_blob = load_diff_blob(repo, delta.new_file(), "new")?;
    if old_blob.is_binary() || new_blob.is_binary() {
        return Ok((
            raw_patch_text,
            Some("binary file/symlink typechange".to_string()),
        ));
    }

    let old_text = std::str::from_utf8(old_blob.content()).map_err(|_| ()).ok();
    let new_text = std::str::from_utf8(new_blob.content()).map_err(|_| ()).ok();
    let (Some(old_text), Some(new_text)) = (old_text, new_text) else {
        return Ok((
            raw_patch_text,
            Some("unsupported file/symlink typechange without UTF-8 text".to_string()),
        ));
    };

    Ok((render_typechange_patch(delta, old_text, new_text)?, None))
}

fn render_typechange_patch(
    delta: git2::DiffDelta<'_>,
    old_text: &str,
    new_text: &str,
) -> Result<String, String> {
    let old_path = required_diff_path(delta.old_file().path(), "typechange old")?;
    let new_path = required_diff_path(delta.new_file().path(), "typechange new")?;
    let old_mode = format_file_mode(delta.old_file().mode())?;
    let new_mode = format_file_mode(delta.new_file().mode())?;
    let old_id = abbreviate_oid(delta.old_file().id());
    let new_id = abbreviate_oid(delta.new_file().id());

    let mut patch = String::new();
    patch.push_str(&format!("diff --git a/{old_path} b/{new_path}\n"));
    patch.push_str(&format!("deleted file mode {old_mode}\n"));
    patch.push_str(&format!("index {old_id}..0000000\n"));
    patch.push_str(&format!("--- a/{old_path}\n"));
    patch.push_str("+++ /dev/null\n");
    append_unified_hunk(&mut patch, old_text, Side::Old);

    patch.push_str(&format!("diff --git a/{old_path} b/{new_path}\n"));
    patch.push_str(&format!("new file mode {new_mode}\n"));
    patch.push_str(&format!("index 0000000..{new_id}\n"));
    patch.push_str("--- /dev/null\n");
    patch.push_str(&format!("+++ b/{new_path}\n"));
    append_unified_hunk(&mut patch, new_text, Side::New);

    Ok(patch)
}

fn append_unified_hunk(patch: &mut String, text: &str, side: Side) {
    let lines = split_patch_lines(text);
    if lines.is_empty() {
        return;
    }

    match side {
        Side::Old => patch.push_str(&format!("@@ -1,{} +0,0 @@\n", lines.len())),
        Side::New => patch.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len())),
    }

    let prefix = match side {
        Side::Old => '-',
        Side::New => '+',
    };
    for line in &lines {
        patch.push(prefix);
        patch.push_str(line);
        patch.push('\n');
    }

    if !text.ends_with('\n') {
        patch.push_str("\\ No newline at end of file\n");
    }
}

fn split_patch_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut lines = text.split('\n').collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn load_diff_blob<'repo>(
    repo: &'repo Repository,
    file: git2::DiffFile<'_>,
    side: &str,
) -> Result<git2::Blob<'repo>, String> {
    repo.find_blob(file.id()).map_err(|error| {
        format!(
            "failed loading {side} blob for `{}`: {error}",
            file.path().map(path_to_string).unwrap_or_default()
        )
    })
}

fn required_diff_path(path: Option<&Path>, description: &str) -> Result<String, String> {
    path.map(path_to_string)
        .ok_or_else(|| format!("missing path for {description}"))
}

fn is_regular_file_symlink_typechange(old_mode: FileMode, new_mode: FileMode) -> bool {
    (is_regular_file_mode(old_mode) && new_mode == FileMode::Link)
        || (old_mode == FileMode::Link && is_regular_file_mode(new_mode))
}

fn is_regular_file_mode(mode: FileMode) -> bool {
    matches!(
        mode,
        FileMode::Blob | FileMode::BlobExecutable | FileMode::BlobGroupWritable
    )
}

fn format_file_mode(mode: FileMode) -> Result<&'static str, String> {
    match mode {
        FileMode::Blob => Ok("100644"),
        FileMode::BlobGroupWritable => Ok("100664"),
        FileMode::BlobExecutable => Ok("100755"),
        FileMode::Link => Ok("120000"),
        other => Err(format!(
            "unsupported file mode `{other:?}` for textual typechange"
        )),
    }
}

fn abbreviate_oid(oid: Oid) -> String {
    oid.to_string().chars().take(7).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeltaKey {
    old_path: Option<String>,
    new_path: Option<String>,
    old_id: Oid,
    new_id: Oid,
    old_mode: u32,
    new_mode: u32,
}

impl DeltaKey {
    fn from_delta(delta: git2::DiffDelta<'_>) -> Self {
        Self {
            old_path: delta.old_file().path().map(path_to_string),
            new_path: delta.new_file().path().map(path_to_string),
            old_id: delta.old_file().id(),
            new_id: delta.new_file().id(),
            old_mode: u32::from(delta.old_file().mode()),
            new_mode: u32::from(delta.new_file().mode()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Side {
    Old,
    New,
}

fn required_path(path: Option<String>, status: &str, side: &str) -> Result<String, String> {
    path.ok_or_else(|| format!("missing {side} path for {status} diff entry"))
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}
