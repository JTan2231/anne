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
        let timestamp = String::from_utf8_lossy(&output.stdout);
        let timestamp = timestamp.trim();
        if is_canonical_timestamp(timestamp) {
            return timestamp.to_string();
        }
    }

    canonical_timestamp_from_system_time(SystemTime::now())
}

pub(crate) fn is_canonical_timestamp(value: &str) -> bool {
    const DIGIT_INDICES: [usize; 14] = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];
    let bytes = value.as_bytes();
    bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && DIGIT_INDICES
            .into_iter()
            .all(|index| bytes[index].is_ascii_digit())
}

fn canonical_timestamp_from_system_time(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format_canonical_utc_from_unix_seconds(secs)
}

fn format_canonical_utc_from_unix_seconds(secs: u64) -> String {
    const SECONDS_PER_DAY: u64 = 86_400;
    let days = secs / SECONDS_PER_DAY;
    let seconds_of_day = secs % SECONDS_PER_DAY;
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = civil_date_from_days_since_unix_epoch(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_date_from_days_since_unix_epoch(days: u64) -> (i128, u8, u8) {
    // Convert days-since-1970 to a Gregorian UTC calendar date without libc/date.
    let z = i128::from(days) + 719_468;
    let era = z / 146_097;
    let day_of_era = z - (era * 146_097);
    let year_of_era =
        (day_of_era - (day_of_era / 1_460) + (day_of_era / 36_524) - (day_of_era / 146_096)) / 365;
    let year = year_of_era + (era * 400);
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year, month as u8, day as u8)
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

#[cfg(test)]
mod tests {
    use super::{
        canonical_timestamp_from_system_time, format_canonical_utc_from_unix_seconds,
        is_canonical_timestamp,
    };
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn fallback_formatter_returns_canonical_utc_timestamp() {
        let timestamp =
            canonical_timestamp_from_system_time(UNIX_EPOCH + Duration::from_secs(1_712_250_929));

        assert_eq!(timestamp, "2024-04-04T17:15:29Z");
        assert!(is_canonical_timestamp(&timestamp));
    }

    #[test]
    fn epoch_clamp_serializes_as_canonical_unix_epoch() {
        let timestamp = canonical_timestamp_from_system_time(UNIX_EPOCH - Duration::from_secs(1));

        assert_eq!(timestamp, "1970-01-01T00:00:00Z");
        assert!(is_canonical_timestamp(&timestamp));
    }

    #[test]
    fn canonical_timestamp_validator_rejects_legacy_prefixes() {
        assert_eq!(
            format_canonical_utc_from_unix_seconds(0),
            "1970-01-01T00:00:00Z"
        );
        assert!(!is_canonical_timestamp("unix-1712250929"));
    }
}
