use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    process::{Command, Stdio},
};

use crate::{git, persisted_review};

const DEFAULT_TERMINAL_ROWS: usize = 24;
const DEFAULT_TERMINAL_COLS: usize = 80;
const MAX_HEADER_COLS: usize = 100;
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_ACCENT: &str = "\x1b[1;36m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_WARNING: &str = "\x1b[1;33m";
const ANSI_ERROR: &str = "\x1b[1;31m";
const ANSI_PATCH_META: &str = "\x1b[34m";
const ANSI_PATCH_ADD: &str = "\x1b[32m";
const ANSI_PATCH_DEL: &str = "\x1b[31m";
const ANSI_PATCH_HUNK: &str = "\x1b[1;36m";

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
        let patch_lines = patch_lines(&comment, patch_text.as_deref());
        let mut scroll = ScrollState::default();
        let mut rendered_plain = false;
        let mut viewport = ViewportMetrics::default();

        loop {
            if stdout_is_terminal {
                viewport = render_terminal_comment(
                    &mut stdout,
                    &review.bundle_path,
                    index,
                    review.comments.len(),
                    &comment,
                    &patch_lines,
                    &mut scroll,
                    current_terminal_size(),
                )
                .map_err(|error| format!("failed writing filter interface: {error}"))?;
            } else if !rendered_plain {
                render_plain_comment(
                    &mut stdout,
                    &review.bundle_path,
                    index,
                    review.comments.len(),
                    &comment,
                    patch_text.as_deref(),
                    false,
                )
                .map_err(|error| format!("failed writing filter interface: {error}"))?;
                rendered_plain = true;
            }

            match read_action(&mut stdin)? {
                Action::Next => {
                    if raw_mode.is_active() || stdout_is_terminal {
                        writeln!(stdout)
                            .map_err(|error| format!("failed writing newline: {error}"))?;
                    }
                    index += 1;
                    break;
                }
                Action::Delete => {
                    if raw_mode.is_active() || stdout_is_terminal {
                        writeln!(stdout)
                            .map_err(|error| format!("failed writing newline: {error}"))?;
                    }
                    review.comments.remove(index);
                    review.persist_comments()?;
                    comments_deleted += 1;
                    break;
                }
                Action::Quit => {
                    if raw_mode.is_active() || stdout_is_terminal {
                        writeln!(stdout)
                            .map_err(|error| format!("failed writing newline: {error}"))?;
                    }
                    return Ok(RunResult {
                        bundle_path: review.bundle_path,
                        comments_loaded,
                        comments_deleted,
                        comments_remaining: review.comments.len(),
                        completed: false,
                    });
                }
                Action::Navigate(navigation) => {
                    if stdout_is_terminal {
                        scroll.apply(navigation, viewport);
                    }
                }
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

fn render_plain_comment(
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

fn patch_lines<'a>(
    comment: &'a persisted_review::SourceComment,
    patch_text: Option<&'a str>,
) -> Vec<&'a str> {
    match (comment.patch_file.as_deref(), patch_text) {
        (Some(_), Some(text)) => {
            let lines = text.lines().collect::<Vec<_>>();
            if lines.is_empty() { vec![""] } else { lines }
        }
        _ => vec!["Patch context unavailable."],
    }
}

fn render_terminal_comment(
    stdout: &mut impl Write,
    bundle_path: &str,
    index: usize,
    total: usize,
    comment: &persisted_review::SourceComment,
    patch_lines: &[&str],
    scroll: &mut ScrollState,
    size: TerminalSize,
) -> io::Result<ViewportMetrics> {
    let rows = size.rows.max(8);
    let cols = size.cols.max(20);
    let text_cols = cols.min(MAX_HEADER_COLS);
    let header_lines = terminal_header_lines(bundle_path, index, total, comment, text_cols);
    let content_lines = terminal_content_lines(comment, patch_lines, text_cols);
    let content_rows = rows.saturating_sub(header_lines.len() + 1).max(1);
    let max_row_offset = content_lines.len().saturating_sub(content_rows);
    let max_col_offset = patch_lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
        .saturating_sub(cols);

    scroll.row_offset = scroll.row_offset.min(max_row_offset);
    scroll.col_offset = scroll.col_offset.min(max_col_offset);

    let first_line = scroll.row_offset + 1;
    let last_line = (scroll.row_offset + content_rows).min(content_lines.len());
    let patch_label = comment.patch_file.as_deref().unwrap_or("unavailable");
    let patch_status = truncate_line(
        &format!(
            "Diff: {patch_label}  {} lines  View {first_line}-{last_line}/{}  Col {}",
            patch_lines.len(),
            content_lines.len(),
            scroll.col_offset + 1
        ),
        text_cols,
    );

    write!(stdout, "\x1b[2J\x1b[H")?;
    for (line_index, line) in header_lines.iter().enumerate() {
        writeln!(
            stdout,
            "{}",
            render_header_line(line, line_index, comment.severity.as_str())
        )?;
    }
    writeln!(stdout, "{}", paint(&patch_status, ANSI_DIM))?;

    let visible_lines = content_lines
        .iter()
        .skip(scroll.row_offset)
        .take(content_rows)
        .collect::<Vec<_>>();
    for line in &visible_lines {
        writeln!(
            stdout,
            "{}",
            render_content_line(line, scroll.col_offset, cols)
        )?;
    }
    for _ in visible_lines.len()..content_rows {
        writeln!(stdout)?;
    }

    stdout.flush()?;
    Ok(ViewportMetrics {
        content_rows,
        max_row_offset,
        max_col_offset,
    })
}

fn terminal_header_lines(
    bundle_path: &str,
    index: usize,
    total: usize,
    comment: &persisted_review::SourceComment,
    cols: usize,
) -> Vec<String> {
    vec![
        truncate_line("anne filter", cols),
        truncate_line(&format!("Bundle: {bundle_path}"), cols),
        truncate_line(&format!("Comment: {}/{}", index + 1, total), cols),
        truncate_line(
            "Keys: arrows scroll, PgUp/PgDn page, n keep, d delete, q quit",
            cols,
        ),
        truncate_line(
            &format!(
                "{} {} {} {}",
                comment.id,
                comment.severity,
                comment.path,
                comment.anchor()
            ),
            cols,
        ),
    ]
}

fn terminal_content_lines<'a>(
    comment: &'a persisted_review::SourceComment,
    patch_lines: &[&'a str],
    cols: usize,
) -> Vec<ContentLine<'a>> {
    let cols = cols.min(MAX_HEADER_COLS);
    let mut lines = vec![ContentLine::Accent(truncate_line("Finding", cols))];
    lines.extend(
        wrap_prefixed_block("Title: ", &comment.title, cols)
            .into_iter()
            .map(ContentLine::Plain),
    );
    lines.push(ContentLine::Blank);
    lines.extend(
        wrap_prefixed_block("Comment: ", &comment.body, cols)
            .into_iter()
            .map(ContentLine::Plain),
    );

    if let Some(hunk_header) = &comment.hunk_header {
        lines.push(ContentLine::Blank);
        lines.extend(
            wrap_prefixed_block("Hunk: ", hunk_header, cols)
                .into_iter()
                .map(ContentLine::Meta),
        );
    }

    if let Some(confidence) = &comment.confidence {
        lines.extend(
            wrap_prefixed_block("Confidence: ", confidence, cols)
                .into_iter()
                .map(ContentLine::Meta),
        );
    }

    let patch_label = comment.patch_file.as_deref().unwrap_or("unavailable");
    lines.push(ContentLine::Blank);
    lines.push(ContentLine::Accent(truncate_line(
        &format!("Diff: {patch_label}"),
        cols,
    )));
    lines.extend(patch_lines.iter().copied().map(ContentLine::Patch));
    lines
}

fn render_header_line(text: &str, line_index: usize, severity: &str) -> String {
    let style = match line_index {
        0 => ANSI_ACCENT,
        1 | 3 => ANSI_DIM,
        2 => ANSI_BOLD,
        4 => severity_style(severity),
        _ => "",
    };
    paint(text, style)
}

fn render_content_line(line: &ContentLine<'_>, col_offset: usize, width: usize) -> String {
    match line {
        ContentLine::Plain(text) => text.clone(),
        ContentLine::Meta(text) => paint(text, ANSI_DIM),
        ContentLine::Accent(text) => paint(text, ANSI_ACCENT),
        ContentLine::Blank => String::new(),
        ContentLine::Patch(text) => render_patch_line(text, col_offset, width),
    }
}

fn render_patch_line(text: &str, col_offset: usize, width: usize) -> String {
    let visible = visible_slice(text, col_offset, width);
    paint(&visible, patch_line_style(text))
}

fn severity_style(severity: &str) -> &'static str {
    match severity {
        "error" => ANSI_ERROR,
        "warning" => ANSI_WARNING,
        _ => ANSI_BOLD,
    }
}

fn patch_line_style(text: &str) -> &'static str {
    if text.starts_with("@@") {
        ANSI_PATCH_HUNK
    } else if text.starts_with('+') && !text.starts_with("+++") {
        ANSI_PATCH_ADD
    } else if text.starts_with('-') && !text.starts_with("---") {
        ANSI_PATCH_DEL
    } else if text.starts_with("diff --git")
        || text.starts_with("index ")
        || text.starts_with("--- ")
        || text.starts_with("+++ ")
        || text.starts_with("new file mode ")
        || text.starts_with("deleted file mode ")
        || text.starts_with("old mode ")
        || text.starts_with("new mode ")
        || text.starts_with("similarity index ")
        || text.starts_with("rename from ")
        || text.starts_with("rename to ")
        || text.starts_with("Binary files ")
    {
        ANSI_PATCH_META
    } else if text == "Patch context unavailable." || text.starts_with("Patch context unavailable ")
    {
        ANSI_DIM
    } else {
        ""
    }
}

fn paint(text: &str, style: &str) -> String {
    if text.is_empty() || style.is_empty() {
        return text.to_string();
    }
    format!("{style}{text}{ANSI_RESET}")
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn wrap_prefixed_block(prefix: &str, text: &str, width: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![truncate_line(prefix, width)];
    }

    let continuation = " ".repeat(prefix.chars().count());
    let mut lines: Vec<String> = Vec::new();
    let mut rendered_any = false;

    for source_line in trimmed.lines() {
        let normalized = collapse_whitespace(source_line);
        if normalized.is_empty() {
            if rendered_any && lines.last().map(|line| !line.is_empty()).unwrap_or(false) {
                lines.push(String::new());
            }
            continue;
        }

        let first_prefix = if rendered_any {
            continuation.as_str()
        } else {
            prefix
        };
        lines.extend(wrap_paragraph(
            first_prefix,
            continuation.as_str(),
            &normalized,
            width,
        ));
        rendered_any = true;
    }

    if rendered_any {
        lines
    } else {
        vec![truncate_line(prefix, width)]
    }
}

fn wrap_paragraph(
    first_prefix: &str,
    continuation_prefix: &str,
    text: &str,
    width: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut remainder = text;
    let mut prefix = first_prefix;

    loop {
        let available = width.saturating_sub(prefix.chars().count()).max(1);
        let (segment, rest) = split_for_width(remainder, available);
        let mut line = String::from(prefix);
        line.push_str(segment);
        lines.push(truncate_line(&line, width));
        remainder = rest.trim_start();
        if remainder.is_empty() {
            return lines;
        }
        prefix = continuation_prefix;
    }
}

fn split_for_width(text: &str, width: usize) -> (&str, &str) {
    if width == 0 || text.is_empty() {
        return ("", text);
    }
    if text.chars().count() <= width {
        return (text, "");
    }

    let mut byte_end = 0usize;
    let mut last_space = None;
    for (seen, (index, ch)) in text.char_indices().enumerate() {
        if seen == width {
            break;
        }
        if ch.is_whitespace() {
            last_space = Some(index);
        }
        byte_end = index + ch.len_utf8();
    }

    if let Some(space) = last_space {
        (&text[..space], &text[space..])
    } else {
        (&text[..byte_end], &text[byte_end..])
    }
}

fn truncate_line(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let char_count = text.chars().count();
    if char_count <= width {
        return text.to_string();
    }
    if width <= 3 {
        return text.chars().take(width).collect();
    }

    let mut output = String::new();
    for ch in text.chars().take(width - 3) {
        output.push(ch);
    }
    output.push_str("...");
    output
}

fn visible_slice(text: &str, offset: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let start = char_boundary(text, offset);
    let visible = &text[start..];
    let end = char_boundary(visible, width);
    visible[..end].to_string()
}

fn char_boundary(text: &str, count: usize) -> usize {
    match text.char_indices().nth(count) {
        Some((index, _)) => index,
        None => text.len(),
    }
}

fn current_terminal_size() -> TerminalSize {
    let output = Command::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let size = String::from_utf8_lossy(&output.stdout);
            let mut parts = size.split_whitespace();
            let rows = parts.next().and_then(|value| value.parse::<usize>().ok());
            let cols = parts.next().and_then(|value| value.parse::<usize>().ok());
            if let (Some(rows), Some(cols)) = (rows, cols) {
                return TerminalSize { rows, cols };
            }
        }
    }

    let rows = env::var("LINES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_TERMINAL_ROWS);
    let cols = env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_TERMINAL_COLS);

    TerminalSize { rows, cols }
}

fn read_action(stdin: &mut impl Read) -> Result<Action, String> {
    loop {
        match read_byte(stdin)? {
            b'n' | b'N' => return Ok(Action::Next),
            b'd' | b'D' => return Ok(Action::Delete),
            b'q' | b'Q' | 3 => return Ok(Action::Quit),
            b'\x1b' => {
                if let Some(navigation) = read_escape_sequence(stdin)? {
                    return Ok(Action::Navigate(navigation));
                }
            }
            b'\n' | b'\r' => continue,
            _ => continue,
        }
    }
}

fn read_byte(stdin: &mut impl Read) -> Result<u8, String> {
    let mut buffer = [0u8; 1];
    let read = stdin
        .read(&mut buffer)
        .map_err(|error| format!("failed reading filter input: {error}"))?;
    if read == 0 {
        return Err("filter input ended before a selection was made".to_string());
    }
    Ok(buffer[0])
}

fn read_optional_byte(stdin: &mut impl Read) -> Result<Option<u8>, String> {
    let mut buffer = [0u8; 1];
    let read = stdin
        .read(&mut buffer)
        .map_err(|error| format!("failed reading filter input: {error}"))?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(buffer[0]))
}

fn read_escape_sequence(stdin: &mut impl Read) -> Result<Option<Navigation>, String> {
    let Some(prefix) = read_optional_byte(stdin)? else {
        return Ok(None);
    };

    if prefix != b'[' && prefix != b'O' {
        return Ok(None);
    }

    let Some(code) = read_optional_byte(stdin)? else {
        return Ok(None);
    };

    let navigation = match code {
        b'A' => Some(Navigation::Up),
        b'B' => Some(Navigation::Down),
        b'C' => Some(Navigation::Right),
        b'D' => Some(Navigation::Left),
        b'H' => Some(Navigation::Home),
        b'F' => Some(Navigation::End),
        b'1' => match read_optional_byte(stdin)? {
            Some(b'~') => Some(Navigation::Home),
            _ => None,
        },
        b'4' => match read_optional_byte(stdin)? {
            Some(b'~') => Some(Navigation::End),
            _ => None,
        },
        b'5' => match read_optional_byte(stdin)? {
            Some(b'~') => Some(Navigation::PageUp),
            _ => None,
        },
        b'6' => match read_optional_byte(stdin)? {
            Some(b'~') => Some(Navigation::PageDown),
            _ => None,
        },
        _ => None,
    };

    Ok(navigation)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Next,
    Delete,
    Quit,
    Navigate(Navigation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Navigation {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ScrollState {
    row_offset: usize,
    col_offset: usize,
}

impl ScrollState {
    fn apply(&mut self, navigation: Navigation, viewport: ViewportMetrics) {
        match navigation {
            Navigation::Up => {
                self.row_offset = self.row_offset.saturating_sub(1);
            }
            Navigation::Down => {
                self.row_offset = (self.row_offset + 1).min(viewport.max_row_offset);
            }
            Navigation::Left => {
                self.col_offset = self.col_offset.saturating_sub(1);
            }
            Navigation::Right => {
                self.col_offset = (self.col_offset + 1).min(viewport.max_col_offset);
            }
            Navigation::PageUp => {
                let page = viewport.content_rows.saturating_sub(1).max(1);
                self.row_offset = self.row_offset.saturating_sub(page);
            }
            Navigation::PageDown => {
                let page = viewport.content_rows.saturating_sub(1).max(1);
                self.row_offset = (self.row_offset + page).min(viewport.max_row_offset);
            }
            Navigation::Home => {
                self.row_offset = 0;
            }
            Navigation::End => {
                self.row_offset = viewport.max_row_offset;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ViewportMetrics {
    content_rows: usize,
    max_row_offset: usize,
    max_col_offset: usize,
}

enum ContentLine<'a> {
    Plain(String),
    Meta(String),
    Accent(String),
    Blank,
    Patch(&'a str),
}

#[derive(Debug, Clone, Copy)]
struct TerminalSize {
    rows: usize,
    cols: usize,
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, io::Cursor};

    use crate::json::JsonValue;

    use super::*;

    #[test]
    fn read_action_supports_arrow_and_page_keys() {
        let mut input = Cursor::new(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1b[5~\x1b[6~n");

        assert_eq!(
            read_action(&mut input).unwrap(),
            Action::Navigate(Navigation::Up)
        );
        assert_eq!(
            read_action(&mut input).unwrap(),
            Action::Navigate(Navigation::Down)
        );
        assert_eq!(
            read_action(&mut input).unwrap(),
            Action::Navigate(Navigation::Right)
        );
        assert_eq!(
            read_action(&mut input).unwrap(),
            Action::Navigate(Navigation::Left)
        );
        assert_eq!(
            read_action(&mut input).unwrap(),
            Action::Navigate(Navigation::PageUp)
        );
        assert_eq!(
            read_action(&mut input).unwrap(),
            Action::Navigate(Navigation::PageDown)
        );
        assert_eq!(read_action(&mut input).unwrap(), Action::Next);
    }

    #[test]
    fn render_terminal_comment_keeps_comment_body_visible() {
        let comment = sample_comment();
        let patch_text = "\
diff --git a/src/lib.rs b/src/lib.rs
@@ -1,2 +1,3 @@
-old line
+new line
+second patch line with extra context
";
        let patch_lines = patch_lines(&comment, Some(patch_text));
        let mut scroll = ScrollState::default();
        let mut output = Vec::new();

        let viewport = render_terminal_comment(
            &mut output,
            ".anne/reviews/example",
            0,
            1,
            &comment,
            &patch_lines,
            &mut scroll,
            TerminalSize { rows: 10, cols: 80 },
        )
        .unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Finding"));
        assert!(text.contains("Comment: This finding body stays visible"));
        assert!(text.contains("Diff: files/0001-src-lib.rs.patch"));
        assert!(text.contains("View 1-4/14  Col 1"), "{text}");
        assert_eq!(viewport.content_rows, 4);
    }

    #[test]
    fn render_terminal_comment_colors_diff_lines() {
        let comment = sample_comment();
        let patch_text = "\
diff --git a/src/lib.rs b/src/lib.rs
@@ -1,2 +1,3 @@
-old line
+new line
+second patch line with extra context
";
        let patch_lines = patch_lines(&comment, Some(patch_text));
        let mut scroll = ScrollState {
            row_offset: 8,
            col_offset: 0,
        };
        let mut output = Vec::new();

        render_terminal_comment(
            &mut output,
            ".anne/reviews/example",
            0,
            1,
            &comment,
            &patch_lines,
            &mut scroll,
            TerminalSize { rows: 12, cols: 80 },
        )
        .unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\x1b[34mdiff --git a/src/lib.rs b/src/lib.rs\x1b[0m"));
        assert!(text.contains("\x1b[1;36m@@ -1,2 +1,3 @@\x1b[0m"));
        assert!(text.contains("\x1b[31m-old line\x1b[0m"));
        assert!(text.contains("\x1b[32m+new line\x1b[0m"));
    }

    #[test]
    fn terminal_content_lines_wrap_long_comments_without_truncation() {
        let mut comment = sample_comment();
        comment.body = "This is a much longer finding body that should wrap across multiple lines inside the viewport instead of being truncated after a short summary.".to_string();
        let lines = terminal_content_lines(&comment, &["diff --git a/src/lib.rs b/src/lib.rs"], 36);
        let rendered = lines
            .iter()
            .filter_map(|line| match line {
                ContentLine::Plain(text) | ContentLine::Meta(text) | ContentLine::Accent(text) => {
                    Some(text.as_str())
                }
                ContentLine::Blank | ContentLine::Patch(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("wrap across multiple lines"));
        assert!(rendered.contains("instead of being truncated"));
        assert!(!rendered.contains("..."));
    }

    #[test]
    fn terminal_header_lines_are_bounded_on_wide_terminals() {
        let comment = sample_comment();
        let lines = terminal_header_lines(".anne/reviews/example", 0, 1, &comment, 200);

        assert!(!lines.is_empty());
        assert!(
            lines
                .iter()
                .all(|line| line.chars().count() <= MAX_HEADER_COLS)
        );
    }

    #[test]
    fn terminal_content_lines_keep_text_bounded_on_wide_terminals() {
        let mut comment = sample_comment();
        comment.title = "T".repeat(180);
        comment.body = "word ".repeat(120);
        let lines =
            terminal_content_lines(&comment, &["diff --git a/src/lib.rs b/src/lib.rs"], 200);

        assert!(lines.iter().all(|line| match line {
            ContentLine::Plain(text) | ContentLine::Meta(text) | ContentLine::Accent(text) => {
                text.chars().count() <= MAX_HEADER_COLS
            }
            ContentLine::Blank | ContentLine::Patch(_) => true,
        }));
    }

    #[test]
    fn scroll_state_clamps_navigation_to_patch_bounds() {
        let mut scroll = ScrollState::default();
        let viewport = ViewportMetrics {
            content_rows: 4,
            max_row_offset: 3,
            max_col_offset: 2,
        };

        scroll.apply(Navigation::PageDown, viewport);
        assert_eq!(
            scroll,
            ScrollState {
                row_offset: 3,
                col_offset: 0
            }
        );

        scroll.apply(Navigation::Down, viewport);
        scroll.apply(Navigation::Right, viewport);
        scroll.apply(Navigation::Right, viewport);
        scroll.apply(Navigation::Right, viewport);
        assert_eq!(
            scroll,
            ScrollState {
                row_offset: 3,
                col_offset: 2
            }
        );

        scroll.apply(Navigation::Home, viewport);
        scroll.apply(Navigation::Left, viewport);
        assert_eq!(
            scroll,
            ScrollState {
                row_offset: 0,
                col_offset: 1
            }
        );
    }

    fn sample_comment() -> persisted_review::SourceComment {
        persisted_review::SourceComment {
            raw_json: JsonValue::Object(BTreeMap::new()),
            id: "R001".to_string(),
            path: "src/lib.rs".to_string(),
            side: "new".to_string(),
            line: 3,
            severity: "warning".to_string(),
            title: "Keep the rendered finding text on screen".to_string(),
            body: "This finding body stays visible while the diff viewport scrolls.".to_string(),
            hunk_header: Some("@@ -1,2 +1,3 @@".to_string()),
            patch_file: Some("files/0001-src-lib.rs.patch".to_string()),
            confidence: Some("high".to_string()),
        }
    }
}
