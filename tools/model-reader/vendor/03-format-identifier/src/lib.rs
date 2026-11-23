//! Format identification over the phase 02 inventory.
//!
//! Every inventory entry is decoded (when it can be) and its payload is
//! classified by signature and, where a structure is known, by walking it.
//! RAGE resources span several entries: a virtual section, recognised by its
//! root object, followed by the physical units its page map declares. Those
//! units are attributed to the resource and, for texture dictionaries, their
//! extent is checked against the texture data pointers.

pub mod resource;
pub mod xml;

use resource::{Kind, Root};
use rpf9_index::{extract, Evidence, Inventory, Payload};
use rpf_archive::rpf9;
use std::collections::HashMap;
use std::io::{self, Read, Seek};

/// How an identification was established, from strongest to weakest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    /// The entry's own structure was validated: PSO framing, Bink frame
    /// table, XML well-formedness, or a resource root whose dictionary keys
    /// are the hashes of its element names.
    Structural,
    /// A physical section whose extent was checked against the pointers of
    /// its resource (every texture's mip chain fits without overlap).
    Bounded,
    /// A physical section attributed by position: it follows a resource root
    /// that declares physical pages. Its content is not checked.
    Positional,
    /// Only a magic number or a text encoding was recognised.
    Signature,
    /// Not identified.
    None,
}

impl Check {
    pub fn label(self) -> &'static str {
        match self {
            Check::Structural => "structural",
            Check::Bounded => "bounded",
            Check::Positional => "positional",
            Check::Signature => "signature",
            Check::None => "none",
        }
    }

    pub const ALL: [Check; 5] = [Check::Structural, Check::Bounded, Check::Positional, Check::Signature, Check::None];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Pso,
    Bink,
    Xml,
    Rbf0,
    Edup,
    Text,
    ResourceVirtual,
    ResourcePhysical,
    Encrypted,
    Opaque,
    Unknown,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Pso => "pso",
            Format::Bink => "bink",
            Format::Xml => "xml",
            Format::Rbf0 => "rbf0",
            Format::Edup => "edup",
            Format::Text => "text",
            Format::ResourceVirtual => "resource_virtual",
            Format::ResourcePhysical => "resource_physical",
            Format::Encrypted => "encrypted",
            Format::Opaque => "opaque",
            Format::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Identified {
    pub ordinal: usize,
    pub format: Format,
    pub variant: String,
    pub check: Check,
    pub resource: Option<usize>,
    /// Bytes after decoding: the length phase 02 validated on extraction.
    pub decoded_len: usize,
    /// The payload was rebuilt by decompression with its checksum and length
    /// validated (zlib entries extracted as decoded).
    pub decompressed: bool,
    /// First bytes of the payload, kept for unidentified entries.
    pub head: Vec<u8>,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ResourceInfo {
    pub id: usize,
    pub header: usize,
    /// Ordinal of the virtual section, or of its first unit.
    pub root_ordinal: usize,
    /// Ordinals of further units of the virtual section, when its page map
    /// lands past the first unit. Their number plus one equals the virtual
    /// pages declared.
    pub virtual_parts: Vec<usize>,
    /// Ordinals of the physical units, in order.
    pub physical: Vec<usize>,
    pub root: Root,
    pub kind: Kind,
    pub virtual_len: usize,
    pub physical_len: u64,
    /// Fewer physical units than pages, explained by the phase 02 join of a
    /// sequence ending in a full 64 KiB member.
    pub merged_pages: bool,
    pub check: Check,
    pub detail: String,
}

#[derive(Debug)]
pub struct Report {
    pub inventory: Inventory,
    pub entries: Vec<Identified>,
    pub resources: Vec<ResourceInfo>,
}

impl Report {
    pub fn count(&self, header: Option<usize>, pick: impl Fn(&Identified) -> bool) -> usize {
        self.entries.iter()
            .filter(|e| header.is_none_or(|h| self.inventory.entries[e.ordinal].header == h))
            .filter(|e| pick(e))
            .count()
    }
}

/// A payload's identification before resources are grouped.
struct Payload1 {
    format: Format,
    variant: String,
    check: Check,
    detail: String,
    root: Option<(Root, Kind)>,
}

fn simple(format: Format, variant: impl Into<String>, check: Check, detail: impl Into<String>) -> Payload1 {
    Payload1 { format, variant: variant.into(), check, detail: detail.into(), root: None }
}

fn is_text(bytes: &[u8]) -> bool {
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    let Ok(text) = std::str::from_utf8(&bytes[..end]) else { return false };
    !text.is_empty() && text.chars().all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
}

/// Classifies one decoded payload.
fn identify_payload(bytes: &[u8]) -> Payload1 {
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if body.starts_with(b"<?xml") {
        return match xml::well_formed(bytes) {
            Ok(root) => simple(Format::Xml, root, Check::Structural, "well-formed"),
            Err(e) => simple(Format::Xml, "", Check::Signature, e),
        };
    }
    if bytes.starts_with(b"RBF0") {
        let class = resource::c_string(bytes, 0x0c).map(|s| String::from_utf8_lossy(s).into_owned());
        return simple(Format::Rbf0, class.unwrap_or_default(), Check::Signature, "magic RBF0");
    }
    if bytes.starts_with(b"EDUP") {
        return simple(Format::Edup, "", Check::Signature, "magic EDUP");
    }
    if bytes.starts_with(b"PSIN") {
        return simple(Format::Pso, "", Check::Signature, "magic PSIN inside a zlib entry");
    }
    if let Some(root) = resource::root(bytes) {
        let kind = resource::classify(bytes);
        let check = root_check(&kind);
        let detail = format!("pages {}+{}", root.virtual_pages, root.physical_pages);
        return Payload1 { format: Format::ResourceVirtual, variant: kind.label(), check, detail, root: Some((root, kind)) };
    }
    if is_text(bytes) {
        return simple(Format::Text, "", Check::Signature, "UTF-8 text");
    }
    simple(Format::Unknown, "", Check::None, "")
}

/// Largest number of zlib units joined to rebuild one virtual section.
const MAX_VIRTUAL_UNITS: usize = 8;

/// A resource root found in one virtual unit, or in several consecutive
/// units when its page map pointer lands past the first one.
struct RootSlot {
    root: Root,
    kind: Kind,
    /// Ordinals of the units after the first that complete the section.
    parts: Vec<usize>,
    virtual_len: usize,
}

/// Virtual units being joined: the first starts like a root (zero first
/// field, page map pointer in the virtual segment) but its page map pointer
/// lands past its end.
struct Pending {
    header: usize,
    ordinals: Vec<usize>,
    bytes: Vec<u8>,
}

/// Identifies every entry of the archive.
pub fn identify<R: Read + Seek>(input: &mut R) -> io::Result<Report> {
    identify_with_progress(input, |_, _, _, _| {})
}

pub fn identify_with_progress<R: Read + Seek>(
    input: &mut R, mut progress: impl FnMut(&'static str, u64, u64, &'static str),
) -> io::Result<Report> {
    let inventory = rpf9_index::inventory_with_progress(input,
        |phase, done, total| progress(phase, done, total, "bytes"))?;
    let mut psos = HashMap::new();
    if inventory.entries.iter().any(|entry| entry.evidence == Evidence::PsoFramed) {
        progress("Checking PSO blocks", 0, 0, "bytes");
        rpf9::recover_psos_with_progress(input, |p, bytes| {
            psos.insert(p.offset, (bytes.len(), bytes.starts_with(b"PSIN")));
            Ok(())
        }, |done, total| progress("Checking PSO blocks", done, total, "bytes"))?;
    }
    let mut entries = Vec::with_capacity(inventory.entries.len());
    let mut roots: Vec<Option<RootSlot>> = Vec::with_capacity(inventory.entries.len());
    let mut pending: Option<Pending> = None;
    let total = inventory.entries.len();
    for (completed, e) in inventory.entries.iter().enumerate() {
        if completed % 32 == 0 || completed + 1 == total {
            progress("Validating entries", completed as u64, total as u64, "entries");
        }
        let mut bytes = None;
        let (p, decompressed, head) = match e.evidence {
            Evidence::PsoFramed => {
                let rebuilt = psos.get(&e.offset) == Some(&(e.decoded_len, true));
                let detail = if rebuilt { "PSO framing (phase 01), rebuilt" } else { "PSO not rebuilt with the inventory length" };
                let check = if rebuilt { Check::Structural } else { Check::Signature };
                (simple(Format::Pso, "PSIN", check, detail), rebuilt, b"PSIN".to_vec())
            }
            Evidence::BinkFramed => (simple(Format::Bink, e.signature, Check::Structural, format!("{} frames", e.chunks)), false, Vec::new()),
            Evidence::EncryptedDelimited => (simple(Format::Encrypted, "", Check::None, "content not decoded"), false, Vec::new()),
            Evidence::OpaqueSpan => (simple(Format::Opaque, "", Check::None, "content not decoded"), false, Vec::new()),
            Evidence::ZlibUnterminated => {
                let mut head = vec![0; e.stored_len().min(16) as usize];
                input.seek(std::io::SeekFrom::Start(e.offset))?;
                input.read_exact(&mut head)?;
                (simple(Format::Unknown, "", Check::None, "zlib sequence without a verified end"), false, head)
            }
            Evidence::ZlibSequence | Evidence::ZlibStream => {
                let decoded = extract(input, e, Payload::Decoded)?;
                let p = identify_payload(&decoded);
                let head = decoded[..decoded.len().min(16)].to_vec();
                bytes = Some(decoded);
                (p, true, head)
            }
        };
        let unknown = p.format == Format::Unknown && bytes.is_some();
        roots.push(p.root.map(|(root, kind)| RootSlot { root, kind, parts: Vec::new(), virtual_len: e.decoded_len }));
        entries.push(Identified {
            ordinal: e.ordinal, format: p.format, variant: p.variant, check: p.check, resource: None,
            decoded_len: e.decoded_len, decompressed, head, detail: p.detail,
        });
        let Some(bytes) = bytes.filter(|_| unknown) else {
            pending = None;
            continue;
        };
        if let Some(mut join) = pending.take().filter(|j| j.header == e.header) {
            join.bytes.extend_from_slice(&bytes);
            join.ordinals.push(e.ordinal);
            if let Some(root) = resource::root(&join.bytes) {
                // Accepted only when the units joined equal the virtual pages.
                if root.virtual_pages as usize == join.ordinals.len() {
                    attach_split_root(&mut entries, &mut roots, join, root);
                }
                continue;
            }
            if join.ordinals.len() < MAX_VIRTUAL_UNITS {
                pending = Some(join);
            }
            continue;
        }
        if resource::split_root_candidate(&bytes) {
            pending = Some(Pending { header: e.header, ordinals: vec![e.ordinal], bytes });
        }
    }
    progress("Validating entries", total as u64, total as u64, "entries");
    let resources = group_resources(&inventory, &mut entries, roots);
    Ok(Report { inventory, entries, resources })
}

fn attach_split_root(entries: &mut [Identified], roots: &mut [Option<RootSlot>], join: Pending, root: Root) {
    let kind = resource::classify(&join.bytes);
    let check = root_check(&kind);
    let first = join.ordinals[0];
    for (n, &k) in join.ordinals.iter().enumerate() {
        entries[k].format = Format::ResourceVirtual;
        entries[k].variant = kind.label();
        entries[k].check = check;
        entries[k].detail = format!(
            "virtual unit {} of {}, pages {}+{}", n + 1, join.ordinals.len(), root.virtual_pages, root.physical_pages
        );
    }
    roots[first] = Some(RootSlot { root, kind, parts: join.ordinals[1..].to_vec(), virtual_len: join.bytes.len() });
}

fn root_check(kind: &Kind) -> Check {
    match kind {
        Kind::TextureDictionary(_) | Kind::NamedDictionary { .. } | Kind::Dictionary { .. } => Check::Structural,
        Kind::Named { .. } | Kind::Unknown => Check::Signature,
    }
}

/// Attributes to every resource root the unidentified zlib entries that
/// follow it in the same header, up to the number of physical pages its page
/// map declares, and checks texture extents against them.
fn group_resources(inv: &Inventory, entries: &mut [Identified], roots: Vec<Option<RootSlot>>) -> Vec<ResourceInfo> {
    let mut resources = Vec::new();
    for (i, slot) in roots.into_iter().enumerate() {
        let Some(RootSlot { root, kind, parts, virtual_len }) = slot else { continue };
        let header = inv.entries[i].header;
        let mut physical = Vec::new();
        let mut j = parts.last().copied().unwrap_or(i) + 1;
        while physical.len() < root.physical_pages as usize && j < entries.len() {
            let e = &inv.entries[j];
            let candidate = e.header == header
                && matches!(e.evidence, Evidence::ZlibSequence | Evidence::ZlibStream)
                && entries[j].format == Format::Unknown;
            if !candidate {
                break;
            }
            physical.push(j);
            j += 1;
        }
        let physical_len: u64 = physical.iter().map(|&k| inv.entries[k].decoded_len as u64).sum();
        let id = resources.len();
        let mut detail = Vec::new();
        let mut merged_pages = false;
        if physical.len() < root.physical_pages as usize {
            // Phase 02 joins a sequence whose last member decodes to exactly
            // 64 KiB with the next contiguous one, so one unit of two or more
            // members longer than 64 KiB may hold two pages.
            merged_pages = physical.last().is_some_and(|&k| {
                let e = &inv.entries[k];
                e.evidence == Evidence::ZlibSequence && e.chunks >= 2 && e.decoded_len > 64 * 1024
            });
            detail.push(format!(
                "{} physical units for {} declared pages{}", physical.len(), root.physical_pages,
                if merged_pages { " (last unit may hold two pages joined by phase 02)" } else { "" }
            ));
        }
        let mut physical_check = Check::Positional;
        if let Kind::TextureDictionary(textures) = &kind {
            match resource::check_texture_extents(textures, physical_len) {
                Ok(()) if !physical.is_empty() => physical_check = Check::Bounded,
                Ok(()) => {}
                Err(e) => detail.push(e),
            }
        }
        for &k in &physical {
            entries[k].format = Format::ResourcePhysical;
            entries[k].variant = kind.label();
            entries[k].check = physical_check;
            entries[k].resource = Some(id);
            entries[k].detail = format!("physical unit of resource {id}");
        }
        for &k in std::iter::once(&i).chain(&parts) {
            entries[k].resource = Some(id);
        }
        resources.push(ResourceInfo {
            id, header, root_ordinal: i, virtual_parts: parts, physical, root, check: entries[i].check,
            virtual_len, physical_len, kind, merged_pages,
            detail: detail.join("; "),
        });
    }
    resources
}

/// Rebuilds the virtual image and the concatenated physical image of one
/// resource from its inventory entries.
pub fn resource_images<R: Read + Seek>(input: &mut R, report: &Report, id: usize) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let r = report.resources.get(id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no resource {id}")))?;
    let mut virt = Vec::with_capacity(r.virtual_len);
    for &k in std::iter::once(&r.root_ordinal).chain(&r.virtual_parts) {
        virt.extend(extract(input, &report.inventory.entries[k], Payload::Decoded)?);
    }
    let mut phys = Vec::with_capacity(r.physical_len as usize);
    for &k in &r.physical {
        phys.extend(extract(input, &report.inventory.entries[k], Payload::Decoded)?);
    }
    Ok((virt, phys))
}

#[cfg(test)]
mod tests;
