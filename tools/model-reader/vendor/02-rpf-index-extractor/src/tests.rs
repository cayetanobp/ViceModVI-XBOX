use super::*;
use flate2::{write::ZlibEncoder, Compression};
use std::io::{Cursor, Write};

fn header(archive: &mut [u8], at: usize, count: u32) {
    archive[at..at + 4].copy_from_slice(b"9FPR");
    for (field, value) in [(4, 0x80u32), (8, 0x60), (12, 0x40), (36, count)] {
        archive[at + field..at + field + 4].copy_from_slice(&value.to_le_bytes());
    }
    archive[at + 0x40..at + 0x60].copy_from_slice(&OBSERVED_INDEX_PREFIX);
}

fn put_zlib(archive: &mut Vec<u8>, at: usize, data: &[u8]) {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    let compressed = encoder.finish().unwrap();
    archive.resize(archive.len().max(at), 0);
    archive.truncate(at);
    archive.extend(compressed);
    archive.resize(archive.len().next_multiple_of(16), 0);
}

fn pso(data_size: usize) -> Vec<u8> {
    let mut bytes = b"PSIN".to_vec();
    bytes.extend_from_slice(&(data_size as u32).to_be_bytes());
    bytes.resize(data_size, 0x55);
    bytes.extend_from_slice(b"LSCH\0\0\0\x08CHKS\0\0\0\x14");
    bytes.extend_from_slice(&(data_size as u32 + 28).to_be_bytes());
    bytes.extend_from_slice(&[0; 8]);
    bytes
}

/// Minimal Bink 2 layout: fixed header, one audio track (16 bytes) and a
/// frame table whose first entry carries the keyframe bit.
fn bink(frames: u32) -> Vec<u8> {
    let table_start = 0x3c;
    let table_end = table_start + 4 * (frames as usize + 1);
    let total = table_end + 16 * frames as usize;
    let mut bytes = b"KB2n".to_vec();
    for value in [total as u32 - 8, frames, 16, frames, 16, 16, 30, 1, 0, 1] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.resize(table_start, 0);
    for k in 0..=frames as usize {
        let entry = (table_end + 16 * k) as u32 | u32::from(k == 0);
        bytes.extend_from_slice(&entry.to_le_bytes());
    }
    bytes.resize(total, 0x33);
    bytes
}

struct Sample {
    archive: Vec<u8>,
    pso: Vec<u8>,
    xml: Vec<u8>,
    large: Vec<u8>,
    bink: Vec<u8>,
}

fn sample() -> Sample {
    let mut archive = vec![0; 0x2000];
    header(&mut archive, 0, 1);
    // Five verified entries; the five opaque spans do not count towards it.
    header(&mut archive, 0x1000, 5);
    let pso = pso(100);
    let xml = b"<?xml version=\"1.0\"?><root/>".to_vec();
    let full: Vec<u8> = (0..CHUNK).map(|i| (i * 7 % 251) as u8).collect();
    put_zlib(&mut archive, 0x2000, &pso);
    put_zlib(&mut archive, 0x3000, &xml);
    put_zlib(&mut archive, 0x4000, &full);
    archive.resize(0x30000, 0);
    archive.extend_from_slice(&[0xaa; 32]);
    // Larger than the 64 KiB per-member limit of the phase 01 scanner.
    let large: Vec<u8> = (0..3 * CHUNK).map(|i| (i * 13 % 241) as u8).collect();
    put_zlib(&mut archive, 0x40000, &large);
    // The same stream with a corrupted Adler-32 trailer must stay unclaimed.
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&large).unwrap();
    let mut corrupt = encoder.finish().unwrap();
    *corrupt.last_mut().unwrap() ^= 1;
    archive.resize(0x60000, 0);
    archive.extend(corrupt);
    archive.resize(0x80000, 0);
    let bink = bink(5);
    archive.extend_from_slice(&bink);
    // A frame table that ends past the declared size must stay unclaimed.
    let mut bad = bink.clone();
    let last = 0x3c + 4 * 5;
    bad[last..last + 4].copy_from_slice(&(bink.len() as u32 + 16).to_le_bytes());
    archive.resize(0xa0000, 0);
    archive.extend_from_slice(&bad);
    // Two opaque runs less than a page apart, each after a 16-byte zero prefix.
    archive.resize(0xb0010, 0);
    archive.extend_from_slice(&[0x77; 32]);
    archive.resize(0xb1010, 0);
    archive.extend_from_slice(&[0x66; 32]);
    archive.resize(0xb2000, 0);
    Sample { archive, pso, xml, large, bink }
}

#[test]
fn inventories_entries_by_evidence_and_compares_counts() {
    let s = sample();
    let inv = inventory(&mut Cursor::new(&s.archive)).unwrap();
    assert_eq!(inv.headers.len(), 2);
    assert!(inv.headers.iter().all(|h| h.prefix_matches));
    assert_eq!(inv.headers[0].region_end, 0x1000);
    assert_eq!(inv.headers[1].index_end, 0x1080);

    let kinds: Vec<_> = inv.entries.iter().map(|e| (e.offset, e.header, e.evidence, e.signature)).collect();
    assert_eq!(kinds, [
        (0x2000, 1, Evidence::PsoFramed, "PSIN"),
        (0x3000, 1, Evidence::ZlibSequence, "XML"),
        (0x4000, 1, Evidence::ZlibUnterminated, "unknown"),
        (0x30000, 1, Evidence::OpaqueSpan, "opaque"),
        (0x40000, 1, Evidence::ZlibStream, "unknown"),
        (0x60000, 1, Evidence::OpaqueSpan, "opaque"),
        (0x80000, 1, Evidence::BinkFramed, "KB2"),
        (0xa0000, 1, Evidence::OpaqueSpan, "opaque"),
        (0xb0000, 1, Evidence::OpaqueSpan, "opaque"),
        (0xb1000, 1, Evidence::OpaqueSpan, "opaque"),
    ]);
    assert_eq!((inv.entries[6].stored_len(), inv.entries[6].chunks), (s.bink.len() as u64, 5));
    let split: Vec<_> = inv.entries[8..].iter().map(|e| (e.offset, e.stored_len())).collect();
    assert_eq!(split, [(0xb0000, 48), (0xb1000, 48)]);
    assert_eq!(inv.entries[4].decoded_len, s.large.len());
    assert_eq!(inv.verified(1), 5);
    assert_eq!((inv.status(0), inv.status(1)), ("below_count", "count_match"));
    assert_eq!(inv.found_with(1, Evidence::PsoFramed), 1);
    assert!(inv.issues.is_empty(), "{:?}", inv.issues);

    let spans: Vec<_> = inv.unclaimed.iter().map(|s| (s.header, s.offset, s.zero_prefix)).collect();
    assert_eq!(spans, [
        (1, 0x30000, false), (1, 0x60000, false), (1, 0xa0000, false), (1, 0xb0010, true), (1, 0xb1010, true),
    ]);
    let span = &inv.unclaimed[0];
    assert_eq!((span.len, span.head.as_slice()), (32, &[0xaa; 8][..]));
}

#[test]
fn phase01_scanner_misses_the_large_stream() {
    let s = sample();
    let mut offsets = Vec::new();
    rpf9::scan_zlib(&mut Cursor::new(&s.archive), |m, _| {
        offsets.push(m.offset);
        Ok(())
    }).unwrap();
    assert!(!offsets.contains(&0x40000));
    assert!(offsets.contains(&0x3000));
}

#[test]
fn extracts_decoded_and_stored_bytes() {
    let s = sample();
    let mut input = Cursor::new(&s.archive);
    let inv = inventory(&mut input).unwrap();

    let by_offset = |o| inv.find(Selector::Offset(o)).unwrap();
    assert_eq!(extract(&mut input, by_offset(0x2000), Payload::Decoded).unwrap(), s.pso);
    assert_eq!(extract(&mut input, inv.find(Selector::Ordinal(1)).unwrap(), Payload::Decoded).unwrap(), s.xml);

    let open = by_offset(0x4000);
    assert!(extract(&mut input, open, Payload::Decoded).is_err());
    let stored = extract(&mut input, open, Payload::Stored).unwrap();
    assert_eq!(stored.len() as u64, open.stored_len());
    assert_eq!(&stored[..], &s.archive[0x4000..open.stored_end as usize]);

    let large = by_offset(0x40000);
    assert_eq!(extract(&mut input, large, Payload::Decoded).unwrap(), s.large);
    assert_eq!(extract(&mut input, large, Payload::Stored).unwrap().len() as u64, large.stored_len());

    let video = by_offset(0x80000);
    assert_eq!(extract(&mut input, video, Payload::Decoded).unwrap(), s.bink);
    let opaque = by_offset(0xb0000);
    assert!(extract(&mut input, opaque, Payload::Decoded).is_err());
    assert_eq!(extract(&mut input, opaque, Payload::Stored).unwrap(), &s.archive[0xb0000..0xb0030]);

    let mut seen = Vec::new();
    extract_all(&mut input, &inv, |e, payload, bytes| {
        seen.push((e.offset, payload, bytes.len()));
        Ok(())
    }).unwrap();
    seen.sort_by_key(|&(offset, _, _)| offset);
    let (seen_opaque, seen): (Vec<_>, Vec<_>) = seen.into_iter()
        .partition(|&(o, _, _)| inv.find(Selector::Offset(o)).unwrap().evidence == Evidence::OpaqueSpan);
    assert!(seen_opaque.iter().all(|&(_, p, _)| p == Payload::Stored));
    assert_eq!(seen, [
        (0x2000, Payload::Decoded, s.pso.len()),
        (0x3000, Payload::Decoded, s.xml.len()),
        (0x4000, Payload::Stored, stored.len()),
        (0x40000, Payload::Decoded, s.large.len()),
        (0x80000, Payload::Decoded, s.bink.len()),
    ]);
    let opaque_lens: Vec<_> = seen_opaque.iter().map(|&(o, _, l)| (o, l)).collect();
    // Opaque spans are measured in whole 16-byte blocks.
    let corrupt_zlib = s.archive[0x60000..0x70000].iter().rposition(|&b| b != 0).unwrap() + 1;
    assert_eq!(opaque_lens, [
        (0x30000, 32),
        (0x60000, corrupt_zlib.next_multiple_of(16)),
        (0xa0000, s.bink.len().next_multiple_of(16)),
        (0xb0000, 48),
        (0xb1000, 48),
    ]);
}

#[test]
fn flags_entries_outside_their_region() {
    let mut s = sample();
    // Grow the nested table so that it covers the PSO at 0x2000.
    s.archive[0x1004..0x1008].copy_from_slice(&0x1100u32.to_le_bytes());
    let inv = inventory(&mut Cursor::new(&s.archive)).unwrap();
    assert_eq!(inv.issues.len(), 1, "{:?}", inv.issues);
    assert!(inv.issues[0].contains("entry 0"));
}

#[test]
fn entropy_bounds() {
    assert_eq!(entropy(&[]), 0.0);
    assert_eq!(entropy(&[7; 64]), 0.0);
    let all: Vec<u8> = (0..=255).collect();
    assert!((entropy(&all) - 8.0).abs() < 1e-9);
}

#[test]
fn probe_flushes_pending_output_at_exact_limit() {
    let mut state = 0x1234_5678u32;
    for len in [CHUNK, 2 * CHUNK, 3 * CHUNK + 17, 9 * CHUNK] {
        let data: Vec<u8> = (0..len).map(|i| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if i % 3 == 0 { (state >> 24) as u8 } else { b'a' + (i % 7) as u8 }
        }).collect();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&data).unwrap();
        let compressed = encoder.finish().unwrap();
        let end = compressed.len() as u64;
        let mut sink = Vec::new();
        let stream = probe_stream(&mut Cursor::new(&compressed), 0, end, Some(&mut sink)).unwrap()
            .unwrap_or_else(|| panic!("stream of {len} bytes not decoded at its exact limit"));
        assert_eq!((stream.end, stream.decoded_len), (end, len));
        assert_eq!(sink, data);
        assert!(probe_stream(&mut Cursor::new(&compressed), 0, end - 1, None).unwrap().is_none());
    }
}

/// 16 zero bytes followed by `len` non-zero bytes starting with `lead`.
fn prefixed(archive: &mut Vec<u8>, at: usize, lead: [u8; 2], len: usize) {
    archive.resize(at + 16, 0);
    archive.extend_from_slice(&lead);
    archive.extend((2..len).map(|i| (i * 37 % 251) as u8 + 1));
}

#[test]
fn groups_zero_prefixed_spans_into_families() {
    let mut archive = vec![0; 0x2000];
    header(&mut archive, 0, 1);
    header(&mut archive, 0x1000, 4);
    let family = [0xab, 0xcd];
    prefixed(&mut archive, 0x2000, family, 64);
    prefixed(&mut archive, 0x3000, family, 64);
    // Ends in a zero byte that belongs to the content.
    prefixed(&mut archive, 0x4000, family, 64);
    *archive.last_mut().unwrap() = 0;
    // Stored right after the previous entry, not at a page.
    prefixed(&mut archive, 0x4050, family, 48);
    // Zero-prefixed but with leading bytes shared by no other span.
    prefixed(&mut archive, 0x5000, [0x12, 0x34], 32);
    archive.resize(0x6000, 0);

    let inv = inventory(&mut Cursor::new(&archive)).unwrap();
    assert!(inv.issues.is_empty(), "{:?}", inv.issues);
    assert_eq!(inv.families, [(family, 3)]);
    let got: Vec<_> = inv.entries.iter().map(|e| (e.offset, e.stored_len(), e.evidence)).collect();
    assert_eq!(got, [
        (0x2000, 80, Evidence::EncryptedDelimited),
        (0x3000, 80, Evidence::EncryptedDelimited),
        (0x4000, 80, Evidence::EncryptedDelimited),
        (0x4050, 64, Evidence::EncryptedDelimited),
        (0x5000, 48, Evidence::OpaqueSpan),
    ]);
    let exact: Vec<_> = inv.unclaimed.iter().map(|s| (s.offset, s.exact_len)).collect();
    assert_eq!(exact, [(0x2010, 64), (0x3010, 64), (0x4010, 63), (0x4060, 48), (0x5010, 32)]);
    assert_eq!((inv.verified(1), inv.delimited(1)), (0, 4));
    assert_eq!((inv.status(1), inv.delimited_status(1)), ("below_count", "count_match"));

    let mut input = Cursor::new(&archive);
    let entry = inv.find(Selector::Offset(0x4050)).unwrap();
    assert!(extract(&mut input, entry, Payload::Decoded).is_err());
    assert_eq!(extract(&mut input, entry, Payload::Stored).unwrap(), &archive[0x4050..0x4090]);
    let mut payloads = Vec::new();
    extract_all(&mut input, &inv, |e, p, _| { payloads.push((e.evidence, p)); Ok(()) }).unwrap();
    assert!(payloads.iter().all(|&(_, p)| p == Payload::Stored));
}

#[test]
fn embedded_split_streams_across_chunk_boundary() {
    struct Bounded(Cursor<Vec<u8>>);
    impl Read for Bounded {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if buffer.len() > 1 << 20 {
                return Err(io::Error::other("read exceeds the span buffer"));
            }
            self.0.read(buffer)
        }
    }
    impl Seek for Bounded {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.0.seek(position)
        }
    }

    let boundary = 1 << 20;
    let mut bytes = vec![0x77; boundary + 16];
    bytes[boundary - 16..boundary].fill(0);
    bytes[..2].copy_from_slice(&[0x12, 0x34]);
    bytes[boundary..boundary + 2].copy_from_slice(&[0x12, 0x34]);
    bytes[boundary + 5..].fill(0);
    let span = Span {
        header: 0, offset: 0, len: bytes.len() as u64,
        head: bytes[..8].to_vec(), zero_prefix: true, exact_len: boundary as u64 + 5,
    };
    let families = BTreeMap::from([([0x12, 0x34], 3)]);
    let pieces = split_embedded(&mut Bounded(Cursor::new(bytes)), vec![span], &families, |_, _| {}).unwrap();
    assert_eq!(pieces.len(), 2);
    assert_eq!((pieces[0].offset, pieces[0].len), (0, boundary as u64 - 16));
    assert_eq!((pieces[1].offset, pieces[1].len, pieces[1].exact_len), (boundary as u64, 16, 5));
    assert_eq!(&pieces[1].head[..2], &[0x12, 0x34]);
}

#[test]
fn excessive_embedded_boundaries_keep_the_original_span() {
    let mut bytes = Vec::new();
    for _ in 0..MAX_SPLITS_PER_SPAN + 2 {
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&[0x12, 0x34, 0x77, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
    let span = Span {
        header: 0, offset: 16, len: bytes.len() as u64 - 16,
        head: bytes[16..24].to_vec(), zero_prefix: true, exact_len: bytes.len() as u64 - 29,
    };
    let families = BTreeMap::from([([0x12, 0x34], 3)]);
    let pieces = split_embedded(&mut Cursor::new(bytes), vec![span.clone()], &families, |_, _| {}).unwrap();
    assert_eq!(pieces.len(), 1);
    assert_eq!((pieces[0].offset, pieces[0].len, pieces[0].exact_len),
        (span.offset, span.len, span.exact_len));
}
