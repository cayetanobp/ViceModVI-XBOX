//! Synthetic RPF9 and TTPF files for tests.
#![allow(dead_code)]

use std::io::Write;

use flate2::{write::ZlibEncoder, Compression};

pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// RDS5 pool whose textures each fill one 64 KiB tail allocation:
/// 256×256 BC1 with 9 mips and 64×64 R8G8B8A8 with 7 mips qualify.
pub fn pool(textures: &[(u32, u32, u32, u32, u64)]) -> Vec<u8> {
    let n = textures.len() as i64;
    let allocation = 65536i64;
    let base = 4096i64;
    let virtual_size = base + n * allocation;
    let words = n as u32 + 1;
    let count = words + 1;
    let etpf = (24 + words as i64 * 8 + 255) / 256 * 256;
    let first_source = etpf + base;
    let mut physical = Vec::new();
    let mut table = Vec::new();
    for i in 0..n {
        let data: Vec<u8> = (0..allocation).map(|b| ((b * 7 + i * 31) % 251) as u8).collect();
        let packed = zlib(&data);
        let source = first_source + physical.len() as i64;
        let pad = (256 - (packed.len() as i64 & 255)) & 255;
        let virtual_start = base + i * allocation;
        table.push((source / 256) as u64 | ((virtual_start / 4096) as u64) << 30 | 8u64 << 26 | (pad as u64) << 52);
        physical.extend_from_slice(&packed);
        physical.resize(physical.len() + pad as usize, 0);
    }
    table.push(((first_source + physical.len() as i64) / 256) as u64 | ((virtual_size / 4096) as u64) << 30);
    let mut file = b"RDS5".to_vec();
    put32(&mut file, count);
    file.extend_from_slice(&(virtual_size as u64 | 22u64 << 56).to_le_bytes());
    put32(&mut file, count.div_ceil(32));
    put32(&mut file, 0);
    for word in &table {
        file.extend_from_slice(&word.to_le_bytes());
    }
    file.resize(etpf as usize, 0);
    file.extend_from_slice(b"ETPF");
    put32(&mut file, 634);
    put32(&mut file, n as u32);
    put32(&mut file, (n * 24) as u32);
    for (i, &(w, h, format, mips, _)) in textures.iter().enumerate() {
        put32(&mut file, (w - 1) | (h - 1) << 14 | mips << 28);
        put32(&mut file, format << 3 | 1 << 21);
        put32(&mut file, ((base + i as i64 * allocation) / 4096) as u32);
        put32(&mut file, 0);
    }
    for &(.., key) in textures {
        file.extend_from_slice(&key.to_le_bytes());
    }
    file.extend((0..n * 32).map(|i| i as u8));
    file.resize(first_source as usize, 0);
    file.extend_from_slice(&physical);
    file
}

fn set(buffer: &mut [u8], at: usize, bytes: &[u8]) {
    buffer[at..at + bytes.len()].copy_from_slice(bytes);
}

/// Dictionary with two keyed textures whose mip chains fit one 4 KiB tail:
/// 64×64 BC1 with 7 mips and 16×16 R8G8B8A8 with 5 mips.
pub fn dictionary() -> (Vec<u8>, Vec<u8>) {
    let base = 0x5000_0000u64;
    let mut sys = vec![0u8; 0x800];
    set(&mut sys, 0x08, &(base + 0x80).to_le_bytes());
    sys[0x88] = 1;
    sys[0x89] = 1;
    set(&mut sys, 0x20, &(base + 0x100).to_le_bytes());
    set(&mut sys, 0x28, &2u16.to_le_bytes());
    set(&mut sys, 0x30, &(base + 0x110).to_le_bytes());
    set(&mut sys, 0x38, &2u16.to_le_bytes());
    set(&mut sys, 0x3A, &2u16.to_le_bytes());
    for (i, (w, h, format, eb, mips, key)) in [(64u16, 64u16, 71u8, 8u16, 7u8, 0xAAAA_0000_0000_0001u64), (16, 16, 28, 4, 5, 0xBBBB_0000_0000_0002)]
        .into_iter()
        .enumerate()
    {
        let t = 0x200 + i * 0x100;
        let d = 0x600 + i * 0x80;
        set(&mut sys, 0x100 + i * 4, &(0x1000_0000u32 + i as u32).to_le_bytes());
        set(&mut sys, 0x110 + i * 8, &(base + t as u64).to_le_bytes());
        set(&mut sys, t + 0x08, &(4096 / eb as u32).to_le_bytes());
        set(&mut sys, t + 0x0C, &eb.to_le_bytes());
        set(&mut sys, t + 0x18, &(0x6000_0000u64 + i as u64 * 4096).to_le_bytes());
        set(&mut sys, t + 0x28, &(base + t as u64 + 0x50).to_le_bytes());
        set(&mut sys, t + 0x30, &(base + d as u64).to_le_bytes());
        set(&mut sys, t + 0x38, &w.to_le_bytes());
        set(&mut sys, t + 0x3A, &h.to_le_bytes());
        set(&mut sys, t + 0x3C, &1u16.to_le_bytes());
        sys[t + 0x3E] = 1;
        sys[t + 0x3F] = format;
        sys[t + 0x40] = 5;
        sys[t + 0x42] = mips;
        set(&mut sys, d - 16, &6u32.to_le_bytes());
        set(&mut sys, d, &(base + d as u64 - 11).to_le_bytes());
        set(&mut sys, d + 8, &key.to_le_bytes());
    }
    let graphics = (0..8192u32).map(|i| (i * 17 % 241) as u8).collect();
    (sys, graphics)
}

/// Minimal 9FPR archive holding one segmented texture dictionary.
pub fn archive() -> Vec<u8> {
    let (system, graphics) = dictionary();
    let mut file = vec![0u8; 4096 + 16];
    file[..4].copy_from_slice(b"9FPR");
    for (at, value) in [(4usize, 608u32), (8, 576), (12, 128)] {
        file[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    file.extend_from_slice(&zlib(&system));
    file.resize(file.len().next_multiple_of(16), 0);
    file.extend_from_slice(&zlib(&graphics));
    file.resize(file.len().next_multiple_of(16) + 4096, 0);
    file
}


/// System and graphics sections of a drawable with one triangle, UVs,
/// vertex colors and one material whose diffuse texture has native key
/// 0x1111.
pub fn drawable() -> (Vec<u8>, Vec<u8>) {
    let p = |offset: usize| (0x5000_0000u64 + offset as u64).to_le_bytes();
    let mut sys = vec![0u8; 0x700];
    // Drawable header.
    set(&mut sys, 0x08, &p(0x100));
    set(&mut sys, 0x10, &p(0x120));
    set(&mut sys, 0x2C, &1.5f32.to_le_bytes());
    for (i, v) in [0f32, 0.0, 0.0, 1.0, 1.0, 0.0].iter().enumerate() {
        set(&mut sys, 0x30 + if i < 3 { i * 4 } else { 16 + (i - 3) * 4 }, &v.to_le_bytes());
    }
    set(&mut sys, 0x50, &p(0x160));
    set(&mut sys, 0xA8, &p(0x600));
    // Page counts, LOD, model list and model.
    sys[0x108] = 1;
    sys[0x109] = 1;
    set(&mut sys, 0x160, &p(0x170));
    set(&mut sys, 0x168, &1u16.to_le_bytes());
    set(&mut sys, 0x16A, &1u16.to_le_bytes());
    set(&mut sys, 0x170, &p(0x180));
    set(&mut sys, 0x188, &p(0x1C0));
    set(&mut sys, 0x190, &1u16.to_le_bytes());
    set(&mut sys, 0x192, &1u16.to_le_bytes());
    set(&mut sys, 0x198, &p(0x1D0));
    set(&mut sys, 0x1B6, &1u16.to_le_bytes());
    set(&mut sys, 0x1C0, &p(0x200));
    for (i, v) in [0f32, 0.0, 0.0].iter().enumerate() {
        set(&mut sys, 0x1D0 + i * 4, &v.to_le_bytes());
    }
    for (i, v) in [1f32, 1.0, 0.0].iter().enumerate() {
        set(&mut sys, 0x1E0 + i * 4, &v.to_le_bytes());
    }
    // Geometry, vertex buffer, index descriptor and vertex format.
    set(&mut sys, 0x208, &p(0x2A0));
    set(&mut sys, 0x240, &p(0x2F0));
    set(&mut sys, 0x2A8, &48u32.to_le_bytes());
    set(&mut sys, 0x2AC, &4u16.to_le_bytes());
    set(&mut sys, 0x2B8, &0x6000_0000u64.to_le_bytes());
    set(&mut sys, 0x2E0, &p(0x300));
    set(&mut sys, 0x2F0, &p(0x440));
    set(&mut sys, 0x2F8, &1u16.to_le_bytes());
    set(&mut sys, 0x2FA, &1u16.to_le_bytes());
    set(&mut sys, 0x2FC, &(3u32 << 8).to_le_bytes());
    let fvf = 0x300;
    set(&mut sys, fvf, &96u32.to_le_bytes());
    set(&mut sys, fvf + 24 * 4, &156u32.to_le_bytes());
    set(&mut sys, fvf + 28 * 4, &132u32.to_le_bytes());
    sys[fvf + 0xD0] = 12;
    sys[fvf + 0x104] = 6;
    sys[fvf + 0xD0 + 24] = 4;
    sys[fvf + 0x104 + 24] = 28;
    sys[fvf + 0xD0 + 28] = 8;
    sys[fvf + 0x104 + 28] = 16;
    set(&mut sys, fvf + 0x138, &((3u32 << 10) | 1).to_le_bytes());
    // Index buffer object and name.
    set(&mut sys, 0x448, &12u32.to_le_bytes());
    set(&mut sys, 0x44C, &4u16.to_le_bytes());
    set(&mut sys, 0x458, &0x6000_0100u64.to_le_bytes());
    set(&mut sys, 0x600, b"test_prop\0");
    // Material: shader group, shader, parameter table, texture and key.
    set(&mut sys, 0x130, &p(0x4A0));
    set(&mut sys, 0x138, &1u16.to_le_bytes());
    set(&mut sys, 0x13A, &1u16.to_le_bytes());
    set(&mut sys, 0x4A0, &p(0x4C0));
    set(&mut sys, 0x4C0, &0x1234_5678u32.to_le_bytes());
    set(&mut sys, 0x4C4, &0x6D65_7461u32.to_le_bytes());
    set(&mut sys, 0x4D0, &p(0x510));
    set(&mut sys, 0x4E0, &p(0x500));
    sys[0x501] = 1;
    sys[0x504] = 1;
    sys[0x507] = 1;
    set(&mut sys, 0x508, &3_370_697_346u32.to_le_bytes());
    set(&mut sys, 0x510, &p(0x520));
    set(&mut sys, 0x530, &0x980u32.to_le_bytes());
    set(&mut sys, 0x550, &p(0x590));
    set(&mut sys, 0x55C, &1u16.to_le_bytes());
    set(&mut sys, 0x55E, &1u16.to_le_bytes());
    sys[0x560] = 255;
    set(&mut sys, 0x580, &6u32.to_le_bytes());
    set(&mut sys, 0x588, &0x0BAD_F00Du32.to_le_bytes());
    set(&mut sys, 0x590, &(0x5000_0000u64 + 0x590 - 11).to_le_bytes());
    set(&mut sys, 0x598, &0x1111u64.to_le_bytes());
    set(&mut sys, 0x1A0, &p(0x5B0));

    let mut gfx = vec![0u8; 304];
    set(&mut gfx, 0, &0x63u32.to_le_bytes());
    set(&mut gfx, 4, &((156u32 << 7) | 32 | 6).to_le_bytes());
    set(&mut gfx, 12, &((132u32 << 7) | 64 | 2).to_le_bytes());
    for (i, v) in [0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0].iter().enumerate() {
        set(&mut gfx, 96 + i * 4, &v.to_le_bytes());
    }
    for (i, v) in [0f32, 0.0, 1.0, 0.0, 0.0, 1.0].iter().enumerate() {
        set(&mut gfx, 132 + i * 4, &v.to_le_bytes());
    }
    for (i, c) in [0xFF00_00FFu32, 0xFF00_FF00, 0xFFFF_0000].iter().enumerate() {
        set(&mut gfx, 156 + i * 4, &c.to_le_bytes());
    }
    let ib = 256;
    set(&mut gfx, ib, &1u32.to_le_bytes());
    set(&mut gfx, ib + 4, &1u32.to_le_bytes());
    set(&mut gfx, ib + 8, &3u32.to_le_bytes());
    set(&mut gfx, ib + 12, &2u32.to_le_bytes());
    set(&mut gfx, ib + 28, &(1u32 << 7 | 2 << 14).to_le_bytes());
    for i in 0..3u32 {
        set(&mut gfx, ib + 32 + i as usize * 4, &i.to_le_bytes());
    }
    (sys, gfx)
}

/// Minimal 9FPR archive holding one segmented drawable at a 4 KiB + 16 offset.
pub fn model_archive() -> Vec<u8> {
    let (system, graphics) = drawable();
    let mut file = vec![0u8; 4096 + 16];
    file[..4].copy_from_slice(b"9FPR");
    for (at, value) in [(4usize, 608u32), (8, 576), (12, 128)] {
        file[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    file.extend_from_slice(&zlib(&system));
    file.resize(file.len().next_multiple_of(16), 0);
    file.extend_from_slice(&zlib(&graphics));
    file.resize(file.len().next_multiple_of(16) + 4096, 0);
    file
}
