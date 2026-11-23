//! Small helpers shared by the core modules.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::Result;

/// Uppercase hexadecimal, like `Convert.ToHexString`.
pub fn hex_upper(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_upper(&sha256(bytes))
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Absolute path without resolving links, like `Path.GetFullPath`.
pub fn full_path(path: impl AsRef<Path>) -> Result<PathBuf> {
    Ok(std::path::absolute(path.as_ref())?)
}

pub fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Ordinal case-insensitive equality for paths and identifiers.
pub fn eq_ignore_case(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(b)
    } else {
        a.to_uppercase() == b.to_uppercase()
    }
}

/// Case-insensitive substring search.
pub fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle.is_ascii() {
        let (h, n) = (haystack.as_bytes(), needle.as_bytes());
        return h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    haystack.to_uppercase().contains(&needle.to_uppercase())
}

/// Lowercase file extension including the dot, like `Path.GetExtension`.
pub fn extension(path: &Path) -> String {
    path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default()
}

/// `0x`-prefixed hexadecimal or decimal integer.
pub fn parse_number(value: &str) -> Result<i64> {
    let invalid = || crate::Error::invalid(format!("Invalid number: {value}"));
    if let Some(hex) = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")) {
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        return u64::from_str_radix(hex, 16).map(|v| v as i64).map_err(|_| invalid());
    }
    value.trim().parse::<i64>().map_err(|_| invalid())
}

/// Parses a 64-bit hexadecimal identifier with an optional `0x` prefix.
pub fn parse_hex_u64(value: &str) -> Option<u64> {
    let text = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")).unwrap_or(value);
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(text, 16).ok()
}
