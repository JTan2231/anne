use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use crate::{
    agent,
    cli::ReviewRequest,
    config::{AgentConfig, AppConfig, ReviewConfig},
    git::{self, ChangeRecord},
    json::JsonValue,
    util::{current_timestamp, slugify_text, string_array},
};

pub struct RunResult {
    pub bundle_path: Option<String>,
    pub files_reviewed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub findings: usize,
    pub success: bool,
    pub error: Option<String>,
}

pub fn run(request: ReviewRequest) -> Result<RunResult, String> {
    let prepared = prepare_run(&request)?;
    Ok(run_materialized(request, prepared))
}

fn prepare_run(request: &ReviewRequest) -> Result<PreparedRun, String> {
    let cwd = env::current_dir().map_err(|error| format!("failed to read current dir: {error}"))?;
    let repo_root = git::repo_root(&cwd)?;
    let config = AppConfig::load(&repo_root)?;
    let review_range = git::review_range(&repo_root, &request.base, &request.head)?;
    let diff_sections = split_diff_sections(&review_range.full_diff);
    if diff_sections.len() != review_range.changes.len() {
        return Err(format!(
            "diff generation returned {} file sections but change enumeration returned {} entries",
            diff_sections.len(),
            review_range.changes.len()
        ));
    }

    let generated_at = current_timestamp();
    let short_merge_base = review_range.merge_base.chars().take(7).collect::<String>();
    let review_id = build_review_id(
        &generated_at,
        &request.base,
        &request.head,
        &short_merge_base,
    );
    let relative_bundle_path = PathBuf::from(".anne").join("reviews").join(&review_id);
    let bundle_root = repo_root.join(&relative_bundle_path);
    Ok(PreparedRun {
        repo_root,
        config,
        review_range,
        diff_sections,
        generated_at,
        review_id,
        relative_bundle_path,
        bundle_root,
    })
}

fn run_materialized(request: ReviewRequest, prepared: PreparedRun) -> RunResult {
    let PreparedRun {
        repo_root,
        config,
        review_range,
        diff_sections,
        generated_at,
        review_id,
        relative_bundle_path,
        bundle_root,
    } = prepared;
    let bundle_path = relative_bundle_path.display().to_string();

    if let Err(error) = fs::create_dir_all(bundle_root.join("files"))
        .map_err(|error| format!("failed creating bundle files dir: {error}"))
    {
        return post_materialization_failure(&bundle_path, error);
    }
    if let Err(error) = fs::create_dir_all(bundle_root.join("agent"))
        .map_err(|error| format!("failed creating bundle agent dir: {error}"))
    {
        return post_materialization_failure(&bundle_path, error);
    }

    let mut files = match build_files(&review_range.changes, &diff_sections, &config) {
        Ok(files) => files,
        Err(error) => return post_materialization_failure(&bundle_path, error),
    };
    assign_patch_paths(&mut files);

    let mut manifest = build_manifest(
        &request,
        &review_range,
        &config,
        review_id,
        generated_at,
        bundle_path,
        &files,
    );

    if let Err(error) = write_manifest(&bundle_root, &manifest) {
        return failure_result_from_manifest(&mut manifest, error);
    }

    if let Err(error) = fs::write(bundle_root.join("diff.patch"), &review_range.full_diff)
        .map_err(|error| format!("failed writing diff.patch: {error}"))
    {
        return persist_failed_manifest(&bundle_root, &mut manifest, error);
    }

    for file in files
        .iter()
        .filter(|file| file.status == FileStatus::Queued)
    {
        let patch_file = match file.patch_file.as_ref() {
            Some(path) => path,
            None => {
                return persist_failed_manifest(
                    &bundle_root,
                    &mut manifest,
                    format!(
                        "reviewable file {} is missing a patch_file path",
                        file.display_path
                    ),
                );
            }
        };
        if let Err(error) = fs::write(bundle_root.join(patch_file), &file.patch_text)
            .map_err(|error| format!("failed writing {patch_file}: {error}"))
        {
            return persist_failed_manifest(&bundle_root, &mut manifest, error);
        }
    }

    if files.iter().any(|file| file.status == FileStatus::Queued) && config.agent.command.is_empty()
    {
        let error =
            "agent.command is not configured; set it in .anne/config.toml before running review"
                .to_string();
        for file in files
            .iter_mut()
            .filter(|file| file.status == FileStatus::Queued)
        {
            file.status = FileStatus::Failed;
            file.error = Some(error.clone());
        }
        manifest.errors.push(error.clone());
        manifest.status = ManifestStatus::Failed;
        sync_manifest_files(&mut manifest, &files);
        update_counts(&mut manifest);
        if let Err(write_error) = write_outputs(&bundle_root, &manifest, &[]) {
            return persist_failed_manifest(&bundle_root, &mut manifest, write_error);
        }
        return to_run_result(&manifest, Some(error));
    }

    let files = RefCell::new(files);
    let manifest = RefCell::new(manifest);
    let findings = RefCell::new(Vec::new());

    let queued_jobs = files
        .borrow()
        .iter()
        .enumerate()
        .filter(|(_, file)| file.status == FileStatus::Queued)
        .map(|(file_index, file)| ReviewJob {
            file_index,
            file: file.clone(),
        })
        .collect::<Vec<_>>();

    let bounded_result = agent::run_bounded(
        config.agent.workers,
        queued_jobs,
        |_, job| {
            let mut files = files.borrow_mut();
            let mut manifest = manifest.borrow_mut();
            let file = &mut files[job.file_index];
            let (prompt_file, response_file) = agent_artifact_paths(file);
            file.status = FileStatus::Running;
            file.agent_prompt_file = Some(prompt_file);
            file.agent_response_file = Some(response_file);
            sync_manifest_files(&mut manifest, &files);
            manifest.status = ManifestStatus::Running;
            update_counts(&mut manifest);
            write_manifest(&bundle_root, &manifest)
        },
        |_, job| {
            review_file(
                &repo_root,
                &bundle_root,
                &config.agent,
                &request.base,
                &request.head,
                &review_range.merge_base,
                job,
            )
        },
        |_, result| {
            let mut files = files.borrow_mut();
            let mut manifest = manifest.borrow_mut();
            let mut findings = findings.borrow_mut();
            let file = &mut files[result.file_index];
            file.agent_stderr = result.agent_stderr;
            file.findings = result.findings.len();
            file.error = result.error.clone();

            if let Some(error) = result.error {
                file.status = FileStatus::Failed;
                manifest
                    .errors
                    .push(format!("{}: {error}", file.display_path));
            } else {
                file.status = FileStatus::Reviewed;
                findings.extend(result.findings);
            }

            sync_manifest_files(&mut manifest, &files);
            manifest.status = ManifestStatus::Running;
            update_counts(&mut manifest);
            write_manifest(&bundle_root, &manifest)
        },
    );

    let mut files = files.into_inner();
    let mut manifest = manifest.into_inner();
    let mut findings = findings.into_inner();

    if let Err(error) = bounded_result {
        sync_manifest_files(&mut manifest, &files);
        return failure_result_from_manifest(&mut manifest, error);
    }

    findings.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.line.cmp(&right.line))
            .then(left.title.cmp(&right.title))
    });
    for (index, finding) in findings.iter_mut().enumerate() {
        finding.id = format!("R{:03}", index + 1);
    }
    reassign_file_counts(&mut files, &findings);

    sync_manifest_files(&mut manifest, &files);
    manifest.status = if manifest.errors.is_empty() {
        ManifestStatus::Succeeded
    } else {
        ManifestStatus::Failed
    };
    update_counts(&mut manifest);
    if let Err(error) = write_outputs(&bundle_root, &manifest, &findings) {
        return persist_failed_manifest(&bundle_root, &mut manifest, error);
    }

    to_run_result(&manifest, manifest.errors.first().cloned())
}

struct PreparedRun {
    repo_root: PathBuf,
    config: AppConfig,
    review_range: git::ReviewRange,
    diff_sections: Vec<String>,
    generated_at: String,
    review_id: String,
    relative_bundle_path: PathBuf,
    bundle_root: PathBuf,
}

fn build_manifest(
    request: &ReviewRequest,
    review_range: &git::ReviewRange,
    config: &AppConfig,
    review_id: String,
    generated_at: String,
    bundle_path: String,
    files: &[ReviewFile],
) -> Manifest {
    let mut manifest = Manifest {
        review_id,
        generated_at,
        range: format!("{}...{}", request.base, request.head),
        base: request.base.clone(),
        head: request.head.clone(),
        merge_base: review_range.merge_base.clone(),
        status: ManifestStatus::Running,
        bundle_path,
        diff_patch: "diff.patch".to_string(),
        comments_markdown: "comments.md".to_string(),
        comments_json: "comments.json".to_string(),
        runtime: RuntimeManifest {
            agent: config.agent.clone(),
            review: config.review.clone(),
        },
        files: files.iter().map(FileRecord::from_review_file).collect(),
        counts: Counts::default(),
        errors: Vec::new(),
    };
    update_counts(&mut manifest);
    manifest
}

fn post_materialization_failure(bundle_path: &str, error: String) -> RunResult {
    RunResult {
        bundle_path: Some(bundle_path.to_string()),
        files_reviewed: 0,
        files_skipped: 0,
        files_failed: 0,
        findings: 0,
        success: false,
        error: Some(error),
    }
}

fn failure_result_from_manifest(manifest: &mut Manifest, error: String) -> RunResult {
    mark_manifest_failed(manifest, &error);
    to_run_result(manifest, Some(error))
}

fn persist_failed_manifest(
    bundle_root: &Path,
    manifest: &mut Manifest,
    error: String,
) -> RunResult {
    mark_manifest_failed(manifest, &error);
    let error = match write_manifest(bundle_root, manifest) {
        Ok(()) => error,
        Err(write_error) if write_error == error => error,
        Err(write_error) => format!("{error}; {write_error}"),
    };
    to_run_result(manifest, Some(error))
}

fn mark_manifest_failed(manifest: &mut Manifest, error: &str) {
    manifest.status = ManifestStatus::Failed;
    if manifest.errors.iter().all(|existing| existing != error) {
        manifest.errors.push(error.to_string());
    }
    update_counts(manifest);
}

fn review_file(
    repo_root: &Path,
    bundle_root: &Path,
    agent_config: &AgentConfig,
    base: &str,
    head: &str,
    merge_base: &str,
    job: ReviewJob,
) -> ReviewJobResult {
    let (prompt_file, response_file) = agent_artifact_paths(&job.file);
    let prompt = build_prompt(base, head, merge_base, &job.file);

    if let Err(error) = fs::write(bundle_root.join(&prompt_file), &prompt) {
        return ReviewJobResult::failure(
            job.file_index,
            Vec::new(),
            format!("failed writing {prompt_file}: {error}"),
        );
    }

    match agent::run_captured(repo_root, agent_config, &prompt) {
        Ok(result) => {
            if let Err(error) = fs::write(bundle_root.join(&response_file), &result.raw_stdout) {
                return ReviewJobResult::failure(
                    job.file_index,
                    result.stderr_lines,
                    format!("failed writing {response_file}: {error}"),
                );
            }

            match parse_agent_findings(&result.assistant_text)
                .and_then(|raw_findings| validate_findings(&job.file, raw_findings))
            {
                Ok(findings) => ReviewJobResult {
                    file_index: job.file_index,
                    agent_stderr: result.stderr_lines,
                    findings,
                    error: None,
                },
                Err(error) => ReviewJobResult::failure(job.file_index, result.stderr_lines, error),
            }
        }
        Err(error) => {
            let message = match fs::write(bundle_root.join(&response_file), &error.raw_stdout) {
                Ok(()) => error.message,
                Err(write_error) => format!(
                    "{}; failed writing {response_file}: {write_error}",
                    error.message
                ),
            };

            ReviewJobResult::failure(job.file_index, error.stderr_lines, message)
        }
    }
}

fn parse_agent_findings(text: &str) -> Result<Vec<RawFinding>, String> {
    let value = crate::json::parse(text)?;
    let array = value
        .as_array()
        .ok_or_else(|| "agent response must be a JSON array".to_string())?;
    let mut findings = Vec::new();
    for item in array {
        let object = item
            .as_object()
            .ok_or_else(|| "each finding must be a JSON object".to_string())?;
        findings.push(RawFinding {
            path: required_string(object, "path")?,
            side: Side::parse(&required_string(object, "side")?)?,
            line: required_usize(object, "line")?,
            severity: Severity::parse(&required_string(object, "severity")?)?,
            title: required_string(object, "title")?,
            body: required_string(object, "body")?,
            hunk_header: optional_string(object, "hunk_header")?,
            confidence: optional_scalar_string(object, "confidence")?,
        });
    }
    Ok(findings)
}

fn validate_findings(
    file: &ReviewFile,
    raw_findings: Vec<RawFinding>,
) -> Result<Vec<Finding>, String> {
    let mut findings = Vec::new();
    for raw in raw_findings {
        if raw.path.trim() != file.display_path {
            return Err(format!(
                "finding path `{}` does not match reviewed path `{}`",
                raw.path, file.display_path
            ));
        }
        if raw.title.trim().is_empty() {
            return Err(format!(
                "finding for {} has an empty title",
                file.display_path
            ));
        }
        if raw.body.trim().is_empty() {
            return Err(format!(
                "finding for {} has an empty body",
                file.display_path
            ));
        }

        let actual_hunk = file
            .anchors
            .headers
            .get(&(raw.side, raw.line))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "finding for {} points at unchanged {} line {}",
                    file.display_path,
                    raw.side.as_str(),
                    raw.line
                )
            })?;
        if let Some(hunk_header) = raw.hunk_header.as_ref() {
            if hunk_header != &actual_hunk {
                return Err(format!(
                    "finding for {} uses hunk `{}` but the anchor belongs to `{}`",
                    file.display_path, hunk_header, actual_hunk
                ));
            }
        }

        findings.push(Finding {
            id: String::new(),
            path: raw.path,
            side: raw.side,
            line: raw.line,
            severity: raw.severity,
            title: raw.title.trim().to_string(),
            body: raw.body.trim().to_string(),
            hunk_header: actual_hunk,
            patch_file: file.patch_file.clone().unwrap_or_default(),
            confidence: raw.confidence,
        });
    }
    Ok(findings)
}

fn build_prompt(base: &str, head: &str, merge_base: &str, file: &ReviewFile) -> String {
    format!(
        "\
You are reviewing a single git patch for Anne.

Return only a JSON array. Use this schema for each finding:
[
  {{
    \"path\": \"{path}\",
    \"side\": \"new\",
    \"line\": 1,
    \"severity\": \"warning\",
    \"title\": \"Short finding title\",
    \"body\": \"One concise explanation.\",
    \"hunk_header\": \"@@ -1,1 +1,2 @@\"
  }}
]

Rules:
- Comment only on concrete correctness, maintainability, or regression issues in changed lines.
- Do not comment on style unless it affects correctness, maintainability, or regression risk.
- Every finding must anchor to an actual changed line from the patch below.
- Use `side = \"new\"` for additions or modified new lines and `side = \"old\"` for deletions.
- Return [] when there are no findings.
- Do not include markdown fences or explanation outside the JSON array.

Base: {base}
Head: {head}
Merge base: {merge_base}
Path: {path}

Patch:
{patch}",
        path = file.display_path,
        patch = file.patch_text
    )
}

fn build_files(
    changes: &[ChangeRecord],
    diff_sections: &[String],
    config: &AppConfig,
) -> Result<Vec<ReviewFile>, String> {
    let mut files = changes
        .iter()
        .zip(diff_sections)
        .map(|(change, patch)| build_file(change, patch, config))
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_by(|left, right| left.display_path.cmp(&right.display_path));
    Ok(files)
}

fn build_file(
    change: &ChangeRecord,
    patch_text: &str,
    config: &AppConfig,
) -> Result<ReviewFile, String> {
    let display_path = change.display_path().to_string();
    let anchors = parse_anchors(patch_text)?;
    let has_hunks = !anchors.headers.is_empty();
    let is_binary = patch_text.contains("GIT binary patch") || patch_text.contains("Binary files ");
    let rename_only = change.status.starts_with('R') && !has_hunks && !is_binary;

    let (status, skip_reason) = if config.review.ignores(&display_path) {
        (
            FileStatus::Skipped,
            Some(format!(
                "ignored by review.ignore_prefixes ({display_path})"
            )),
        )
    } else if is_binary {
        (FileStatus::Skipped, Some("binary diff".to_string()))
    } else if rename_only {
        (
            FileStatus::Skipped,
            Some("pure rename without textual changes".to_string()),
        )
    } else if !has_hunks {
        (
            FileStatus::Skipped,
            Some("metadata-only change without textual diff".to_string()),
        )
    } else if patch_text.as_bytes().len() > config.review.max_patch_bytes {
        (
            FileStatus::Skipped,
            Some(format!(
                "patch exceeds review.max_patch_bytes ({})",
                config.review.max_patch_bytes
            )),
        )
    } else {
        (FileStatus::Queued, None)
    };

    Ok(ReviewFile {
        display_path,
        old_path: change.old_path.clone(),
        new_path: change.new_path.clone(),
        patch_text: patch_text.to_string(),
        status,
        skip_reason,
        patch_file: None,
        agent_prompt_file: None,
        agent_response_file: None,
        anchors,
        findings: 0,
        error: None,
        agent_stderr: Vec::new(),
    })
}

fn assign_patch_paths(files: &mut [ReviewFile]) {
    let mut index = 1;
    for file in files
        .iter_mut()
        .filter(|file| file.status == FileStatus::Queued)
    {
        let stem = format!("{:04}-{}", index, slugify_text(&file.display_path));
        file.patch_file = Some(format!("files/{stem}.patch"));
        index += 1;
    }
}

fn agent_artifact_paths(file: &ReviewFile) -> (String, String) {
    let patch_file = file
        .patch_file
        .as_deref()
        .unwrap_or("files/0000-unknown.patch");
    let stem = patch_file
        .strip_prefix("files/")
        .unwrap_or(patch_file)
        .strip_suffix(".patch")
        .unwrap_or(patch_file);
    (
        format!("agent/{stem}.prompt.md"),
        format!("agent/{stem}.response.txt"),
    )
}

fn split_diff_sections(text: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = Vec::new();

    for line in text.lines() {
        if line.starts_with("diff --git ") {
            if !current.is_empty() {
                sections.push(current.join("\n") + "\n");
                current.clear();
            }
        }
        if line.starts_with("diff --git ") || !current.is_empty() {
            current.push(line.to_string());
        }
    }

    if !current.is_empty() {
        sections.push(current.join("\n") + "\n");
    }

    sections
}

fn build_review_id(timestamp: &str, base: &str, head: &str, short_merge_base: &str) -> String {
    format!(
        "{}-{}...{}-{}",
        timestamp.replace(':', "-"),
        slugify_text(base),
        slugify_text(head),
        short_merge_base
    )
}

fn parse_anchors(patch: &str) -> Result<AnchorMap, String> {
    let mut headers = BTreeMap::new();
    let mut changed_new = BTreeSet::new();
    let mut changed_old = BTreeSet::new();
    let mut current_hunk = None;
    let mut old_line = 0usize;
    let mut new_line = 0usize;

    for line in patch.lines() {
        if let Some((header, old_start, new_start)) = parse_hunk_header(line)? {
            current_hunk = Some(header);
            old_line = old_start;
            new_line = new_start;
            continue;
        }

        let Some(hunk) = current_hunk.clone() else {
            continue;
        };

        if line.starts_with('+') && !line.starts_with("+++") {
            changed_new.insert(new_line);
            headers.insert((Side::New, new_line), hunk.clone());
            new_line += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            changed_old.insert(old_line);
            headers.insert((Side::Old, old_line), hunk.clone());
            old_line += 1;
        } else if line.starts_with(' ') {
            old_line += 1;
            new_line += 1;
        } else if line.starts_with('\\') {
        }
    }

    Ok(AnchorMap {
        headers,
        changed_new,
        changed_old,
    })
}

fn parse_hunk_header(line: &str) -> Result<Option<(String, usize, usize)>, String> {
    if !line.starts_with("@@ ") {
        return Ok(None);
    }
    let closing = line
        .rfind(" @@")
        .ok_or_else(|| format!("malformed hunk header `{line}`"))?;
    let descriptor = &line[3..closing];
    let mut parts = descriptor.split_whitespace();
    let old = parts
        .next()
        .ok_or_else(|| format!("malformed hunk header `{line}`"))?;
    let new = parts
        .next()
        .ok_or_else(|| format!("malformed hunk header `{line}`"))?;
    let old_start = parse_range_start(old.trim_start_matches('-'))?;
    let new_start = parse_range_start(new.trim_start_matches('+'))?;
    Ok(Some((line.to_string(), old_start, new_start)))
}

fn parse_range_start(value: &str) -> Result<usize, String> {
    let start = value.split(',').next().unwrap_or(value);
    start
        .parse::<usize>()
        .map_err(|error| format!("invalid hunk range `{value}`: {error}"))
}

fn required_string(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| format!("finding field `{key}` must be a string"))
}

fn required_usize(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<usize, String> {
    let value = object
        .get(key)
        .and_then(JsonValue::as_i64)
        .ok_or_else(|| format!("finding field `{key}` must be an integer"))?;
    if value < 0 {
        return Err(format!("finding field `{key}` must be positive"));
    }
    Ok(value as usize)
}

fn optional_string(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| format!("finding field `{key}` must be a string when present")),
    }
}

fn optional_scalar_string(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) => Ok(Some(value.clone())),
        Some(JsonValue::Number(value)) => Ok(Some(value.to_string())),
        Some(JsonValue::Bool(value)) => Ok(Some(value.to_string())),
        Some(_) => Err(format!(
            "finding field `{key}` must be a string, number, or boolean when present"
        )),
    }
}

fn write_manifest(bundle_root: &Path, manifest: &Manifest) -> Result<(), String> {
    fs::write(
        bundle_root.join("manifest.json"),
        manifest.to_json().render_pretty(),
    )
    .map_err(|error| format!("failed writing manifest.json: {error}"))
}

fn write_outputs(
    bundle_root: &Path,
    manifest: &Manifest,
    findings: &[Finding],
) -> Result<(), String> {
    write_manifest(bundle_root, manifest)?;
    fs::write(
        bundle_root.join("comments.json"),
        JsonValue::Array(findings.iter().map(Finding::to_json).collect()).render_pretty(),
    )
    .map_err(|error| format!("failed writing comments.json: {error}"))?;
    fs::write(
        bundle_root.join("comments.md"),
        render_markdown(manifest, findings),
    )
    .map_err(|error| format!("failed writing comments.md: {error}"))?;
    Ok(())
}

fn render_markdown(manifest: &Manifest, findings: &[Finding]) -> String {
    let mut text = String::new();
    text.push_str(&format!("# Review: {}\n\n", manifest.range));
    text.push_str(&format!("- Generated: {}\n", manifest.generated_at));
    text.push_str(&format!("- Merge base: {}\n", manifest.merge_base));
    text.push_str(&format!(
        "- Files reviewed: {}\n",
        manifest.counts.files_reviewed
    ));
    text.push_str(&format!(
        "- Files skipped: {}\n",
        manifest.counts.files_skipped
    ));
    if manifest.counts.files_failed > 0 {
        text.push_str(&format!(
            "- Files failed: {}\n",
            manifest.counts.files_failed
        ));
    }
    text.push_str(&format!("- Findings: {}\n", manifest.counts.findings));
    text.push_str(&format!("- Patch: {}\n\n", manifest.diff_patch));

    let mut findings_by_path: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for finding in findings {
        findings_by_path
            .entry(finding.path.as_str())
            .or_default()
            .push(finding);
    }

    for file in &manifest.files {
        if file.status == FileStatus::Skipped {
            continue;
        }

        text.push_str(&format!("## {}\n\n", file.path));
        match file.status {
            FileStatus::Reviewed => {
                let file_findings = findings_by_path
                    .get(file.path.as_str())
                    .cloned()
                    .unwrap_or_default();
                if file_findings.is_empty() {
                    text.push_str("No findings.\n\n");
                } else {
                    for finding in file_findings {
                        text.push_str(&format!(
                            "### {} {} {}:{}\n{}\n\n{}\n\nHunk: {}\n\n",
                            finding.id,
                            finding.severity.as_str(),
                            finding.side.as_str(),
                            finding.line,
                            finding.title,
                            finding.body,
                            finding.hunk_header
                        ));
                    }
                }
            }
            FileStatus::Failed => {
                text.push_str("Review failed.\n\n");
                if let Some(error) = &file.error {
                    text.push_str(error);
                    text.push_str("\n\n");
                }
            }
            FileStatus::Running => {
                text.push_str("Review running.\n\n");
            }
            FileStatus::Queued | FileStatus::Skipped => {}
        }
    }

    let skipped = manifest
        .files
        .iter()
        .filter(|file| file.status == FileStatus::Skipped)
        .collect::<Vec<_>>();
    if !skipped.is_empty() {
        text.push_str("## Skipped Files\n\n");
        for file in skipped {
            text.push_str(&format!(
                "- {}: {}\n",
                file.path,
                file.skip_reason.as_deref().unwrap_or("skipped")
            ));
        }
    }

    text
}

fn sync_manifest_files(manifest: &mut Manifest, files: &[ReviewFile]) {
    manifest.files = files.iter().map(FileRecord::from_review_file).collect();
}

fn update_counts(manifest: &mut Manifest) {
    manifest.counts = Counts {
        files_reviewed: manifest
            .files
            .iter()
            .filter(|file| file.status == FileStatus::Reviewed || file.status == FileStatus::Failed)
            .count(),
        files_skipped: manifest
            .files
            .iter()
            .filter(|file| file.status == FileStatus::Skipped)
            .count(),
        files_failed: manifest
            .files
            .iter()
            .filter(|file| file.status == FileStatus::Failed)
            .count(),
        findings: manifest.files.iter().map(|file| file.findings).sum(),
    };
}

fn reassign_file_counts(files: &mut [ReviewFile], findings: &[Finding]) {
    let mut counts = BTreeMap::<&str, usize>::new();
    for finding in findings {
        *counts.entry(finding.path.as_str()).or_default() += 1;
    }
    for file in files {
        if file.status == FileStatus::Reviewed {
            file.findings = *counts.get(file.display_path.as_str()).unwrap_or(&0);
        }
    }
}

fn to_run_result(manifest: &Manifest, error: Option<String>) -> RunResult {
    RunResult {
        bundle_path: Some(manifest.bundle_path.clone()),
        files_reviewed: manifest.counts.files_reviewed,
        files_skipped: manifest.counts.files_skipped,
        files_failed: manifest.counts.files_failed,
        findings: manifest.counts.findings,
        success: manifest.status == ManifestStatus::Succeeded,
        error,
    }
}

#[derive(Debug, Clone)]
struct ReviewJob {
    file_index: usize,
    file: ReviewFile,
}

#[derive(Debug, Clone)]
struct ReviewJobResult {
    file_index: usize,
    agent_stderr: Vec<String>,
    findings: Vec<Finding>,
    error: Option<String>,
}

impl ReviewJobResult {
    fn failure(file_index: usize, agent_stderr: Vec<String>, error: String) -> Self {
        Self {
            file_index,
            agent_stderr,
            findings: Vec::new(),
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone)]
struct ReviewFile {
    display_path: String,
    old_path: Option<String>,
    new_path: Option<String>,
    patch_text: String,
    status: FileStatus,
    skip_reason: Option<String>,
    patch_file: Option<String>,
    agent_prompt_file: Option<String>,
    agent_response_file: Option<String>,
    anchors: AnchorMap,
    findings: usize,
    error: Option<String>,
    agent_stderr: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct AnchorMap {
    headers: BTreeMap<(Side, usize), String>,
    #[cfg_attr(not(test), allow(dead_code))]
    changed_new: BTreeSet<usize>,
    #[cfg_attr(not(test), allow(dead_code))]
    changed_old: BTreeSet<usize>,
}

#[derive(Debug, Clone)]
struct RawFinding {
    path: String,
    side: Side,
    line: usize,
    severity: Severity,
    title: String,
    body: String,
    hunk_header: Option<String>,
    confidence: Option<String>,
}

#[derive(Debug, Clone)]
struct Finding {
    id: String,
    path: String,
    side: Side,
    line: usize,
    severity: Severity,
    title: String,
    body: String,
    hunk_header: String,
    patch_file: String,
    confidence: Option<String>,
}

impl Finding {
    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert("body".to_string(), JsonValue::string(self.body.clone()));
        if let Some(confidence) = &self.confidence {
            object.insert(
                "confidence".to_string(),
                JsonValue::string(confidence.clone()),
            );
        }
        object.insert(
            "hunk_header".to_string(),
            JsonValue::string(self.hunk_header.clone()),
        );
        object.insert("id".to_string(), JsonValue::string(self.id.clone()));
        object.insert("line".to_string(), JsonValue::number(self.line));
        object.insert(
            "patch_file".to_string(),
            JsonValue::string(self.patch_file.clone()),
        );
        object.insert("path".to_string(), JsonValue::string(self.path.clone()));
        object.insert(
            "severity".to_string(),
            JsonValue::string(self.severity.as_str()),
        );
        object.insert("side".to_string(), JsonValue::string(self.side.as_str()));
        object.insert("title".to_string(), JsonValue::string(self.title.clone()));
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Old,
    New,
}

impl Side {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "old" => Ok(Side::Old),
            "new" => Ok(Side::New),
            other => Err(format!("unsupported side `{other}`")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Side::Old => "old",
            Side::New => "new",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Severity {
    Note,
    Warning,
    Error,
}

impl Severity {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "note" => Ok(Severity::Note),
            "warning" => Ok(Severity::Warning),
            "error" => Ok(Severity::Error),
            other => Err(format!("unsupported severity `{other}`")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Severity::Note => "note",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileStatus {
    Queued,
    Running,
    Reviewed,
    Skipped,
    Failed,
}

impl FileStatus {
    fn as_str(self) -> &'static str {
        match self {
            FileStatus::Queued => "queued",
            FileStatus::Running => "running",
            FileStatus::Reviewed => "reviewed",
            FileStatus::Skipped => "skipped",
            FileStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestStatus {
    Running,
    Succeeded,
    Failed,
}

impl ManifestStatus {
    fn as_str(self) -> &'static str {
        match self {
            ManifestStatus::Running => "running",
            ManifestStatus::Succeeded => "succeeded",
            ManifestStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
struct Manifest {
    review_id: String,
    generated_at: String,
    range: String,
    base: String,
    head: String,
    merge_base: String,
    status: ManifestStatus,
    bundle_path: String,
    diff_patch: String,
    comments_markdown: String,
    comments_json: String,
    runtime: RuntimeManifest,
    files: Vec<FileRecord>,
    counts: Counts,
    errors: Vec<String>,
}

impl Manifest {
    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert("base".to_string(), JsonValue::string(self.base.clone()));
        object.insert(
            "bundle_path".to_string(),
            JsonValue::string(self.bundle_path.clone()),
        );
        object.insert(
            "comments_json".to_string(),
            JsonValue::string(self.comments_json.clone()),
        );
        object.insert(
            "comments_markdown".to_string(),
            JsonValue::string(self.comments_markdown.clone()),
        );
        object.insert("counts".to_string(), self.counts.to_json());
        object.insert(
            "diff_patch".to_string(),
            JsonValue::string(self.diff_patch.clone()),
        );
        object.insert(
            "errors".to_string(),
            JsonValue::Array(
                self.errors
                    .iter()
                    .cloned()
                    .map(JsonValue::string)
                    .collect::<Vec<_>>(),
            ),
        );
        object.insert(
            "files".to_string(),
            JsonValue::Array(self.files.iter().map(FileRecord::to_json).collect()),
        );
        object.insert(
            "generated_at".to_string(),
            JsonValue::string(self.generated_at.clone()),
        );
        object.insert("head".to_string(), JsonValue::string(self.head.clone()));
        object.insert(
            "merge_base".to_string(),
            JsonValue::string(self.merge_base.clone()),
        );
        object.insert("range".to_string(), JsonValue::string(self.range.clone()));
        object.insert(
            "review_id".to_string(),
            JsonValue::string(self.review_id.clone()),
        );
        object.insert("runtime".to_string(), self.runtime.to_json());
        object.insert(
            "status".to_string(),
            JsonValue::string(self.status.as_str()),
        );
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone)]
struct RuntimeManifest {
    agent: AgentConfig,
    review: ReviewConfig,
}

impl RuntimeManifest {
    fn to_json(&self) -> JsonValue {
        let mut agent = BTreeMap::new();
        agent.insert("command".to_string(), string_array(&self.agent.command));
        agent.insert(
            "enable_script_wrapper".to_string(),
            JsonValue::Bool(self.agent.enable_script_wrapper),
        );
        agent.insert(
            "label".to_string(),
            JsonValue::string(self.agent.label.clone()),
        );
        agent.insert(
            "output".to_string(),
            JsonValue::string(self.agent.output.as_str()),
        );
        agent.insert(
            "progress_filter".to_string(),
            string_array(&self.agent.progress_filter),
        );
        agent.insert("workers".to_string(), JsonValue::number(self.agent.workers));

        let mut review = BTreeMap::new();
        review.insert(
            "ignore_prefixes".to_string(),
            string_array(&self.review.ignore_prefixes),
        );
        review.insert(
            "max_patch_bytes".to_string(),
            JsonValue::number(self.review.max_patch_bytes),
        );

        let mut object = BTreeMap::new();
        object.insert("agent".to_string(), JsonValue::Object(agent));
        object.insert("review".to_string(), JsonValue::Object(review));
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone, Default)]
struct Counts {
    files_reviewed: usize,
    files_skipped: usize,
    files_failed: usize,
    findings: usize,
}

impl Counts {
    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "files_failed".to_string(),
            JsonValue::number(self.files_failed),
        );
        object.insert(
            "files_reviewed".to_string(),
            JsonValue::number(self.files_reviewed),
        );
        object.insert(
            "files_skipped".to_string(),
            JsonValue::number(self.files_skipped),
        );
        object.insert("findings".to_string(), JsonValue::number(self.findings));
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone)]
struct FileRecord {
    path: String,
    old_path: Option<String>,
    new_path: Option<String>,
    status: FileStatus,
    skip_reason: Option<String>,
    patch_file: Option<String>,
    agent_prompt_file: Option<String>,
    agent_response_file: Option<String>,
    findings: usize,
    error: Option<String>,
    agent_stderr: Vec<String>,
}

impl FileRecord {
    fn from_review_file(file: &ReviewFile) -> Self {
        Self {
            path: file.display_path.clone(),
            old_path: file.old_path.clone(),
            new_path: file.new_path.clone(),
            status: file.status,
            skip_reason: file.skip_reason.clone(),
            patch_file: file.patch_file.clone(),
            agent_prompt_file: file.agent_prompt_file.clone(),
            agent_response_file: file.agent_response_file.clone(),
            findings: file.findings,
            error: file.error.clone(),
            agent_stderr: file.agent_stderr.clone(),
        }
    }

    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        if let Some(prompt) = &self.agent_prompt_file {
            object.insert(
                "agent_prompt_file".to_string(),
                JsonValue::string(prompt.clone()),
            );
        } else {
            object.insert("agent_prompt_file".to_string(), JsonValue::Null);
        }
        if let Some(response) = &self.agent_response_file {
            object.insert(
                "agent_response_file".to_string(),
                JsonValue::string(response.clone()),
            );
        } else {
            object.insert("agent_response_file".to_string(), JsonValue::Null);
        }
        object.insert(
            "agent_stderr".to_string(),
            JsonValue::Array(
                self.agent_stderr
                    .iter()
                    .cloned()
                    .map(JsonValue::string)
                    .collect(),
            ),
        );
        if let Some(error) = &self.error {
            object.insert("error".to_string(), JsonValue::string(error.clone()));
        } else {
            object.insert("error".to_string(), JsonValue::Null);
        }
        object.insert("findings".to_string(), JsonValue::number(self.findings));
        if let Some(old_path) = &self.old_path {
            object.insert("old_path".to_string(), JsonValue::string(old_path.clone()));
        } else {
            object.insert("old_path".to_string(), JsonValue::Null);
        }
        if let Some(new_path) = &self.new_path {
            object.insert("new_path".to_string(), JsonValue::string(new_path.clone()));
        } else {
            object.insert("new_path".to_string(), JsonValue::Null);
        }
        if let Some(patch_file) = &self.patch_file {
            object.insert(
                "patch_file".to_string(),
                JsonValue::string(patch_file.clone()),
            );
        } else {
            object.insert("patch_file".to_string(), JsonValue::Null);
        }
        object.insert("path".to_string(), JsonValue::string(self.path.clone()));
        if let Some(skip_reason) = &self.skip_reason {
            object.insert(
                "skip_reason".to_string(),
                JsonValue::string(skip_reason.clone()),
            );
        } else {
            object.insert("skip_reason".to_string(), JsonValue::Null);
        }
        object.insert(
            "status".to_string(),
            JsonValue::string(self.status.as_str()),
        );
        JsonValue::Object(object)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FileStatus, RawFinding, Severity, Side, build_file, parse_anchors, parse_hunk_header,
    };
    use crate::{config::AppConfig, git::ChangeRecord};

    #[test]
    fn parses_hunk_headers() {
        let (_, old_start, new_start) = parse_hunk_header("@@ -12,4 +20,6 @@ fn review()")
            .unwrap()
            .unwrap();
        assert_eq!(old_start, 12);
        assert_eq!(new_start, 20);
    }

    #[test]
    fn tracks_changed_lines_for_both_sides() {
        let anchors = parse_anchors(
            "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 line1
-line2
+line2 updated
+line3
 line4
",
        )
        .unwrap();

        assert!(anchors.changed_old.contains(&2));
        assert!(anchors.changed_new.contains(&2));
        assert!(anchors.changed_new.contains(&3));
    }

    #[test]
    fn skips_rename_only_changes() {
        let config = AppConfig::default();
        let file = build_file(
            &ChangeRecord {
                status: "R100".to_string(),
                old_path: Some("src/old.rs".to_string()),
                new_path: Some("src/new.rs".to_string()),
            },
            "\
diff --git a/src/old.rs b/src/new.rs
similarity index 100%
rename from src/old.rs
rename to src/new.rs
",
            &config,
        )
        .unwrap();

        assert_eq!(file.status, FileStatus::Skipped);
        assert_eq!(
            file.skip_reason.as_deref(),
            Some("pure rename without textual changes")
        );
    }

    #[test]
    fn parses_finding_enum_values() {
        assert_eq!(Side::parse("new").unwrap(), Side::New);
        assert!(matches!(
            Severity::parse("warning").unwrap(),
            Severity::Warning
        ));

        let finding = RawFinding {
            path: "src/main.rs".to_string(),
            side: Side::New,
            line: 12,
            severity: Severity::Warning,
            title: "problem".to_string(),
            body: "details".to_string(),
            hunk_header: None,
            confidence: None,
        };
        assert_eq!(finding.path, "src/main.rs");
    }
}
