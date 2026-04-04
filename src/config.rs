use std::{fs, path::Path};

#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    pub agent: AgentConfig,
    pub review: ReviewConfig,
}

impl AppConfig {
    pub fn load(repo_root: &Path) -> Result<Self, String> {
        let path = repo_root.join(".anne").join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&path)
            .map_err(|error| format!("failed reading {}: {error}", path.display()))?;
        parse_config(&contents)
            .map_err(|error| format!("failed parsing {}: {error}", path.display()))
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub label: String,
    pub command: Vec<String>,
    pub progress_filter: Vec<String>,
    pub output: AgentOutput,
    pub enable_script_wrapper: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            label: "agent".to_string(),
            command: Vec::new(),
            progress_filter: Vec::new(),
            output: AgentOutput::default(),
            enable_script_wrapper: false,
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
        agent: AgentConfig {
            label: "agent".to_string(),
            ..AgentConfig::default()
        },
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
            ("agent", "command") => config.agent.command = parse_string_array(value)?,
            ("agent", "progress_filter") => {
                config.agent.progress_filter = parse_string_array(value)?
            }
            ("agent", "output") => {
                config.agent.output = match parse_string(value)?.as_str() {
                    "text" => AgentOutput::Text,
                    "wrapped-json" => AgentOutput::WrappedJson,
                    other => return Err(format!("unsupported agent.output `{other}`")),
                };
            }
            ("agent", "enable_script_wrapper") => {
                config.agent.enable_script_wrapper = parse_bool(value)?
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

#[cfg(test)]
mod tests {
    use super::{AgentOutput, parse_config};

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
        assert_eq!(config.agent.progress_filter, vec!["./filter.sh"]);
        assert_eq!(config.agent.output, AgentOutput::WrappedJson);
        assert_eq!(config.review.max_patch_bytes, 4096);
        assert_eq!(config.review.ignore_prefixes, vec!["vendor/", "dist/"]);
    }
}
