use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use crate::config::AgentConfig;

#[derive(Debug, Clone)]
pub struct AgentResult {
    pub assistant_text: String,
    pub raw_stdout: String,
    pub stderr_lines: Vec<String>,
}

pub fn run(repo_root: &Path, config: &AgentConfig, prompt: &str) -> Result<AgentResult, String> {
    if config.command.is_empty() {
        return Err(
            "agent.command is not configured; set it in .anne/config.toml before running review"
                .to_string(),
        );
    }

    let (program, args) = config
        .command
        .split_first()
        .ok_or_else(|| "agent.command is empty".to_string())?;

    let mut child = Command::new(program)
        .args(args)
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed spawning agent `{program}`: {error}"))?;

    let filter = if config.progress_filter.is_empty() {
        None
    } else {
        let (filter_program, filter_args) = config
            .progress_filter
            .split_first()
            .ok_or_else(|| "agent.progress_filter is empty".to_string())?;
        Some(
            Command::new(filter_program)
                .args(filter_args)
                .current_dir(repo_root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| {
                    format!("failed spawning progress filter `{filter_program}`: {error}")
                })?,
        )
    };

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .map_err(|error| format!("failed writing prompt to agent stdin: {error}"))?;
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
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "agent stdout was not captured".to_string())?;
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
        let stdout = filter_child
            .stdout
            .take()
            .ok_or_else(|| "progress filter stdout was not captured".to_string())?;
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

    let status = child
        .wait()
        .map_err(|error| format!("failed waiting for agent process: {error}"))?;
    forward_thread
        .join()
        .map_err(|_| "agent stdout forwarding thread panicked".to_string())??;

    let raw_stdout = raw_stdout
        .lock()
        .map_err(|_| "failed locking raw stdout buffer".to_string())?
        .clone();

    let assistant_text = if let Some(mut filter_child) = filter {
        let text = filter_stdout
            .ok_or_else(|| "progress filter stdout thread was not started".to_string())?
            .join()
            .map_err(|_| "progress filter stdout thread panicked".to_string())??;
        let filter_status = filter_child
            .wait()
            .map_err(|error| format!("failed waiting for progress filter: {error}"))?;
        if !filter_status.success() {
            let stderr_lines = stderr_lines
                .lock()
                .map_err(|_| "failed locking stderr lines".to_string())?
                .clone();
            return Err(format!(
                "progress filter exited with status {}: {}",
                filter_status.code().unwrap_or(-1),
                stderr_lines.join("; ")
            ));
        }
        text
    } else {
        raw_stdout.clone()
    };

    for handle in stderr_threads {
        handle
            .join()
            .map_err(|_| "stderr reader thread panicked".to_string())??;
    }

    let stderr_lines = stderr_lines
        .lock()
        .map_err(|_| "failed locking stderr lines".to_string())?
        .clone();

    if !status.success() {
        return Err(format!(
            "agent exited with status {}: {}",
            status.code().unwrap_or(-1),
            stderr_lines.join("; ")
        ));
    }

    Ok(AgentResult {
        assistant_text,
        raw_stdout,
        stderr_lines,
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
