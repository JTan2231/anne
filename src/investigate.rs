use std::{
    collections::BTreeMap,
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use crate::{
    agent,
    cli::InvestigationRequest,
    config::{AgentConfig, AppConfig},
    git::{self, RepoContext},
    json::JsonValue,
    util::{current_timestamp, slugify_text, string_array},
};

pub struct RunResult {
    pub bundle_path: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}

pub fn run(request: InvestigationRequest) -> Result<RunResult, String> {
    let prepared = prepare_run(&request.prompt)?;
    Ok(run_reserved(prepared))
}

struct PreparedRun {
    repo_root: PathBuf,
    config: AppConfig,
    prompt: String,
    generated_at: String,
    bundle: ReservedInvestigationBundle,
}

struct ReservedInvestigationBundle {
    investigation_id: String,
    relative_bundle_path: PathBuf,
    bundle_root: PathBuf,
}

fn prepare_run(prompt: &str) -> Result<PreparedRun, String> {
    let cwd = env::current_dir().map_err(|error| format!("failed to read current dir: {error}"))?;
    let repo_root = git::repo_root(&cwd)?;
    let config = AppConfig::load(&repo_root)?;
    let generated_at = current_timestamp();
    let bundle = reserve_investigation_bundle(&repo_root, &generated_at, prompt)?;

    Ok(PreparedRun {
        repo_root,
        config,
        prompt: prompt.to_string(),
        generated_at,
        bundle,
    })
}

fn run_reserved(prepared: PreparedRun) -> RunResult {
    let PreparedRun {
        repo_root,
        config,
        prompt,
        generated_at,
        bundle,
    } = prepared;
    let ReservedInvestigationBundle {
        investigation_id,
        relative_bundle_path,
        bundle_root,
    } = bundle;
    let bundle_path = relative_bundle_path.display().to_string();

    let mut manifest = Manifest {
        investigation_id,
        generated_at,
        status: ManifestStatus::Running,
        bundle_path,
        prompt_file: None,
        report_markdown: None,
        agent_prompt_file: None,
        agent_response_file: None,
        repository: RepositoryManifest::new(&repo_root),
        runtime: RuntimeManifest::new(config.agent.clone()),
        agent_stderr: Vec::new(),
        errors: Vec::new(),
    };

    if let Err(error) = fs::create_dir_all(bundle_root.join("agent"))
        .map_err(|error| format!("failed creating investigation agent dir: {error}"))
    {
        return persist_failed_bundle(&bundle_root, &mut manifest, &prompt, error);
    }

    if let Err(error) = fs::write(bundle_root.join("prompt.txt"), &prompt)
        .map_err(|error| format!("failed writing prompt.txt: {error}"))
    {
        return persist_failed_bundle(&bundle_root, &mut manifest, &prompt, error);
    }
    manifest.prompt_file = Some("prompt.txt".to_string());

    if let Err(result) = persist_running_manifest(&bundle_root, &mut manifest, &prompt) {
        return result;
    }

    let repo_context_error = match git::capture_repo_context(&repo_root) {
        Ok(context) => {
            manifest.repository = RepositoryManifest::from_repo_context(context);
            None
        }
        Err(error) => {
            manifest.repository.capture_error = Some(error.clone());
            Some(error)
        }
    };

    let rendered_prompt = build_agent_prompt(&prompt, &manifest.repository);
    if let Err(error) = fs::write(bundle_root.join("agent/prompt.md"), &rendered_prompt)
        .map_err(|error| format!("failed writing agent/prompt.md: {error}"))
    {
        return persist_failed_bundle(&bundle_root, &mut manifest, &prompt, error);
    }
    manifest.agent_prompt_file = Some("agent/prompt.md".to_string());

    if let Err(result) = persist_running_manifest(&bundle_root, &mut manifest, &prompt) {
        return result;
    }

    if let Some(error) = repo_context_error {
        return persist_failed_bundle(&bundle_root, &mut manifest, &prompt, error);
    }

    match agent::run_captured(&repo_root, &config.agent, &rendered_prompt) {
        Ok(result) => {
            manifest.agent_stderr = result.stderr_lines.clone();
            manifest.runtime.invocation = Some(result.runtime.clone());

            if let Err(error) =
                fs::write(bundle_root.join("agent/response.txt"), &result.raw_stdout)
                    .map_err(|error| format!("failed writing agent/response.txt: {error}"))
            {
                return persist_failed_bundle(&bundle_root, &mut manifest, &prompt, error);
            }
            manifest.agent_response_file = Some("agent/response.txt".to_string());

            let report = match finalize_report(&prompt, &result.assistant_text) {
                Ok(report) => report,
                Err(error) => {
                    return persist_failed_bundle(&bundle_root, &mut manifest, &prompt, error);
                }
            };

            if let Err(error) = fs::write(bundle_root.join("report.md"), &report)
                .map_err(|error| format!("failed writing report.md: {error}"))
            {
                return persist_failed_bundle(&bundle_root, &mut manifest, &prompt, error);
            }
            manifest.report_markdown = Some("report.md".to_string());
            manifest.status = ManifestStatus::Succeeded;

            if let Err(error) = write_manifest(&bundle_root, &manifest) {
                return persist_failed_bundle(&bundle_root, &mut manifest, &prompt, error);
            }

            to_run_result(&manifest, None)
        }
        Err(error) => {
            manifest.agent_stderr = error.stderr_lines;
            manifest.runtime.invocation = Some(error.runtime.clone());

            let message = match fs::write(bundle_root.join("agent/response.txt"), &error.raw_stdout)
                .map_err(|write_error| format!("failed writing agent/response.txt: {write_error}"))
            {
                Ok(()) => {
                    manifest.agent_response_file = Some("agent/response.txt".to_string());
                    error.message
                }
                Err(write_error) => format!("{}; {write_error}", error.message),
            };

            persist_failed_bundle(&bundle_root, &mut manifest, &prompt, message)
        }
    }
}

fn build_investigation_id(timestamp: &str, prompt: &str) -> String {
    format!("{}-{}", timestamp.replace(':', "-"), slugify_text(prompt))
}

fn reserve_investigation_bundle(
    repo_root: &Path,
    timestamp: &str,
    prompt: &str,
) -> Result<ReservedInvestigationBundle, String> {
    let investigations_root = repo_root.join(".anne").join("investigations");
    fs::create_dir_all(&investigations_root)
        .map_err(|error| format!("failed creating investigations storage root: {error}"))?;

    let base_id = build_investigation_id(timestamp, prompt);
    for collision_index in 1usize.. {
        let investigation_id = if collision_index == 1 {
            base_id.clone()
        } else {
            format!("{base_id}-{collision_index}")
        };
        let relative_bundle_path = PathBuf::from(".anne")
            .join("investigations")
            .join(&investigation_id);
        let bundle_root = repo_root.join(&relative_bundle_path);
        match fs::create_dir(&bundle_root) {
            Ok(()) => {
                return Ok(ReservedInvestigationBundle {
                    investigation_id,
                    relative_bundle_path,
                    bundle_root,
                });
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("failed reserving investigation bundle: {error}")),
        }
    }

    unreachable!("investigation bundle reservation must eventually return or error")
}

fn build_agent_prompt(prompt: &str, repository: &RepositoryManifest) -> String {
    let mut text = String::new();
    text.push_str(
        "You are investigating a reported problem in the current repository for Anne.\n\n",
    );
    text.push_str(
        "Inspect the repository and any relevant local artifacts before writing the final report. Anne artifacts may already exist under `.anne/`.\n\n",
    );
    text.push_str(
        "Return Markdown only. Do not wrap the whole report in code fences. Do not add commentary before or after the report.\n\n",
    );
    text.push_str("Repository context:\n");
    text.push_str(&format!("- Repo root: {}\n", repository.repo_root));
    text.push_str(&format!(
        "- Head: {}\n",
        repository.head.as_deref().unwrap_or("unavailable")
    ));
    text.push_str(&format!(
        "- Branch: {}\n",
        repository
            .branch
            .as_deref()
            .unwrap_or("detached or unavailable")
    ));
    text.push_str(&format!(
        "- Worktree dirty: {}\n",
        match repository.worktree_dirty {
            Some(value) => yes_no(value),
            None => "unavailable",
        }
    ));
    if let Some(error) = &repository.capture_error {
        text.push_str(&format!("- Repository context error: {error}\n"));
    }

    text.push_str("\nReported problem (verbatim):\n");
    text.push_str(prompt);
    if !prompt.ends_with('\n') {
        text.push('\n');
    }

    text.push_str(
        "\nWrite a handoff-ready Markdown report with headings such as:\n\
- # Investigation Report\n\
- ## Prompt\n\
- ## Findings\n\
- ## Evidence\n\
- ## Likely Causes\n\
- ## Recommended Next Steps\n\
- ## Open Questions when uncertainty remains\n\n\
Requirements:\n\
- Distinguish confirmed evidence from inference.\n\
- Cite concrete files, commands, artifacts, or observations when possible.\n\
- If you cannot confirm the issue directly, say so and provide the most likely causes plus concrete next steps.\n\
- Optimize the report for later handoff, not stream-of-consciousness notes.\n",
    );
    text
}

fn finalize_report(prompt: &str, assistant_text: &str) -> Result<String, String> {
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

    let mut report = trimmed.to_string();
    if !contains_prompt_heading(&report) {
        report = insert_prompt_section(&report, &render_prompt_section(prompt));
    }
    if !report.ends_with('\n') {
        report.push('\n');
    }
    Ok(report)
}

fn contains_prompt_heading(report: &str) -> bool {
    report.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with('#')
            && trimmed
                .trim_start_matches('#')
                .trim()
                .eq_ignore_ascii_case("prompt")
    })
}

fn insert_prompt_section(report: &str, prompt_section: &str) -> String {
    let mut lines = report.lines();
    let Some(first_line) = lines.next() else {
        return prompt_section.to_string();
    };

    if first_line.trim_start().starts_with("# ") {
        let remainder = lines.collect::<Vec<_>>().join("\n");
        if remainder.trim().is_empty() {
            format!("{first_line}\n\n{prompt_section}\n")
        } else {
            format!("{first_line}\n\n{prompt_section}\n\n{remainder}")
        }
    } else {
        format!("{prompt_section}\n\n{report}")
    }
}

fn render_prompt_section(prompt: &str) -> String {
    let mut section = String::new();
    section.push_str("## Prompt\n\n");

    let mut wrote_line = false;
    for line in prompt.split('\n') {
        if wrote_line {
            section.push('\n');
        }
        if line.is_empty() {
            section.push('>');
        } else {
            section.push_str("> ");
            section.push_str(line);
        }
        wrote_line = true;
    }

    if !wrote_line {
        section.push('>');
    }

    section
}

fn persist_running_manifest(
    bundle_root: &Path,
    manifest: &mut Manifest,
    prompt: &str,
) -> Result<(), RunResult> {
    write_manifest(bundle_root, manifest)
        .map_err(|error| persist_failed_bundle(bundle_root, manifest, prompt, error))
}

fn persist_failed_bundle(
    bundle_root: &Path,
    manifest: &mut Manifest,
    prompt: &str,
    error: String,
) -> RunResult {
    mark_manifest_failed(manifest, &error);

    let failure_report = render_failure_report(prompt, manifest, &error);
    let mut combined_error = error;

    match fs::write(bundle_root.join("report.md"), failure_report)
        .map_err(|error| format!("failed writing report.md: {error}"))
    {
        Ok(()) => manifest.report_markdown = Some("report.md".to_string()),
        Err(write_error) if write_error != combined_error => {
            combined_error = format!("{combined_error}; {write_error}");
        }
        Err(_) => {}
    }

    if let Err(write_error) = write_manifest(bundle_root, manifest)
        && write_error != combined_error
    {
        combined_error = format!("{combined_error}; {write_error}");
    }

    to_run_result(manifest, Some(combined_error))
}

fn render_failure_report(prompt: &str, manifest: &Manifest, error: &str) -> String {
    let mut text = String::new();
    text.push_str("# Investigation Report\n\n");
    text.push_str(&render_prompt_section(prompt));
    text.push_str("\n\n## Failure\n\n");
    text.push_str("- Status: failed\n");
    text.push_str(&format!("- Error: {error}\n"));

    text.push_str("\n## Evidence\n\n");
    if let Some(prompt_file) = &manifest.prompt_file {
        text.push_str(&format!("- Original prompt: {prompt_file}\n"));
    }
    if let Some(agent_prompt_file) = &manifest.agent_prompt_file {
        text.push_str(&format!("- Agent prompt: {agent_prompt_file}\n"));
    }
    if let Some(agent_response_file) = &manifest.agent_response_file {
        text.push_str(&format!("- Raw agent response: {agent_response_file}\n"));
    }
    if !manifest.agent_stderr.is_empty() {
        text.push_str("- Agent stderr details were captured in manifest.json\n");
    }
    if let Some(capture_error) = &manifest.repository.capture_error {
        text.push_str(&format!(
            "- Repository context capture failed: {capture_error}\n"
        ));
    }

    text.push_str("\n## Recommended Next Steps\n\n");
    if let Some(agent_response_file) = &manifest.agent_response_file {
        text.push_str(&format!(
            "- Inspect `{agent_response_file}` to review the raw agent output.\n"
        ));
    }
    text.push_str("- Fix the reported failure and rerun `anne investigate`.\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn write_manifest(bundle_root: &Path, manifest: &Manifest) -> Result<(), String> {
    fs::write(
        bundle_root.join("manifest.json"),
        manifest.to_json().render_pretty(),
    )
    .map_err(|error| format!("failed writing manifest.json: {error}"))
}

fn mark_manifest_failed(manifest: &mut Manifest, error: &str) {
    manifest.status = ManifestStatus::Failed;
    if manifest.errors.iter().all(|existing| existing != error) {
        manifest.errors.push(error.to_string());
    }
}

fn to_run_result(manifest: &Manifest, error: Option<String>) -> RunResult {
    RunResult {
        bundle_path: Some(manifest.bundle_path.clone()),
        success: manifest.status == ManifestStatus::Succeeded,
        error,
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
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
    investigation_id: String,
    generated_at: String,
    status: ManifestStatus,
    bundle_path: String,
    prompt_file: Option<String>,
    report_markdown: Option<String>,
    agent_prompt_file: Option<String>,
    agent_response_file: Option<String>,
    repository: RepositoryManifest,
    runtime: RuntimeManifest,
    agent_stderr: Vec<String>,
    errors: Vec<String>,
}

impl Manifest {
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
            "bundle_path".to_string(),
            JsonValue::string(self.bundle_path.clone()),
        );
        object.insert(
            "errors".to_string(),
            JsonValue::Array(self.errors.iter().cloned().map(JsonValue::string).collect()),
        );
        object.insert(
            "generated_at".to_string(),
            JsonValue::string(self.generated_at.clone()),
        );
        object.insert(
            "investigation_id".to_string(),
            JsonValue::string(self.investigation_id.clone()),
        );
        object.insert(
            "prompt_file".to_string(),
            optional_json_string(&self.prompt_file),
        );
        object.insert(
            "report_markdown".to_string(),
            optional_json_string(&self.report_markdown),
        );
        object.insert("repository".to_string(), self.repository.to_json());
        object.insert("runtime".to_string(), self.runtime.to_json());
        object.insert(
            "status".to_string(),
            JsonValue::string(self.status.as_str()),
        );
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone)]
struct RepositoryManifest {
    repo_root: String,
    head: Option<String>,
    branch: Option<String>,
    worktree_dirty: Option<bool>,
    capture_error: Option<String>,
}

impl RepositoryManifest {
    fn new(repo_root: &Path) -> Self {
        Self {
            repo_root: repo_root.display().to_string(),
            head: None,
            branch: None,
            worktree_dirty: None,
            capture_error: None,
        }
    }

    fn from_repo_context(context: RepoContext) -> Self {
        Self {
            repo_root: context.repo_root,
            head: context.head,
            branch: context.branch,
            worktree_dirty: Some(context.worktree_dirty),
            capture_error: None,
        }
    }

    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert("branch".to_string(), optional_json_string(&self.branch));
        object.insert(
            "capture_error".to_string(),
            optional_json_string(&self.capture_error),
        );
        object.insert("head".to_string(), optional_json_string(&self.head));
        object.insert(
            "repo_root".to_string(),
            JsonValue::string(self.repo_root.clone()),
        );
        object.insert(
            "worktree_dirty".to_string(),
            optional_json_bool(self.worktree_dirty),
        );
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone)]
struct RuntimeManifest {
    agent: AgentConfig,
    invocation: Option<agent::AgentInvocation>,
}

impl RuntimeManifest {
    fn new(agent: AgentConfig) -> Self {
        Self {
            agent,
            invocation: None,
        }
    }

    fn to_json(&self) -> JsonValue {
        let mut agent = BTreeMap::new();
        agent.insert("command".to_string(), string_array(&self.agent.command));
        agent.insert(
            "command_source".to_string(),
            JsonValue::string(self.agent.command_source.as_str()),
        );
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
        agent.insert(
            "progress_filter_source".to_string(),
            JsonValue::string(self.agent.progress_filter_source.as_str()),
        );
        agent.insert("workers".to_string(), JsonValue::number(self.agent.workers));

        let mut object = BTreeMap::new();
        object.insert("agent".to_string(), JsonValue::Object(agent));
        if let Some(invocation) = &self.invocation {
            object.insert("invocation".to_string(), invocation.to_json());
        } else {
            object.insert("invocation".to_string(), JsonValue::Null);
        }
        JsonValue::Object(object)
    }
}

fn optional_json_string(value: &Option<String>) -> JsonValue {
    value.clone().map_or(JsonValue::Null, JsonValue::string)
}

fn optional_json_bool(value: Option<bool>) -> JsonValue {
    value.map_or(JsonValue::Null, JsonValue::Bool)
}
