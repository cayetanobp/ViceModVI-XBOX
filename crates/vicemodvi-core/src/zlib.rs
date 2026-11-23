//! zlib helpers for catalog segments.

use std::io::Write;

use flate2::{write::ZlibEncoder, Compression, Decompress, FlushDecompress, Status};

use crate::{Error, Result};

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
}
