//! zlib helpers for catalog segments, pool chunks and resource scans.

use std::io::{Read, Seek, SeekFrom, Write};

use flate2::{write::ZlibEncoder, Compression, Decompress, FlushDecompress, Status};

use crate::{Cancel, Error, Result};

/// True when two bytes form a zlib header: deflate, a window of at most
/// 32 KiB, a valid check value and no preset dictionary.
pub(crate) fn is_header(bytes: &[u8]) -> bool {
    match bytes {
        [cmf, flg, ..] => cmf & 0x0F == 8 && cmf >> 4 <= 7 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0 && flg & 0x20 == 0,
        _ => false,
    }
}

/// Inflates a pool chunk. The stream must use every input byte, decode to
/// exactly `size` bytes and end with a matching Adler-32.
pub(crate) fn inflate_chunk(packed: &[u8], size: usize, cancel: &Cancel) -> Result<Vec<u8>> {
    let mismatch = || Error::invalid("Zlib output length/checksum disagrees with its chunk.");
    if packed.len() < 6 {
        return Err(Error::invalid("Truncated zlib chunk."));
    }
    let mut stream = Decompress::new(true);
    let mut output = vec![0u8; size];
    let mut status = Status::Ok;
    while (stream.total_out() as usize) < size && status != Status::StreamEnd {
        cancel.check()?;
        let (read, written) = (stream.total_in() as usize, stream.total_out() as usize);
        let window = &mut output[written..(written + 64 * 1024).min(size)];
        status = stream
            .decompress(&packed[read..], window, FlushDecompress::None)
            .map_err(|error| Error::invalid(format!("Invalid zlib data: {error}")))?;
        if stream.total_in() as usize == read && stream.total_out() as usize == written {
            break;
        }
    }
    if (stream.total_out() as usize) < size {
        return Err(Error::invalid("Zlib output is shorter than its virtual chunk."));
    }
    if status != Status::StreamEnd {
        // The output is full; the stream must end without another byte.
        let mut spare = [0u8; 1];
        let read = stream.total_in() as usize;
        status = stream.decompress(&packed[read..], &mut spare, FlushDecompress::None).map_err(|_| mismatch())?;
        if stream.total_out() as usize != size || status != Status::StreamEnd {
            return Err(mismatch());
        }
    }
    let mut adler = adler2::Adler32::new();
    adler.write_slice(&output);
    let trailer = u32::from_be_bytes([packed[packed.len() - 4], packed[packed.len() - 3], packed[packed.len() - 2], packed[packed.len() - 1]]);
    if stream.total_in() as usize != packed.len() || trailer != adler.checksum() {
        return Err(mismatch());
    }
    Ok(output)
}

/// A zlib member found in a file: where it starts, how many packed bytes
/// it spans and what it decodes to.
#[derive(Clone, Debug)]
pub(crate) struct Member {
    pub offset: i64,
    pub packed: i64,
    pub data: Vec<u8>,
}

/// Bounds for [`find_member`].
pub(crate) struct MemberLimits {
    /// Most decoded bytes this member may produce.
    pub output: i64,
    /// Most packed bytes to read after the start.
    pub input: i64,
    /// Ceiling for the running decoded-byte total of a scan.
    pub total: i64,
}

/// Inflates the zlib member that starts at `offset`, if there is one that
/// ends within the limits and decodes to at least 8 bytes. Every decoded
/// byte counts against `spent`, also for rejected members, so one scan
/// cannot be made to inflate without bound.
pub(crate) fn find_member<R: Read + Seek>(
    file: &mut R,
    file_length: i64,
    offset: i64,
    limits: MemberLimits,
    spent: &mut i64,
    cancel: &Cancel,
) -> Result<Option<Member>> {
    if limits.output < 1 || offset < 0 || offset > file_length - 6 || *spent >= limits.total {
        return Ok(None);
    }
    match read_member(file, file_length, offset, &limits, spent, cancel) {
        Err(Error::Cancelled) => Err(Error::Cancelled),
        Err(_) => Ok(None),
        Ok(found) => Ok(found),
    }
}

fn read_member<R: Read + Seek>(
    file: &mut R,
    file_length: i64,
    offset: i64,
    limits: &MemberLimits,
    spent: &mut i64,
    cancel: &Cancel,
) -> Result<Option<Member>> {
    file.seek(SeekFrom::Start(offset as u64))?;
    let mut input = vec![0u8; 64 * 1024];
    let mut budget = limits.input.min(file_length - offset).max(0) as u64;
    // Buffered readers may return short reads; the header needs two bytes.
    let mut first = 0;
    let want = (budget as usize).min(input.len());
    while first < want.min(2) {
        match file.read(&mut input[first..want])? {
            0 => break,
            n => first += n,
        }
    }
    if !is_header(&input[..first]) {
        return Ok(None);
    }
    budget -= first as u64;
    let (mut start, mut end, mut exhausted) = (0usize, first, false);
    let mut stream = Decompress::new(true);
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        cancel.check()?;
        if start == end && !exhausted {
            let want = (budget as usize).min(input.len());
            end = if want == 0 { 0 } else { file.read(&mut input[..want])? };
            budget -= end as u64;
            start = 0;
            exhausted = end == 0;
        }
        let (read, written) = (stream.total_in(), stream.total_out());
        let status = stream.decompress(&input[start..end], &mut chunk, FlushDecompress::None).map_err(|e| Error::invalid(e.to_string()))?;
        start += (stream.total_in() - read) as usize;
        let made = (stream.total_out() - written) as usize;
        *spent += made as i64;
        if *spent > limits.total || (data.len() + made) as i64 > limits.output {
            return Ok(None);
        }
        data.extend_from_slice(&chunk[..made]);
        if status == Status::StreamEnd {
            break;
        }
        if made == 0 && stream.total_in() == read && (exhausted || start != end) {
            return Ok(None);
        }
    }
    Ok((data.len() >= 8).then(|| Member { offset, packed: stream.total_in() as i64, data }))
}

/// Inflates `packed` into exactly `count` bytes. The stream must not decode
/// past `count`.
pub fn inflate_exactly(packed: &[u8], count: usize) -> Result<Vec<u8>> {
    let mut decoder = Decompress::new(true);
    let mut output = vec![0u8; count];
    let mut produced = 0usize;
    let mut ended = false;
    while produced < count {
        let consumed = decoder.total_in() as usize;
        let before = decoder.total_out();
        let status = decoder
            .decompress(&packed[consumed..], &mut output[produced..], FlushDecompress::None)
            .map_err(|e| Error::invalid(format!("Invalid zlib segment: {e}")))?;
        let made = (decoder.total_out() - before) as usize;
        produced += made;
        if status == Status::StreamEnd {
            ended = true;
            break;
        }
        if made == 0 && decoder.total_in() as usize == consumed {
            break;
        }
    }
    if produced < count {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "A compressed segment ended before its catalog length.",
        )));
    }
    if !ended {
        let mut extra = [0u8; 1];
        let consumed = decoder.total_in() as usize;
        let before = decoder.total_out();
        let status = decoder
            .decompress(&packed[consumed..], &mut extra, FlushDecompress::None)
            .map_err(|e| Error::invalid(format!("Invalid zlib segment: {e}")))?;
        if decoder.total_out() != before {
            return Err(Error::invalid("Decoded segment exceeds its catalog length."));
        }
        if status != Status::StreamEnd {
            return Err(Error::invalid("A compressed segment has no valid end."));
        }
    }
    Ok(output)
}

/// Recompresses `decoded` at levels 9 to 1 and returns the first result that
/// has exactly `required` bytes.
pub fn pack_exactly(decoded: &[u8], required: i64) -> Result<Option<Vec<u8>>> {
    for level in (1..=9).rev() {
        let mut encoder = ZlibEncoder::new(Vec::with_capacity(required.max(0) as usize), Compression::new(level));
        encoder.write_all(decoded)?;
        let packed = encoder.finish()?;
        if packed.len() as i64 == required {
            return Ok(Some(packed));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_inflate_and_repack() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i % 97) as u8).collect();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(&data).unwrap();
        let packed = encoder.finish().unwrap();
        assert_eq!(inflate_exactly(&packed, data.len()).unwrap(), data);
        assert!(inflate_exactly(&packed, data.len() - 1).is_err());
        assert!(inflate_exactly(&packed, data.len() + 1).is_err());
        assert_eq!(pack_exactly(&data, packed.len() as i64).unwrap().unwrap().len(), packed.len());
        assert!(pack_exactly(&data, 3).unwrap().is_none());
    }

    fn pack(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn limits(output: i64) -> MemberLimits {
        MemberLimits { output, input: 1 << 21, total: 1 << 30 }
    }

    #[test]
    fn members_report_their_packed_length() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let packed = pack(&data);
        let mut file = vec![0u8; 16];
        file.extend_from_slice(&packed);
        file.extend_from_slice(&[0xAB; 300]);
        let length = file.len() as i64;
        let mut spent = 0;
        let reader = &mut std::io::BufReader::with_capacity(17, std::io::Cursor::new(file));
        let member = find_member(reader, length, 16, limits(1 << 20), &mut spent, &Cancel::none()).unwrap().unwrap();
        assert_eq!((member.packed, member.data.len()), (packed.len() as i64, data.len()));
        assert_eq!(member.data, data);
        assert_eq!(spent, data.len() as i64);
    }

    #[test]
    fn members_above_the_limit_are_skipped() {
        let packed = pack(&[7u8; 50_000]);
        let length = packed.len() as i64;
        let mut spent = 0;
        let found = find_member(&mut std::io::Cursor::new(packed), length, 0, limits(49_999), &mut spent, &Cancel::none()).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn chunks_check_length_and_trailer() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 13) as u8).collect();
        let packed = pack(&data);
        let none = Cancel::none();
        assert_eq!(inflate_chunk(&packed, data.len(), &none).unwrap(), data);
        assert!(inflate_chunk(&packed, data.len() + 1, &none).is_err());
        assert!(inflate_chunk(&packed, data.len() - 1, &none).is_err());
        let mut trailing = packed.clone();
        trailing.push(0);
        assert!(inflate_chunk(&trailing, data.len(), &none).is_err());
        assert!(is_header(&packed) && !is_header(&[0x78]));
    }
}
