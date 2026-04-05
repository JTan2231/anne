use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::{
    json::{self, JsonValue},
    util::is_canonical_timestamp,
};

#[derive(Debug, Clone)]
pub(crate) struct ReviewContext {
    pub(crate) review_id: String,
    pub(crate) bundle_path: String,
    pub(crate) bundle_root: PathBuf,
    pub(crate) generated_at: Option<String>,
    pub(crate) range: Option<String>,
    pub(crate) merge_base: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) diff_patch: Option<String>,
    pub(crate) comments: Vec<SourceComment>,
    manifest: PersistedManifest,
}

impl ReviewContext {
    pub(crate) fn persist_comments(&mut self) -> Result<(), String> {
        self.manifest.sync_with_comments(&self.comments);

        fs::write(
            self.bundle_root.join(&self.manifest.comments_json_path),
            JsonValue::Array(
                self.comments
                    .iter()
                    .map(|comment| comment.raw_json.clone())
                    .collect(),
            )
            .render_pretty(),
        )
        .map_err(|error| {
            format!(
                "failed writing {}: {error}",
                self.manifest.comments_json_path
            )
        })?;

        fs::write(
            self.bundle_root.join(&self.manifest.comments_markdown_path),
            self.render_markdown(),
        )
        .map_err(|error| {
            format!(
                "failed writing {}: {error}",
                self.manifest.comments_markdown_path
            )
        })?;

        fs::write(
            self.bundle_root.join("manifest.json"),
            JsonValue::Object(self.manifest.raw.clone()).render_pretty(),
        )
        .map_err(|error| format!("failed writing manifest.json: {error}"))
    }

    fn render_markdown(&self) -> String {
        if self.manifest.stage.as_deref() == Some("preflight") {
            return self.render_preflight_markdown();
        }

        self.render_file_review_markdown()
    }

    fn render_preflight_markdown(&self) -> String {
        let status = self.status.as_deref().unwrap_or("unknown");
        let mut text = String::new();
        text.push_str(&format!(
            "# Review: {}\n\n",
            self.range.as_deref().unwrap_or(self.review_id.as_str())
        ));
        text.push_str(&format!(
            "- Generated: {}\n",
            self.generated_at.as_deref().unwrap_or("unknown")
        ));
        text.push_str(&format!("- Status: {status}\n"));
        text.push_str(&format!(
            "- Repository opened: {}\n",
            yes_no(self.manifest.preflight_bool("repository_opened"))
        ));
        text.push_str(&format!(
            "- Base resolved: {}\n",
            yes_no(self.manifest.preflight_bool("base_resolved"))
        ));
        text.push_str(&format!(
            "- Head resolved: {}\n",
            yes_no(self.manifest.preflight_bool("head_resolved"))
        ));
        text.push_str(&format!(
            "- Merge base resolved: {}\n",
            yes_no(self.manifest.preflight_bool("merge_base_resolved"))
        ));
        text.push_str(&format!(
            "- Diff generated: {}\n\n",
            yes_no(self.manifest.preflight_bool("diff_generated"))
        ));

        match status {
            "running" => {
                text.push_str(
                    "Review preflight is still running. `comments.json` remains `[]` until Anne reaches file analysis and can publish a usable review snapshot.\n",
                );
            }
            "failed" => {
                if let Some(error) = self.manifest.errors.last() {
                    text.push_str("## Failure\n\n");
                    text.push_str(error);
                    text.push_str("\n\n");
                }
                text.push_str(
                    "No file review occurred. comments.json is empty because Anne never reached file analysis.\n",
                );
            }
            _ => {
                text.push_str("Review preflight completed.\n");
            }
        }

        text
    }

    fn render_file_review_markdown(&self) -> String {
        let status = self.status.as_deref().unwrap_or("unknown");
        let counts = self.manifest.counts();
        let mut text = String::new();
        text.push_str(&format!(
            "# Review: {}\n\n",
            self.range.as_deref().unwrap_or(self.review_id.as_str())
        ));
        text.push_str(&format!(
            "- Generated: {}\n",
            self.generated_at.as_deref().unwrap_or("unknown")
        ));
        text.push_str(&format!("- Status: {status}\n"));
        text.push_str(&format!(
            "- Merge base: {}\n",
            self.merge_base.as_deref().unwrap_or("unresolved")
        ));
        text.push_str(&format!("- Files reviewed: {}\n", counts.files_reviewed));
        text.push_str(&format!("- Files skipped: {}\n", counts.files_skipped));
        if counts.files_failed > 0 {
            text.push_str(&format!("- Files failed: {}\n", counts.files_failed));
        }
        if status == "running" {
            text.push_str(&format!("- Findings observed: {}\n", counts.findings));
        } else {
            text.push_str(&format!("- Findings: {}\n", counts.findings));
        }
        text.push_str(&format!(
            "- Patch: {}\n\n",
            self.diff_patch.as_deref().unwrap_or("unavailable")
        ));
        if status == "running" {
            text.push_str(
                "This review is still running. `comments.json` remains a readable placeholder until Anne publishes final stable finding ids.\n\n",
            );
        }

        let runtime_notes = self
            .manifest
            .files
            .iter()
            .filter_map(|file| file.runtime_note.as_deref())
            .collect::<BTreeSet<_>>();
        if !runtime_notes.is_empty() {
            text.push_str("## Runtime Notes\n\n");
            for note in runtime_notes {
                text.push_str(&format!("- {note}\n"));
            }
            text.push('\n');
        }

        let mut findings_by_path: BTreeMap<&str, Vec<&SourceComment>> = BTreeMap::new();
        for comment in &self.comments {
            findings_by_path
                .entry(comment.path.as_str())
                .or_default()
                .push(comment);
        }

        for file in &self.manifest.files {
            if file.status == "skipped" {
                continue;
            }

            text.push_str(&format!("## {}\n\n", file.path));
            match file.status.as_str() {
                "reviewed" => {
                    if status == "running" {
                        text.push_str(
                            "Review complete for this file. Final findings publish when the full review finishes.\n\n",
                        );
                        if file.findings == 0 {
                            text.push_str(
                                "No unpublished findings were reported for this file so far.\n\n",
                            );
                        } else {
                            text.push_str(&format!(
                                "{} unpublished finding{} pending final publication.\n\n",
                                file.findings,
                                if file.findings == 1 { "" } else { "s" }
                            ));
                        }
                        continue;
                    }

                    let file_findings = findings_by_path.get(file.path.as_str());
                    if file_findings.is_none_or(|findings| findings.is_empty()) {
                        text.push_str("No findings.\n\n");
                    } else if let Some(file_findings) = file_findings {
                        for finding in file_findings {
                            text.push_str(&format!(
                                "### {} {} {}:{}\n{}\n\n{}\n\nHunk: {}\n\n",
                                finding.id,
                                finding.severity,
                                finding.side,
                                finding.line,
                                finding.title,
                                finding.body,
                                finding.hunk_header.as_deref().unwrap_or("unavailable")
                            ));
                        }
                    }
                }
                "failed" => {
                    text.push_str("Review failed.\n\n");
                    if let Some(error) = &file.error {
                        text.push_str(error);
                        text.push_str("\n\n");
                    }
                }
                "running" => {
                    text.push_str("Review running.\n\n");
                }
                "queued" => {
                    if status == "running" {
                        text.push_str("Queued for review.\n\n");
                    }
                }
                _ => {}
            }
        }

        let skipped = self
            .manifest
            .files
            .iter()
            .filter(|file| file.status == "skipped")
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
}

#[derive(Debug, Clone)]
pub(crate) struct SourceComment {
    pub(crate) raw_json: JsonValue,
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) side: String,
    pub(crate) line: usize,
    pub(crate) severity: String,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) hunk_header: Option<String>,
    pub(crate) patch_file: Option<String>,
    pub(crate) confidence: Option<String>,
}

impl SourceComment {
    pub(crate) fn anchor(&self) -> String {
        format!("{}:{}", self.side, self.line)
    }
}

#[derive(Debug, Clone)]
struct PersistedManifest {
    raw: BTreeMap<String, JsonValue>,
    stage: Option<String>,
    comments_json_path: String,
    comments_markdown_path: String,
    files: Vec<PersistedFileRecord>,
    errors: Vec<String>,
}

impl PersistedManifest {
    fn from_raw(raw: BTreeMap<String, JsonValue>) -> Result<Self, String> {
        let comments_json_path = object_string(&raw, "comments_json")
            .ok_or_else(|| "manifest.json did not advertise comments.json".to_string())?;
        let comments_markdown_path = object_string(&raw, "comments_markdown")
            .ok_or_else(|| "manifest.json did not advertise comments.md".to_string())?;

        Ok(Self {
            stage: object_string(&raw, "stage"),
            files: parse_files(&raw)?,
            errors: string_array_field(&raw, "errors"),
            raw,
            comments_json_path,
            comments_markdown_path,
        })
    }

    fn sync_with_comments(&mut self, comments: &[SourceComment]) {
        let mut findings_by_path = BTreeMap::<&str, usize>::new();
        for comment in comments {
            *findings_by_path.entry(comment.path.as_str()).or_default() += 1;
        }

        for file in &mut self.files {
            if file.status == "reviewed" {
                file.findings = *findings_by_path.get(file.path.as_str()).unwrap_or(&0);
            }
        }

        if let Some(JsonValue::Array(files)) = self.raw.get_mut("files") {
            for file in files {
                let JsonValue::Object(file) = file else {
                    continue;
                };
                let path = object_string(file, "path").unwrap_or_default();
                let status = object_string(file, "status").unwrap_or_default();
                if status == "reviewed" {
                    let findings = *findings_by_path.get(path.as_str()).unwrap_or(&0);
                    file.insert("findings".to_string(), JsonValue::number(findings));
                }
            }
        }

        let counts = self.counts();
        let mut counts_object = BTreeMap::new();
        counts_object.insert(
            "files_failed".to_string(),
            JsonValue::number(counts.files_failed),
        );
        counts_object.insert(
            "files_reviewed".to_string(),
            JsonValue::number(counts.files_reviewed),
        );
        counts_object.insert(
            "files_skipped".to_string(),
            JsonValue::number(counts.files_skipped),
        );
        counts_object.insert("findings".to_string(), JsonValue::number(counts.findings));
        self.raw
            .insert("counts".to_string(), JsonValue::Object(counts_object));
    }

    fn counts(&self) -> Counts {
        Counts {
            files_reviewed: self
                .files
                .iter()
                .filter(|file| file.status == "reviewed" || file.status == "failed")
                .count(),
            files_skipped: self
                .files
                .iter()
                .filter(|file| file.status == "skipped")
                .count(),
            files_failed: self
                .files
                .iter()
                .filter(|file| file.status == "failed")
                .count(),
            findings: self.files.iter().map(|file| file.findings).sum(),
        }
    }

    fn preflight_bool(&self, key: &str) -> bool {
        self.raw
            .get("preflight")
            .and_then(|value| match value {
                JsonValue::Object(object) => object.get(key),
                _ => None,
            })
            .and_then(|value| match value {
                JsonValue::Bool(value) => Some(*value),
                _ => None,
            })
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
struct PersistedFileRecord {
    path: String,
    status: String,
    skip_reason: Option<String>,
    findings: usize,
    error: Option<String>,
    runtime_note: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Counts {
    files_reviewed: usize,
    files_skipped: usize,
    files_failed: usize,
    findings: usize,
}

#[derive(Debug, Clone)]
struct CandidateManifest {
    raw: BTreeMap<String, JsonValue>,
    generated_at: Option<String>,
    range: Option<String>,
    merge_base: Option<String>,
    status: Option<String>,
    diff_patch: Option<String>,
    comments_json: Option<String>,
    comments_markdown: Option<String>,
}

#[derive(Debug, Clone)]
struct ReviewCandidate {
    review_id: String,
    bundle_root: PathBuf,
    manifest: Option<CandidateManifest>,
}

impl ReviewCandidate {
    fn selection_preference(&self) -> Result<ReviewCandidatePreference, String> {
        let manifest = self
            .manifest
            .as_ref()
            .ok_or_else(|| "manifest.json was not readable or valid".to_string())?;
        match manifest.status.as_deref() {
            Some("succeeded") | Some("failed") => Ok(ReviewCandidatePreference::Completed),
            Some("running") => Ok(ReviewCandidatePreference::Running),
            Some(status) => Err(format!("manifest status `{status}` was not supported")),
            None => Err("manifest.json did not contain a supported status".to_string()),
        }
    }

    fn generated_at(&self) -> Option<&String> {
        self.manifest
            .as_ref()
            .and_then(|manifest| manifest.generated_at.as_ref())
    }

    fn review_context(
        &self,
        manifest: PersistedManifest,
        comments: Vec<SourceComment>,
    ) -> ReviewContext {
        let manifest_info = self.manifest.as_ref().expect("missing candidate manifest");
        ReviewContext {
            review_id: self.review_id.clone(),
            bundle_path: PathBuf::from(".anne")
                .join("reviews")
                .join(&self.review_id)
                .display()
                .to_string(),
            bundle_root: self.bundle_root.clone(),
            generated_at: self.generated_at().cloned(),
            range: manifest_info.range.clone(),
            merge_base: manifest_info.merge_base.clone(),
            status: manifest_info.status.clone(),
            diff_patch: manifest_info.diff_patch.clone(),
            comments,
            manifest,
        }
    }
}

#[derive(Debug, Clone)]
struct SkippedReviewCandidate {
    review_id: String,
    reason: String,
}

#[derive(Debug, Clone, Copy)]
enum ReviewCandidatePreference {
    Completed,
    Running,
}

pub(crate) fn discover_review_context(repo_root: &Path) -> Result<ReviewContext, String> {
    let reviews_root = repo_root.join(".anne").join("reviews");
    let entries = match fs::read_dir(&reviews_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(no_review_outputs_error());
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
        return Err(no_review_outputs_error());
    }

    sort_review_candidates(&mut candidates);

    let mut completed_candidates = Vec::new();
    let mut running_candidates = Vec::new();
    let mut skipped = Vec::new();
    for candidate in candidates.into_iter().rev() {
        match candidate.selection_preference() {
            Ok(ReviewCandidatePreference::Completed) => completed_candidates.push(candidate),
            Ok(ReviewCandidatePreference::Running) => running_candidates.push(candidate),
            Err(reason) => skipped.push(SkippedReviewCandidate {
                review_id: candidate.review_id,
                reason,
            }),
        }
    }

    for candidate in completed_candidates {
        match load_review_context(&candidate) {
            Ok(context) => return Ok(context),
            Err(reason) => skipped.push(SkippedReviewCandidate {
                review_id: candidate.review_id,
                reason,
            }),
        }
    }

    for candidate in running_candidates {
        match load_review_context(&candidate) {
            Ok(context) => return Ok(context),
            Err(reason) => skipped.push(SkippedReviewCandidate {
                review_id: candidate.review_id,
                reason,
            }),
        }
    }

    Err(no_usable_review_bundle_error(&skipped))
}

fn read_review_manifest(bundle_root: &Path) -> Option<CandidateManifest> {
    let path = bundle_root.join("manifest.json");
    let text = fs::read_to_string(path).ok()?;
    let value = json::parse(&text).ok()?;
    let JsonValue::Object(object) = value else {
        return None;
    };

    Some(CandidateManifest {
        generated_at: object_string(&object, "generated_at"),
        range: object_string(&object, "range"),
        merge_base: object_string(&object, "merge_base"),
        status: object_string(&object, "status"),
        diff_patch: object_string(&object, "diff_patch"),
        comments_json: object_string(&object, "comments_json"),
        comments_markdown: object_string(&object, "comments_markdown"),
        raw: object,
    })
}

fn load_review_context(candidate: &ReviewCandidate) -> Result<ReviewContext, String> {
    let manifest = candidate
        .manifest
        .as_ref()
        .ok_or_else(|| "manifest.json was not readable or valid".to_string())?;
    let diff_patch = manifest
        .diff_patch
        .as_deref()
        .ok_or_else(|| "manifest.json did not advertise diff.patch".to_string())?;
    read_bundle_artifact(&candidate.bundle_root, diff_patch, "diff.patch")?;
    let comments_markdown = manifest
        .comments_markdown
        .as_deref()
        .ok_or_else(|| "manifest.json did not advertise comments.md".to_string())?;
    read_bundle_artifact(&candidate.bundle_root, comments_markdown, "comments.md")?;
    let comments_json = manifest
        .comments_json
        .as_deref()
        .ok_or_else(|| "manifest.json did not advertise comments.json".to_string())?;
    let comments_text =
        read_bundle_artifact(&candidate.bundle_root, comments_json, "comments.json")?;
    let comments = parse_comments(&comments_text)
        .map_err(|error| format!("comments.json was not valid: {error}"))?;
    let manifest = PersistedManifest::from_raw(manifest.raw.clone())
        .map_err(|error| format!("manifest.json was not valid: {error}"))?;
    Ok(candidate.review_context(manifest, comments))
}

fn read_bundle_artifact(
    bundle_root: &Path,
    relative_path: &str,
    label: &str,
) -> Result<String, String> {
    fs::read_to_string(bundle_root.join(relative_path))
        .map_err(|error| format!("{label} was not readable: {error}"))
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

fn parse_files(object: &BTreeMap<String, JsonValue>) -> Result<Vec<PersistedFileRecord>, String> {
    let files = object
        .get("files")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "manifest.json did not contain a files array".to_string())?;
    let mut output = Vec::new();
    for file in files {
        let file = file
            .as_object()
            .ok_or_else(|| "manifest file entries must be JSON objects".to_string())?;
        let runtime_note = file.get("agent_runtime").and_then(|value| match value {
            JsonValue::Object(runtime) => runtime.get("runtime_note").and_then(JsonValue::as_str),
            _ => None,
        });
        output.push(PersistedFileRecord {
            path: required_string(file, "path")?,
            status: required_string(file, "status")?,
            skip_reason: optional_string(file, "skip_reason")?,
            findings: required_usize(file, "findings")?,
            error: optional_string(file, "error")?,
            runtime_note: runtime_note.map(ToString::to_string),
        });
    }
    Ok(output)
}

fn no_review_outputs_error() -> String {
    "no review outputs discovered under `.anne/reviews/`".to_string()
}

fn no_usable_review_bundle_error(skipped: &[SkippedReviewCandidate]) -> String {
    let mut error =
        "review bundles were found under `.anne/reviews/`, but no usable review bundle was found"
            .to_string();
    if skipped.is_empty() {
        return error;
    }

    error.push_str(": ");
    for (index, candidate) in skipped.iter().enumerate() {
        if index > 0 {
            error.push_str("; ");
        }
        error.push_str(&format!("`{}` ({})", candidate.review_id, candidate.reason));
    }

    error
}

fn sort_review_candidates(candidates: &mut [ReviewCandidate]) {
    candidates.sort_by(|left, right| {
        sortable_generated_at(left)
            .cmp(&sortable_generated_at(right))
            .then(left.review_id.cmp(&right.review_id))
    });
}

fn sortable_generated_at(candidate: &ReviewCandidate) -> Option<&str> {
    candidate
        .generated_at()
        .and_then(|timestamp| is_canonical_timestamp(timestamp).then_some(timestamp.as_str()))
}

fn object_string(object: &BTreeMap<String, JsonValue>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn string_array_field(object: &BTreeMap<String, JsonValue>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
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

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
