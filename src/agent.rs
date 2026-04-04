use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
};

use crate::config::AgentConfig;

#[derive(Debug, Clone)]
pub struct AgentResult {
    pub assistant_text: String,
    pub raw_stdout: String,
    pub stderr_lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AgentFailure {
    pub message: String,
    pub raw_stdout: String,
    pub stderr_lines: Vec<String>,
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
    if config.command.is_empty() {
        return Err(AgentFailure {
            message:
                "agent.command is not configured; set it in .anne/config.toml before running Anne"
                    .to_string(),
            raw_stdout: String::new(),
            stderr_lines: Vec::new(),
        });
    }

    let (program, args) = config.command.split_first().ok_or_else(|| AgentFailure {
        message: "agent.command is empty".to_string(),
        raw_stdout: String::new(),
        stderr_lines: Vec::new(),
    })?;

    let mut child = Command::new(program)
        .args(args)
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AgentFailure {
            message: format!("failed spawning agent `{program}`: {error}"),
            raw_stdout: String::new(),
            stderr_lines: Vec::new(),
        })?;

    let filter = if config.progress_filter.is_empty() {
        None
    } else {
        let (filter_program, filter_args) =
            config
                .progress_filter
                .split_first()
                .ok_or_else(|| AgentFailure {
                    message: "agent.progress_filter is empty".to_string(),
                    raw_stdout: String::new(),
                    stderr_lines: Vec::new(),
                })?;
        Some(
            Command::new(filter_program)
                .args(filter_args)
                .current_dir(repo_root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| AgentFailure {
                    message: format!("failed spawning progress filter `{filter_program}`: {error}"),
                    raw_stdout: String::new(),
                    stderr_lines: Vec::new(),
                })?,
        )
    };

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .map_err(|error| AgentFailure {
                message: format!("failed writing prompt to agent stdin: {error}"),
                raw_stdout: String::new(),
                stderr_lines: Vec::new(),
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
        let stdout = child.stdout.take().ok_or_else(|| AgentFailure {
            message: "agent stdout was not captured".to_string(),
            raw_stdout: String::new(),
            stderr_lines: Vec::new(),
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
        let stdout = filter_child.stdout.take().ok_or_else(|| AgentFailure {
            message: "progress filter stdout was not captured".to_string(),
            raw_stdout: String::new(),
            stderr_lines: Vec::new(),
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

    let status = child.wait().map_err(|error| AgentFailure {
        message: format!("failed waiting for agent process: {error}"),
        raw_stdout: String::new(),
        stderr_lines: Vec::new(),
    })?;
    forward_thread
        .join()
        .map_err(|_| AgentFailure {
            message: "agent stdout forwarding thread panicked".to_string(),
            raw_stdout: String::new(),
            stderr_lines: Vec::new(),
        })?
        .map_err(|message| AgentFailure {
            message,
            raw_stdout: String::new(),
            stderr_lines: Vec::new(),
        })?;

    let raw_stdout = raw_stdout
        .lock()
        .map_err(|_| AgentFailure {
            message: "failed locking raw stdout buffer".to_string(),
            raw_stdout: String::new(),
            stderr_lines: Vec::new(),
        })?
        .clone();

    let assistant_text = if let Some(mut filter_child) = filter {
        let text = filter_stdout
            .ok_or_else(|| AgentFailure {
                message: "progress filter stdout thread was not started".to_string(),
                raw_stdout: raw_stdout.clone(),
                stderr_lines: Vec::new(),
            })?
            .join()
            .map_err(|_| AgentFailure {
                message: "progress filter stdout thread panicked".to_string(),
                raw_stdout: raw_stdout.clone(),
                stderr_lines: Vec::new(),
            })?
            .map_err(|message| AgentFailure {
                message,
                raw_stdout: raw_stdout.clone(),
                stderr_lines: Vec::new(),
            })?;
        let filter_status = filter_child.wait().map_err(|error| AgentFailure {
            message: format!("failed waiting for progress filter: {error}"),
            raw_stdout: raw_stdout.clone(),
            stderr_lines: Vec::new(),
        })?;
        if !filter_status.success() {
            let stderr_lines = stderr_lines
                .lock()
                .map_err(|_| AgentFailure {
                    message: "failed locking stderr lines".to_string(),
                    raw_stdout: raw_stdout.clone(),
                    stderr_lines: Vec::new(),
                })?
                .clone();
            return Err(AgentFailure {
                message: format!(
                    "progress filter exited with status {}: {}",
                    filter_status.code().unwrap_or(-1),
                    stderr_lines.join("; ")
                ),
                raw_stdout,
                stderr_lines,
            });
        }
        text
    } else {
        raw_stdout.clone()
    };

    for handle in stderr_threads {
        handle
            .join()
            .map_err(|_| AgentFailure {
                message: "stderr reader thread panicked".to_string(),
                raw_stdout: raw_stdout.clone(),
                stderr_lines: Vec::new(),
            })?
            .map_err(|message| AgentFailure {
                message,
                raw_stdout: raw_stdout.clone(),
                stderr_lines: Vec::new(),
            })?;
    }

    let stderr_lines = stderr_lines
        .lock()
        .map_err(|_| AgentFailure {
            message: "failed locking stderr lines".to_string(),
            raw_stdout: raw_stdout.clone(),
            stderr_lines: Vec::new(),
        })?
        .clone();

    if !status.success() {
        return Err(AgentFailure {
            message: format!(
                "agent exited with status {}: {}",
                status.code().unwrap_or(-1),
                stderr_lines.join("; ")
            ),
            raw_stdout,
            stderr_lines,
        });
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
