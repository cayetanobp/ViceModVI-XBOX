//! Physical inventory of observed `9FPR` (RPF9) archives.
//!
//! The table that follows every `9FPR` header could not be read: it starts
//! with a constant 32-byte block ([`OBSERVED_INDEX_PREFIX`]) and the rest has
//! near-maximal entropy, no repeated 16-byte blocks and no keystream reuse
//! between headers. This crate therefore does not decode the index. Instead it
//! locates entries from their own bytes, attributes each one to the header
//! whose region contains it, and compares the result with the entry count the
//! header declares. Ordinals are positions in this inventory, not indices of
//! the original table, and no original names are recovered.

use flate2::{Decompress, FlushDecompress, Status};
use rpf_archive::rpf9;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Seek, SeekFrom};

pub const PAGE: u64 = 4096;
const CHUNK: usize = 64 * 1024;
const ALIGN: u64 = 16;
const HEADER_LEN: usize = 64;
const PREFIX_LEN: usize = 32;
/// Largest decoded size accepted for one zlib stream found in unclaimed bytes.
pub const MAX_STREAM_OUTPUT: u64 = 256 << 20;
const PROBE_FIRST_READ: usize = 4096;
const PROBE_READ: usize = 256 * 1024;
/// Leading bytes that group zero-prefixed spans into families.
pub const FAMILY_PREFIX: usize = 2;
/// Page-aligned zero-prefixed spans that must share their leading bytes for
/// those bytes to define a family. With 744 spans of random content, three
/// sharing the same two bytes would be expected about 0.016 times.
pub const FAMILY_MIN: usize = 3;

/// Bytes found at `header + 0x40` in every root and nested header of the
/// three samples examined. Their meaning is unknown.
pub const OBSERVED_INDEX_PREFIX: [u8; PREFIX_LEN] = [
    0x5f, 0x08, 0xda, 0xab, 0xec, 0xbd, 0xd4, 0x02, 0x58, 0xc3, 0x51, 0x68, 0x07, 0x02, 0x4c, 0x24,
    0xfb, 0x9f, 0x75, 0x67, 0x55, 0xef, 0x4b, 0xe4, 0x58, 0x78, 0x5b, 0xfe, 0xd8, 0xaa, 0x69, 0x89,
];

#[derive(Debug, Clone)]
pub struct HeaderInfo {
    pub id: usize,
    pub offset: u64,
    /// `offset + word_04`: end of the header and its table.
    pub index_end: u64,
    pub word_08: u32,
    pub word_0c: u32,
    /// `word_24`. Equals the number of recovered PSO objects in the two
    /// PSO-bearing headers measured so far.
    pub declared_entries: u32,
    pub raw: [u8; HEADER_LEN],
    pub prefix_matches: bool,
    /// Shannon entropy in bits per byte of `[offset + 0x60, index_end)`.
    pub index_entropy: f64,
    /// Start of the next header, or the end of the file.
    pub region_end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// A PSO object whose sections and footer validate (`rpf9::recover_psos`).
    PsoFramed,
    /// Contiguous checksum-valid zlib members ending in a short member.
    ZlibSequence,
    /// Contiguous zlib members whose last member decodes to a full 64 KiB
    /// chunk not followed by a contiguous member. Its end is not verified.
    ZlibUnterminated,
    /// One checksum-valid zlib stream found in bytes left unclaimed by the
    /// passes above. Its decoded size may exceed the 64 KiB per-member limit of
    /// the phase 01 scanner, up to [`MAX_STREAM_OUTPUT`].
    ZlibStream,
    /// A Bink or Bink 2 video (`BIK?` / `KB2?`) found in unclaimed bytes. Its
    /// frame table must start where its first entry points, be non-decreasing
    /// and end exactly at the size declared in the header.
    BinkFramed,
    /// A run that follows 16 zero bytes and starts with the leading bytes of a
    /// family ([`FAMILY_MIN`] or more page-aligned zero-prefixed spans that
    /// share them). Its bounds are delimited by that prefix and by the next
    /// entry; its content is not verified and looks encrypted. Extracted as
    /// stored only.
    EncryptedDelimited,
    /// A delimited run of bytes with no verified format, kept so that every
    /// non-zero byte belongs to an entry. It may hold several original entries
    /// or part of one, and can only be extracted as stored.
    OpaqueSpan,
}

impl Evidence {
    /// True when the format and extent were checked: framing, checksums or
    /// frame tables. False for opaque spans.
    pub fn verified(self) -> bool {
        !matches!(self, Evidence::OpaqueSpan | Evidence::EncryptedDelimited)
    }

    /// True for verified entries and for encrypted entries delimited by their
    /// zero prefix and family.
    pub fn delimited(self) -> bool {
        self != Evidence::OpaqueSpan
    }

    /// Entries whose content can only be extracted as stored.
    pub fn stored_only(self) -> bool {
        matches!(self, Evidence::ZlibUnterminated | Evidence::OpaqueSpan | Evidence::EncryptedDelimited)
    }
}

impl Evidence {
    pub fn label(self) -> &'static str {
        match self {
            Evidence::PsoFramed => "pso_framed",
            Evidence::ZlibSequence => "zlib_sequence",
            Evidence::ZlibUnterminated => "zlib_unterminated",
            Evidence::ZlibStream => "zlib_stream",
            Evidence::BinkFramed => "bink_framed",
            Evidence::EncryptedDelimited => "encrypted_delimited",
            Evidence::OpaqueSpan => "opaque_span",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub ordinal: usize,
    pub header: usize,
    pub offset: u64,
    pub stored_end: u64,
    pub decoded_len: usize,
    /// zlib members plus 64 KiB pages stored without compression; frames for
    /// Bink videos.
    pub chunks: usize,
    pub evidence: Evidence,
    pub signature: &'static str,
}

impl Entry {
    pub fn stored_len(&self) -> u64 {
        self.stored_end - self.offset
    }
}

/// Non-zero bytes inside a header region with no verified format. Runs closer
/// than one page are merged, except that a run is split where a page starts
/// with 16 zero bytes followed by data: the prefix observed on the encrypted
/// entries of story000_script.rpf. A span of a family is also split where a
/// zero block is followed by the leading bytes of a family. Each span becomes
/// an `EncryptedDelimited` or `OpaqueSpan` entry.
#[derive(Debug, Clone)]
pub struct Span {
    pub header: usize,
    pub offset: u64,
    pub len: u64,
    /// The first 8 bytes of the span, after any zero prefix.
    pub head: Vec<u8>,
    /// The span follows a 16-byte zero block: at the start of a page, or
    /// inside a family span when it starts with a family prefix.
    pub zero_prefix: bool,
    /// Bytes up to and including the last non-zero byte. `len` rounds it up
    /// to 16.
    pub exact_len: u64,
}

impl Span {
    pub fn family(&self) -> Option<[u8; FAMILY_PREFIX]> {
        self.head.get(..FAMILY_PREFIX).map(|b| [b[0], b[1]])
    }
}

#[derive(Debug, Clone)]
pub struct Inventory {
    pub file_len: u64,
    pub headers: Vec<HeaderInfo>,
    pub entries: Vec<Entry>,
    pub unclaimed: Vec<Span>,
    /// Leading bytes shared by at least [`FAMILY_MIN`] page-aligned
    /// zero-prefixed spans, with that count.
    pub families: Vec<([u8; FAMILY_PREFIX], usize)>,
    /// Bounds or overlap violations. Empty when every entry lies inside its
    /// header region, after the table, without overlapping another entry.
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    Ordinal(usize),
    Offset(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    Decoded,
    Stored,
}

impl Inventory {
    pub fn found(&self, header: usize) -> usize {
        self.entries.iter().filter(|e| e.header == header).count()
    }

    pub fn verified(&self, header: usize) -> usize {
        self.entries.iter().filter(|e| e.header == header && e.evidence.verified()).count()
    }

    /// Verified entries plus encrypted delimited ones.
    pub fn delimited(&self, header: usize) -> usize {
        self.entries.iter().filter(|e| e.header == header && e.evidence.delimited()).count()
    }

    pub fn found_with(&self, header: usize, evidence: Evidence) -> usize {
        self.entries.iter().filter(|e| e.header == header && e.evidence == evidence).count()
    }

    /// Compares the number of verified entries (all but opaque spans) with
    /// `word_24`. A match is only a count comparison: zlib sequences are
    /// compression units and an original entry may span several of them.
    pub fn status(&self, header: usize) -> &'static str {
        compare(self.verified(header), self.headers[header].declared_entries)
    }

    /// The same comparison counting encrypted delimited entries as well.
    pub fn delimited_status(&self, header: usize) -> &'static str {
        compare(self.delimited(header), self.headers[header].declared_entries)
    }

    pub fn find(&self, selector: Selector) -> Option<&Entry> {
        match selector {
            Selector::Ordinal(n) => self.entries.get(n),
            Selector::Offset(o) => self.entries.iter().find(|e| e.offset == o),
        }
    }
}

fn compare(found: usize, declared: u32) -> &'static str {
    match found.cmp(&(declared as usize)) {
        std::cmp::Ordering::Equal => "count_match",
        std::cmp::Ordering::Less => "below_count",
        std::cmp::Ordering::Greater => "above_count",
    }
}

pub fn entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let n = bytes.len() as f64;
    counts.iter().filter(|&&c| c > 0).map(|&c| {
        let p = c as f64 / n;
        -p * p.log2()
    }).sum()
}

fn signature(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"PSIN") {
        "PSIN"
    } else if bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes).starts_with(b"<?xml") {
        "XML"
    } else if bytes.starts_with(b"SPS1") {
        "SPS1"
    } else {
        "unknown"
    }
}

fn align16(value: u64) -> u64 {
    value.next_multiple_of(ALIGN)
}

fn read_headers<R: Read + Seek>(
    input: &mut R, file_len: u64, progress: impl FnMut(u64, u64),
) -> io::Result<Vec<HeaderInfo>> {
    let mut found = Vec::new();
    rpf9::inspect_with_progress(input, |h| found.push(h), progress)?;
    let mut headers = Vec::with_capacity(found.len());
    for (id, h) in found.iter().enumerate() {
        let mut raw = [0; HEADER_LEN];
        input.seek(SeekFrom::Start(h.offset))?;
        input.read_exact(&mut raw)?;
        let mut table = vec![0; h.word_04 as usize - HEADER_LEN];
        input.read_exact(&mut table)?;
        let prefix_matches = table.get(..PREFIX_LEN) == Some(&OBSERVED_INDEX_PREFIX[..]);
        headers.push(HeaderInfo {
            id,
            offset: h.offset,
            index_end: h.offset + u64::from(h.word_04),
            word_08: h.word_08,
            word_0c: h.word_0c,
            declared_entries: h.word_24,
            raw,
            prefix_matches,
            index_entropy: entropy(table.get(PREFIX_LEN..).unwrap_or_default()),
            region_end: found.get(id + 1).map_or(file_len, |next| next.offset),
        });
    }
    Ok(headers)
}

struct OpenSequence {
    offset: u64,
    end: u64,
    decoded_len: usize,
    chunks: usize,
    signature: &'static str,
    /// The last member is a full chunk followed by zero padding.
    open: bool,
    /// The last member decoded to fewer than 64 KiB.
    terminated: bool,
}

/// Builds the inventory with sequential passes over the archive: PSO
/// recovery, zlib member grouping, a scan for unclaimed bytes, a search for
/// larger zlib streams and Bink videos inside those bytes and, if any are
/// found, a second scan for unclaimed bytes. The spans left at the end are
/// grouped into families by their leading bytes, family spans holding several
/// entries are split, and each span is added as an `EncryptedDelimited` or
/// `OpaqueSpan` entry.
pub fn inventory<R: Read + Seek>(input: &mut R) -> io::Result<Inventory> {
    inventory_with_progress(input, |_, _, _| {})
}

pub fn inventory_with_progress<R: Read + Seek>(
    input: &mut R, mut progress: impl FnMut(&'static str, u64, u64),
) -> io::Result<Inventory> {
    let file_len = input.seek(SeekFrom::End(0))?;
    progress("Scanning archive headers", 0, file_len);
    let headers = read_headers(input, file_len, |done, total|
        progress("Scanning archive headers", done, total))?;

    let mut entries = Vec::new();
    progress("Scanning PSO blocks", 0, file_len);
    rpf9::recover_psos_with_progress(input, |p, _| {
        entries.push(Entry {
            ordinal: 0, header: 0, offset: p.offset, stored_end: p.compressed_end,
            decoded_len: p.decoded_len, chunks: p.chunks,
            evidence: Evidence::PsoFramed, signature: "PSIN",
        });
        Ok(())
    }, |done, total| progress("Scanning PSO blocks", done, total))?;
    let pso_extents: Vec<(u64, u64)> = entries.iter().map(|e| (e.offset, e.stored_end)).collect();
    let inside_pso = |offset: u64| {
        let i = pso_extents.partition_point(|&(start, _)| start <= offset);
        i > 0 && offset < pso_extents[i - 1].1
    };

    let mut current: Option<OpenSequence> = None;
    let close = |seq: OpenSequence, entries: &mut Vec<Entry>| {
        entries.push(Entry {
            ordinal: 0, header: 0, offset: seq.offset, stored_end: seq.end,
            decoded_len: seq.decoded_len, chunks: seq.chunks,
            evidence: if seq.terminated { Evidence::ZlibSequence } else { Evidence::ZlibUnterminated },
            signature: seq.signature,
        });
    };
    progress("Scanning compressed blocks", 0, file_len);
    rpf9::scan_zlib_with_progress(input, |member, bytes| {
        if inside_pso(member.offset) {
            return Ok(());
        }
        let continues = current.as_ref()
            .is_some_and(|s| s.open && member.offset == align16(s.end));
        if continues {
            let seq = current.as_mut().unwrap();
            seq.end = member.compressed_end;
            seq.decoded_len += member.decoded_len;
            seq.chunks += 1;
            seq.open = member.decoded_len == CHUNK && member.zero_alignment_padding;
            seq.terminated = member.decoded_len < CHUNK;
        } else {
            if let Some(seq) = current.take() {
                close(seq, &mut entries);
            }
            current = Some(OpenSequence {
                offset: member.offset,
                end: member.compressed_end,
                decoded_len: member.decoded_len,
                chunks: 1,
                signature: signature(bytes),
                open: member.decoded_len == CHUNK && member.zero_alignment_padding,
                terminated: member.decoded_len < CHUNK,
            });
        }
        if current.as_ref().is_some_and(|s| s.terminated) {
            close(current.take().unwrap(), &mut entries);
        }
        Ok(())
    }, |done, total| progress("Scanning compressed blocks", done, total))?;
    if let Some(seq) = current.take() {
        close(seq, &mut entries);
    }

    let mut entries = assign(entries, &headers);
    let mut unclaimed = all_unclaimed(input, &headers, &entries,
        |done, total| progress("Finding unclaimed ranges", done, total))?;
    let streams = find_framed(input, &headers, &entries, &unclaimed,
        |done, total| progress("Checking remaining ranges", done, total))?;
    if !streams.is_empty() {
        entries.extend(streams);
        entries = assign(entries, &headers);
        unclaimed = all_unclaimed(input, &headers, &entries,
            |done, total| progress("Updating unclaimed ranges", done, total))?;
    }
    let families = families(&unclaimed);
    let unclaimed = split_embedded(input, unclaimed, &families,
        |done, total| progress("Splitting embedded ranges", done, total))?;
    entries.extend(unclaimed.iter().map(|span| {
        let offset = span.offset - if span.zero_prefix { ALIGN } else { 0 };
        let stored_end = span.offset + span.len;
        let encrypted = span.zero_prefix && span.family().is_some_and(|f| families.contains_key(&f));
        Entry {
            ordinal: 0, header: span.header, offset, stored_end,
            decoded_len: (stored_end - offset) as usize, chunks: 0,
            evidence: if encrypted { Evidence::EncryptedDelimited } else { Evidence::OpaqueSpan },
            signature: if encrypted { "encrypted" } else { "opaque" },
        }
    }));
    let entries = assign(entries, &headers);
    let issues = check(&headers, &entries);
    let families = families.into_iter().collect();
    Ok(Inventory { file_len, headers, entries, unclaimed, families, issues })
}

/// Leading bytes shared by at least [`FAMILY_MIN`] spans that start 16 bytes
/// into a page after a zero block.
fn families(spans: &[Span]) -> BTreeMap<[u8; FAMILY_PREFIX], usize> {
    let mut counts = BTreeMap::new();
    for family in spans.iter().filter(|s| s.zero_prefix).filter_map(Span::family) {
        *counts.entry(family).or_insert(0) += 1;
    }
    counts.retain(|_, n| *n >= FAMILY_MIN);
    counts
}

const MAX_SPLITS_PER_SPAN: usize = 65_536;
const MAX_EMBEDDED_SPLITS: usize = 262_144;

// A repeating byte pattern is weak evidence. Retain the original span if
// accepting its inferred boundaries would exceed either metadata budget.
/// Splits family spans where a 16-byte zero block is followed by a block that
/// starts with a family prefix: an entry stored right after the previous one
/// instead of at the next page.
fn split_embedded<R: Read + Seek>(
    input: &mut R,
    spans: Vec<Span>,
    families: &BTreeMap<[u8; FAMILY_PREFIX], usize>,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<Vec<Span>> {
    let prefixes: BTreeSet<_> = families.keys().copied().collect();
    let total = spans.iter().filter(|span| span.zero_prefix &&
        span.family().is_some_and(|f| prefixes.contains(&f))).map(|span| span.len).sum();
    let mut meter = ByteProgress::new(total, &mut progress);
    let mut done = 0;
    let mut buffer = vec![0; 1 << 20];
    let mut out = Vec::with_capacity(spans.len());
    let mut split_budget = MAX_EMBEDDED_SPLITS;
    'spans: for span in spans {
        if !(span.zero_prefix && span.family().is_some_and(|f| prefixes.contains(&f))) {
            out.push(span);
            continue;
        }
        let start = out.len();
        if split_budget == 0 {
            done += span.len;
            meter.report(done);
            out.push(span);
            continue;
        }
        input.seek(SeekFrom::Start(span.offset))?;
        let mut remaining = span.len;
        let mut position = span.offset;
        let mut piece = Span {
            header: span.header, offset: span.offset, len: 0,
            head: Vec::new(), zero_prefix: true, exact_len: 0,
        };
        let mut zero_before = false;
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            input.read_exact(&mut buffer[..count])?;
            for block in buffer[..count].chunks(ALIGN as usize) {
                if position - span.offset >= 2 * ALIGN && zero_before &&
                    prefixes.contains(&[block[0], block.get(1).copied().unwrap_or(0)]) {
                    if out.len() - start >= split_budget.min(MAX_SPLITS_PER_SPAN) {
                        out.truncate(start);
                        done += remaining;
                        meter.report(done);
                        out.push(span);
                        continue 'spans;
                    }
                    let next = Span {
                        header: span.header, offset: position, len: 0,
                        head: Vec::new(), zero_prefix: true, exact_len: 0,
                    };
                    piece.len = align16(piece.exact_len);
                    out.push(std::mem::replace(&mut piece, next));
                }
                if position == piece.offset {
                    piece.head.extend_from_slice(&block[..block.len().min(8)]);
                }
                let last = block.iter().rposition(|&byte| byte != 0);
                if let Some(last) = last {
                    piece.exact_len = position - piece.offset + last as u64 + 1;
                }
                zero_before = last.is_none();
                position += block.len() as u64;
            }
            remaining -= count as u64;
            done += count as u64;
            meter.report(done);
        }
        split_budget -= out.len() - start;
        piece.len = align16(piece.exact_len);
        out.push(piece);
    }
    Ok(out)
}

/// Sorts entries by offset, numbers them and attributes each to the last
/// header that starts at or before it.
fn assign(mut entries: Vec<Entry>, headers: &[HeaderInfo]) -> Vec<Entry> {
    entries.sort_by_key(|e| e.offset);
    for (ordinal, entry) in entries.iter_mut().enumerate() {
        entry.ordinal = ordinal;
        entry.header = headers.partition_point(|h| h.offset <= entry.offset).saturating_sub(1);
    }
    entries
}

fn check(headers: &[HeaderInfo], entries: &[Entry]) -> Vec<String> {
    let mut issues = Vec::new();
    for entry in entries {
        let h = &headers[entry.header];
        if entry.offset < h.index_end || entry.stored_end > h.region_end {
            issues.push(format!(
                "entry {} [{:#x}, {:#x}) outside header {} data region [{:#x}, {:#x})",
                entry.ordinal, entry.offset, entry.stored_end, h.id, h.index_end, h.region_end
            ));
        }
    }
    for pair in entries.windows(2) {
        if pair[1].offset < pair[0].stored_end {
            issues.push(format!("entries {} and {} overlap", pair[0].ordinal, pair[1].ordinal));
        }
    }
    issues
}

struct ByteProgress<F> {
    total: u64,
    next: u64,
    callback: F,
}

impl<F: FnMut(u64, u64)> ByteProgress<F> {
    fn new(total: u64, mut callback: F) -> Self {
        callback(0, total);
        Self { total, next: 32 * 1024 * 1024, callback }
    }

    fn report(&mut self, done: u64) {
        if self.next != u64::MAX && (done >= self.next || done == self.total) {
            (self.callback)(done, self.total);
            self.next = if done == self.total { u64::MAX } else { done + 32 * 1024 * 1024 };
        }
    }
}

fn all_unclaimed<R: Read + Seek>(
    input: &mut R, headers: &[HeaderInfo], entries: &[Entry], progress: impl FnMut(u64, u64),
) -> io::Result<Vec<Span>> {
    let total = headers.iter().map(|h| h.region_end.saturating_sub(h.index_end)).sum();
    let mut meter = ByteProgress::new(total, progress);
    let mut done = 0;
    let mut unclaimed = Vec::new();
    for h in headers {
        let claimed: Vec<(u64, u64)> = entries.iter().filter(|e| e.header == h.id)
            .map(|e| (e.offset, align16(e.stored_end))).collect();
        unclaimed_spans(input, h, &claimed, &mut unclaimed, |bytes| {
            done += bytes;
            meter.report(done);
        })?;
    }
    Ok(unclaimed)
}

fn is_zlib_header(cmf: u8, flg: u8) -> bool {
    cmf == 0x78 && flg & 0x20 == 0 && u16::from_be_bytes([cmf, flg]).is_multiple_of(31)
}

/// Searches unclaimed spans, at 16-byte alignment, for complete zlib streams
/// and Bink videos. An object may end past its span but not past the next
/// entry or the end of its header region.
fn find_framed<R: Read + Seek>(
    input: &mut R,
    headers: &[HeaderInfo],
    entries: &[Entry],
    spans: &[Span],
    progress: impl FnMut(u64, u64),
) -> io::Result<Vec<Entry>> {
    let mut meter = ByteProgress::new(spans.iter().map(|span| span.len).sum(), progress);
    let mut done = 0;
    let mut found = Vec::new();
    let mut window = vec![0; 1 << 20];
    let mut resume = 0;
    for span in spans {
        let region_end = headers[span.header].region_end;
        let next_entry = entries.partition_point(|e| e.offset <= span.offset);
        let limit = entries.get(next_entry).map_or(region_end, |e| e.offset.min(region_end));
        let end = span.offset + span.len;
        let mut position = span.offset.max(resume);
        let (mut window_start, mut window_len) = (0u64, 0usize);
        while position < end {
            if position < window_start || position + 2 > window_start + window_len as u64 {
                meter.report(done + position - span.offset);
                window_start = position;
                window_len = (end - position).min(window.len() as u64) as usize;
                if window_len < 2 {
                    break;
                }
                input.seek(SeekFrom::Start(position))?;
                input.read_exact(&mut window[..window_len])?;
            }
            let i = (position - window_start) as usize;
            let mut hit = None;
            if is_zlib_header(window[i], window[i + 1]) {
                hit = probe_stream(input, position, limit, None)?.map(|stream| Entry {
                    ordinal: 0, header: span.header, offset: position, stored_end: stream.end,
                    decoded_len: stream.decoded_len, chunks: 1,
                    evidence: Evidence::ZlibStream, signature: signature(&stream.head),
                });
            } else if bink_signature(&window[i..window_len]).is_some() {
                hit = probe_bink(input, position, limit)?.map(|bink| Entry {
                    ordinal: 0, header: span.header, offset: position, stored_end: position + bink.len,
                    decoded_len: bink.len as usize, chunks: bink.frames as usize,
                    evidence: Evidence::BinkFramed, signature: bink.signature,
                });
            }
            if let Some(entry) = hit {
                position = align16(entry.stored_end);
                resume = position;
                window_len = 0;
                found.push(entry);
                continue;
            }
            position += ALIGN;
        }
        done += span.len;
        meter.report(done);
    }
    Ok(found)
}

const BINK_FIXED_HEADER: usize = 0x2c;
const MAX_BINK_FRAMES: u32 = 1 << 20;
const MAX_BINK_TRACKS: u32 = 16;

fn bink_signature(bytes: &[u8]) -> Option<&'static str> {
    let name = match bytes.get(..3)? {
        b"BIK" => "BIK",
        b"KB2" => "KB2",
        _ => return None,
    };
    bytes.get(3).is_some_and(u8::is_ascii_alphabetic).then_some(name)
}

struct Bink {
    len: u64,
    frames: u32,
    signature: &'static str,
}

/// Validates a Bink header at `offset`. The fixed header holds the size minus
/// 8 at 0x04, the frame count at 0x08 and the audio track count at 0x28. The
/// frame table follows the per-track fields, whose length differs between
/// Bink and Bink 2, so its start is searched for: it is the position whose
/// first entry, without the keyframe bit, points just past a table of
/// `frames + 1` entries. Every entry must be non-decreasing and the last one
/// must equal the declared size.
fn probe_bink<R: Read + Seek>(input: &mut R, offset: u64, limit: u64) -> io::Result<Option<Bink>> {
    let room = limit.saturating_sub(offset);
    let mut fixed = [0; BINK_FIXED_HEADER];
    if room < fixed.len() as u64 {
        return Ok(None);
    }
    input.seek(SeekFrom::Start(offset))?;
    input.read_exact(&mut fixed)?;
    let Some(signature) = bink_signature(&fixed) else { return Ok(None) };
    let word = |bytes: &[u8], at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let len = u64::from(word(&fixed, 4)) + 8;
    let frames = word(&fixed, 8);
    let tracks = word(&fixed, 0x28);
    if frames == 0 || frames > MAX_BINK_FRAMES || tracks > MAX_BINK_TRACKS || len > room {
        return Ok(None);
    }
    let table_len = 4 * (u64::from(frames) + 1);
    let search_len = (16 * tracks + 20) as usize;
    if (BINK_FIXED_HEADER + search_len) as u64 > len {
        return Ok(None);
    }
    let mut after = vec![0; search_len];
    input.read_exact(&mut after)?;
    let table_start = (0..search_len - 3).step_by(4).map(|i| (i, BINK_FIXED_HEADER + i))
        .find(|&(i, start)| u64::from(word(&after, i) & !1) == start as u64 + table_len)
        .map(|(_, start)| start as u64);
    let Some(table_start) = table_start else { return Ok(None) };
    if table_start + table_len > len {
        return Ok(None);
    }
    let mut table = vec![0; table_len as usize];
    input.seek(SeekFrom::Start(offset + table_start))?;
    input.read_exact(&mut table)?;
    let mut previous = 0;
    for entry in table.as_chunks::<4>().0 {
        let value = u64::from(u32::from_le_bytes(*entry) & !1);
        if value < previous {
            return Ok(None);
        }
        previous = value;
    }
    Ok((previous == len).then_some(Bink { len, frames, signature }))
}

struct Stream {
    end: u64,
    decoded_len: usize,
    head: Vec<u8>,
}

/// Decodes one zlib stream starting at `offset` without reading at or past
/// `limit`. Returns `None` unless the stream ends, with a valid Adler-32
/// checksum, after producing between 1 byte and [`MAX_STREAM_OUTPUT`].
/// Decoded bytes are appended to `sink` when one is given.
fn probe_stream<R: Read + Seek>(
    input: &mut R,
    offset: u64,
    limit: u64,
    mut sink: Option<&mut Vec<u8>>,
) -> io::Result<Option<Stream>> {
    let mut decoder = Decompress::new(true);
    let mut compressed = vec![0; PROBE_READ];
    let mut decoded = vec![0; CHUNK];
    let mut head = Vec::new();
    let (mut next, mut start, mut available) = (offset, 0usize, 0usize);
    loop {
        // With no input left the decoder may still hold output to flush.
        let exhausted = start == available && next >= limit;
        if start == available && !exhausted {
            let want = if next == offset { PROBE_FIRST_READ } else { PROBE_READ };
            available = (limit - next).min(want as u64) as usize;
            start = 0;
            input.seek(SeekFrom::Start(next))?;
            input.read_exact(&mut compressed[..available])?;
            next += available as u64;
        }
        let (in_before, out_before) = (decoder.total_in(), decoder.total_out());
        let Ok(status) = decoder.decompress(&compressed[start..available], &mut decoded, FlushDecompress::None) else {
            return Ok(None);
        };
        let consumed = (decoder.total_in() - in_before) as usize;
        let produced = (decoder.total_out() - out_before) as usize;
        start += consumed;
        if head.len() < 16 {
            let n = (16 - head.len()).min(produced);
            head.extend_from_slice(&decoded[..n]);
        }
        if let Some(sink) = sink.as_mut() {
            sink.extend_from_slice(&decoded[..produced]);
        }
        if decoder.total_out() > MAX_STREAM_OUTPUT {
            return Ok(None);
        }
        match status {
            Status::StreamEnd => {
                return Ok((decoder.total_out() > 0).then(|| Stream {
                    end: offset + decoder.total_in(),
                    decoded_len: decoder.total_out() as usize,
                    head,
                }));
            }
            _ if consumed == 0 && produced == 0 && (exhausted || start < available) => return Ok(None),
            _ => {}
        }
    }
}

fn unclaimed_spans<R: Read + Seek>(
    input: &mut R,
    header: &HeaderInfo,
    claimed: &[(u64, u64)],
    out: &mut Vec<Span>,
    mut on_bytes: impl FnMut(u64),
) -> io::Result<()> {
    let (start, end) = (header.index_end, header.region_end);
    if start >= end {
        return Ok(());
    }
    input.seek(SeekFrom::Start(start))?;
    let mut buffer = vec![0; 1 << 20];
    let mut next_claim = 0;
    let mut run: Option<Span> = None;
    let mut flush = |run: &mut Option<Span>| {
        if let Some(span) = run.take() {
            out.push(span);
        }
    };
    let mut position = start;
    // The previous 16-byte block was zero and unclaimed.
    let mut previous_free_zero = false;
    while position < end {
        let len = (end - position).min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..len])?;
        for (i, block) in buffer[..len].chunks(ALIGN as usize).enumerate() {
            let at = position + (i as u64) * ALIGN;
            while next_claim < claimed.len() && claimed[next_claim].1 <= at {
                next_claim += 1;
            }
            if claimed.get(next_claim).is_some_and(|&(s, _)| s <= at) {
                flush(&mut run);
                previous_free_zero = false;
                continue;
            }
            let nonzero = block.iter().any(|&b| b != 0);
            if nonzero {
                let block_end = at + block.len() as u64;
                let zero_prefix = previous_free_zero && at % PAGE == ALIGN;
                if zero_prefix {
                    flush(&mut run);
                }
                let last = at + block.iter().rposition(|&b| b != 0).unwrap() as u64 + 1;
                match run.as_mut() {
                    Some(span) => {
                        span.len = block_end - span.offset;
                        span.exact_len = last - span.offset;
                    }
                    None => run = Some(Span {
                        header: header.id, offset: at, len: block.len() as u64,
                        head: block[..block.len().min(8)].to_vec(), zero_prefix,
                        exact_len: last - at,
                    }),
                }
            } else if run.as_ref().is_some_and(|s| at - (s.offset + s.len) >= PAGE) {
                flush(&mut run);
            }
            previous_free_zero = !nonzero;
        }
        position += len as u64;
        on_bytes(len as u64);
    }
    flush(&mut run);
    Ok(())
}

/// Returns the bytes of one inventory entry.
///
/// `Stored` returns the archive bytes `[offset, stored_end)`. `Decoded`
/// rebuilds the object: PSO entries through a new `recover_psos` pass and
/// zlib sequences through `read_zlib_sequence` and zlib streams by decoding
/// them again; all are checked against the inventory lengths. Bink videos
/// have no compression layer here: their header and frame table are checked
/// again and the stored bytes are returned. Unterminated sequences are only
/// available as stored.
pub fn extract<R: Read + Seek>(input: &mut R, entry: &Entry, payload: Payload) -> io::Result<Vec<u8>> {
    let invalid = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
    let read_stored = |input: &mut R| -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; entry.stored_len() as usize];
        input.seek(SeekFrom::Start(entry.offset))?;
        input.read_exact(&mut bytes)?;
        Ok(bytes)
    };
    let bytes = match (payload, entry.evidence) {
        (Payload::Stored, _) => return read_stored(input),
        (Payload::Decoded, Evidence::BinkFramed) => {
            match probe_bink(input, entry.offset, entry.stored_end)? {
                Some(bink) if entry.offset + bink.len == entry.stored_end => read_stored(input)?,
                _ => return Err(invalid(format!("Bink video at {:#x} no longer validates", entry.offset))),
            }
        }
        (Payload::Decoded, Evidence::PsoFramed) => {
            let mut found = None;
            rpf9::recover_psos(input, |p, bytes| {
                if p.offset == entry.offset {
                    found = Some(bytes.to_vec());
                }
                Ok(())
            })?;
            found.ok_or_else(|| invalid(format!("PSO at {:#x} no longer recovered", entry.offset)))?
        }
        (Payload::Decoded, Evidence::ZlibSequence) => {
            let (bytes, members) = rpf9::read_zlib_sequence_range(
                input, entry.offset, entry.stored_end, entry.decoded_len,
            )?;
            if members != entry.chunks {
                return Err(invalid(format!("sequence at {:#x}: {members} members, inventory has {}", entry.offset, entry.chunks)));
            }
            bytes
        }
        (Payload::Decoded, Evidence::ZlibStream) => {
            let mut bytes = Vec::with_capacity(entry.decoded_len);
            let stream = probe_stream(input, entry.offset, entry.stored_end, Some(&mut bytes))?
                .ok_or_else(|| invalid(format!("zlib stream at {:#x} no longer decodes", entry.offset)))?;
            if stream.end != entry.stored_end {
                return Err(invalid(format!(
                    "zlib stream at {:#x} ends at {:#x}, inventory has {:#x}",
                    entry.offset, stream.end, entry.stored_end
                )));
            }
            bytes
        }
        (Payload::Decoded, Evidence::ZlibUnterminated) => {
            return Err(invalid(format!("entry at {:#x} has no verified end; extract it as stored", entry.offset)));
        }
        (Payload::Decoded, Evidence::OpaqueSpan | Evidence::EncryptedDelimited) => {
            return Err(invalid(format!("entry at {:#x} has no known encoding; extract it as stored", entry.offset)));
        }
    };
    if bytes.len() != entry.decoded_len {
        return Err(invalid(format!("decoded {} bytes, inventory has {}", bytes.len(), entry.decoded_len)));
    }
    Ok(bytes)
}

/// Calls `on_entry` once per inventory entry with its bytes: decoded for PSO
/// objects, terminated zlib sequences, zlib streams and Bink videos, stored
/// for unterminated sequences, encrypted delimited entries and opaque spans.
/// PSO objects are rebuilt in a single `recover_psos` pass.
pub fn extract_all<R: Read + Seek>(
    input: &mut R,
    inventory: &Inventory,
    mut on_entry: impl FnMut(&Entry, Payload, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let invalid = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
    let mut seen = 0;
    rpf9::recover_psos(input, |p, bytes| {
        let i = inventory.entries.partition_point(|e| e.offset < p.offset);
        match inventory.entries.get(i) {
            Some(e) if e.offset == p.offset && e.evidence == Evidence::PsoFramed && e.decoded_len == bytes.len() => {
                seen += 1;
                on_entry(e, Payload::Decoded, bytes)
            }
            _ => Err(invalid(format!("PSO at {:#x} is not in the inventory", p.offset))),
        }
    })?;
    let expected = inventory.entries.iter().filter(|e| e.evidence == Evidence::PsoFramed).count();
    if seen != expected {
        return Err(invalid(format!("rebuilt {seen} PSO objects, inventory has {expected}")));
    }
    for entry in inventory.entries.iter().filter(|e| e.evidence != Evidence::PsoFramed) {
        let payload = if entry.evidence.stored_only() { Payload::Stored } else { Payload::Decoded };
        let bytes = extract(input, entry, payload)?;
        on_entry(entry, payload, &bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
