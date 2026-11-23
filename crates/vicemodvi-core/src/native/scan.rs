//! Recovers drawables and texture dictionaries from RPF9 archives.
//!
//! The archive's path table is not decoded, so resources are found from
//! candidate offsets: a resource starts as a zlib member after 16 zero
//! bytes, and its system and graphics sections continue in further members
//! aligned to 16 bytes. A candidate is accepted only when the joined
//! sections pass the full resource validation.

use std::{
    collections::HashSet,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use super::{
    bin::all_zero,
    dictionary::{self, Dictionary},
    drawable,
    model::Model,
};
use crate::{
    zlib::{find_member, Member, MemberLimits},
    Cancel, Error, Result,
};

pub const MAX_RECOVERED_MODELS: usize = 4096;
pub const MODEL_DECODE_BUDGET: i64 = 1024 * 1024 * 1024;
const TEXTURE_DECODE_BUDGET: i64 = 512 * 1024 * 1024;
const MAX_CANDIDATES: usize = 200_000;
/// Extra packed bytes allowed beyond the decoded limit of a member.
const SLACK: i64 = 1024 * 1024;
const MAX_MODEL_MEMBERS: usize = 64;
const MAX_SYSTEM_SPLIT: usize = 32;

/// One zlib member of a recovered resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub offset: i64,
    pub stored_size: i64,
    pub size: i64,
}

/// A validated resource and what its validation decoded.
#[derive(Clone, Debug)]
pub struct Recovered<T> {
    pub name: String,
    pub offset: i64,
    pub stored_size: i64,
    pub size: i64,
    pub compression: &'static str,
    pub segments: Vec<Segment>,
    pub system_bytes: i32,
    pub content: T,
}

/// Reads a scan's members and enforces the shared limits.
struct Scanner<'a> {
    file: BufReader<File>,
    length: i64,
    cancel: &'a Cancel,
}

impl Scanner<'_> {
    fn open<'a>(path: &Path, length: i64, cancel: &'a Cancel) -> Result<Scanner<'a>> {
        let file = File::open(path)?;
        if file.metadata()?.len() as i64 != length {
            return Err(Error::io("The archive changed while it was being opened."));
        }
        Ok(Scanner { file: BufReader::with_capacity(64 * 1024, file), length, cancel })
    }

    /// Resources start after 16 zero bytes.
    fn zero_prefixed(&mut self, at: i64) -> Result<bool> {
        let mut prefix = [0u8; 16];
        self.file.seek(SeekFrom::Start((at - 16) as u64))?;
        self.file.read_exact(&mut prefix)?;
        Ok(all_zero(&prefix))
    }

    fn member(&mut self, at: i64, output: i64, total: i64, spent: &mut i64) -> Result<Option<Member>> {
        find_member(&mut self.file, self.length, at, MemberLimits { output, input: output + SLACK, total }, spent, self.cancel)
    }

    /// The member after `last`, once the gap up to the next 16-byte
    /// boundary is zero.
    fn next_member(&mut self, last: &Member, output: i64, total: i64, spent: &mut i64) -> Result<Option<Member>> {
        let end = last.offset + last.packed;
        let next = (end + 15) & !15;
        if next > self.length - 6 {
            return Ok(None);
        }
        let mut gap = [0u8; 15];
        let gap = &mut gap[..(next - end) as usize];
        self.file.seek(SeekFrom::Start(end as u64))?;
        self.file.read_exact(gap)?;
        if !all_zero(gap) {
            return Ok(None);
        }
        self.member(next, output, total, spent)
    }

    fn accepts(&self, at: i64) -> bool {
        at >= 16 && at <= self.length - 6
    }
}

fn segments(members: &[Member]) -> Vec<Segment> {
    members.iter().map(|m| Segment { offset: m.offset, stored_size: m.packed, size: m.data.len() as i64 }).collect()
}

fn joined(members: &[Member]) -> Vec<u8> {
    members.iter().flat_map(|m| m.data.iter().copied()).collect()
}

/// File-name-safe form of a resource name: ASCII letters, digits, `_`,
/// `-` and `.`; one `_` for every UTF-16 unit of anything else.
fn file_safe(name: &str) -> String {
    let mut safe = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
            safe.push(c);
        } else {
            safe.extend(std::iter::repeat_n('_', c.len_utf16()));
        }
    }
    match safe.trim_matches('.') {
        "" => "drawable".to_string(),
        trimmed => trimmed.chars().take(160).collect(),
    }
}

/// Validates drawable and fragment resources at the candidate offsets.
pub fn recover_models(path: &Path, length: i64, candidates: &[i64], cancel: &Cancel) -> Result<Vec<Recovered<Model>>> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(Error::argument("Too many candidate offsets."));
    }
    let mut scanner = Scanner::open(path, length, cancel)?;
    let (mut found, mut seen, mut spent) = (Vec::new(), HashSet::new(), 0i64);
    for &at in candidates {
        cancel.check()?;
        if found.len() >= MAX_RECOVERED_MODELS || spent >= MODEL_DECODE_BUDGET {
            break;
        }
        // Drawables start 16 bytes into a 4 KiB page.
        if !seen.insert(at) || !scanner.accepts(at) || at & 4095 != 16 || !scanner.zero_prefixed(at)? {
            continue;
        }
        let Some((members, system_bytes, model)) = model_at(&mut scanner, at, &mut spent)? else { continue };
        found.push(Recovered {
            name: format!("{}_at_{at:016X}.r9modeldata", file_safe(&model.name)),
            offset: at,
            stored_size: members.iter().map(|m| m.packed).sum(),
            size: members.iter().map(|m| m.data.len() as i64).sum(),
            compression: "Recovered model",
            segments: segments(&members),
            system_bytes,
            content: model,
        });
    }
    Ok(found)
}

fn model_at(scanner: &mut Scanner, at: i64, spent: &mut i64) -> Result<Option<(Vec<Member>, i32, Model)>> {
    let Some(first) = scanner.member(at, drawable::MAX_SYSTEM_BYTES, MODEL_DECODE_BUDGET, spent)? else { return Ok(None) };
    if !drawable::looks_like_resource(&first.data) {
        return Ok(None);
    }
    let limit = drawable::MAX_SYSTEM_BYTES + drawable::MAX_GRAPHICS_BYTES;
    let mut total = first.data.len() as i64;
    let mut members = vec![first];
    loop {
        scanner.cancel.check()?;
        let (model, wants_more) = split_model(&members, total, scanner.cancel)?;
        if let Some((system_bytes, model)) = model {
            return Ok(Some((members, system_bytes, model)));
        }
        if !wants_more || members.len() >= MAX_MODEL_MEMBERS || total >= limit {
            return Ok(None);
        }
        let Some(next) = scanner.next_member(&members[members.len() - 1], limit - total, MODEL_DECODE_BUDGET, spent)? else { return Ok(None) };
        total += next.data.len() as i64;
        members.push(next);
    }
}

/// Tries each member boundary as the end of the system section. Returns
/// the decoded model, or whether more members could still complete it.
fn split_model(members: &[Member], total: i64, cancel: &Cancel) -> Result<(Option<(i32, Model)>, bool)> {
    let mut wants_more = false;
    let mut system_length = 0i64;
    for split in 1..=members.len().min(MAX_SYSTEM_SPLIT) {
        cancel.check()?;
        system_length += members[split - 1].data.len() as i64;
        if system_length > drawable::MAX_SYSTEM_BYTES {
            break;
        }
        let system = joined(&members[..split]);
        let Some(extent) = drawable::probe_required_extents(&system) else { continue };
        if !extent.metadata_complete {
            wants_more = true;
            continue;
        }
        // Bytes past what the metadata references must be zero padding.
        if extent.required_system_bytes > system_length || !all_zero(&system[extent.required_system_bytes as usize..]) {
            continue;
        }
        let graphics_length = total - system_length;
        if graphics_length < extent.required_graphics_bytes {
            wants_more = true;
            continue;
        }
        if graphics_length > drawable::MAX_GRAPHICS_BYTES {
            continue;
        }
        let graphics = joined(&members[split..]);
        if !all_zero(&graphics[extent.required_graphics_bytes as usize..]) {
            continue;
        }
        if let Some(model) = drawable::decode(&system, &graphics) {
            return Ok((Some((system_length as i32, model)), wants_more));
        }
    }
    Ok((None, wants_more))
}

/// Validates standalone texture dictionaries at the candidate offsets.
pub fn recover_dictionaries(path: &Path, length: i64, candidates: &[i64], cancel: &Cancel) -> Result<Vec<Recovered<Dictionary>>> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(Error::argument("Too many candidate offsets."));
    }
    let mut scanner = Scanner::open(path, length, cancel)?;
    let (mut found, mut seen) = (Vec::new(), HashSet::new());
    for &at in candidates {
        cancel.check()?;
        if !seen.insert(at) || !scanner.accepts(at) || !scanner.zero_prefixed(at)? {
            continue;
        }
        // Each candidate gets its own budget.
        if let Some(dictionary) = dictionary_at(&mut scanner, at, &mut 0)? {
            found.push(dictionary);
        }
    }
    Ok(found)
}

fn dictionary_at(scanner: &mut Scanner, at: i64, spent: &mut i64) -> Result<Option<Recovered<Dictionary>>> {
    let system_limit = dictionary::MAX_SYSTEM_BYTES as i64;
    let graphics_limit = dictionary::MAX_GRAPHICS_BYTES as i64;
    let Some(first) = scanner.member(at, system_limit, TEXTURE_DECODE_BUDGET, spent)? else { return Ok(None) };
    let mut system = first.data.clone();
    let mut members = vec![first];
    // Read members until the system section holds all its metadata.
    loop {
        let Some(needed) = dictionary::minimum_system_bytes(&system) else { return Ok(None) };
        if needed <= system.len() as i64 {
            break;
        }
        scanner.cancel.check()?;
        if members.len() >= 128 {
            return Ok(None);
        }
        let room = system_limit - system.len() as i64;
        let Some(next) = scanner.next_member(&members[members.len() - 1], room, TEXTURE_DECODE_BUDGET, spent)? else { return Ok(None) };
        system.extend_from_slice(&next.data);
        members.push(next);
    }
    if dictionary::page_counts(&system).is_none() {
        return Ok(None);
    }
    let Some(required) = dictionary::required_graphics_bytes(&system) else { return Ok(None) };
    let system_members = members.len();
    let mut graphics_length = 0i64;
    while graphics_length < required {
        scanner.cancel.check()?;
        let room = graphics_limit - system.len() as i64 - graphics_length;
        if room < 1 || members.len() > 4095 {
            return Ok(None);
        }
        let Some(next) = scanner.next_member(&members[members.len() - 1], room, TEXTURE_DECODE_BUDGET, spent)? else { return Ok(None) };
        graphics_length += next.data.len() as i64;
        members.push(next);
    }
    if graphics_length < 1 {
        return Ok(None);
    }
    let Some(content) = dictionary::inspect(&system, &joined(&members[system_members..])) else { return Ok(None) };
    let segments = segments(&members);
    Ok(Some(Recovered {
        name: format!("dictionary_at_{at:016X}.r9texdata"),
        offset: at,
        stored_size: segments.iter().map(|s| s.stored_size).sum(),
        size: system.len() as i64 + graphics_length,
        compression: "Recovered texture dictionary",
        segments,
        system_bytes: system.len() as i32,
        content,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn recovers_a_dictionary_split_in_two_members() {
        let (system, graphics) = dictionary::tests::sample(64, 64, 28, 7, true);
        let mut file = vec![0u8; 4096 + 16];
        let packed_system = zlib(&system);
        file.extend_from_slice(&packed_system);
        file.resize(file.len().div_ceil(16) * 16, 0);
        file.extend_from_slice(&zlib(&graphics));
        file.extend_from_slice(&[0u8; 64]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.rpf");
        std::fs::write(&path, &file).unwrap();
        let found = recover_dictionaries(&path, file.len() as i64, &[4096 + 16], &Cancel::none()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].segments.len(), 2);
        assert_eq!(found[0].segments[0].stored_size, packed_system.len() as i64);
        assert_eq!(found[0].system_bytes, system.len() as i32);
    }

    #[test]
    fn names_become_file_safe() {
        assert_eq!(file_safe("a b😀.#dr"), "a_b__._dr");
        assert_eq!(file_safe("..."), "drawable");
    }
}
