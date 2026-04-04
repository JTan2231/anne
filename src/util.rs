use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::json::JsonValue;

pub fn current_timestamp() -> String {
    if let Ok(output) = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        && output.status.success()
    {
        return String::from_utf8_lossy(&output.stdout).trim().to_string();
    }

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("unix-{secs}")
}

pub fn slugify_text(text: &str) -> String {
    let mut output = String::new();
    let mut last_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' {
            output.push(ch);
            last_dash = false;
        } else if !last_dash {
            output.push('-');
            last_dash = true;
        }
    }

    let output = output.trim_matches('-').to_string();
    if output.is_empty() {
        "file".to_string()
    } else {
        output
    }
}

pub fn string_array(values: &[String]) -> JsonValue {
    JsonValue::Array(values.iter().cloned().map(JsonValue::string).collect())
}
