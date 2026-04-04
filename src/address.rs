use std::{
    cell::RefCell,
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use crate::{
    agent,
    config::{AgentConfig, AppConfig},
    git,
    json::{self, JsonValue},
    util::{current_timestamp, slugify_text, string_array},
};

pub struct RunResult {
    pub bundle_path: Option<String>,
    pub comments_selected: usize,
    pub specs_generated: usize,
    pub specs_failed: usize,
    pub success: bool,
    pub error: Option<String>,
}

pub fn run(comment_id: Option<String>) -> Result<RunResult, String> {
    let cwd = env::current_dir().map_err(|error| format!("failed to read current dir: {error}"))?;
    let repo_root = git::repo_root(&cwd)?;
    let config = AppConfig::load(&repo_root)?;
    let review_context = discover_review_context(&repo_root)?;

    let mut selected_comments = review_context.comments.clone();
    selected_comments.sort_by(|left, right| left.id.cmp(&right.id));

    let selection_filter = comment_id.clone().unwrap_or_else(|| "all".to_string());
    if let Some(comment_id) = comment_id.as_deref() {
        let selected = selected_comments
            .into_iter()
            .find(|comment| comment.id == comment_id)
            .ok_or_else(|| {
                format!(
                    "comment `{comment_id}` was not found in review `{}`",
                    review_context.review_id
                )
            })?;
        selected_comments = vec![selected];
    }

    let init_path = repo_root.join("INIT.md");
    let init_text = fs::read_to_string(&init_path).map_err(|error| {
        format!(
            "address requires a readable INIT.md as the current feature-spec structure reference: {error}"
        )
    })?;
    let init_structure = InitStructure::derive(&init_text)?;

    if !selected_comments.is_empty() && config.agent.command.is_empty() {
        return Err(
            "agent.command is not configured; set it in .anne/config.toml before running address"
                .to_string(),
        );
    }

    let generated_at = current_timestamp();
    let address_id = build_address_id(&generated_at, &selection_filter);
    let relative_bundle_path = PathBuf::from(".anne").join("address").join(&address_id);
    let bundle_root = repo_root.join(&relative_bundle_path);
    fs::create_dir_all(bundle_root.join("specs"))
        .map_err(|error| format!("failed creating address specs dir: {error}"))?;
    fs::create_dir_all(bundle_root.join("agent"))
        .map_err(|error| format!("failed creating address agent dir: {error}"))?;

    fs::write(
        bundle_root.join("selected_comments.json"),
        JsonValue::Array(
            selected_comments
                .iter()
                .map(|comment| comment.raw_json.clone())
                .collect(),
        )
        .render_pretty(),
    )
    .map_err(|error| format!("failed writing selected_comments.json: {error}"))?;

    let mut manifest = Manifest {
        address_id,
        generated_at: generated_at.clone(),
        status: if selected_comments.is_empty() {
            ManifestStatus::Succeeded
        } else {
            ManifestStatus::Running
        },
        bundle_path: relative_bundle_path.display().to_string(),
        summary_markdown: "summary.md".to_string(),
        selected_comments_json: "selected_comments.json".to_string(),
        selection_filter,
        source_review: SourceReviewManifest::from_context(&review_context),
        runtime: RuntimeManifest {
            agent: config.agent.clone(),
            init_path: "INIT.md".to_string(),
            init_headings: init_structure.headings.clone(),
        },
        comments: selected_comments
            .iter()
            .map(CommentRun::queued_from_source)
            .collect(),
        counts: Counts::default(),
        errors: Vec::new(),
    };
    update_counts(&mut manifest);
    write_outputs(&bundle_root, &manifest)?;

    let manifest = RefCell::new(manifest);
    let queued_jobs = selected_comments
        .iter()
        .enumerate()
        .map(|(comment_index, comment)| CommentJob {
            comment_index,
            comment: comment.clone(),
        })
        .collect::<Vec<_>>();

    agent::run_bounded(
        config.agent.workers,
        queued_jobs,
        |_, job| {
            let mut manifest = manifest.borrow_mut();
            let run = &mut manifest.comments[job.comment_index];
            run.status = CommentStatus::Running;
            run.agent_prompt_file = Some(format!("agent/{}.prompt.md", job.comment.id));
            run.agent_response_file = Some(format!("agent/{}.response.txt", job.comment.id));
            manifest.status = ManifestStatus::Running;
            update_counts(&mut manifest);
            write_outputs(&bundle_root, &manifest)
        },
        |_, job| {
            process_comment(
                &repo_root,
                &bundle_root,
                &review_context,
                &init_structure,
                &config.agent,
                job,
            )
        },
        |_, result| {
            let mut manifest = manifest.borrow_mut();
            let run = &mut manifest.comments[result.comment_index];
            let comment_id = run.comment_id.clone();
            run.status = result.status;
            run.spec_file = result.spec_file;
            run.agent_stderr = result.agent_stderr;
            run.error = result.error.clone();

            if let Some(error) = result.error {
                manifest.errors.push(format!("{comment_id}: {error}"));
            }

            manifest.status = ManifestStatus::Running;
            update_counts(&mut manifest);
            write_outputs(&bundle_root, &manifest)
        },
    )?;

    let mut manifest = manifest.into_inner();

    if !selected_comments.is_empty() {
        manifest.status = if manifest.errors.is_empty() {
            ManifestStatus::Succeeded
        } else {
            ManifestStatus::Failed
        };
        update_counts(&mut manifest);
        write_outputs(&bundle_root, &manifest)?;
    }

    Ok(to_run_result(&manifest))
}

fn discover_review_context(repo_root: &Path) -> Result<ReviewContext, String> {
    let reviews_root = repo_root.join(".anne").join("reviews");
    let entries = match fs::read_dir(&reviews_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("no review outputs discovered under `.anne/reviews/`".to_string());
        }
        Err(error) => {
            return Err(format!(
                "failed reading review storage at {}: {error}",
                reviews_root.display()
            ));
        }
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed reading review bundle entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed reading review bundle entry type: {error}"))?;
        if !file_type.is_dir() {
            continue;
        }

        let review_id = entry.file_name().to_string_lossy().to_string();
        let bundle_root = entry.path();
        let manifest = read_review_manifest(&bundle_root);
        candidates.push(ReviewCandidate {
            review_id,
            bundle_root,
            manifest,
        });
    }

    if candidates.is_empty() {
        return Err("no review outputs discovered under `.anne/reviews/`".to_string());
    }

    candidates.sort_by(|left, right| {
        left.generated_at()
            .cmp(&right.generated_at())
            .then(left.review_id.cmp(&right.review_id))
    });

    let selected = candidates
        .pop()
        .ok_or_else(|| "no review outputs discovered under `.anne/reviews/`".to_string())?;

    let comments_path = selected.bundle_root.join("comments.json");
    let comments_text = fs::read_to_string(&comments_path).map_err(|error| {
        format!(
            "selected review `{}` is missing a readable comments.json: {error}",
            selected.review_id
        )
    })?;
    let comments = parse_comments(&comments_text).map_err(|error| {
        format!(
            "failed parsing comments.json for review `{}`: {error}",
            selected.review_id
        )
    })?;

    Ok(ReviewContext {
        review_id: selected.review_id.clone(),
        bundle_path: PathBuf::from(".anne")
            .join("reviews")
            .join(&selected.review_id)
            .display()
            .to_string(),
        bundle_root: selected.bundle_root.clone(),
        generated_at: selected.generated_at().cloned(),
        range: selected
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.range.clone()),
        merge_base: selected
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.merge_base.clone()),
        status: selected
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.status.clone()),
        diff_patch: selected
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.diff_patch.clone()),
        comments,
    })
}

fn read_review_manifest(bundle_root: &Path) -> Option<ReviewManifest> {
    let path = bundle_root.join("manifest.json");
    let text = fs::read_to_string(path).ok()?;
    let value = json::parse(&text).ok()?;
    let object = value.as_object()?;

    Some(ReviewManifest {
        generated_at: object_string(object, "generated_at"),
        range: object_string(object, "range"),
        merge_base: object_string(object, "merge_base"),
        status: object_string(object, "status"),
        diff_patch: object_string(object, "diff_patch"),
    })
}

fn parse_comments(text: &str) -> Result<Vec<SourceComment>, String> {
    let value = json::parse(text)?;
    let array = value
        .as_array()
        .ok_or_else(|| "comments.json must be a JSON array".to_string())?;

    let mut comments = Vec::new();
    for item in array {
        let object = item
            .as_object()
            .ok_or_else(|| "each selected comment must be a JSON object".to_string())?;
        comments.push(SourceComment {
            raw_json: item.clone(),
            id: required_string(object, "id")?,
            path: required_string(object, "path")?,
            side: required_string(object, "side")?,
            line: required_usize(object, "line")?,
            severity: required_string(object, "severity")?,
            title: required_string(object, "title")?,
            body: required_string(object, "body")?,
            hunk_header: optional_string(object, "hunk_header")?,
            patch_file: optional_string(object, "patch_file")?,
            confidence: optional_scalar_string(object, "confidence")?,
        });
    }

    Ok(comments)
}

fn process_comment(
    repo_root: &Path,
    bundle_root: &Path,
    review_context: &ReviewContext,
    init_structure: &InitStructure,
    agent_config: &AgentConfig,
    job: CommentJob,
) -> CommentJobResult {
    let prompt_file = format!("agent/{}.prompt.md", job.comment.id);
    let response_file = format!("agent/{}.response.txt", job.comment.id);
    let prompt = build_prompt(review_context, init_structure, &job.comment);

    if let Err(error) = fs::write(bundle_root.join(&prompt_file), &prompt) {
        return CommentJobResult::failure(
            job.comment_index,
            Vec::new(),
            format!("failed writing {prompt_file}: {error}"),
        );
    }

    match agent::run_captured(repo_root, agent_config, &prompt) {
        Ok(result) => {
            if let Err(error) = fs::write(bundle_root.join(&response_file), &result.raw_stdout) {
                return CommentJobResult::failure(
                    job.comment_index,
                    result.stderr_lines,
                    format!("failed writing {response_file}: {error}"),
                );
            }

            let spec_text =
                match finalize_spec(review_context, &job.comment, &result.assistant_text) {
                    Ok(spec_text) => spec_text,
                    Err(error) => {
                        return CommentJobResult::failure(
                            job.comment_index,
                            result.stderr_lines,
                            error,
                        );
                    }
                };

            let spec_file = build_spec_file(&job.comment);
            if let Err(error) = fs::write(bundle_root.join(&spec_file), spec_text) {
                return CommentJobResult::failure(
                    job.comment_index,
                    result.stderr_lines,
                    format!("failed writing {spec_file}: {error}"),
                );
            }

            CommentJobResult {
                comment_index: job.comment_index,
                status: CommentStatus::Generated,
                spec_file: Some(spec_file),
                agent_stderr: result.stderr_lines,
                error: None,
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
            CommentJobResult::failure(job.comment_index, error.stderr_lines, message)
        }
    }
}

fn build_prompt(
    review_context: &ReviewContext,
    init_structure: &InitStructure,
    comment: &SourceComment,
) -> String {
    let mut prompt = String::new();
    prompt
        .push_str("You are writing one Anne feature spec to address a single review comment.\n\n");
    prompt.push_str(
        "Return markdown only. Do not use code fences around the final document. Do not add commentary before or after the document.\n\n",
    );
    prompt.push_str("Selected source comment:\n");
    prompt.push_str(&format!("- Review: {}\n", review_context.review_id));
    prompt.push_str(&format!("- Comment: {}\n", comment.id));
    prompt.push_str(&format!("- Path: {}\n", comment.path));
    prompt.push_str(&format!("- Anchor: {}\n", comment.anchor()));
    prompt.push_str(&format!("- Severity: {}\n", comment.severity));
    prompt.push_str(&format!("- Title: {}\n", comment.title));
    if let Some(hunk_header) = &comment.hunk_header {
        prompt.push_str(&format!("- Hunk header: {hunk_header}\n"));
    }
    if let Some(patch_file) = &comment.patch_file {
        prompt.push_str(&format!("- Patch file: {patch_file}\n"));
    }
    if let Some(confidence) = &comment.confidence {
        prompt.push_str(&format!("- Confidence: {confidence}\n"));
    }

    prompt.push_str("\nComment body:\n");
    prompt.push_str(comment.body.trim());
    prompt.push_str("\n\nRelevant review context:\n");
    prompt.push_str(&format!(
        "- Review bundle: {}\n",
        review_context.bundle_path
    ));
    if let Some(generated_at) = &review_context.generated_at {
        prompt.push_str(&format!("- Review generated: {generated_at}\n"));
    }
    if let Some(range) = &review_context.range {
        prompt.push_str(&format!("- Review range: {range}\n"));
    }
    if let Some(merge_base) = &review_context.merge_base {
        prompt.push_str(&format!("- Merge base: {merge_base}\n"));
    }
    if let Some(status) = &review_context.status {
        prompt.push_str(&format!("- Review status: {status}\n"));
    }
    if let Some(diff_patch) = &review_context.diff_patch {
        prompt.push_str(&format!("- Full diff artifact: {diff_patch}\n"));
    }

    let related = review_context
        .comments
        .iter()
        .filter(|other| other.path == comment.path && other.id != comment.id)
        .collect::<Vec<_>>();
    if related.is_empty() {
        prompt.push_str("- Related comments on the same path: none\n");
    } else {
        prompt.push_str("- Related comments on the same path:\n");
        for other in related {
            prompt.push_str(&format!(
                "  - {} {} {} {}\n",
                other.id,
                other.severity,
                other.anchor(),
                other.title
            ));
        }
    }

    prompt.push_str("\nAnne feature-spec structure guidance derived from INIT.md:\n");
    prompt.push_str(&init_structure.render_guidance());

    prompt.push_str(
        "\nRequirements for the generated spec:\n\
- Follow Anne's current feature-spec structure and level of detail without copying INIT.md's review-specific subject matter.\n\
- Include a `## Source Comment` section near the top for traceability.\n\
- Stay focused on addressing comment ",
    );
    prompt.push_str(&comment.id);
    prompt.push_str(
        " only, though you may mention adjacent implementation work needed to make the fix coherent.\n\
- Mention assumptions or open questions when runtime context is incomplete.\n\
- Produce a complete markdown document, not a patch.\n",
    );

    if let Some(patch_file) = &comment.patch_file {
        let patch_path = review_context.bundle_root.join(patch_file);
        match fs::read_to_string(&patch_path) {
            Ok(patch) => {
                prompt.push_str(&format!(
                    "\nPatch context from `{patch_file}`:\n\n```diff\n"
                ));
                prompt.push_str(&patch);
                if !patch.ends_with('\n') {
                    prompt.push('\n');
                }
                prompt.push_str("```\n");
            }
            Err(error) => {
                prompt.push_str(&format!(
                    "\nPatch context from `{patch_file}` was unavailable: {error}\n"
                ));
            }
        }
    } else {
        prompt.push_str(
            "\nPatch context: no per-file patch artifact was recorded for this comment.\n",
        );
    }

    prompt
}

fn finalize_spec(
    review_context: &ReviewContext,
    comment: &SourceComment,
    assistant_text: &str,
) -> Result<String, String> {
    let trimmed = assistant_text.trim();
    if trimmed.is_empty() {
        return Err("agent response was empty".to_string());
    }
    if !trimmed
        .lines()
        .any(|line| line.trim_start().starts_with('#'))
    {
        return Err(
            "agent response must be a markdown document with at least one heading".to_string(),
        );
    }

    let mut spec = trimmed.to_string();
    if !spec.contains("## Source Comment") {
        spec = insert_source_comment_section(
            &spec,
            &render_source_comment_section(review_context, comment),
        );
    }
    if !spec.ends_with('\n') {
        spec.push('\n');
    }
    Ok(spec)
}

fn insert_source_comment_section(spec: &str, source_comment_section: &str) -> String {
    let mut lines = spec.lines();
    let Some(first_line) = lines.next() else {
        return source_comment_section.to_string();
    };

    if first_line.trim_start().starts_with("# ") {
        let remainder = lines.collect::<Vec<_>>().join("\n");
        if remainder.trim().is_empty() {
            format!("{first_line}\n\n{source_comment_section}\n")
        } else {
            format!("{first_line}\n\n{source_comment_section}\n\n{remainder}")
        }
    } else {
        format!("{source_comment_section}\n\n{spec}")
    }
}

fn render_source_comment_section(
    review_context: &ReviewContext,
    comment: &SourceComment,
) -> String {
    let mut section = String::new();
    section.push_str("## Source Comment\n\n");
    section.push_str(&format!("- Review: {}\n", review_context.review_id));
    section.push_str(&format!("- Comment: {}\n", comment.id));
    section.push_str(&format!("- Path: {}\n", comment.path));
    section.push_str(&format!("- Anchor: {}\n", comment.anchor()));
    section.push_str(&format!("- Severity: {}\n", comment.severity));
    if let Some(hunk_header) = &comment.hunk_header {
        section.push_str(&format!("- Hunk: {hunk_header}\n"));
    }
    section
}

fn build_address_id(timestamp: &str, selection_filter: &str) -> String {
    format!(
        "{}-latest-{}",
        timestamp.replace(':', "-"),
        slugify_text(selection_filter)
    )
}

fn build_spec_file(comment: &SourceComment) -> String {
    let mut slug = slugify_text(&comment.title);
    if slug.len() > 48 {
        slug.truncate(48);
        slug = slug.trim_matches('-').to_string();
    }
    if slug.is_empty() {
        slug = "spec".to_string();
    }
    format!("specs/{}-{}.md", comment.id, slug)
}

fn write_outputs(bundle_root: &Path, manifest: &Manifest) -> Result<(), String> {
    fs::write(
        bundle_root.join("manifest.json"),
        manifest.to_json().render_pretty(),
    )
    .map_err(|error| format!("failed writing manifest.json: {error}"))?;
    fs::write(bundle_root.join("summary.md"), render_summary(manifest))
        .map_err(|error| format!("failed writing summary.md: {error}"))?;
    Ok(())
}

fn render_summary(manifest: &Manifest) -> String {
    let mut text = String::new();
    text.push_str("# Address Run\n\n");
    text.push_str(&format!("- Generated: {}\n", manifest.generated_at));
    text.push_str(&format!(
        "- Source review: {}\n",
        manifest.source_review.review_id
    ));
    text.push_str(&format!(
        "- Comment filter: {}\n",
        manifest.selection_filter
    ));
    text.push_str(&format!(
        "- Comments selected: {}\n",
        manifest.counts.comments_selected
    ));
    text.push_str(&format!(
        "- Specs generated: {}\n",
        manifest.counts.specs_generated
    ));
    text.push_str(&format!(
        "- Specs failed: {}\n",
        manifest.counts.specs_failed
    ));

    if manifest.comments.is_empty() {
        text.push_str("\nNo comments selected from the source review.\n");
        return text;
    }

    text.push('\n');
    for comment in &manifest.comments {
        text.push_str(&format!(
            "## {} {} {}\n{}\n\n",
            comment.comment_id,
            comment.path,
            comment.anchor(),
            comment.title
        ));
        text.push_str(&format!("- Severity: {}\n", comment.severity));
        if let Some(spec_file) = &comment.spec_file {
            text.push_str(&format!("- Spec: {spec_file}\n"));
        } else {
            text.push_str(&format!("- Status: {}\n", comment.status.as_str()));
        }
        text.push('\n');
    }

    let failed = manifest
        .comments
        .iter()
        .filter(|comment| comment.status == CommentStatus::Failed)
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        text.push_str("## Failed\n\n");
        for comment in failed {
            text.push_str(&format!(
                "- {}: {}\n",
                comment.comment_id,
                comment.error.as_deref().unwrap_or("unknown failure")
            ));
        }
    }

    text
}

fn update_counts(manifest: &mut Manifest) {
    manifest.counts = Counts {
        comments_selected: manifest.comments.len(),
        specs_generated: manifest
            .comments
            .iter()
            .filter(|comment| comment.status == CommentStatus::Generated)
            .count(),
        specs_failed: manifest
            .comments
            .iter()
            .filter(|comment| comment.status == CommentStatus::Failed)
            .count(),
    };
}

fn to_run_result(manifest: &Manifest) -> RunResult {
    RunResult {
        bundle_path: Some(manifest.bundle_path.clone()),
        comments_selected: manifest.counts.comments_selected,
        specs_generated: manifest.counts.specs_generated,
        specs_failed: manifest.counts.specs_failed,
        success: manifest.status == ManifestStatus::Succeeded,
        error: manifest.errors.first().cloned(),
    }
}

#[derive(Debug, Clone)]
struct CommentJob {
    comment_index: usize,
    comment: SourceComment,
}

#[derive(Debug, Clone)]
struct CommentJobResult {
    comment_index: usize,
    status: CommentStatus,
    spec_file: Option<String>,
    agent_stderr: Vec<String>,
    error: Option<String>,
}

impl CommentJobResult {
    fn failure(comment_index: usize, agent_stderr: Vec<String>, error: String) -> Self {
        Self {
            comment_index,
            status: CommentStatus::Failed,
            spec_file: None,
            agent_stderr,
            error: Some(error),
        }
    }
}

fn required_string(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| format!("comment field `{key}` must be a string"))
}

fn required_usize(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<usize, String> {
    let value = object
        .get(key)
        .and_then(JsonValue::as_i64)
        .ok_or_else(|| format!("comment field `{key}` must be an integer"))?;
    if value < 0 {
        return Err(format!("comment field `{key}` must be positive"));
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
            .ok_or_else(|| format!("comment field `{key}` must be a string when present")),
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
            "comment field `{key}` must be a string, number, or boolean when present"
        )),
    }
}

fn object_string(object: &BTreeMap<String, JsonValue>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn optional_json_string(value: &Option<String>) -> JsonValue {
    value.clone().map_or(JsonValue::Null, JsonValue::string)
}

#[derive(Debug, Clone)]
struct ReviewManifest {
    generated_at: Option<String>,
    range: Option<String>,
    merge_base: Option<String>,
    status: Option<String>,
    diff_patch: Option<String>,
}

#[derive(Debug, Clone)]
struct ReviewCandidate {
    review_id: String,
    bundle_root: PathBuf,
    manifest: Option<ReviewManifest>,
}

impl ReviewCandidate {
    fn generated_at(&self) -> Option<&String> {
        self.manifest
            .as_ref()
            .and_then(|manifest| manifest.generated_at.as_ref())
    }
}

#[derive(Debug, Clone)]
struct ReviewContext {
    review_id: String,
    bundle_path: String,
    bundle_root: PathBuf,
    generated_at: Option<String>,
    range: Option<String>,
    merge_base: Option<String>,
    status: Option<String>,
    diff_patch: Option<String>,
    comments: Vec<SourceComment>,
}

#[derive(Debug, Clone)]
struct SourceComment {
    raw_json: JsonValue,
    id: String,
    path: String,
    side: String,
    line: usize,
    severity: String,
    title: String,
    body: String,
    hunk_header: Option<String>,
    patch_file: Option<String>,
    confidence: Option<String>,
}

impl SourceComment {
    fn anchor(&self) -> String {
        format!("{}:{}", self.side, self.line)
    }
}

#[derive(Debug, Clone)]
struct InitStructure {
    title: String,
    feature_heading: Option<String>,
    feature_subsections: Vec<String>,
    other_sections: Vec<String>,
    headings: Vec<String>,
}

impl InitStructure {
    fn derive(init_text: &str) -> Result<Self, String> {
        let headings = init_text
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                (trimmed.starts_with("# ")
                    || trimmed.starts_with("## ")
                    || trimmed.starts_with("### "))
                .then_some(trimmed.to_string())
            })
            .collect::<Vec<_>>();

        let title = headings
            .iter()
            .find_map(|heading| heading.strip_prefix("# ").map(ToString::to_string))
            .ok_or_else(|| "INIT.md must contain a `# ...` title heading".to_string())?;

        let mut feature_heading = None;
        let mut feature_subsections = Vec::new();
        let mut other_sections = Vec::new();
        let mut in_feature = false;

        for heading in &headings {
            if let Some(section) = heading.strip_prefix("## ") {
                if section.starts_with("Feature:") {
                    feature_heading = Some(section.to_string());
                    in_feature = true;
                } else {
                    other_sections.push(section.to_string());
                    in_feature = false;
                }
            } else if let Some(subsection) = heading.strip_prefix("### ")
                && in_feature
            {
                feature_subsections.push(subsection.to_string());
            }
        }

        Ok(Self {
            title,
            feature_heading,
            feature_subsections,
            other_sections,
            headings,
        })
    }

    fn render_guidance(&self) -> String {
        let mut text = String::new();
        text.push_str(&format!("- Root title: # {}\n", self.title));
        if self.feature_heading.is_some() {
            text.push_str("- Core feature heading pattern: ## Feature: <short feature name>\n");
        }
        if self.feature_subsections.is_empty() {
            text.push_str("- INIT.md does not define feature subsections explicitly.\n");
        } else {
            text.push_str("- Feature subsections in INIT.md order:\n");
            for subsection in &self.feature_subsections {
                text.push_str(&format!("  - ### {subsection}\n"));
            }
        }
        if self.other_sections.is_empty() {
            text.push_str("- No additional top-level sections were detected in INIT.md.\n");
        } else {
            text.push_str("- Additional top-level sections currently present in INIT.md:\n");
            for section in &self.other_sections {
                text.push_str(&format!("  - ## {section}\n"));
            }
        }
        text.push_str("- Use that structure and level of detail, but adapt section names and content to this comment-specific feature spec.\n");
        text
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommentStatus {
    Queued,
    Running,
    Generated,
    Failed,
}

impl CommentStatus {
    fn as_str(self) -> &'static str {
        match self {
            CommentStatus::Queued => "queued",
            CommentStatus::Running => "running",
            CommentStatus::Generated => "generated",
            CommentStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
struct Manifest {
    address_id: String,
    generated_at: String,
    status: ManifestStatus,
    bundle_path: String,
    summary_markdown: String,
    selected_comments_json: String,
    selection_filter: String,
    source_review: SourceReviewManifest,
    runtime: RuntimeManifest,
    comments: Vec<CommentRun>,
    counts: Counts,
    errors: Vec<String>,
}

impl Manifest {
    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "address_id".to_string(),
            JsonValue::string(self.address_id.clone()),
        );
        object.insert(
            "bundle_path".to_string(),
            JsonValue::string(self.bundle_path.clone()),
        );
        object.insert("counts".to_string(), self.counts.to_json());
        object.insert(
            "comments".to_string(),
            JsonValue::Array(self.comments.iter().map(CommentRun::to_json).collect()),
        );
        object.insert(
            "errors".to_string(),
            JsonValue::Array(self.errors.iter().cloned().map(JsonValue::string).collect()),
        );
        object.insert(
            "generated_at".to_string(),
            JsonValue::string(self.generated_at.clone()),
        );
        object.insert("runtime".to_string(), self.runtime.to_json());
        object.insert(
            "selected_comments_json".to_string(),
            JsonValue::string(self.selected_comments_json.clone()),
        );
        object.insert(
            "selection_filter".to_string(),
            JsonValue::string(self.selection_filter.clone()),
        );
        object.insert("source_review".to_string(), self.source_review.to_json());
        object.insert(
            "status".to_string(),
            JsonValue::string(self.status.as_str()),
        );
        object.insert(
            "summary_markdown".to_string(),
            JsonValue::string(self.summary_markdown.clone()),
        );
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone)]
struct SourceReviewManifest {
    review_id: String,
    bundle_path: String,
    generated_at: Option<String>,
    range: Option<String>,
    merge_base: Option<String>,
    status: Option<String>,
    diff_patch: Option<String>,
}

impl SourceReviewManifest {
    fn from_context(context: &ReviewContext) -> Self {
        Self {
            review_id: context.review_id.clone(),
            bundle_path: context.bundle_path.clone(),
            generated_at: context.generated_at.clone(),
            range: context.range.clone(),
            merge_base: context.merge_base.clone(),
            status: context.status.clone(),
            diff_patch: context.diff_patch.clone(),
        }
    }

    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "bundle_path".to_string(),
            JsonValue::string(self.bundle_path.clone()),
        );
        object.insert(
            "diff_patch".to_string(),
            optional_json_string(&self.diff_patch),
        );
        object.insert(
            "generated_at".to_string(),
            optional_json_string(&self.generated_at),
        );
        object.insert(
            "merge_base".to_string(),
            optional_json_string(&self.merge_base),
        );
        object.insert("range".to_string(), optional_json_string(&self.range));
        object.insert(
            "review_id".to_string(),
            JsonValue::string(self.review_id.clone()),
        );
        object.insert("status".to_string(), optional_json_string(&self.status));
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone)]
struct RuntimeManifest {
    agent: AgentConfig,
    init_path: String,
    init_headings: Vec<String>,
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

        let mut object = BTreeMap::new();
        object.insert("agent".to_string(), JsonValue::Object(agent));
        object.insert(
            "init_headings".to_string(),
            JsonValue::Array(
                self.init_headings
                    .iter()
                    .cloned()
                    .map(JsonValue::string)
                    .collect(),
            ),
        );
        object.insert(
            "init_path".to_string(),
            JsonValue::string(self.init_path.clone()),
        );
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone, Default)]
struct Counts {
    comments_selected: usize,
    specs_generated: usize,
    specs_failed: usize,
}

impl Counts {
    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "comments_selected".to_string(),
            JsonValue::number(self.comments_selected),
        );
        object.insert(
            "specs_failed".to_string(),
            JsonValue::number(self.specs_failed),
        );
        object.insert(
            "specs_generated".to_string(),
            JsonValue::number(self.specs_generated),
        );
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone)]
struct CommentRun {
    comment_id: String,
    path: String,
    side: String,
    line: usize,
    severity: String,
    title: String,
    status: CommentStatus,
    spec_file: Option<String>,
    agent_prompt_file: Option<String>,
    agent_response_file: Option<String>,
    agent_stderr: Vec<String>,
    error: Option<String>,
}

impl CommentRun {
    fn queued_from_source(comment: &SourceComment) -> Self {
        Self {
            comment_id: comment.id.clone(),
            path: comment.path.clone(),
            side: comment.side.clone(),
            line: comment.line,
            severity: comment.severity.clone(),
            title: comment.title.clone(),
            status: CommentStatus::Queued,
            spec_file: None,
            agent_prompt_file: None,
            agent_response_file: None,
            agent_stderr: Vec::new(),
            error: None,
        }
    }

    fn anchor(&self) -> String {
        format!("{}:{}", self.side, self.line)
    }

    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "agent_prompt_file".to_string(),
            optional_json_string(&self.agent_prompt_file),
        );
        object.insert(
            "agent_response_file".to_string(),
            optional_json_string(&self.agent_response_file),
        );
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
        object.insert(
            "comment_id".to_string(),
            JsonValue::string(self.comment_id.clone()),
        );
        object.insert("error".to_string(), optional_json_string(&self.error));
        object.insert("line".to_string(), JsonValue::number(self.line));
        object.insert("path".to_string(), JsonValue::string(self.path.clone()));
        object.insert(
            "severity".to_string(),
            JsonValue::string(self.severity.clone()),
        );
        object.insert("side".to_string(), JsonValue::string(self.side.clone()));
        object.insert(
            "spec_file".to_string(),
            optional_json_string(&self.spec_file),
        );
        object.insert(
            "status".to_string(),
            JsonValue::string(self.status.as_str()),
        );
        object.insert("title".to_string(), JsonValue::string(self.title.clone()));
        JsonValue::Object(object)
    }
}

#[cfg(test)]
mod tests {
    use super::{InitStructure, ReviewCandidate, ReviewManifest};
    use std::path::PathBuf;

    #[test]
    fn derives_feature_shape_from_init() {
        let structure = InitStructure::derive(
            "\
# Anne

## Feature: Example

### Problem

### Goals

### Non-Goals

## Proposed Approach
",
        )
        .unwrap();

        assert_eq!(structure.title, "Anne");
        assert_eq!(
            structure.feature_subsections,
            ["Problem", "Goals", "Non-Goals"]
        );
        assert_eq!(structure.other_sections, ["Proposed Approach"]);
    }

    #[test]
    fn review_candidates_prefer_timestamp_then_review_id() {
        let mut candidates = [
            ReviewCandidate {
                review_id: "2026-04-04T01-00-00Z-a".to_string(),
                bundle_root: PathBuf::from("a"),
                manifest: Some(ReviewManifest {
                    generated_at: Some("2026-04-04T01:00:00Z".to_string()),
                    range: None,
                    merge_base: None,
                    status: None,
                    diff_patch: None,
                }),
            },
            ReviewCandidate {
                review_id: "2026-04-04T01-00-00Z-b".to_string(),
                bundle_root: PathBuf::from("b"),
                manifest: Some(ReviewManifest {
                    generated_at: Some("2026-04-04T01:00:00Z".to_string()),
                    range: None,
                    merge_base: None,
                    status: None,
                    diff_patch: None,
                }),
            },
        ];

        candidates.sort_by(|left, right| {
            left.generated_at()
                .cmp(&right.generated_at())
                .then(left.review_id.cmp(&right.review_id))
        });

        assert_eq!(
            candidates.last().unwrap().review_id,
            "2026-04-04T01-00-00Z-b"
        );
    }
}
