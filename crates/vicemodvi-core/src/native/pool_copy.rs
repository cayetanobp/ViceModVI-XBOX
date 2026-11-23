//! Writes a copy of a TTPF pool with one texture replaced and checks the
//! copy before it is used.
//!
//! Only the chunks that overlap the texture are re-encoded. Every other
//! byte is copied, the ETPF tables are kept, and the RDS5 table words get
//! new physical starts and padding.

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

use flate2::{write::ZlibEncoder, Compression};
use sha2::{Digest, Sha256};

use super::{
    dds,
    pool::{decode_chunk, word, PoolIndex, STORED, ZLIB},
};
use crate::{util::sha256, Cancel, Error, Result};

/// Highest physical offset a table word can express.
const MAX_PHYSICAL: i64 = word::SOURCE_MASK as i64 * 256;
const PADDING_LIMIT: i64 = 4095;
const BUFFER: usize = 1024 * 1024;
const TABLE_START: u64 = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolCopy {
    pub changed_chunks: i32,
    pub changed_decoded_bytes: i64,
    pub source_bytes: i64,
    pub destination_bytes: i64,
    pub unchanged_samples_verified: i32,
}

/// Counts and hashes what passes through, and refuses to grow past the
/// physical offset range.
struct Tally<W> {
    inner: W,
    position: i64,
    digest: Sha256,
}

impl<W: Write> Write for Tally<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() as i64 > MAX_PHYSICAL - self.position {
            return Err(io::Error::other("The pool copy exceeds the 26-bit physical offset range."));
        }
        let written = self.inner.write(buffer)?;
        self.digest.update(&buffer[..written]);
        self.position += written as i64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn copy_span(from: &mut impl Read, to: &mut impl Write, mut count: i64, buffer: &mut [u8], cancel: &Cancel) -> Result<()> {
    while count > 0 {
        cancel.check()?;
        let n = (count as usize).min(buffer.len());
        from.read_exact(&mut buffer[..n])?;
        to.write_all(&buffer[..n])?;
        count -= n as i64;
    }
    Ok(())
}

/// Writes the copy to `destination`, which must not exist yet. The caller
/// removes the file when this fails.
pub fn write(source: &Path, index: i32, replacement: &[u8], destination: &Path, cancel: &Cancel) -> Result<PoolCopy> {
    cancel.check()?;
    let original = PoolIndex::read(source, cancel)?;
    let mut input = File::open(source)?;
    if input.metadata()?.len() as i64 != original.info.source_length {
        return Err(Error::io("The source pool changed while its index was read."));
    }
    let imported = dds::inspect(replacement)?;
    let allocation = original.retile(&mut input, index, replacement, cancel)?;
    let texture = original.exportable(index)?.0.clone();
    let (texture_start, texture_end) = (texture.virtual_offset, texture.virtual_offset + allocation.len() as i64);

    let mut table = vec![0u8; (original.chunks.len() + 1) * 8];
    input.seek(SeekFrom::Start(TABLE_START))?;
    input.read_exact(&mut table)?;
    let prefix = original.chunks[0].source;
    let mut expected_chunks: HashMap<usize, [u8; 32]> = HashMap::new();
    let (mut changed_chunks, mut changed_bytes) = (0i32, 0i64);
    let mut buffer = vec![0u8; BUFFER];

    let output = OpenOptions::new().write(true).create_new(true).open(destination)?;
    let mut out = Tally { inner: BufWriter::with_capacity(BUFFER, output), position: prefix, digest: Sha256::new() };
    input.seek(SeekFrom::Start(0))?;
    copy_span(&mut input, &mut out.inner, prefix, &mut buffer, cancel)?;
    for (i, chunk) in original.chunks.iter().enumerate() {
        cancel.check()?;
        let start = out.position;
        let (from, to) = (texture_start.max(chunk.virtual_offset), texture_end.min(chunk.virtual_offset + chunk.size));
        let mut edited = None;
        if from < to {
            let mut data = decode_chunk(&mut input, chunk, cancel)?;
            let inside = (from - chunk.virtual_offset) as usize..(to - chunk.virtual_offset) as usize;
            let wanted = &allocation[(from - texture_start) as usize..(to - texture_start) as usize];
            let differing = data[inside.clone()].iter().zip(wanted).filter(|(a, b)| a != b).count();
            if differing > 0 {
                data[inside].copy_from_slice(wanted);
                changed_bytes += differing as i64;
                edited = Some(data);
            }
        }
        let padding = match edited {
            None => {
                input.seek(SeekFrom::Start(chunk.source as u64))?;
                copy_span(&mut input, &mut out, chunk.packed + chunk.padding, &mut buffer, cancel)?;
                chunk.padding
            }
            Some(data) => {
                changed_chunks += 1;
                expected_chunks.insert(i, sha256(&data));
                match chunk.codec {
                    STORED => out.write_all(&data)?,
                    ZLIB => {
                        let mut encoder = ZlibEncoder::new(&mut out, Compression::best());
                        for part in data.chunks(BUFFER) {
                            cancel.check()?;
                            encoder.write_all(part)?;
                        }
                        encoder.finish()?;
                    }
                    _ => return Err(Error::not_supported("BCPack-dependent chunks cannot be replaced.")),
                }
                // Keep the original filler, then pad to the next 256 bytes.
                let packed = out.position - start;
                let extra = (256 - ((packed + chunk.padding) & 255)) & 255;
                if chunk.padding + extra > PADDING_LIMIT {
                    return Err(Error::not_supported("Recompressed chunk cannot retain its padding within the 12-bit field."));
                }
                input.seek(SeekFrom::Start((chunk.source + chunk.packed) as u64))?;
                copy_span(&mut input, &mut out, chunk.padding, &mut buffer, cancel)?;
                out.write_all(&vec![0u8; extra as usize])?;
                chunk.padding + extra
            }
        };
        if start & 255 != 0 || out.position & 255 != 0 || out.position > MAX_PHYSICAL {
            return Err(Error::invalid("Rewritten chunk exceeds the 26-bit physical offset range or alignment."));
        }
        set_word(&mut table, i, |value| {
            let cleared = value & !(word::SOURCE_MASK | 0xFFF << word::PADDING_SHIFT);
            cleared | (start / 256) as u64 | (padding as u64) << word::PADDING_SHIFT
        });
    }
    let length = out.position;
    set_word(&mut table, original.chunks.len(), |value| (value & !word::SOURCE_MASK) | (length / 256) as u64);
    let written: [u8; 32] = out.digest.finalize().into();
    let mut file = out.inner.into_inner().map_err(|e| Error::Io(e.into_error()))?;
    file.seek(SeekFrom::Start(TABLE_START))?;
    file.write_all(&table)?;
    file.sync_all()?;
    drop(file);

    cancel.check()?;
    let copy = PoolIndex::read(destination, cancel)?;
    if copy.info.virtual_size != original.info.virtual_size
        || copy.chunks.len() != original.chunks.len()
        || copy.info.textures.len() != original.info.textures.len()
    {
        return Err(Error::invalid("Rewritten pool changed the native virtual index shape."));
    }
    let mut reopened = File::open(destination)?;
    same_prefix(&mut input, &mut reopened, prefix, &table, &mut buffer, cancel)?;
    if physical_digest(&mut reopened, prefix, length, &mut buffer, cancel)? != written {
        return Err(Error::invalid("Rewritten physical payload checksum differs from the bytes written."));
    }
    for (i, (before, after)) in original.chunks.iter().zip(&copy.chunks).enumerate() {
        cancel.check()?;
        if before.virtual_offset != after.virtual_offset || before.size != after.size || before.codec != after.codec {
            return Err(Error::invalid("Rewritten chunk changed virtual range or compression codec."));
        }
        if let Some(expected) = expected_chunks.get(&i) {
            if &sha256(&decode_chunk(&mut reopened, after, cancel)?) != expected {
                return Err(Error::invalid("Affected chunk does not match its complete expected decoded contents."));
            }
        }
    }
    let exported = copy.create_dds(index, cancel)?;
    if exported.get(dds::DX10_HEADER_BYTES..) != Some(imported.payload(replacement)) {
        return Err(Error::invalid("Selected native DDS payload does not reproduce the replacement exactly."));
    }
    let verified = check_untouched(&original, &copy, index, texture_start, texture_end, cancel)?;
    Ok(PoolCopy {
        changed_chunks,
        changed_decoded_bytes: changed_bytes,
        source_bytes: original.info.source_length,
        destination_bytes: length,
        unchanged_samples_verified: verified,
    })
}

fn set_word(table: &mut [u8], index: usize, change: impl Fn(u64) -> u64) {
    let slot = &mut table[index * 8..index * 8 + 8];
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(slot);
    slot.copy_from_slice(&change(u64::from_le_bytes(bytes)).to_le_bytes());
}

/// The two smallest exportable textures outside the replaced range must
/// export identically from both pools.
fn check_untouched(original: &PoolIndex, copy: &PoolIndex, index: i32, start: i64, end: i64, cancel: &Cancel) -> Result<i32> {
    let mut samples: Vec<_> = original
        .info
        .textures
        .iter()
        .filter(|t| t.index != index && t.can_extract())
        .filter(|t| original.surface_size(t.index).is_some_and(|size| t.virtual_offset >= end || t.virtual_offset + size <= start))
        .collect();
    samples.sort_by_key(|t| t.dds_size);
    let mut verified = 0;
    for sample in samples.into_iter().take(2) {
        cancel.check()?;
        if sha256(&original.create_dds(sample.index, cancel)?) != sha256(&copy.create_dds(sample.index, cancel)?) {
            return Err(Error::invalid("An unaffected texture sample changed in the pool copy."));
        }
        verified += 1;
    }
    Ok(verified)
}

/// The header and ETPF tables must match the source, apart from the new
/// table words.
fn same_prefix(source: &mut File, copy: &mut File, count: i64, table: &[u8], buffer: &mut [u8], cancel: &Cancel) -> Result<()> {
    let mut expected = vec![0u8; buffer.len()];
    source.seek(SeekFrom::Start(0))?;
    copy.seek(SeekFrom::Start(0))?;
    let table_range = TABLE_START as i64..TABLE_START as i64 + table.len() as i64;
    let mut at = 0i64;
    while at < count {
        cancel.check()?;
        let n = ((count - at) as usize).min(buffer.len());
        source.read_exact(&mut expected[..n])?;
        copy.read_exact(&mut buffer[..n])?;
        let (lo, hi) = (at.max(table_range.start), (at + n as i64).min(table_range.end));
        if lo < hi {
            expected[(lo - at) as usize..(hi - at) as usize].copy_from_slice(&table[(lo - table_range.start) as usize..(hi - table_range.start) as usize]);
        }
        if expected[..n] != buffer[..n] {
            return Err(Error::invalid("Original header/ETPF metadata was not preserved exactly."));
        }
        at += n as i64;
    }
    Ok(())
}

fn physical_digest(file: &mut File, from: i64, to: i64, buffer: &mut [u8], cancel: &Cancel) -> Result<[u8; 32]> {
    file.seek(SeekFrom::Start(from as u64))?;
    let mut digest = Sha256::new();
    let mut remaining = to - from;
    while remaining > 0 {
        cancel.check()?;
        let want = (remaining as usize).min(buffer.len());
        let n = file.read(&mut buffer[..want])?;
        if n == 0 {
            return Err(Error::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "Rewritten physical data was truncated.")));
        }
        digest.update(&buffer[..n]);
        remaining -= n as i64;
    }
    Ok(digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::pool::{self, tests::sample};

    #[test]
    fn replaces_one_texture_and_exports_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.ttpf");
        std::fs::write(&source, sample(256, 256, 71, 9)).unwrap();
        let original = pool::create_dds(&source, 0, &Cancel::none()).unwrap();
        let mut edited = original.clone();
        for (i, byte) in edited[dds::DX10_HEADER_BYTES..].iter_mut().enumerate() {
            *byte = (i % 7) as u8;
        }
        let destination = dir.path().join("copy.ttpf");
        let result = write(&source, 0, &edited, &destination, &Cancel::none()).unwrap();
        assert_eq!(result.changed_chunks, 2);
        assert_eq!(pool::create_dds(&destination, 0, &Cancel::none()).unwrap(), edited);
        assert!(write(&source, 0, &edited, &destination, &Cancel::none()).is_err());
    }
}
