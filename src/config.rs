use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    pub agent: AgentConfig,
    pub review: ReviewConfig,
}

impl AppConfig {
    pub fn load(repo_root: &Path) -> Result<Self, String> {
        let path = repo_root.join(".anne").join("config.toml");
        if !path.exists() {
            let mut config = Self::default();
            config.agent.apply_bundled_defaults();
            return Ok(config);
        }

        let contents = fs::read_to_string(&path)
            .map_err(|error| format!("failed reading {}: {error}", path.display()))?;
        let mut config = parse_config(&contents)
            .map_err(|error| format!("failed parsing {}: {error}", path.display()))?;
        config.agent.apply_bundled_defaults();
        Ok(config)
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub label: String,
    pub command: Vec<String>,
    pub command_source: AgentSettingSource,
    pub progress_filter: Vec<String>,
    pub progress_filter_source: AgentSettingSource,
    pub output: AgentOutput,
    output_source: AgentSettingSource,
    pub enable_script_wrapper: bool,
    pub workers: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            label: "default".to_string(),
            command: Vec::new(),
            command_source: AgentSettingSource::None,
            progress_filter: Vec::new(),
            progress_filter_source: AgentSettingSource::None,
            output: AgentOutput::default(),
            output_source: AgentSettingSource::None,
            enable_script_wrapper: false,
            workers: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentSettingSource {
    #[default]
    None,
    Explicit,
    BundledDefault,
}

impl AgentSettingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentSettingSource::None => "none",
            AgentSettingSource::Explicit => "explicit",
            AgentSettingSource::BundledDefault => "bundled-default",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentOutput {
    #[default]
    Text,
    WrappedJson,
}

impl AgentOutput {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentOutput::Text => "text",
            AgentOutput::WrappedJson => "wrapped-json",
        }
    }
}

impl AgentConfig {
    fn apply_bundled_defaults(&mut self) {
        let label = normalized_agent_label(&self.label);
        self.label = label.clone();

        if self.command.is_empty()
            && let Some(path) = bundled_agent_command(&label)
        {
            self.command = vec![path.display().to_string()];
            self.command_source = AgentSettingSource::BundledDefault;
        } else if !self.command.is_empty() && self.command_source == AgentSettingSource::None {
            self.command_source = AgentSettingSource::Explicit;
        }

        if self.progress_filter.is_empty()
            && self.progress_filter_source != AgentSettingSource::Explicit
            && self.command_source == AgentSettingSource::BundledDefault
            && let Some(path) = bundled_progress_filter(&label)
        {
            self.progress_filter = vec![path.display().to_string()];
            self.progress_filter_source = AgentSettingSource::BundledDefault;
        } else if !self.progress_filter.is_empty()
            && self.progress_filter_source == AgentSettingSource::None
        {
            self.progress_filter_source = AgentSettingSource::Explicit;
        }

        if self.output_source == AgentSettingSource::None
            && self.command_source == AgentSettingSource::BundledDefault
        {
            self.output = AgentOutput::WrappedJson;
            self.output_source = AgentSettingSource::BundledDefault;
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReviewConfig {
    pub max_patch_bytes: usize,
    pub ignore_prefixes: Vec<String>,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            max_patch_bytes: 128 * 1024,
            ignore_prefixes: vec![
                "vendor/".to_string(),
                "third_party/".to_string(),
                "node_modules/".to_string(),
                "dist/".to_string(),
                "build/".to_string(),
                "target/".to_string(),
            ],
        }
    }
}

impl ReviewConfig {
    pub fn ignores(&self, path: &str) -> bool {
        self.ignore_prefixes
            .iter()
            .any(|prefix| path == prefix || path.starts_with(prefix))
    }
}

fn parse_config(text: &str) -> Result<AppConfig, String> {
    let mut config = AppConfig {
        agent: AgentConfig::default(),
        review: ReviewConfig::default(),
    };
    let mut section = String::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("expected `key = value`, got `{line}`"));
        };
        let key = key.trim();
        let value = value.trim();

        match (section.as_str(), key) {
            ("agent", "label") => config.agent.label = parse_string(value)?,
            ("agent", "command") => {
                config.agent.command = parse_string_array(value)?;
                config.agent.command_source = AgentSettingSource::Explicit;
            }
            ("agent", "progress_filter") => {
                config.agent.progress_filter = parse_string_array(value)?;
                config.agent.progress_filter_source = AgentSettingSource::Explicit;
            }
            ("agent", "output") => {
                config.agent.output = match parse_string(value)?.as_str() {
                    "text" => AgentOutput::Text,
                    "wrapped-json" => AgentOutput::WrappedJson,
                    other => return Err(format!("unsupported agent.output `{other}`")),
                };
                config.agent.output_source = AgentSettingSource::Explicit;
            }
            ("agent", "enable_script_wrapper") => {
                config.agent.enable_script_wrapper = parse_bool(value)?
            }
            ("agent", "workers") => {
                config.agent.workers = parse_worker_count(value)?;
            }
            ("review", "max_patch_bytes") => config.review.max_patch_bytes = parse_usize(value)?,
            ("review", "ignore_prefixes") => {
                config.review.ignore_prefixes = parse_string_array(value)?
            }
            ("", _) => return Err(format!("key `{key}` appears outside a section")),
            (other, _) => return Err(format!("unsupported config entry [{other}] {key}")),
        }
    }

    Ok(config)
}

fn parse_string(value: &str) -> Result<String, String> {
    let value = value.trim();
    if !(value.starts_with('"') && value.ends_with('"')) {
        return Err(format!("expected quoted string, got `{value}`"));
    }
    Ok(value[1..value.len() - 1]
        .replace("\\\"", "\"")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\\", "\\"))
}

fn parse_string_array(value: &str) -> Result<Vec<String>, String> {
    let value = value.trim();
    if !(value.starts_with('[') && value.ends_with(']')) {
        return Err(format!("expected string array, got `{value}`"));
    }

    let inner = value[1..value.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }

    inner
        .split(',')
        .map(|item| parse_string(item.trim()))
        .collect()
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("expected boolean, got `{other}`")),
    }
}

fn parse_usize(value: &str) -> Result<usize, String> {
    value
        .trim()
        .parse::<usize>()
        .map_err(|error| format!("expected integer, got `{value}`: {error}"))
}

fn parse_worker_count(value: &str) -> Result<usize, String> {
    let workers = parse_usize(value)?;
    if workers == 0 {
        return Err("agent.workers must be greater than 0".to_string());
    }
    Ok(workers)
}

fn normalized_agent_label(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed == "agent" {
        "default".to_string()
    } else {
        trimmed.to_string()
    }
}

fn bundled_agent_shim_dir_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(dir) = env::var("ANNE_AGENT_SHIMS_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            dirs.push(PathBuf::from(trimmed));
        }
    }

    if let Ok(exe) = env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.join("agents"));
        if let Some(prefix) = dir.parent() {
            dirs.push(prefix.join("share").join("anne").join("agents"));
        }
    }

    dirs.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples")
            .join("agents"),
    );

    let mut seen = HashSet::new();
    dirs.retain(|path| path.is_dir() && seen.insert(path.clone()));
    dirs
}

fn bundled_agent_command(label: &str) -> Option<PathBuf> {
    find_in_shim_dirs(&format!("{label}/agent.sh"))
}

fn bundled_progress_filter(label: &str) -> Option<PathBuf> {
    find_in_shim_dirs(&format!("{label}/filter.sh"))
}

fn find_in_shim_dirs(filename: &str) -> Option<PathBuf> {
    for dir in bundled_agent_shim_dir_candidates() {
        let candidate = dir.join(filename);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{AgentOutput, AgentSettingSource, AppConfig, bundled_agent_command, parse_config};
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn parses_minimal_config() {
        let config = parse_config(
            r#"
[agent]
label = "codex"
command = ["./agent.sh"]
progress_filter = ["./filter.sh"]
output = "wrapped-json"
enable_script_wrapper = false

[review]
max_patch_bytes = 4096
ignore_prefixes = ["vendor/", "dist/"]
"#,
        )
        .unwrap();

        assert_eq!(config.agent.label, "codex");
        assert_eq!(config.agent.command, vec!["./agent.sh"]);
        assert_eq!(config.agent.command_source, AgentSettingSource::Explicit);
        assert_eq!(config.agent.progress_filter, vec!["./filter.sh"]);
        assert_eq!(
            config.agent.progress_filter_source,
            AgentSettingSource::Explicit
        );
        assert_eq!(config.agent.output, AgentOutput::WrappedJson);
        assert_eq!(config.agent.workers, 4);
        assert_eq!(config.review.max_patch_bytes, 4096);
        assert_eq!(config.review.ignore_prefixes, vec!["vendor/", "dist/"]);
    }

    #[test]
    fn parses_explicit_worker_count() {
        let config = parse_config(
            r#"
[agent]
workers = 1
"#,
        )
        .unwrap();

        assert_eq!(config.agent.workers, 1);
    }

    #[test]
    fn rejects_zero_workers() {
        let error = parse_config(
            r#"
[agent]
workers = 0
"#,
        )
        .unwrap_err();

        assert!(error.contains("agent.workers must be greater than 0"));
    }

    #[test]
    fn loads_bundled_default_agent_when_config_is_missing() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let repo_root = std::env::temp_dir().join(format!(
            "anne-config-default-agent-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&repo_root).unwrap();

        let config = AppConfig::load(&repo_root).unwrap();

        assert_eq!(config.agent.label, "default");
        assert_eq!(config.agent.output, AgentOutput::WrappedJson);
        assert_eq!(config.agent.workers, 4);
        assert_eq!(config.agent.command.len(), 1);
        assert_eq!(
            config.agent.command_source,
            AgentSettingSource::BundledDefault
        );
        assert_eq!(config.agent.progress_filter.len(), 1);
        assert_eq!(
            config.agent.progress_filter_source,
            AgentSettingSource::BundledDefault
        );
        assert_eq!(
            config.agent.command[0],
            bundled_agent_command("default")
                .unwrap()
                .display()
                .to_string()
        );

        let _ = fs::remove_dir_all(repo_root);
    }

    #[test]
    fn explicit_empty_progress_filter_suppresses_bundled_filter() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let repo_root = std::env::temp_dir().join(format!(
            "anne-config-explicit-empty-filter-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(repo_root.join(".anne")).unwrap();
        fs::write(
            repo_root.join(".anne/config.toml"),
            "[agent]\nlabel = \"default\"\nprogress_filter = []\n",
        )
        .unwrap();

        let config = AppConfig::load(&repo_root).unwrap();

        assert_eq!(config.agent.label, "default");
        assert_eq!(config.agent.command.len(), 1);
        assert_eq!(
            config.agent.command_source,
            AgentSettingSource::BundledDefault
        );
        assert!(config.agent.progress_filter.is_empty());
        assert_eq!(
            config.agent.progress_filter_source,
            AgentSettingSource::Explicit
        );
        assert_eq!(config.agent.output, AgentOutput::WrappedJson);

        let _ = fs::remove_dir_all(repo_root);
    }

    #[test]
    fn explicit_text_output_survives_load_with_custom_filter() {
        let repo_root = unique_repo_root("anne-config-explicit-text-filter");
        fs::create_dir_all(repo_root.join(".anne")).unwrap();
        fs::write(
            repo_root.join(".anne/config.toml"),
            "[agent]\ncommand = [\"./agent.sh\"]\nprogress_filter = [\"./filter.sh\"]\noutput = \"text\"\n",
        )
        .unwrap();

        let config = AppConfig::load(&repo_root).unwrap();

        assert_eq!(config.agent.command, vec!["./agent.sh"]);
        assert_eq!(config.agent.command_source, AgentSettingSource::Explicit);
        assert_eq!(config.agent.progress_filter, vec!["./filter.sh"]);
        assert_eq!(
            config.agent.progress_filter_source,
            AgentSettingSource::Explicit
        );
        assert_eq!(config.agent.output, AgentOutput::Text);

        let _ = fs::remove_dir_all(repo_root);
    }

    #[test]
    fn omitted_output_with_custom_filter_stays_text() {
        let repo_root = unique_repo_root("anne-config-implicit-text-filter");
        fs::create_dir_all(repo_root.join(".anne")).unwrap();
        fs::write(
            repo_root.join(".anne/config.toml"),
            "[agent]\ncommand = [\"./agent.sh\"]\nprogress_filter = [\"./filter.sh\"]\n",
        )
        .unwrap();

        let config = AppConfig::load(&repo_root).unwrap();

        assert_eq!(config.agent.command, vec!["./agent.sh"]);
        assert_eq!(config.agent.command_source, AgentSettingSource::Explicit);
        assert_eq!(config.agent.progress_filter, vec!["./filter.sh"]);
        assert_eq!(
            config.agent.progress_filter_source,
            AgentSettingSource::Explicit
        );
        assert_eq!(config.agent.output, AgentOutput::Text);

        let _ = fs::remove_dir_all(repo_root);
    }

    fn unique_repo_root(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()))
    }
}
