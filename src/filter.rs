use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    process::{Command, Stdio},
};

use crate::{git, persisted_review};

pub struct RunResult {
    pub bundle_path: String,
    pub comments_loaded: usize,
    pub comments_deleted: usize,
    pub comments_remaining: usize,
    pub completed: bool,
}

pub fn run() -> Result<RunResult, String> {
    let cwd = env::current_dir().map_err(|error| format!("failed to read current dir: {error}"))?;
    let repo_root = git::repo_root(&cwd)?;
    let mut review = persisted_review::discover_review_context(&repo_root)?;

    let comments_loaded = review.comments.len();
    if comments_loaded == 0 {
        return Ok(RunResult {
            bundle_path: review.bundle_path,
            comments_loaded,
            comments_deleted: 0,
            comments_remaining: 0,
            completed: true,
        });
    }

    let stdin_is_terminal = io::stdin().is_terminal();
    let stdout_is_terminal = io::stdout().is_terminal();
    let raw_mode = RawModeGuard::activate(stdin_is_terminal)?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut comments_deleted = 0usize;
    let mut index = 0usize;

    while index < review.comments.len() {
        let comment = review.comments[index].clone();
        let patch_text = load_patch_text(&review, &comment);
        render_comment(
            &mut stdout,
            &review.bundle_path,
            index,
            review.comments.len(),
            &comment,
            patch_text.as_deref(),
            stdout_is_terminal,
        )
        .map_err(|error| format!("failed writing filter interface: {error}"))?;

        match read_action(&mut stdin)? {
            Action::Next => {
                if raw_mode.is_active() {
                    writeln!(stdout).map_err(|error| format!("failed writing newline: {error}"))?;
                }
                index += 1;
            }
            Action::Delete => {
                if raw_mode.is_active() {
                    writeln!(stdout).map_err(|error| format!("failed writing newline: {error}"))?;
                }
                review.comments.remove(index);
                review.persist_comments()?;
                comments_deleted += 1;
            }
            Action::Quit => {
                if raw_mode.is_active() {
                    writeln!(stdout).map_err(|error| format!("failed writing newline: {error}"))?;
                }
                return Ok(RunResult {
                    bundle_path: review.bundle_path,
                    comments_loaded,
                    comments_deleted,
                    comments_remaining: review.comments.len(),
                    completed: false,
                });
            }
        }
    }

    Ok(RunResult {
        bundle_path: review.bundle_path,
        comments_loaded,
        comments_deleted,
        comments_remaining: review.comments.len(),
        completed: true,
    })
}

fn load_patch_text(
    review: &persisted_review::ReviewContext,
    comment: &persisted_review::SourceComment,
) -> Option<String> {
    let patch_file = comment.patch_file.as_deref()?;
    let path = review.bundle_root.join(patch_file);
    Some(match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => format!("Patch context unavailable from {patch_file}: {error}"),
    })
}

fn render_comment(
    stdout: &mut impl Write,
    bundle_path: &str,
    index: usize,
    total: usize,
    comment: &persisted_review::SourceComment,
    patch_text: Option<&str>,
    clear_screen: bool,
) -> io::Result<()> {
    if clear_screen {
        write!(stdout, "\x1b[2J\x1b[H")?;
    }

    writeln!(stdout, "anne filter")?;
    writeln!(stdout)?;
    writeln!(stdout, "Bundle: {bundle_path}")?;
    writeln!(stdout, "Comment: {}/{}", index + 1, total)?;
    writeln!(stdout, "Keys: n keep next, d delete, q quit")?;
    writeln!(stdout)?;
    writeln!(
        stdout,
        "{} {} {} {}",
        comment.id,
        comment.severity,
        comment.path,
        comment.anchor()
    )?;
    writeln!(stdout, "{}", comment.title)?;
    writeln!(stdout)?;
    writeln!(stdout, "{}", comment.body.trim())?;

    if let Some(hunk_header) = &comment.hunk_header {
        writeln!(stdout)?;
        writeln!(stdout, "Hunk: {hunk_header}")?;
    }

    if let Some(confidence) = &comment.confidence {
        writeln!(stdout, "Confidence: {confidence}")?;
    }

    match (comment.patch_file.as_deref(), patch_text) {
        (Some(patch_file), Some(patch_text)) => {
            writeln!(stdout)?;
            writeln!(stdout, "Patch: {patch_file}")?;
            writeln!(stdout)?;
            writeln!(stdout, "{patch_text}")?;
        }
        (Some(patch_file), None) => {
            writeln!(stdout)?;
            writeln!(stdout, "Patch: {patch_file}")?;
            writeln!(stdout, "Patch context unavailable.")?;
        }
        (None, _) => {
            writeln!(stdout)?;
            writeln!(stdout, "Patch context unavailable.")?;
        }
    }

    write!(stdout, "Action [n/d/q]: ")?;
    stdout.flush()
}

fn read_action(stdin: &mut impl Read) -> Result<Action, String> {
    let mut buffer = [0u8; 1];
    loop {
        let read = stdin
            .read(&mut buffer)
            .map_err(|error| format!("failed reading filter input: {error}"))?;
        if read == 0 {
            return Err("filter input ended before a selection was made".to_string());
        }

        match buffer[0] {
            b'n' | b'N' => return Ok(Action::Next),
            b'd' | b'D' => return Ok(Action::Delete),
            b'q' | b'Q' | 3 => return Ok(Action::Quit),
            b'\n' | b'\r' => continue,
            _ => continue,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Next,
    Delete,
    Quit,
}

struct RawModeGuard {
    original_state: Option<String>,
}

impl RawModeGuard {
    fn activate(enabled: bool) -> Result<Self, String> {
        if !enabled {
            return Ok(Self {
                original_state: None,
            });
        }

        let output = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()
            .map_err(|error| format!("failed capturing terminal state with stty: {error}"))?;
        if !output.status.success() {
            return Err("stty -g failed while preparing filter input mode".to_string());
        }

        let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let status = Command::new("stty")
            .args(["-icanon", "-echo", "min", "1", "time", "0"])
            .stdin(Stdio::inherit())
            .status()
            .map_err(|error| format!("failed enabling raw terminal mode with stty: {error}"))?;
        if !status.success() {
            return Err("stty failed enabling raw terminal mode for filter".to_string());
        }

        Ok(Self {
            original_state: Some(state),
        })
    }

    fn is_active(&self) -> bool {
        self.original_state.is_some()
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if let Some(state) = &self.original_state {
            let _ = Command::new("stty")
                .arg(state)
                .stdin(Stdio::inherit())
                .status();
        }
    }
}
