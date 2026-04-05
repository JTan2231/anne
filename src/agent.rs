use std::{
    collections::BTreeMap,
    collections::VecDeque,
    env,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
};

use crate::{
    config::{AgentConfig, AgentOutput, AgentSettingSource},
    json::{self, JsonValue},
};

#[derive(Debug, Clone)]
pub struct AgentResult {
    pub assistant_text: String,
    pub raw_stdout: String,
    pub stderr_lines: Vec<String>,
    pub runtime: AgentInvocation,
}

#[derive(Debug, Clone)]
pub struct AgentFailure {
    pub message: String,
    pub raw_stdout: String,
    pub stderr_lines: Vec<String>,
    pub runtime: AgentInvocation,
}

#[derive(Debug, Clone)]
pub struct AgentInvocation {
    pub effective_output: AgentOutput,
    pub assistant_text_source: AssistantTextSource,
    pub bundled_filter_requested: bool,
    pub progress_filter_ran: bool,
    pub progress_filter_skip_reason: Option<String>,
    pub runtime_note: Option<String>,
}

impl AgentInvocation {
    fn from_config(config: &AgentConfig) -> Self {
        let filter_ran = !config.progress_filter.is_empty()
            && config.progress_filter_source != AgentSettingSource::BundledDefault;
        Self {
            effective_output: config.output,
            assistant_text_source: assistant_text_source(config.output, filter_ran),
            bundled_filter_requested: config.progress_filter_source
                == AgentSettingSource::BundledDefault
                && !config.progress_filter.is_empty(),
            progress_filter_ran: false,
            progress_filter_skip_reason: None,
            runtime_note: None,
        }
    }

    pub fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "assistant_text_source".to_string(),
            JsonValue::string(self.assistant_text_source.as_str()),
        );
        object.insert(
            "bundled_filter_requested".to_string(),
            JsonValue::Bool(self.bundled_filter_requested),
        );
        object.insert(
            "effective_output".to_string(),
            JsonValue::string(self.effective_output.as_str()),
        );
        object.insert(
            "progress_filter_ran".to_string(),
            JsonValue::Bool(self.progress_filter_ran),
        );
        object.insert(
            "progress_filter_skip_reason".to_string(),
            optional_json_string(&self.progress_filter_skip_reason),
        );
        object.insert(
            "runtime_note".to_string(),
            optional_json_string(&self.runtime_note),
        );
        JsonValue::Object(object)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantTextSource {
    RawStdout,
    ProgressFilter,
    WrappedJsonDecoder,
}

impl AssistantTextSource {
    pub fn as_str(self) -> &'static str {
        match self {
            AssistantTextSource::RawStdout => "raw-stdout",
            AssistantTextSource::ProgressFilter => "progress-filter",
            AssistantTextSource::WrappedJsonDecoder => "wrapped-json-decoder",
        }
    }
}

pub fn run_bounded<Job, Output, OnStart, Worker, OnResult>(
    workers: usize,
    jobs: Vec<Job>,
    mut on_start: OnStart,
    worker: Worker,
    mut on_result: OnResult,
) -> Result<(), String>
where
    Job: Send,
    Output: Send,
    OnStart: FnMut(usize, &Job) -> Result<(), String>,
    Worker: Fn(usize, Job) -> Output + Sync,
    OnResult: FnMut(usize, Output) -> Result<(), String>,
{
    let workers = workers.max(1);
    let mut pending = jobs.into_iter().enumerate().collect::<VecDeque<_>>();
    let (sender, receiver) = mpsc::channel::<(usize, Output)>();
    let mut active = 0usize;
    let mut error = None;

    thread::scope(|scope| {
        let mut handles = Vec::new();

        let mut dispatch = |pending: &mut VecDeque<(usize, Job)>,
                            active: &mut usize,
                            error: &mut Option<String>| {
            while error.is_none() && *active < workers {
                let Some((index, job)) = pending.pop_front() else {
                    break;
                };
                if let Err(dispatch_error) = on_start(index, &job) {
                    *error = Some(dispatch_error);
                    pending.clear();
                    break;
                }

                *active += 1;
                let sender = sender.clone();
                let worker = &worker;
                handles.push(scope.spawn(move || {
                    let output = worker(index, job);
                    let _ = sender.send((index, output));
                }));
            }
        };

        dispatch(&mut pending, &mut active, &mut error);

        while active > 0 {
            match receiver.recv() {
                Ok((index, output)) => {
                    active -= 1;
                    if error.is_none()
                        && let Err(result_error) = on_result(index, output)
                    {
                        error = Some(result_error);
                        pending.clear();
                    }

                    if error.is_none() {
                        dispatch(&mut pending, &mut active, &mut error);
                    }
                }
                Err(_) => {
                    if error.is_none() {
                        error = Some("bounded worker channel closed unexpectedly".to_string());
                    }
                    break;
                }
            }
        }

        for handle in handles {
            if handle.join().is_err() && error.is_none() {
                error = Some("bounded worker thread panicked".to_string());
            }
        }
    });

    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub fn run_captured(
    repo_root: &Path,
    config: &AgentConfig,
    prompt: &str,
) -> Result<AgentResult, AgentFailure> {
    let mut runtime = AgentInvocation::from_config(config);

    if config.command.is_empty() {
        return Err(failure(
            "agent.command is not configured; set it in .anne/config.toml before running Anne",
            String::new(),
            Vec::new(),
            runtime,
        ));
    }

    let (program, args) = config.command.split_first().ok_or_else(|| {
        failure(
            "agent.command is empty",
            String::new(),
            Vec::new(),
            runtime.clone(),
        )
    })?;

    let use_progress_filter = if config.progress_filter.is_empty() {
        false
    } else if runtime.bundled_filter_requested {
        if command_exists_on_path("jq") {
            true
        } else {
            runtime.progress_filter_skip_reason = Some("jq was not found on PATH".to_string());
            runtime.runtime_note = Some(
                "Bundled default progress filter skipped because `jq` was not found on PATH; Anne decoded wrapped-json output directly."
                    .to_string(),
            );
            if let Some(note) = runtime.runtime_note.as_deref() {
                eprintln!("{note}");
            }
            false
        }
    } else {
        true
    };
    runtime.assistant_text_source = assistant_text_source(config.output, use_progress_filter);

    let mut child = Command::new(program)
        .args(args)
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            failure(
                format!("failed spawning agent `{program}`: {error}"),
                String::new(),
                Vec::new(),
                runtime.clone(),
            )
        })?;

    let filter = if !use_progress_filter {
        None
    } else {
        let (filter_program, filter_args) =
            config.progress_filter.split_first().ok_or_else(|| {
                failure(
                    "agent.progress_filter is empty",
                    String::new(),
                    Vec::new(),
                    runtime.clone(),
                )
            })?;
        Some(
            Command::new(filter_program)
                .args(filter_args)
                .current_dir(repo_root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| {
                    failure(
                        format!("failed spawning progress filter `{filter_program}`: {error}"),
                        String::new(),
                        Vec::new(),
                        runtime.clone(),
                    )
                })?,
        )
    };
    runtime.progress_filter_ran = filter.is_some();

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).map_err(|error| {
            failure(
                format!("failed writing prompt to agent stdin: {error}"),
                String::new(),
                Vec::new(),
                runtime.clone(),
            )
        })?;
    }

    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let mut stderr_threads = Vec::new();

    if let Some(stderr) = child.stderr.take() {
        stderr_threads.push(read_stderr(stderr, stderr_lines.clone()));
    }

    let mut filter = filter;
    if let Some(filter_child) = filter.as_mut() {
        if let Some(stderr) = filter_child.stderr.take() {
            stderr_threads.push(read_stderr(stderr, stderr_lines.clone()));
        }
    }

    let raw_stdout = Arc::new(Mutex::new(String::new()));
    let forward_thread = {
        let stdout = child.stdout.take().ok_or_else(|| {
            failure(
                "agent stdout was not captured",
                String::new(),
                Vec::new(),
                runtime.clone(),
            )
        })?;
        let raw_stdout = raw_stdout.clone();
        let mut filter_stdin = filter.as_mut().and_then(|child| child.stdin.take());

        thread::spawn(move || -> Result<(), String> {
            let mut reader = BufReader::new(stdout);
            let mut buffer = [0_u8; 8192];
            loop {
                let read = reader
                    .read(&mut buffer)
                    .map_err(|error| format!("failed reading agent stdout: {error}"))?;
                if read == 0 {
                    break;
                }

                {
                    let mut raw = raw_stdout
                        .lock()
                        .map_err(|_| "failed locking raw stdout buffer".to_string())?;
                    raw.push_str(&String::from_utf8_lossy(&buffer[..read]));
                }

                if let Some(stdin) = filter_stdin.as_mut() {
                    stdin.write_all(&buffer[..read]).map_err(|error| {
                        format!("failed writing to progress filter stdin: {error}")
                    })?;
                }
            }

            drop(filter_stdin);
            Ok(())
        })
    };

    let filter_stdout = if let Some(filter_child) = filter.as_mut() {
        let stdout = filter_child.stdout.take().ok_or_else(|| {
            failure(
                "progress filter stdout was not captured",
                String::new(),
                Vec::new(),
                runtime.clone(),
            )
        })?;
        Some(thread::spawn(move || -> Result<String, String> {
            let mut reader = BufReader::new(stdout);
            let mut text = String::new();
            reader
                .read_to_string(&mut text)
                .map_err(|error| format!("failed reading progress filter stdout: {error}"))?;
            Ok(text)
        }))
    } else {
        None
    };

    let status = child.wait().map_err(|error| {
        failure(
            format!("failed waiting for agent process: {error}"),
            String::new(),
            Vec::new(),
            runtime.clone(),
        )
    })?;
    forward_thread
        .join()
        .map_err(|_| {
            failure(
                "agent stdout forwarding thread panicked",
                String::new(),
                Vec::new(),
                runtime.clone(),
            )
        })?
        .map_err(|message| failure(message, String::new(), Vec::new(), runtime.clone()))?;

    let raw_stdout = raw_stdout
        .lock()
        .map_err(|_| {
            failure(
                "failed locking raw stdout buffer",
                String::new(),
                Vec::new(),
                runtime.clone(),
            )
        })?
        .clone();

    let assistant_text = if let Some(mut filter_child) = filter {
        let text = filter_stdout
            .ok_or_else(|| {
                failure(
                    "progress filter stdout thread was not started",
                    raw_stdout.clone(),
                    Vec::new(),
                    runtime.clone(),
                )
            })?
            .join()
            .map_err(|_| {
                failure(
                    "progress filter stdout thread panicked",
                    raw_stdout.clone(),
                    Vec::new(),
                    runtime.clone(),
                )
            })?
            .map_err(|message| failure(message, raw_stdout.clone(), Vec::new(), runtime.clone()))?;
        let filter_status = filter_child.wait().map_err(|error| {
            failure(
                format!("failed waiting for progress filter: {error}"),
                raw_stdout.clone(),
                Vec::new(),
                runtime.clone(),
            )
        })?;
        if !filter_status.success() {
            let stderr_lines = stderr_lines
                .lock()
                .map_err(|_| {
                    failure(
                        "failed locking stderr lines",
                        raw_stdout.clone(),
                        Vec::new(),
                        runtime.clone(),
                    )
                })?
                .clone();
            return Err(failure(
                format!(
                    "progress filter exited with status {}: {}",
                    filter_status.code().unwrap_or(-1),
                    stderr_lines.join("; ")
                ),
                raw_stdout,
                stderr_lines,
                runtime.clone(),
            ));
        }
        text
    } else if config.output == AgentOutput::WrappedJson {
        decode_wrapped_json_assistant_text(&raw_stdout).map_err(|error| {
            failure(
                format!(
                    "failed recovering final assistant message from wrapped-json stdout: {error}"
                ),
                raw_stdout.clone(),
                Vec::new(),
                runtime.clone(),
            )
        })?
    } else {
        raw_stdout.clone()
    };

    for handle in stderr_threads {
        handle
            .join()
            .map_err(|_| {
                failure(
                    "stderr reader thread panicked",
                    raw_stdout.clone(),
                    Vec::new(),
                    runtime.clone(),
                )
            })?
            .map_err(|message| failure(message, raw_stdout.clone(), Vec::new(), runtime.clone()))?;
    }

    let stderr_lines = stderr_lines
        .lock()
        .map_err(|_| {
            failure(
                "failed locking stderr lines",
                raw_stdout.clone(),
                Vec::new(),
                runtime.clone(),
            )
        })?
        .clone();

    if !status.success() {
        return Err(failure(
            format!(
                "agent exited with status {}: {}",
                status.code().unwrap_or(-1),
                stderr_lines.join("; ")
            ),
            raw_stdout,
            stderr_lines,
            runtime.clone(),
        ));
    }

    Ok(AgentResult {
        assistant_text,
        raw_stdout,
        stderr_lines,
        runtime,
    })
}

fn read_stderr(
    stream: impl Read + Send + 'static,
    stderr_lines: Arc<Mutex<Vec<String>>>,
) -> thread::JoinHandle<Result<(), String>> {
    thread::spawn(move || -> Result<(), String> {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = line.map_err(|error| format!("failed reading stderr: {error}"))?;
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            eprintln!("{trimmed}");
            stderr_lines
                .lock()
                .map_err(|_| "failed locking stderr lines".to_string())?
                .push(trimmed);
        }
        Ok(())
    })
}

fn failure(
    message: impl Into<String>,
    raw_stdout: String,
    stderr_lines: Vec<String>,
    runtime: AgentInvocation,
) -> AgentFailure {
    AgentFailure {
        message: message.into(),
        raw_stdout,
        stderr_lines,
        runtime,
    }
}

fn assistant_text_source(output: AgentOutput, filter_ran: bool) -> AssistantTextSource {
    if filter_ran {
        AssistantTextSource::ProgressFilter
    } else if output == AgentOutput::WrappedJson {
        AssistantTextSource::WrappedJsonDecoder
    } else {
        AssistantTextSource::RawStdout
    }
}

fn command_exists_on_path(command: &str) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(command).is_file();
    }

    let Some(path) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&path)
        .map(|dir| dir.join(command))
        .any(|candidate| candidate.is_file())
}

fn decode_wrapped_json_assistant_text(raw_stdout: &str) -> Result<String, String> {
    let mut saw_event = false;
    let mut final_text = None;

    for (index, line) in raw_stdout.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        saw_event = true;

        let event = json::parse(trimmed)
            .map_err(|error| format!("stdout line {} was not valid JSON: {error}", index + 1))?;
        let object = event
            .as_object()
            .ok_or_else(|| format!("stdout line {} was not a JSON object", index + 1))?;

        let event_type = object.get("type").and_then(JsonValue::as_str);
        if !matches!(event_type, Some("item.started" | "item.completed")) {
            continue;
        }

        let Some(item) = object.get("item").and_then(JsonValue::as_object) else {
            continue;
        };
        if item.get("type").and_then(JsonValue::as_str) != Some("agent_message") {
            continue;
        }

        let text = item
            .get("text")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                format!(
                    "stdout line {} contained `agent_message` without a string `item.text`",
                    index + 1
                )
            })?;
        final_text = Some(text.to_string());
    }

    if !saw_event {
        return Err("stdout was empty".to_string());
    }

    final_text.ok_or_else(|| "stdout did not contain a final `agent_message` event".to_string())
}

fn optional_json_string(value: &Option<String>) -> JsonValue {
    value.clone().map_or(JsonValue::Null, JsonValue::string)
}

#[cfg(test)]
mod tests {
    use super::decode_wrapped_json_assistant_text;

    #[test]
    fn recovers_final_agent_message_from_wrapped_json_stdout() {
        let text = decode_wrapped_json_assistant_text(
            "{\"type\":\"item.started\",\"item\":{\"type\":\"reasoning\",\"text\":\"thinking\"}}\n\
             {\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"[]\"}}\n",
        )
        .unwrap();

        assert_eq!(text, "[]");
    }

    #[test]
    fn wrapped_json_decoder_requires_agent_message_event() {
        let error = decode_wrapped_json_assistant_text(
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"reasoning\",\"text\":\"thinking\"}}\n",
        )
        .unwrap_err();

        assert!(error.contains("agent_message"));
    }
}
