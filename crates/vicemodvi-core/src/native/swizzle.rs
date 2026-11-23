//! AMD tiled surface layout used by Xbox Series textures.
//!
//! A surface is split into tiles of 4 KiB or 64 KiB. Inside a tile the
//! element address interleaves bits of x, y and (for volumes) z in a fixed
//! order per element size. Small mips share one tile, the mip tail, at
//! fixed positions. Because each address bit comes from exactly one
//! coordinate bit, the address is a sum of independent x, y and z terms,
//! so each level precomputes one term per column and per row.

use super::{bin, dxgi};
use crate::{Error, Result};

const MAX_SURFACE_BYTES: u64 = 256 * 1024 * 1024;

/// Source of one address bit: always zero, or bit `n` of x, y or z.
#[derive(Clone, Copy, Debug)]
enum Bit {
    O,
    X(u8),
    Y(u8),
    Z(u8),
}

use Bit::{O, X, Y, Z};

/// 4 KiB 2D tiles, by element size class (1, 2, 4, 8 and 16 bytes).
const TILE_4K: [[Bit; 12]; 5] = [
    [X(0), X(1), X(2), X(3), Y(0), Y(1), Y(2), Y(3), Y(4), X(4), Y(5), X(5)],
    [O, X(0), X(1), X(2), Y(0), Y(1), Y(2), X(3), Y(3), X(4), Y(4), X(5)],
    [O, O, X(0), X(1), Y(0), Y(1), Y(2), X(2), Y(3), X(3), Y(4), X(4)],
    [O, O, O, X(0), Y(0), Y(1), X(1), X(2), Y(2), X(3), Y(3), X(4)],
    [O, O, O, O, Y(0), Y(1), X(0), X(1), Y(2), X(2), Y(3), X(3)],
];

/// 64 KiB 2D tiles, by element size class.
const TILE_64K: [[Bit; 16]; 5] = [
    [X(0), X(1), X(2), Y(1), Y(0), Y(2), X(3), Y(3), Y(4), X(4), Y(5), X(5), Y(6), X(6), Y(7), X(7)],
    [O, X(0), X(1), X(2), Y(0), Y(1), Y(2), X(3), Y(3), X(4), Y(4), X(5), Y(5), X(6), Y(6), X(7)],
    [O, O, X(0), X(1), Y(0), Y(1), Y(2), X(2), Y(3), X(3), Y(4), X(4), Y(5), X(5), Y(6), X(6)],
    [O, O, O, X(0), Y(0), X(1), X(2), Y(1), Y(2), X(3), Y(3), X(4), Y(4), X(5), Y(5), X(6)],
    [O, O, O, O, X(0), Y(0), X(1), Y(1), Y(2), X(2), Y(3), X(3), Y(4), X(4), Y(5), X(5)],
];

/// 4 KiB volume tiles, by element size class.
const VOLUME_4K: [[Bit; 12]; 5] = [
    [X(0), X(1), Z(0), Y(0), Z(1), Y(1), X(2), Z(2), Y(2), X(3), Z(3), Y(3)],
    [O, X(0), Z(0), Y(0), Z(1), Y(1), X(1), Z(2), Y(2), X(2), Z(3), Y(3)],
    [O, O, X(0), Y(0), Z(0), Y(1), X(1), Z(1), Y(2), X(2), Z(2), Y(3)],
    [O, O, O, X(0), Z(0), Y(0), X(1), Z(1), Y(1), X(2), Z(2), Y(2)],
    [O, O, O, O, Z(0), Y(0), X(0), Z(1), Y(1), X(1), Z(2), Y(2)],
];

/// The tile kinds the readers support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tiling {
    Flat4K,
    Flat64K,
    Volume4K,
}

/// One mip level, measured in elements.
#[derive(Clone, Debug)]
pub(crate) struct Level {
    pub width: u64,
    pub height: u64,
    padded_width: u64,
    /// Byte offset of the level inside the allocation (0 for tail mips).
    pub offset: u64,
    /// Bytes reserved for the level, per z block for volumes.
    pub size: u64,
    tail_x: u64,
    tail_y: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Surface {
    /// Bytes of one allocation (one slice for 2D arrays and cubes).
    pub size: u64,
    /// Index of the first mip stored in the tail, or the mip count.
    pub first_tail: usize,
    pub element: u64,
    pub levels: Vec<Level>,
    tile: u64,
    block: (u64, u64, u64),
    z_stride: u64,
    pattern: &'static [Bit],
}

/// Per-level address terms.
struct Terms {
    base: u64,
    columns: Vec<u64>,
    rows: Vec<u64>,
}

fn gather(pattern: &[Bit], value: u64, pick: fn(Bit) -> Option<u8>) -> u64 {
    pattern
        .iter()
        .enumerate()
        .filter_map(|(bit, &source)| pick(source).map(|n| ((value >> n) & 1) << bit))
        .fold(0, |acc, term| acc | term)
}

fn x_of(bit: Bit) -> Option<u8> {
    if let X(n) = bit {
        Some(n)
    } else {
        None
    }
}

fn y_of(bit: Bit) -> Option<u8> {
    if let Y(n) = bit {
        Some(n)
    } else {
        None
    }
}

fn z_of(bit: Bit) -> Option<u8> {
    if let Z(n) = bit {
        Some(n)
    } else {
        None
    }
}

impl Surface {
    pub fn new(width: u64, height: u64, depth: u64, format: i32, mips: u64, tiling: Tiling) -> Result<Self> {
        let volume = tiling == Tiling::Volume4K;
        let largest = width.max(height).max(if volume { depth } else { 1 });
        if mips < 1 || mips as i64 > dxgi::full_chain(largest as i64) {
            return Err(Error::invalid("The mip count does not fit the texture size."));
        }
        let element = dxgi::element_size_of(format)?;
        let class = dxgi::size_class(element)?;
        let side = dxgi::block_side(format);
        let (tile, pattern, block, tail_block, max_tail): (u64, &'static [Bit], _, _, u64) = match tiling {
            Tiling::Flat4K => (4096, &TILE_4K[class], flat_block(6, class), flat_tail(class), 8),
            Tiling::Flat64K => (65536, &TILE_64K[class], flat_block(8, class), flat_tail(class), 12),
            Tiling::Volume4K => {
                let block = [(16, 16, 16), (8, 16, 16), (8, 16, 8), (8, 8, 8), (4, 8, 8)][class];
                let tail = [(8, 4), (4, 4), (4, 4), (4, 2), (2, 2)][class];
                (4096, &VOLUME_4K[class], block, tail, 5)
            }
        };
        let (block_w, block_h, block_d) = block;
        // A level joins the tail once it fits the tail window.
        let (tail_w, tail_h) = if volume { (block_w, block_h / 2) } else { (block_w / 2, block_h) };
        let base_w = width.div_ceil(side);
        let base_h = height.div_ceil(side);
        let elements = |pixels: u64, level: u64| (pixels >> level).max(1).div_ceil(side);
        let mut levels = Vec::with_capacity(mips as usize);
        let mut first_tail = mips;
        for level in 0..mips {
            let w = base_w.div_ceil(1 << level).max(1);
            let h = base_h.div_ceil(1 << level).max(1);
            if mips > 1 && w <= tail_w && h <= tail_h && mips - level <= max_tail {
                first_tail = level;
                break;
            }
            let padded_width = bin::align(w, block_w);
            let size = padded_width * bin::align(h, block_h) * element;
            levels.push(Level { width: elements(width, level), height: elements(height, level), padded_width, offset: 0, size, tail_x: 0, tail_y: 0 });
        }
        // Levels are stored smallest first, after the tail tile.
        let mut end = if first_tail < mips { tile } else { 0 };
        for level in levels.iter_mut().rev() {
            level.offset = end;
            end += level.size * block_d;
            if end > MAX_SURFACE_BYTES {
                return Err(Error::not_supported("Texture exceeds the 256 MiB resource limit."));
            }
        }
        for level in first_tail..mips {
            let slot = max_tail - 1 - (level - first_tail);
            let position = if slot > 6 { 16 << slot } else { slot << 8 };
            // Even bits from 8 hold y, odd bits from 9 hold x.
            let (mut x, mut y) = (0, 0);
            for bit in 0..6 {
                x |= ((position >> (9 + 2 * bit)) & 1) << bit;
                y |= ((position >> (8 + 2 * bit)) & 1) << bit;
            }
            levels.push(Level {
                width: elements(width, level),
                height: elements(height, level),
                padded_width: block_w,
                offset: 0,
                size: tile,
                tail_x: x * tail_block.0,
                tail_y: y * tail_block.1,
            });
        }
        let size = if volume { end / block_d * bin::align(depth, block_d) } else { end };
        if size == 0 || size > MAX_SURFACE_BYTES {
            return Err(Error::not_supported("Texture exceeds the 256 MiB resource limit."));
        }
        Ok(Self { size, first_tail: first_tail as usize, element, levels, tile, block, z_stride: if volume { end } else { 0 }, pattern })
    }

    fn terms(&self, level: &Level, z: u64) -> Terms {
        let (block_w, block_h, block_d) = self.block;
        let tiles_per_row = level.padded_width / block_w;
        let columns = (0..level.width).map(|x| x / block_w * self.tile + gather(self.pattern, x + level.tail_x, x_of)).collect();
        let rows = (0..level.height)
            .map(|y| y / block_h * tiles_per_row * self.tile + gather(self.pattern, y + level.tail_y, y_of))
            .collect();
        let base = if self.z_stride == 0 { 0 } else { z / block_d * self.z_stride } + level.offset + gather(self.pattern, z, z_of);
        Terms { base, columns, rows }
    }

    /// Copies one level out of `tiled`, whose first byte sits at `origin`
    /// inside the allocation, into a linear row-major buffer.
    pub fn read_level(&self, tiled: &[u8], origin: u64, level: usize, z: u64) -> Result<Vec<u8>> {
        let info = self.level(level)?;
        let terms = self.terms(info, z);
        let element = self.element as usize;
        let mut linear = Vec::with_capacity((info.width * info.height) as usize * element);
        for &row in &terms.rows {
            for &column in &terms.columns {
                let at = (terms.base + row + column).checked_sub(origin).ok_or_else(bin::outside)?;
                linear.extend_from_slice(&tiled[bin::range(tiled.len(), at as usize, element)?]);
            }
        }
        Ok(linear)
    }

    /// Writes a linear level into `tiled`, the inverse of [`Surface::read_level`].
    pub fn write_level(&self, tiled: &mut [u8], origin: u64, level: usize, z: u64, linear: &[u8]) -> Result<()> {
        let info = self.level(level)?;
        let terms = self.terms(info, z);
        let element = self.element as usize;
        if linear.len() != (info.width * info.height) as usize * element {
            return Err(Error::invalid("DDS mip data is shorter than its descriptor."));
        }
        let mut source = linear.chunks_exact(element);
        for &row in &terms.rows {
            for &column in &terms.columns {
                let at = (terms.base + row + column).checked_sub(origin).ok_or_else(bin::outside)?;
                let target = bin::range(tiled.len(), at as usize, element)?;
                tiled[target].copy_from_slice(source.next().ok_or_else(bin::outside)?);
            }
        }
        Ok(())
    }

    pub fn level(&self, level: usize) -> Result<&Level> {
        self.levels.get(level).ok_or_else(|| Error::argument("The mip level is outside the texture."))
    }

    /// Bytes of a level once detiled.
    pub fn linear_size(&self, level: usize) -> u64 {
        self.levels.get(level).map_or(0, |l| l.width * l.height * self.element)
    }
}

/// 2D tile size in elements: the tile is square in bytes, so the side
/// shrinks by half a bit per element size step.
fn flat_block(half_bits: u32, class: usize) -> (u64, u64, u64) {
    let class = class as u32;
    (1 << (half_bits - class / 2), 1 << (half_bits - class.div_ceil(2)), 1)
}

/// Spacing of mip-tail slots in elements.
fn flat_tail(class: usize) -> (u64, u64) {
    [(16, 16), (16, 8), (8, 8), (8, 4), (4, 4)][class]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_hold_a_whole_number_of_elements() {
        for class in 0..5 {
            let (w, h, _) = flat_block(6, class);
            assert_eq!((w * h) << class, 4096);
            let (w, h, _) = flat_block(8, class);
            assert_eq!((w * h) << class, 65536);
        }
    }

    #[test]
    fn levels_roundtrip_through_tiles() {
        for (tiling, format, w, h, d, mips) in
            [(Tiling::Flat4K, 28, 300, 200, 1, 9), (Tiling::Flat64K, 71, 512, 256, 1, 10), (Tiling::Volume4K, 28, 32, 32, 8, 6), (Tiling::Flat4K, 98, 64, 64, 1, 7)]
        {
            let surface = Surface::new(w, h, d, format, mips, tiling).unwrap();
            let mut tiled = vec![0u8; surface.size as usize];
            let planes = if tiling == Tiling::Volume4K { d } else { 1 };
            for level in 0..mips as usize {
                for z in 0..(planes >> level).max(1) {
                    let count = surface.linear_size(level) as usize;
                    let linear: Vec<u8> = (0..count).map(|i| (i as u64 * 7 + level as u64 * 13 + z * 5) as u8).collect();
                    surface.write_level(&mut tiled, 0, level, z, &linear).unwrap();
                    assert_eq!(surface.read_level(&tiled, 0, level, z).unwrap(), linear);
                }
            }
        }
    }

    #[test]
    fn distinct_texels_get_distinct_addresses() {
        let surface = Surface::new(256, 128, 1, 28, 9, Tiling::Flat4K).unwrap();
        let mut seen = std::collections::HashSet::new();
        for level in &surface.levels[..surface.first_tail] {
            let terms = surface.terms(level, 0);
            for &row in &terms.rows {
                for &column in &terms.columns {
                    assert!(seen.insert(terms.base + row + column));
                }
            }
        }
    }
}
