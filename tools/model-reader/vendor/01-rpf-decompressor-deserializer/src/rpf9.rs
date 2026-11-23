//! Read-only structural inspection of observed `9FPR` archives.

use std::io::{self, Read, Seek, SeekFrom};
use flate2::{Decompress, FlushDecompress, Status};

const HEADER_LEN: usize = 64;
const CANDIDATE_ALIGNMENT: usize = 4096;
const SCAN_CHUNK_LEN: usize = 64 * 1024;
const MAX_PSO_LEN: usize = 16 * 1024 * 1024;
const MEMBER_ALIGNMENT: u64 = 16;
const STREAM_WINDOW: usize = 1024 * 1024;
const MAX_STORED_MEMBER: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub offset: u64,
    pub word_04: u32,
    pub word_08: u32,
    pub word_0c: u32,
    /// In script.rpf and story000_script.rpf it equals the nested-header count
    /// for the root and the recovered PSO count for each PSO-bearing nested
    /// header (see 02-rpf-index-extractor). Unverified for other headers.
    pub word_24: u32,
}

impl Header {
    fn observed_layout(bytes: &[u8], offset: u64, remaining: u64) -> Option<Self> {
        let bytes = bytes.get(..HEADER_LEN)?;
        if &bytes[..4] != b"9FPR" {
            return None;
        }
        let word = |start| u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
        let header = Self {
            offset,
            word_04: word(4),
            word_08: word(8),
            word_0c: word(12),
            word_24: word(36),
        };
        if u64::from(header.word_04) > remaining
            || header.word_04 <= header.word_08
            || header.word_08 < header.word_0c
            || header.word_0c < HEADER_LEN as u32
            || header.word_0c % 16 != 0
            || header.word_08 % 16 != 0
            || header.word_04 % 16 != 0
        {
            return None;
        }
        Some(header)
    }
}

/// Scans the observed Xbox layout, including possible nested archive headers.
///
/// Candidate offsets are absolute and aligned to 4 KiB. A header match alone
/// does not determine the extent or name of an archive. The file index remains
/// opaque, and this function never decompresses or extracts content.
pub fn inspect(
    input: &mut (impl Read + Seek),
    on_header: impl FnMut(Header),
) -> io::Result<()> {
    inspect_with_progress(input, on_header, |_, _| {})
}

pub fn inspect_with_progress(
    input: &mut (impl Read + Seek),
    mut on_header: impl FnMut(Header),
    mut progress: impl FnMut(u64, u64),
) -> io::Result<()> {
    let len = input.seek(SeekFrom::End(0))?;
    input.seek(SeekFrom::Start(0))?;
    if len < HEADER_LEN as u64 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated 9FPR header"));
    }
    let mut buffer = [0; SCAN_CHUNK_LEN];
    let mut offset = 0;
    while offset < len {
        let read_len = (len - offset).min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..read_len])?;
        for local in (0..read_len).step_by(CANDIDATE_ALIGNMENT) {
            let position = offset + local as u64;
            match Header::observed_layout(&buffer[local..read_len], position, len - position) {
                Some(header) => on_header(header),
                None if position == 0 => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "unrecognized 9FPR header layout"));
                }
                None => {}
            }
        }
        offset += read_len as u64;
        if offset == len || offset.is_multiple_of(32 * 1024 * 1024) {
            progress(offset, len);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct RecoveredPso {
    pub offset: u64,
    pub compressed_end: u64,
    pub chunks: usize,
    pub decoded_len: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ZlibMember {
    pub offset: u64,
    pub compressed_end: u64,
    pub decoded_len: usize,
    pub zero_alignment_padding: bool,
}

fn validate_root(input: &mut (impl Read + Seek)) -> io::Result<u64> {
    let length = input.seek(SeekFrom::End(0))?;
    input.seek(SeekFrom::Start(0))?;
    let mut header = [0; HEADER_LEN];
    input.read_exact(&mut header)?;
    Header::observed_layout(&header, 0, length)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unrecognized 9FPR header layout"))?;
    Ok(length)
}

/// Reports checksum-valid zlib members at the observed 16-byte alignment.
/// Offsets refer to physical streams, which may be only part of an entry.
pub fn scan_zlib(
    input: &mut (impl Read + Seek),
    mut on_member: impl FnMut(ZlibMember, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    scan_zlib_with_progress(input, &mut on_member, |_, _| {})
}

pub fn scan_zlib_with_progress(
    input: &mut (impl Read + Seek),
    mut on_member: impl FnMut(ZlibMember, &[u8]) -> io::Result<()>,
    progress: impl FnMut(u64, u64),
) -> io::Result<()> {
    scan_zlib_inner(input, |_, member, bytes| on_member(member, bytes), progress)
}

fn scan_zlib_inner<T: Read + Seek>(
    input: &mut T,
    mut on_member: impl FnMut(&mut T, ZlibMember, &[u8]) -> io::Result<()>,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<()> {
    let length = validate_root(input)?;
    let mut window = vec![0; STREAM_WINDOW + MAX_STORED_MEMBER + MEMBER_ALIGNMENT as usize];
    let mut decoded = vec![0; SCAN_CHUNK_LEN + 1];
    let mut window_offset = 0;
    let mut offset = 0;
    while window_offset < length {
        let available = (length - window_offset).min(window.len() as u64) as usize;
        input.seek(SeekFrom::Start(window_offset))?;
        input.read_exact(&mut window[..available])?;
        let scan_end = window_offset + available.min(STREAM_WINDOW) as u64;
        while offset < scan_end {
            let local = (offset - window_offset) as usize;
            if let Some(member) = decode_member(&window[local..available], offset, &mut decoded) {
                on_member(input, member, &decoded[..member.decoded_len])?;
                offset = member.compressed_end.next_multiple_of(MEMBER_ALIGNMENT);
            } else {
                offset += MEMBER_ALIGNMENT;
            }
        }
        window_offset = scan_end;
        if window_offset == length || window_offset % (32 * STREAM_WINDOW as u64) == 0 {
            progress(window_offset, length);
        }
    }
    Ok(())
}

/// Reconstructs a candidate object from a known start through its first
/// short decoded chunk, requiring checksum-valid members and zero padding.
pub fn read_zlib_sequence(
    input: &mut (impl Read + Seek),
    offset: u64,
) -> io::Result<(Vec<u8>, usize)> {
    let length = validate_root(input)?;
    let (bytes, count, _) = read_sequence_to(input, offset, length, MAX_PSO_LEN)?;
    Ok((bytes, count))
}

/// Reads a cataloged sequence within its recorded range. The caller can
/// validate the source independently without identifying its root layout.
pub fn read_zlib_sequence_range(
    input: &mut (impl Read + Seek),
    offset: u64,
    end: u64,
    decoded_len: usize,
) -> io::Result<(Vec<u8>, usize)> {
    if end > input.seek(SeekFrom::End(0))? || decoded_len > MAX_PSO_LEN {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "zlib sequence range exceeds its limit"));
    }
    let (bytes, count, actual_end) = read_sequence_to(input, offset, end, decoded_len)?;
    if actual_end != end {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "zlib sequence ends before the recorded boundary"));
    }
    Ok((bytes, count))
}

fn read_sequence_to(
    input: &mut (impl Read + Seek),
    mut offset: u64,
    length: u64,
    max_decoded_len: usize,
) -> io::Result<(Vec<u8>, usize, u64)> {
    if offset >= length || offset % MEMBER_ALIGNMENT != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "offset is outside the archive or unaligned"));
    }
    let mut stored = vec![0; MAX_STORED_MEMBER + MEMBER_ALIGNMENT as usize];
    let mut chunk = vec![0; SCAN_CHUNK_LEN + 1];
    let mut result = Vec::new();
    let mut count = 0;
    while offset < length && result.len() < max_decoded_len {
        input.seek(SeekFrom::Start(offset))?;
        let available = (length - offset).min(stored.len() as u64) as usize;
        input.read_exact(&mut stored[..available])?;
        let member = decode_member(&stored[..available], offset, &mut chunk)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing or invalid zlib member at {offset:#x}")))?;
        if member.decoded_len > max_decoded_len - result.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "zlib sequence exceeds its decoded size"));
        }
        result.extend_from_slice(&chunk[..member.decoded_len]);
        count += 1;
        if member.decoded_len < SCAN_CHUNK_LEN {
            return Ok((result, count, member.compressed_end));
        }
        if !member.zero_alignment_padding {
            break;
        }
        offset = member.compressed_end.next_multiple_of(MEMBER_ALIGNMENT);
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "sequence lacks a verified short terminal member"))
}

/// Recovers framed PSO objects from the observed zlib payloads.
///
/// The RPF9 index is not decoded. Results are identified by source offset.
/// Each zlib chunk must terminate with a valid checksum. PSO section lengths
/// must span the complete decoded object, including its schema and either
/// the legacy PMAP or the CHKS footer observed with LSCH.
pub fn recover_psos(
    input: &mut (impl Read + Seek),
    on_pso: impl FnMut(RecoveredPso, &[u8]) -> io::Result<()>,
) -> io::Result<usize> {
    recover_psos_with_progress(input, on_pso, |_, _| {})
}

pub fn recover_psos_with_progress(
    input: &mut (impl Read + Seek),
    mut on_pso: impl FnMut(RecoveredPso, &[u8]) -> io::Result<()>,
    progress: impl FnMut(u64, u64),
) -> io::Result<usize> {
    let mut object = Vec::new();
    let mut pending: Option<RecoveredPso> = None;
    let mut next_offset = None;
    let mut count = 0;
    scan_zlib_inner(input, |source, member, bytes| {
        if bytes.starts_with(b"PSIN") {
            if let (Some(mut previous), Some(start)) = (pending, next_offset) {
                let old_len = object.len();
                let available = member.offset.saturating_sub(start)
                    .min((MAX_PSO_LEN - old_len) as u64) as usize;
                if available != 0 {
                    object.resize(old_len + available, 0);
                    source.seek(SeekFrom::Start(start))?;
                    source.read_exact(&mut object[old_len..])?;
                    if let Some(end) = pso_prefix_length(&object).filter(|&end| end > old_len) {
                        previous.compressed_end = start + (end - old_len) as u64;
                        previous.decoded_len = end;
                        previous.chunks += (end - old_len).div_ceil(SCAN_CHUNK_LEN);
                        on_pso(previous, &object[..end])?;
                        count += 1;
                    }
                }
            }
            object.clear();
            pending = Some(RecoveredPso {
                offset: member.offset, compressed_end: member.compressed_end, chunks: 0, decoded_len: 0,
            });
        } else if let Some(entry) = pending.as_mut() {
            let start = next_offset.unwrap_or(member.offset + 1);
            let gap = member.offset.checked_sub(start);
            match gap.filter(|&n| n % SCAN_CHUNK_LEN as u64 == 0
                && n <= (MAX_PSO_LEN - object.len()).saturating_sub(bytes.len()) as u64)
            {
                Some(0) if object.len() + bytes.len() <= MAX_PSO_LEN => {}
                Some(gap) if gap > 0 => {
                    // Some archives store full pages without compression.
                    // Only emit the assembled object if its framing and footer validate.
                    let previous_len = object.len();
                    object.resize(previous_len + gap as usize, 0);
                    source.seek(SeekFrom::Start(start))?;
                    source.read_exact(&mut object[previous_len..])?;
                    entry.chunks += gap as usize / SCAN_CHUNK_LEN;
                }
                _ => pending = None,
            }
        }
        if let Some(entry) = pending.as_mut() {
            object.extend_from_slice(bytes);
            entry.compressed_end = member.compressed_end;
            entry.chunks += 1;
            entry.decoded_len = object.len();
            next_offset = (bytes.len() == SCAN_CHUNK_LEN && member.zero_alignment_padding)
                .then(|| member.compressed_end.next_multiple_of(MEMBER_ALIGNMENT));
            if complete_pso(&object) {
                on_pso(*entry, &object)?;
                count += 1;
                pending = None;
            }
        }
        Ok(())
    }, progress)?;
    Ok(count)
}

fn decode_member(data: &[u8], offset: u64, output: &mut [u8]) -> Option<ZlibMember> {
    let input = &data[..data.len().min(MAX_STORED_MEMBER)];
    let [cmf, flg] = *input.get(..2)?.first_chunk::<2>()?;
    if cmf != 0x78 || flg & 0x20 != 0 || u16::from_be_bytes([cmf, flg]) % 31 != 0 {
        return None;
    }
    let mut decoder = Decompress::new(true);
    if decoder.decompress(input, output, FlushDecompress::Finish).ok()? != Status::StreamEnd {
        return None;
    }
    let size = decoder.total_out() as usize;
    if size == 0 || size > SCAN_CHUNK_LEN {
        return None;
    }
    let stored = decoder.total_in() as usize;
    let padded = stored.next_multiple_of(MEMBER_ALIGNMENT as usize);
    Some(ZlibMember {
        offset,
        compressed_end: offset + stored as u64,
        decoded_len: size,
        zero_alignment_padding: data.get(stored..padded).is_some_and(|p| p.iter().all(|&b| b == 0)),
    })
}

fn complete_pso(data: &[u8]) -> bool {
    pso_prefix_length(data) == Some(data.len())
}

fn pso_prefix_length(data: &[u8]) -> Option<usize> {
    if !data.starts_with(b"PSIN") {
        return None;
    }
    let mut offset = 0;
    let (mut has_map, mut has_schema, mut has_layout) = (false, false, false);
    while offset < data.len() {
        let header = data.get(offset..offset + 8)?;
        let section_len = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
        if section_len < 8 || section_len > data.len() - offset
            || !header[..4].iter().all(u8::is_ascii_uppercase)
        {
            return None;
        }
        has_map |= &header[..4] == b"PMAP";
        has_schema |= &header[..4] == b"PSCH";
        has_layout |= &header[..4] == b"LSCH";
        let validated_footer = &header[..4] == b"CHKS"
            && section_len == 20
            && u32::from_be_bytes(data[offset + 8..offset + 12].try_into().unwrap()) as usize == offset + section_len;
        offset += section_len;
        if has_layout && validated_footer {
            return Some(offset);
        }
    }
    (has_map && has_schema).then_some(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::{Cursor, Write};

    #[test]
    fn accepts_story_root_with_48_byte_tail() {
        let mut header = [0; HEADER_LEN];
        header[..4].copy_from_slice(b"9FPR");
        for (field, value) in [(4, 0x2b0u32), (8, 0x280), (12, 0xe0), (36, 4)] {
            header[field..field + 4].copy_from_slice(&value.to_le_bytes());
        }
        let parsed = Header::observed_layout(&header, 0, 1024).unwrap();
        assert_eq!(parsed.word_04 - parsed.word_08, 48);
        assert!(Header::observed_layout(&header, 0, 687).is_none());
    }

    #[test]
    fn recovers_raw_tail_before_next_object() {
        fn pso(data_size: usize) -> Vec<u8> {
            let mut bytes = b"PSIN".to_vec();
            bytes.extend_from_slice(&(data_size as u32).to_be_bytes());
            bytes.resize(data_size, 0x55);
            bytes.extend_from_slice(b"LSCH\0\0\0\x08CHKS\0\0\0\x14");
            bytes.extend_from_slice(&(data_size as u32 + 28).to_be_bytes());
            bytes.extend_from_slice(&[0; 8]);
            bytes
        }

        let mut archive = vec![0; CANDIDATE_ALIGNMENT];
        archive[..4].copy_from_slice(b"9FPR");
        for (offset, value) in [(4, 608u32), (8, 576), (12, 128)] {
            archive[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        let first = pso(2 * SCAN_CHUNK_LEN + 32);
        let second = pso(64);
        for (zlib, raw) in [(&first[..SCAN_CHUNK_LEN], &first[SCAN_CHUNK_LEN..]), (&second[..], &[][..])] {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(zlib).unwrap();
            archive.extend(encoder.finish().unwrap());
            archive.resize(archive.len().next_multiple_of(16), 0);
            archive.extend_from_slice(raw);
            archive.resize(archive.len().next_multiple_of(16), 0);
        }
        let mut decoded = Vec::new();
        assert_eq!(recover_psos(&mut Cursor::new(archive), |_, bytes| {
            decoded.push(bytes.to_vec());
            Ok(())
        }).unwrap(), 2);
        assert_eq!(decoded, [first, second]);
    }

    #[test]
    fn recovers_multichunk_pso_and_rejects_corrupt_stream() {
        let first_offset = STREAM_WINDOW - MEMBER_ALIGNMENT as usize;
        let mut archive = vec![0; first_offset];
        archive[..4].copy_from_slice(b"9FPR");
        for (offset, value) in [(4, 608u32), (8, 576), (12, 128)] {
            archive[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        let mut object = b"PSIN".to_vec();
        object.extend_from_slice(&(SCAN_CHUNK_LEN as u32 + 16).to_be_bytes());
        object.resize(SCAN_CHUNK_LEN + 16, 0);
        object.extend_from_slice(b"LSCH\0\0\0\x08CHKS\0\0\0\x14");
        object.extend_from_slice(&(SCAN_CHUNK_LEN as u32 + 44).to_be_bytes());
        object.extend_from_slice(&[0; 8]);
        for part in object.chunks(SCAN_CHUNK_LEN) {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(part).unwrap();
            archive.extend(encoder.finish().unwrap());
            archive.resize(archive.len().next_multiple_of(16), 0);
        }
        let mut seen = Vec::new();
        assert_eq!(recover_psos(&mut Cursor::new(&archive), |entry, bytes| {
            assert_eq!(entry.chunks, 2);
            assert_eq!(bytes, object);
            seen.push(entry.compressed_end as usize);
            Ok(())
        }).unwrap(), 1);
        let (decoded, members) = read_zlib_sequence(&mut Cursor::new(&archive), first_offset as u64).unwrap();
        assert_eq!((decoded, members), (object, 2));
        archive[seen[0] - 1] ^= 1;
        assert_eq!(recover_psos(&mut Cursor::new(&archive), |_, _| unreachable!()).unwrap(), 0);
    }

    #[test]
    fn reports_root_and_valid_candidate_only() {
        let mut sample = vec![0; 3 * CANDIDATE_ALIGNMENT];
        for offset in [0, CANDIDATE_ALIGNMENT, 2 * CANDIDATE_ALIGNMENT] {
            sample[offset..offset + 4].copy_from_slice(b"9FPR");
            for (field, value) in [(4, 608u32), (8, 576), (12, 128), (36, 2)] {
                sample[offset + field..offset + field + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        sample[2 * CANDIDATE_ALIGNMENT + 4..2 * CANDIDATE_ALIGNMENT + 8]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        let mut headers = Vec::new();
        inspect(&mut Cursor::new(sample), |header| headers.push(header.offset)).unwrap();
        assert_eq!(headers, [0, CANDIDATE_ALIGNMENT as u64]);
    }

    #[test]
    fn rejects_old_magic_and_short_headers() {
        let mut sample = [0; HEADER_LEN];
        sample[..4].copy_from_slice(b"8FPR");
        assert_eq!(
            inspect(&mut Cursor::new(sample), |_| {}).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            inspect(&mut Cursor::new(*b"9FPR"), |_| {}).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
