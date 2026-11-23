//! Crash-safe local file writes and JSON documents.

use std::{
    fs,
    io::{BufWriter, Write},
    path::Path,
};

use serde::{de::DeserializeOwned, Serialize};

use crate::{util::full_path, Error, Result};

/// Reads a JSON document.
pub fn load<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Indented JSON with camelCase keys, like the C# `LocalFiles.Json`.
pub fn to_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

pub fn save<T: Serialize>(path: &Path, value: &T, replace: bool) -> Result<()> {
    write(
        path,
        |output| {
            serde_json::to_writer_pretty(&mut *output, value)?;
            Ok(())
        },
        replace,
    )
}

/// Writes through a hidden temporary file in the destination folder, flushes
/// it to disk and then publishes it. Without `replace`, an existing
/// destination is left untouched and reported as an error.
pub fn write(path: &Path, fill: impl FnOnce(&mut dyn Write) -> Result<()>, replace: bool) -> Result<()> {
    let path = full_path(path)?;
    let directory = path.parent().ok_or_else(|| Error::io("The destination has no folder."))?;
    let temporary = tempfile::Builder::new().prefix(".vicemodvi-").suffix(".tmp").tempfile_in(directory)?;
    {
        let mut output = BufWriter::with_capacity(1024 * 1024, temporary.as_file());
        fill(&mut output)?;
        output.flush()?;
    }
    temporary.as_file().sync_all()?;
    let published = if replace { temporary.persist(&path) } else { temporary.persist_noclobber(&path) };
    published.map(|_| ()).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            Error::io(format!("The file '{}' already exists.", path.display()))
        } else {
            Error::Io(error.error)
        }
    })
}

/// Moves a finished file into place. Without `replace`, the move fails if
/// the destination exists; a hard link makes that check atomic where the
/// file system supports it.
pub fn publish(temporary: &Path, destination: &Path, replace: bool) -> Result<()> {
    let exists = || Error::io(format!("The file '{}' already exists.", destination.display()));
    if replace {
        fs::rename(temporary, destination)?;
        return Ok(());
    }
    match fs::hard_link(temporary, destination) {
        Ok(()) => {
            let _ = fs::remove_file(temporary);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(exists()),
        Err(_) if destination.exists() => Err(exists()),
        Err(_) => Ok(fs::rename(temporary, destination)?),
    }
}

pub fn write_bytes(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    write(
        path,
        |output| {
            output.write_all(bytes)?;
            Ok(())
        },
        replace,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_overwrite_without_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.bin");
        write_bytes(&path, b"one", false).unwrap();
        assert!(write_bytes(&path, b"two", false).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"one");
        write_bytes(&path, b"two", true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "temporary files are removed");
    }
}
