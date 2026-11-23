//! TTPF texture pools: an RDS5 chunk container whose virtual address space
//! starts with an ETPF descriptor table (version 634) and continues with
//! tiled texture allocations.
//!
//! The RDS5 header lists one 64-bit word per chunk. Each word packs the
//! physical start (in 256-byte units), the codec, the virtual start (in
//! 4 KiB units) and the padding after the packed bytes; a final sentinel
//! word closes the last chunk.

use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use super::{
    bin::{self, Bin},
    dds::{self, Layers},
    dxgi,
    swizzle::{Surface, Tiling},
};
use crate::{util::hex_upper, zlib, Cancel, Error, Result};

const MAX_OUTPUT: i64 = 256 * 1024 * 1024;
const MAX_INDEX_BYTES: i64 = 64 * 1024 * 1024;
const MAX_TEXTURES: i64 = 250_000;
const ETPF_VERSION: u32 = 634;

/// Codecs a chunk may use. Other codes need an Xbox BCPack decoder.
pub(crate) const STORED: i32 = 0;
pub(crate) const ZLIB: i32 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolTexture {
    pub index: i32,
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub dxgi_format: i32,
    pub format: String,
    /// Mip levels the export contains.
    pub mips: i32,
    pub depth: i32,
    pub dimension: i32,
    pub stored_size: i64,
    pub dds_size: i64,
    pub source_mips: i32,
    pub non_tail_mip_count: i32,
    pub packed_mip_count: i32,
    pub supports_native_replacement: bool,
    pub base_mip_level: i32,
    pub asset_key: u64,
    pub id: String,
    pub virtual_offset: i64,
    pub limitation: Option<String>,
    pub preview_note: Option<String>,
}

impl PoolTexture {
    pub fn export_width(&self) -> i32 {
        dxgi::mip_extent(self.width, self.base_mip_level)
    }

    pub fn export_height(&self) -> i32 {
        dxgi::mip_extent(self.height, self.base_mip_level)
    }

    pub fn top_only(&self) -> bool {
        self.base_mip_level == 0 && self.mips == 1 && self.source_mips > 1
    }

    pub fn partial_mips(&self) -> bool {
        self.base_mip_level > 0 || self.mips < self.source_mips
    }

    pub fn can_extract(&self) -> bool {
        self.limitation.is_none() && self.dds_size > 0
    }

    pub fn preview_mip_level(&self) -> i32 {
        self.base_mip_level + dxgi::preview_level(self.export_width(), self.export_height(), self.mips)
    }
}

#[derive(Clone, Debug)]
pub struct PoolInfo {
    pub source_path: PathBuf,
    pub source_length: i64,
    pub virtual_size: i64,
    pub version: i32,
    pub textures: Vec<PoolTexture>,
}

/// One RDS5 chunk: `packed` bytes at `source` that decode to `size` bytes
/// at `virtual_offset`, followed by `padding` filler bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub source: i64,
    pub packed: i64,
    pub virtual_offset: i64,
    pub size: i64,
    pub codec: i32,
    pub padding: i64,
}

impl Chunk {
    fn end(&self) -> i64 {
        self.virtual_offset + self.size
    }

    fn readable(&self) -> bool {
        matches!(self.codec, STORED | ZLIB)
    }
}

/// Field layout of an RDS5 table word.
pub(crate) mod word {
    pub const SOURCE_MASK: u64 = 0x3FF_FFFF;
    pub const PADDING_SHIFT: u32 = 52;

    pub fn source(value: u64) -> i64 {
        (value & SOURCE_MASK) as i64 * 256
    }

    pub fn codec(value: u64) -> i32 {
        ((value >> 26) & 15) as i32
    }

    pub fn virtual_start(value: u64) -> i64 {
        ((value >> 30) & 0x3F_FFFF) as i64 * 4096
    }

    pub fn padding(value: u64) -> i64 {
        (value >> PADDING_SHIFT) as i64
    }
}

/// A validated pool. Keep it to export several textures without reading
/// the tables again.
#[derive(Clone, Debug)]
pub struct PoolIndex {
    pub info: PoolInfo,
    pub(crate) chunks: Vec<Chunk>,
    surfaces: Vec<Option<Surface>>,
}

pub fn inspect(path: &Path, cancel: &Cancel) -> Result<PoolInfo> {
    Ok(PoolIndex::read(path, cancel)?.info)
}

pub fn create_dds(path: &Path, index: i32, cancel: &Cancel) -> Result<Vec<u8>> {
    PoolIndex::read(path, cancel)?.create_dds(index, cancel)
}

fn read_bytes(file: &mut File, count: i64) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; count as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn fits(value: u32) -> Result<i64> {
    i32::try_from(value).map(i64::from).map_err(|_| Error::invalid("RDS5 value overflows its supported range."))
}

impl PoolIndex {
    pub fn read(path: &Path, cancel: &Cancel) -> Result<Self> {
        cancel.check()?;
        let path = std::path::absolute(path)?;
        let mut file = File::open(&path)?;
        let file_size = file.metadata()?.len() as i64;
        if file_size < 256 {
            return Err(Error::invalid("Truncated RDS5 header."));
        }
        let header = read_bytes(&mut file, 24)?;
        let head = Bin(&header);
        if &header[..4] != b"RDS5" {
            return Err(Error::invalid("Not an RDS5 container."));
        }
        let words = head.u32(4)? as i64;
        let size_word = head.u64(8)?;
        let virtual_size = (size_word & 0x00FF_FFFF_FFFF_FFFF) as i64;
        let groups = head.u32(16)? as i64;
        let group_estimate = (words + 31) / 32;
        if !(3..=1_000_002).contains(&words)
            || size_word >> 56 != 22
            || !(4096..=1 << 34).contains(&virtual_size)
            || !(group_estimate..=group_estimate + 1).contains(&groups)
        {
            return Err(Error::invalid("Unsupported RDS5 index layout."));
        }
        // `words` counts the header itself; the table holds the rest.
        let table_bytes = (words - 1) * 8;
        let etpf = bin::align((24 + table_bytes) as u64, 256) as i64;
        if 24 + table_bytes > file_size - 16 || table_bytes > MAX_INDEX_BYTES {
            return Err(Error::invalid("RDS5 index exceeds its source or limit."));
        }
        let table = read_bytes(&mut file, table_bytes)?;
        let table = Bin(&table);
        let chunk_count = (words - 2) as usize;
        let sentinel = table.u64(chunk_count * 8)?;
        if word::source(sentinel) != file_size || word::virtual_start(sentinel) != virtual_size {
            return Err(Error::invalid("RDS5 sentinel does not match its source/virtual length."));
        }
        let mut chunks = Vec::with_capacity(chunk_count);
        for i in 0..chunk_count {
            if i % 4096 == 0 {
                cancel.check()?;
            }
            let (this, next) = (table.u64(i * 8)?, table.u64(i * 8 + 8)?);
            let chunk = Chunk {
                source: word::source(this),
                packed: word::source(next) - word::source(this) - word::padding(this),
                virtual_offset: word::virtual_start(this),
                size: word::virtual_start(next) - word::virtual_start(this),
                codec: word::codec(this),
                padding: word::padding(this),
            };
            if chunk.source < etpf + 16
                || word::source(next) > file_size
                || !(1..=MAX_OUTPUT).contains(&chunk.packed)
                || !(1..=MAX_OUTPUT).contains(&chunk.size)
                || chunk.end() > virtual_size
                || (chunk.codec == STORED && chunk.packed != chunk.size)
            {
                return Err(Error::invalid("RDS5 chunk range is invalid or exceeds the per-chunk limit."));
            }
            chunks.push(chunk);
        }
        file.seek(SeekFrom::Start(etpf as u64))?;
        let table_header = read_bytes(&mut file, 16)?;
        let etpf_header = Bin(&table_header);
        if &table_header[..4] != b"ETPF" || etpf_header.u32(4)? != ETPF_VERSION {
            return Err(Error::invalid("Unsupported embedded ETPF version."));
        }
        let count = fits(etpf_header.u32(8)?)?;
        let area = fits(etpf_header.u32(12)?)?;
        let metadata = 16 + area + count * 32;
        let padded_metadata = bin::align(metadata.max(0) as u64, 4096) as i64;
        let alignment = head.u32(20)?;
        let source_padding = (alignment >> 28) as i64 * 256;
        if !(1..=MAX_TEXTURES).contains(&count)
            || area < count * 16
            || metadata > MAX_INDEX_BYTES
            || alignment & 0x0FFF_FFFF != 0
            || padded_metadata != chunks[0].virtual_offset
            || etpf + padded_metadata + source_padding != chunks[0].source
        {
            return Err(Error::invalid("ETPF tables do not match the source/virtual data boundary."));
        }
        let records = read_bytes(&mut file, area)?;
        let ids = read_bytes(&mut file, count * 32)?;
        let tables = Tables { records: &records, ids: &ids, count, padded_metadata, virtual_size, chunks: &chunks };
        let mut textures = Vec::with_capacity(count as usize);
        let mut surfaces = Vec::with_capacity(count as usize);
        for i in 0..count {
            if i % 1024 == 0 {
                cancel.check()?;
            }
            let (texture, surface) = tables.describe(i)?;
            textures.push(texture);
            surfaces.push(surface);
        }
        let info = PoolInfo { source_path: path, source_length: file_size, virtual_size, version: ETPF_VERSION as i32, textures };
        Ok(Self { info, chunks, surfaces })
    }

    /// The texture and surface at `index`, if it can be exported.
    pub(crate) fn exportable(&self, index: i32) -> Result<(&PoolTexture, &Surface)> {
        let at = usize::try_from(index).ok().filter(|&i| i < self.info.textures.len()).ok_or_else(|| Error::argument("Texture index is out of range."))?;
        let texture = &self.info.textures[at];
        match (&self.surfaces[at], &texture.limitation) {
            (Some(surface), None) if texture.dds_size > 0 => Ok((texture, surface)),
            (_, limitation) => Err(Error::not_supported(limitation.clone().unwrap_or_else(|| "Texture layout unavailable.".into()))),
        }
    }

    pub(crate) fn surface_size(&self, index: i32) -> Option<i64> {
        self.surfaces.get(usize::try_from(index).ok()?)?.as_ref().map(|s| s.size as i64)
    }

    fn open(&self) -> Result<File> {
        let file = File::open(&self.info.source_path)?;
        if file.metadata()?.len() as i64 != self.info.source_length {
            return Err(Error::io("The texture pool changed while it was being read."));
        }
        Ok(file)
    }

    /// Exports the readable mip chain of one texture as a DX10 DDS.
    pub fn create_dds(&self, index: i32, cancel: &Cancel) -> Result<Vec<u8>> {
        let (texture, surface) = self.exportable(index)?;
        let mut file = self.open()?;
        let mut decoded = HashMap::new();
        let mut out = dds::dx10_header(texture.export_width(), texture.export_height(), Layers::Flat(1), texture.mips, texture.dxgi_format)?;
        out.reserve(texture.dds_size as usize);
        for level in texture.base_mip_level..texture.base_mip_level + texture.mips {
            let info = surface.level(level as usize)?;
            let start = texture.virtual_offset + info.offset as i64;
            let region = self.gather(&mut file, start, info.size as i64, &mut decoded, cancel)?;
            cancel.check()?;
            out.extend(surface.read_level(&region, info.offset, level as usize, 0)?);
        }
        if out.len() as i64 != texture.dds_size {
            return Err(Error::invalid("Generated DDS length disagrees with the inspected texture."));
        }
        Ok(out)
    }

    /// Virtual bytes `start..start + count`, decoding the covering chunks
    /// once each and keeping them in `decoded`.
    fn gather(&self, file: &mut File, start: i64, count: i64, decoded: &mut HashMap<usize, Vec<u8>>, cancel: &Cancel) -> Result<Vec<u8>> {
        let mut region = vec![0u8; count as usize];
        for key in chunks_for(&self.chunks, start, count)? {
            cancel.check()?;
            let chunk = &self.chunks[key];
            let data = match decoded.entry(key) {
                std::collections::hash_map::Entry::Occupied(found) => found.into_mut(),
                std::collections::hash_map::Entry::Vacant(slot) => slot.insert(decode_chunk(file, chunk, cancel)?),
            };
            let (from, to) = (start.max(chunk.virtual_offset), (start + count).min(chunk.end()));
            region[(from - start) as usize..(to - start) as usize]
                .copy_from_slice(&data[(from - chunk.virtual_offset) as usize..(to - chunk.virtual_offset) as usize]);
        }
        Ok(region)
    }

    /// The native allocation of `index` with a complete matching DDS
    /// written into it.
    pub(crate) fn retile(&self, file: &mut File, index: i32, replacement: &[u8], cancel: &Cancel) -> Result<Vec<u8>> {
        let (texture, surface) = self.exportable(index)?;
        if !texture.supports_native_replacement {
            return Err(Error::not_supported("Native replacement is not validated for this texture storage profile or partial mip chain."));
        }
        if texture.base_mip_level != 0 || texture.mips != texture.source_mips || texture.dimension != 1 || texture.depth != 1 {
            return Err(Error::not_supported(
                "Pool replacement requires a complete supported 2D mip chain; top-only exports are unavailable for replacement.",
            ));
        }
        let imported = dds::inspect(replacement)?;
        let same_shape = imported.width == texture.width
            && imported.height == texture.height
            && imported.dxgi_format == texture.dxgi_format
            && imported.mips == texture.source_mips
            && imported.depth == 1
            && imported.dimension == 1
            && imported.resource_dimension == 3
            && imported.array_size == 1
            && imported.slice_count == 1;
        if !same_shape {
            return Err(Error::invalid(
                "DDS dimensions, DXGI format, complete mip count and 2D shape must match the native pool descriptor exactly.",
            ));
        }
        if !matches!(imported.alpha_mode, 0 | 1) {
            return Err(Error::not_supported("Replacement DDS requires unknown or straight alpha semantics."));
        }
        let mut allocation = self.gather(file, texture.virtual_offset, surface.size as i64, &mut HashMap::new(), cancel)?;
        for level in 0..texture.source_mips {
            cancel.check()?;
            let linear = dds::mip_plane(replacement, &imported, level, 0)?;
            surface.write_level(&mut allocation, 0, level as usize, 0, linear)?;
        }
        Ok(allocation)
    }
}

/// Indices of the chunks that cover `start..start + count` without gaps.
pub(crate) fn chunks_for(chunks: &[Chunk], start: i64, count: i64) -> Result<Vec<usize>> {
    let end = start + count;
    let first = chunks.partition_point(|c| c.end() <= start);
    let mut covered = start;
    let mut found = Vec::new();
    for (i, chunk) in chunks.iter().enumerate().skip(first).take_while(|(_, c)| c.virtual_offset < end) {
        if chunk.virtual_offset > covered {
            return Err(Error::invalid("Texture crosses an unmapped virtual range."));
        }
        found.push(i);
        covered = end.min(chunk.end());
    }
    if covered != end {
        return Err(Error::invalid("Texture range is not completely mapped."));
    }
    Ok(found)
}

/// True when every chunk under the range can be decoded here.
fn readable(chunks: &[Chunk], start: i64, count: i64) -> Result<bool> {
    Ok(chunks_for(chunks, start, count)?.iter().all(|&k| chunks[k].readable()))
}

pub(crate) fn decode_chunk<R: Read + Seek>(input: &mut R, chunk: &Chunk, cancel: &Cancel) -> Result<Vec<u8>> {
    if !chunk.readable() {
        return Err(Error::not_supported("An Xbox BCPack decompression backend is required."));
    }
    let mut packed = vec![0u8; chunk.packed as usize];
    input.seek(SeekFrom::Start(chunk.source as u64))?;
    input.read_exact(&mut packed)?;
    if chunk.codec == STORED {
        Ok(packed)
    } else {
        zlib::inflate_chunk(&packed, chunk.size as usize, cancel)
    }
}

/// One 16-byte ETPF descriptor, unpacked.
#[derive(Clone, Copy, Debug)]
struct Record {
    width: i32,
    height: i32,
    format: i32,
    depth: i32,
    dimension: i32,
    /// Mips stored outside the packed tail.
    non_tail: i32,
    /// Mips stored in the packed tail.
    packed: i32,
    mode: i32,
    /// Virtual allocation start in 4 KiB units (21 bits).
    page: u32,
    /// Bits above the page field of the third word.
    meta: u32,
    extra: u32,
    second: u32,
    third: u32,
}

impl Record {
    fn read(records: Bin, i: usize) -> Result<Self> {
        let at = i * 16;
        let (a, b, c, d) = (records.u32(at)?, records.u32(at + 4)?, records.u32(at + 8)?, records.u32(at + 12)?);
        Ok(Self {
            width: (a & 0x3FFF) as i32 + 1,
            height: ((a >> 14) & 0x3FFF) as i32 + 1,
            packed: (a >> 28) as i32,
            non_tail: (b & 7) as i32,
            format: ((b >> 3) & 255) as i32,
            depth: ((b >> 11) & 1023) as i32 + 1,
            dimension: ((b >> 21) & 3) as i32,
            mode: (b >> 23) as i32,
            page: c & 0x1F_FFFF,
            meta: c >> 21,
            extra: d,
            second: b,
            third: c,
        })
    }

    fn mips(&self) -> i32 {
        self.packed + self.non_tail
    }

    fn base(&self) -> i64 {
        self.page as i64 * 4096
    }

    fn auxiliary(&self) -> bool {
        self.meta != 0 || self.extra != 0
    }

    /// Compact layouts keep standalone mips readable but the tail packed.
    fn compact(&self) -> bool {
        matches!(self.mode, 4 | 6 | 36) || (self.mode == 2 && self.auxiliary())
    }
}

/// What the analysis of a descriptor found.
struct Readable {
    surface: Surface,
    base_mip: i32,
    exported: i32,
    stored: i64,
    dds_size: i64,
}

struct Tables<'a> {
    records: &'a [u8],
    ids: &'a [u8],
    count: i64,
    padded_metadata: i64,
    virtual_size: i64,
    chunks: &'a [Chunk],
}

impl Tables<'_> {
    /// The native key array follows the descriptors and may continue into
    /// the id array.
    fn asset_key(&self, i: i64) -> Result<u64> {
        let at = (self.count * 16 + i * 8) as usize;
        let records = self.records.len();
        if at + 8 <= records {
            Bin(self.records).u64(at)
        } else if at >= records {
            Bin(self.ids).u64(at - records)
        } else {
            Err(Error::invalid("ETPF asset key straddles an unsupported metadata boundary."))
        }
    }

    fn describe(&self, i: i64) -> Result<(PoolTexture, Option<Surface>)> {
        let record = Record::read(Bin(self.records), i as usize)?;
        let id = hex_upper(Bin(self.ids).bytes(i as usize * 32, 32)?);
        let asset_key = self.asset_key(i)?;
        let top_only = self.legacy_top_only(&record);
        let (readable, limitation) = match self.analyze(i, &record, top_only) {
            Ok(found) => (Some(found), None),
            Err(Error::Io(error)) => return Err(Error::Io(error)),
            Err(error) => (None, Some(error.to_string())),
        };
        let (base_mip, exported, stored, dds_size) = readable.as_ref().map_or((0, 0, 0, 0), |r| (r.base_mip, r.exported, r.stored, r.dds_size));
        let format_name = dxgi::name(record.format);
        let texture = PoolTexture {
            index: i as i32,
            name: format!("texture_{i:06}_{}{}.dds", &id[..16], if top_only { "_top_mip" } else { "" }),
            width: record.width,
            height: record.height,
            dxgi_format: record.format,
            format: if format_name == "Unknown" { format!("DXGI {}", record.format) } else { format_name.to_string() },
            mips: exported,
            depth: record.depth,
            dimension: record.dimension,
            stored_size: stored,
            dds_size,
            source_mips: record.mips(),
            non_tail_mip_count: record.non_tail,
            packed_mip_count: record.packed,
            supports_native_replacement: !record.compact() && limitation.is_none() && base_mip == 0 && exported == record.mips(),
            base_mip_level: base_mip,
            asset_key,
            id,
            virtual_offset: record.base(),
            preview_note: preview_note(&record, base_mip, exported),
            limitation,
        };
        Ok((texture, readable.map(|r| r.surface)))
    }

    fn analyze(&self, i: i64, record: &Record, top_only: bool) -> Result<Readable> {
        let base = record.base();
        if base == 0 || record.page == 0x1F_FFFF {
            return Err(Error::not_supported("This catalog record has no mapped allocation in this pool shard."));
        }
        if base < self.padded_metadata || base >= self.virtual_size {
            return Err(Error::invalid("Texture virtual base exceeds the pool data."));
        }
        if record.dimension != 1 || record.depth != 1 {
            return Err(Error::not_supported("This ETPF array/volume layout has not been validated."));
        }
        let mips = record.mips();
        match record.mode {
            0 | 2 if record.auxiliary() && !(record.mode == 2 && profiles::auxiliary(record)) => {
                return Err(Error::not_supported("Auxiliary compact mip mapping is not decoded."));
            }
            0 | 2 => {}
            _ if !(record.compact() && profiles::compact(record)) => {
                return Err(Error::not_supported("This compact mip mapping profile has not been validated."));
            }
            _ => {}
        }
        let tiling = if record.mode == 0 { Tiling::Flat64K } else { Tiling::Flat4K };
        let surface = Surface::new(record.width as u64, record.height as u64, 1, record.format, mips as u64, tiling)?;
        let size = surface.size as i64;
        if size > self.virtual_size - base {
            return Err(Error::invalid("Texture allocation exceeds the virtual pool."));
        }
        // The allocation runs up to the next texture with a higher base.
        let mut next = self.virtual_size;
        for following in i + 1..self.count {
            let candidate = (Bin(self.records).u32(following as usize * 16 + 8)? & 0x1F_FFFF) as i64 * 4096;
            if candidate < base {
                return Err(Error::invalid("Texture allocations are not ordered."));
            }
            if candidate > base {
                next = candidate;
                break;
            }
        }
        if size != next - base {
            return Err(Error::invalid("Texture footprint does not match its native allocation span."));
        }
        let usable = if record.compact() { record.non_tail } else { mips };
        if record.compact() {
            check_compact_order(&surface, record.non_tail)?;
        }
        let mut available = Vec::with_capacity(usable.max(0) as usize);
        for level in &surface.levels[..usable.max(0) as usize] {
            available.push(readable(self.chunks, base + level.offset as i64, level.size as i64)?);
        }
        let base_mip = available.iter().position(|&ok| ok).ok_or_else(|| Error::not_supported("All source mips require an unavailable Xbox BCPack decoder."))?;
        let exported = if base_mip == 0 && top_only { 1 } else { available[base_mip..].iter().take_while(|&&ok| ok).count() };
        let mut used = Vec::new();
        let mut dds_size = dds::DX10_HEADER_BYTES as i64;
        for level in base_mip..base_mip + exported {
            let info = &surface.levels[level];
            for key in chunks_for(self.chunks, base + info.offset as i64, info.size as i64)? {
                if !used.contains(&key) {
                    used.push(key);
                }
            }
            dds_size += surface.linear_size(level) as i64;
        }
        if dds_size > MAX_OUTPUT {
            return Err(Error::not_supported("DDS exceeds the 256 MiB output limit."));
        }
        let stored = used.iter().map(|&k| self.chunks[k].packed).sum();
        Ok(Readable { surface, base_mip: base_mip as i32, exported: exported as i32, stored, dds_size })
    }

    /// Older pools mark a texture whose tail is BCPack-compressed while its
    /// top mip is readable. Only the top mip is exported for those.
    fn legacy_top_only(&self, record: &Record) -> bool {
        let old_mips = record.packed + 1;
        let shaped = record.second >> 23 == 0
            && (record.second >> 11) & 1023 == 0
            && (record.second >> 21) & 3 == 1
            && record.third >> 22 == 0
            && record.extra == 0
            && old_mips > 1;
        if !shaped {
            return false;
        }
        let base = (record.third & 0x3F_FFFF) as i64 * 4096;
        if base < self.padded_metadata || base >= self.virtual_size {
            return false;
        }
        let test = || -> Result<bool> {
            let surface = Surface::new(record.width as u64, record.height as u64, 1, record.format, old_mips as u64, Tiling::Flat64K)?;
            if record.non_tail as usize != surface.first_tail || surface.size as i64 > self.virtual_size - base {
                return Ok(false);
            }
            if readable(self.chunks, base, surface.size as i64)? {
                return Ok(false);
            }
            let top = &surface.levels[0];
            readable(self.chunks, base + top.offset as i64, top.size as i64)
        };
        test().unwrap_or(false)
    }
}

/// Compact layouts must store their standalone mips, largest last, right
/// after the packed tail and fill the allocation exactly.
fn check_compact_order(surface: &Surface, standalone: i32) -> Result<()> {
    let standalone = standalone.max(0) as usize;
    if standalone < 1 || standalone > surface.first_tail {
        return Err(Error::not_supported("No validated standalone compact mip range is available."));
    }
    let levels = &surface.levels[..standalone];
    let mut offset = surface.size as i64 - levels.iter().map(|l| l.size as i64).sum::<i64>();
    if offset < 0 {
        return Err(Error::invalid("Compact tail region exceeds its native allocation."));
    }
    for level in levels.iter().rev() {
        if level.offset as i64 != offset {
            return Err(Error::not_supported("Compact standalone mip offsets are ambiguous."));
        }
        offset += level.size as i64;
    }
    if offset != surface.size as i64 {
        return Err(Error::invalid("Compact standalone mip ranges do not cover their expected allocation window."));
    }
    Ok(())
}

fn preview_note(record: &Record, base_mip: i32, exported: i32) -> Option<String> {
    let (width, height, mips) = (record.width, record.height, record.mips());
    let mut notes = Vec::new();
    if record.compact() {
        notes.push(format!(
            "Read-only compact profile: {} standalone mips and {} packed tail mips. Packed tail decoding and native replacement are unavailable.",
            record.non_tail, record.packed
        ));
    }
    if base_mip > 0 {
        notes.push(format!(
            "Source is {width} × {height} with {mips} mips. Export starts at source mip {base_mip}: {} × {}, with {exported} decoded mip levels. Other source mips remain unavailable.",
            dxgi::mip_extent(width, base_mip),
            dxgi::mip_extent(height, base_mip)
        ));
    } else if exported > 0 && exported < mips {
        notes.push(format!(
            "Source is {width} × {height} with {mips} mips. Export contains {exported} decoded mip levels starting at source mip 0: {width} × {height}. Other source mips remain unavailable."
        ));
    }
    if matches!(record.format, 10 | 34 | 41 | 54 | 95 | 96) {
        notes.push("Floating-point channels clip to 0–1 for display; DDS preserves source values.".into());
    }
    if record.format == 65 {
        notes.push("Alpha mask displayed as grayscale.".into());
    }
    let preview = base_mip + dxgi::preview_level(dxgi::mip_extent(width, base_mip), dxgi::mip_extent(height, base_mip), exported);
    if preview > 0 {
        notes.push(format!("Preview uses mip {preview}: {} × {}.", dxgi::mip_extent(width, preview), dxgi::mip_extent(height, preview)));
    }
    (!notes.is_empty()).then(|| notes.join(" "))
}

/// Storage profiles observed on the Xbox Series 1.0.2.1 sample and checked
/// against decoded output. Other combinations stay read-blocked until
/// they are verified.
mod profiles {
    use super::{dxgi, Record};

    /// (mode, width, height, mips, standalone mips, element bytes, meta, extra)
    type Profile = (i32, i32, i32, i32, i32, u64, u32, u32);

    #[rustfmt::skip]
    const COMPACT: &[Profile] = &[
        (4, 1024, 1024, 9, 3, 8, 0x503, 0x00B0_0106), (6, 512, 512, 8, 3, 8, 0x041, 0x00B8_00D4),
        (4, 256, 256, 7, 1, 8, 0x103, 0), (4, 512, 512, 8, 3, 16, 0x482, 0x0150_00F5),
        (4, 1024, 512, 8, 3, 8, 0x482, 0x0150_00F5), (4, 512, 1024, 8, 3, 8, 0x483, 0x0130_00F5),
        (4, 128, 256, 6, 1, 8, 0x083, 0), (4, 256, 512, 7, 1, 8, 0x167, 0),
        (4, 512, 256, 7, 1, 8, 0x166, 0), (4, 256, 256, 7, 1, 16, 0x166, 0),
        (4, 1024, 512, 8, 3, 16, 0x504, 0x0090_0106), (4, 1024, 256, 7, 3, 16, 0x484, 0x0110_00F5),
        (6, 1024, 256, 7, 3, 8, 0x042, 0x0098_00D4), (6, 64, 1024, 5, 3, 8, 0x083, 0x0038_00B4),
        (4, 512, 1024, 8, 3, 16, 0x503, 0x00B0_0106), (4, 512, 128, 6, 1, 16, 0x168, 0),
        (6, 256, 1024, 7, 3, 8, 0x083, 0x0038_00D4), (4, 1024, 128, 6, 1, 8, 0x168, 0),
        (4, 2048, 256, 7, 3, 16, 0x508, 0x0010_0106), (4, 2048, 512, 8, 3, 8, 0x504, 0x0090_0106),
        (6, 512, 256, 7, 3, 16, 0x042, 0x0098_00D4), (6, 1024, 128, 6, 3, 16, 0x084, 0x0018_00D4),
        (4, 512, 128, 6, 1, 8, 0x104, 0), (4, 256, 1024, 7, 3, 16, 0x483, 0x0130_00F5),
        (6, 1024, 64, 5, 3, 16, 0x083, 0x0038_00B4), (6, 256, 512, 7, 3, 16, 0x041, 0x00B8_00D4),
        (4, 128, 128, 6, 1, 16, 0x082, 0), (4, 512, 64, 5, 1, 16, 0x108, 0),
        (4, 256, 128, 6, 1, 16, 0x104, 0), (6, 128, 1024, 6, 3, 16, 0x083, 0x0038_00D4),
        (4, 512, 64, 5, 1, 8, 0x084, 0), (4, 128, 512, 6, 1, 16, 0x167, 0),
        (4, 256, 128, 6, 1, 8, 0x082, 0), (4, 1024, 64, 5, 1, 8, 0x108, 0),
        (6, 1024, 32, 4, 3, 16, 0x082, 0x0058_00B4), (4, 128, 512, 6, 1, 8, 0x107, 0),
        (4, 512, 2048, 8, 3, 8, 0x507, 0x0030_0106), (4, 256, 2048, 7, 3, 8, 0x507, 0x0030_00F5),
        (4, 512, 32, 4, 1, 16, 0x107, 0), (6, 128, 1024, 6, 3, 8, 0x083, 0x0038_00B4),
        (6, 64, 1024, 5, 3, 16, 0x083, 0x0038_00B4), (4, 256, 256, 5, 1, 16, 0x166, 0),
        (4, 128, 256, 6, 1, 16, 0x103, 0), (4, 256, 64, 5, 1, 16, 0x084, 0),
        (4, 16, 256, 3, 1, 16, 0x083, 0), (4, 256, 32, 4, 1, 16, 0x084, 0),
        (4, 64, 512, 5, 1, 16, 0x107, 0), (4, 2048, 256, 7, 3, 8, 0x484, 0x0110_00F5),
        (4, 64, 512, 5, 1, 8, 0x107, 0), (4, 64, 256, 5, 1, 8, 0x083, 0),
        (6, 256, 512, 9, 3, 16, 0x041, 0x00B8_00D4), (6, 32, 1024, 4, 3, 16, 0x082, 0x0058_00B4),
        (4, 512, 512, 3, 3, 8, 0x502, 0x00D0_0006), (36, 1024, 1024, 9, 3, 8, 0x503, 0x00B0_0106),
        (36, 256, 256, 7, 1, 8, 0x103, 0),
    ];

    /// Compact profiles of uncompressed formats, keyed by exact format.
    const COMPACT_RAW: &[(i32, Profile)] = &[(65, (4, 512, 512, 8, 3, 1, 0x482, 0x0150_00F5))];

    fn key(record: &Record, element: u64) -> Profile {
        (record.mode, record.width, record.height, record.mips(), record.non_tail, element, record.meta, record.extra)
    }

    pub(super) fn compact(record: &Record) -> bool {
        let Some(element) = dxgi::element_size(record.format) else { return false };
        if dxgi::is_compressed(record.format) {
            COMPACT.contains(&key(record, element))
        } else {
            COMPACT_RAW.contains(&(record.format, key(record, element)))
        }
    }

    /// Mode-2 layouts with an auxiliary mapping that decode as standard.
    pub(super) fn auxiliary(record: &Record) -> bool {
        let shape = (record.mips(), record.non_tail, record.meta, record.extra) == (3, 3, 0x1A8, 0x0108_0008) && record.width == 1024;
        let r8 = record.height == 512 && record.format == 61;
        let bc8 = record.height == 1024 && dxgi::is_compressed(record.format) && dxgi::element_size(record.format) == Some(8);
        shape && (r8 || bc8)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::native::bin::Out;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    /// A one-texture pool whose allocation is split across two zlib chunks.
    pub(crate) fn sample(width: u32, height: u32, format: u32, mips: u32) -> Vec<u8> {
        let surface = Surface::new(width as u64, height as u64, 1, format as i32, mips as u64, Tiling::Flat64K).unwrap();
        let size = surface.size as i64;
        let base = 4096i64;
        let virtual_size = base + size;
        let texels: Vec<u8> = (0..size).map(|i| (i * 13 % 253) as u8).collect();
        let half = size / 2 / 4096 * 4096;
        let table_words = 3u32;
        let etpf = bin::align(24 + table_words as u64 * 8, 256) as i64;
        let first_source = etpf + base;
        let mut physical = Vec::new();
        let mut words = Vec::new();
        for (data, start) in [(zlib(&texels[..half as usize]), base), (zlib(&texels[half as usize..]), base + half)] {
            let source = first_source + physical.len() as i64;
            let pad = (256 - (data.len() as i64 & 255)) & 255;
            words.push((source / 256) as u64 | ((start / 4096) as u64) << 30 | (ZLIB as u64) << 26 | (pad as u64) << 52);
            physical.extend_from_slice(&data);
            physical.resize(physical.len() + pad as usize, 0);
        }
        let end = first_source + physical.len() as i64;
        words.push((end / 256) as u64 | ((virtual_size / 4096) as u64) << 30);
        let mut file = Out::default();
        file.raw(b"RDS5").u32(table_words + 1).raw(&(virtual_size as u64 | 22u64 << 56).to_le_bytes()).u32((table_words + 1).div_ceil(32)).u32(0);
        for value in &words {
            file.raw(&value.to_le_bytes());
        }
        file.0.resize(etpf as usize, 0);
        let standalone = surface.first_tail.min(mips as usize) as u32;
        file.raw(b"ETPF").u32(ETPF_VERSION).u32(1).u32(24);
        file.u32((width - 1) | (height - 1) << 14 | (mips - standalone) << 28);
        file.u32(standalone | format << 3 | 1 << 21).u32((base / 4096) as u32).u32(0);
        file.raw(&0x1122_3344_5566_7788u64.to_le_bytes());
        file.raw(&(0..32).collect::<Vec<u8>>());
        file.0.resize(first_source as usize, 0);
        file.raw(&physical);
        file.0
    }

    #[test]
    fn reads_and_exports_a_pool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.ttpf");
        std::fs::write(&path, sample(256, 256, 71, 9)).unwrap();
        let pool = PoolIndex::read(&path, &Cancel::none()).unwrap();
        let texture = &pool.info.textures[0];
        assert_eq!(texture.limitation, None);
        assert_eq!(texture.asset_key, 0x1122_3344_5566_7788);
        assert!(texture.supports_native_replacement);
        let exported = pool.create_dds(0, &Cancel::none()).unwrap();
        let info = dds::inspect(&exported).unwrap();
        assert_eq!((info.width, info.height, info.mips), (256, 256, 9));
    }

    #[test]
    fn table_words_unpack() {
        let value = 3u64 | 5 << 30 | (ZLIB as u64) << 26 | 17 << 52;
        assert_eq!((word::source(value), word::virtual_start(value), word::codec(value), word::padding(value)), (768, 20480, ZLIB, 17));
    }
}
