//! UTC timestamps in the System.Text.Json format, with 100 ns precision.

use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serializer};

/// Formats like System.Text.Json: seven fractional digits with trailing
/// zeros removed, and a `Z` suffix.
pub fn format(value: &DateTime<Utc>) -> String {
    let text = value.to_rfc3339_opts(SecondsFormat::Nanos, true);
    // Nanos gives nine digits; .NET ticks hold seven.
    let (main, _) = text.split_at(text.len() - 1);
    let (seconds, fraction) = main.split_once('.').unwrap_or((main, ""));
    let fraction = fraction.get(..7).unwrap_or(fraction).trim_end_matches('0');
    if fraction.is_empty() {
        format!("{seconds}Z")
    } else {
        format!("{seconds}.{fraction}Z")
    }
}

pub fn parse(text: &str) -> Option<DateTime<Utc>> {
    if let Ok(value) = DateTime::parse_from_rfc3339(text) {
        return Some(value.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").ok().map(|naive| naive.and_utc())
}

pub fn serialize<S: Serializer>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&format(value))
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<DateTime<Utc>, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse(&text).ok_or_else(|| serde::de::Error::custom(format!("Invalid timestamp: {text}")))
}

/// Last write time of a file in UTC.
pub fn last_write_utc(path: &std::path::Path) -> std::io::Result<DateTime<Utc>> {
    Ok(DateTime::<Utc>::from(std::fs::metadata(path)?.modified()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_dotnet_timestamps() {
        for text in ["2025-11-02T10:20:30.1234567Z", "2025-11-02T10:20:30.12Z", "2025-11-02T10:20:30Z"] {
            let value = parse(text).unwrap();
            assert_eq!(format(&value), text);
        }
    }
}
