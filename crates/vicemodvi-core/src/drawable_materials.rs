//! Shader texture references of drawables listed by the Resource Tools reader.

use crate::{
    native::model::{Material, TextureRef},
    Error, Result,
};

struct Reader<'a> {
    data: &'a [u8],
}

impl Reader<'_> {
    fn bytes<const N: usize>(&self, at: i64) -> Result<[u8; N]> {
        usize::try_from(at)
            .ok()
            .and_then(|start| self.data.get(start..start.checked_add(N)?))
            .and_then(|slice| slice.try_into().ok())
            .ok_or_else(|| Error::argument("Drawable material read is outside the section."))
    }

    fn u8(&self, at: i64) -> Result<u8> {
        Ok(self.bytes::<1>(at)?[0])
    }

    fn u16(&self, at: i64) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(at)?))
    }

    fn u32(&self, at: i64) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(at)?))
    }

    fn u64(&self, at: i64) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(at)?))
    }

    fn pointer(&self, at: i64, size: i64) -> Result<i64> {
        let address = self.u64(at)?;
        if address < 0x5000_0000 || address - 0x5000_0000 > self.data.len() as u64 {
            return Err(Error::invalid("Invalid drawable material pointer."));
        }
        let offset = (address - 0x5000_0000) as i64;
        if size > self.data.len() as i64 - offset {
            return Err(Error::invalid("Truncated drawable material."));
        }
        Ok(offset)
    }
}

/// Reads the materials of the drawable at `drawable`. Unknown shader layouts
/// are skipped; a drawable with an unknown layout returns no materials.
pub fn read(data: &[u8], drawable: i64) -> Vec<Material> {
    let reader = Reader { data };
    let mut materials = Vec::new();
    let _ = read_into(&reader, drawable, &mut materials);
    materials
}

fn read_into(r: &Reader, drawable: i64, materials: &mut Vec<Material>) -> Result<()> {
    let group = r.pointer(drawable + 16, 64)?;
    let count = r.u16(group + 24)? as i64;
    if !(1..=512).contains(&count) || count != r.u16(group + 26)? as i64 {
        return Ok(());
    }
    let shaders = r.pointer(group + 16, count * 8)?;
    for i in 0..count {
        match read_shader(r, shaders, i) {
            Ok(Some(material)) => materials.push(material),
            Ok(None) | Err(Error::InvalidData(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_shader(r: &Reader, shaders: i64, i: i64) -> Result<Option<Material>> {
    let shader = r.pointer(shaders + i * 8, 64)?;
    let hash = r.u32(shader)?;
    let preset = r.u32(shader + 4)?;
    if preset != 0x6D65_7461 {
        return Ok(None);
    }
    let info = r.pointer(shader + 32, 8)?;
    let textures = r.u8(info + 1)? as i64;
    let parameters = r.u8(info + 4)? as i64;
    let slots = r.u8(info + 7)?;
    if r.u8(info + 5)? != 0 || r.u8(info + 6)? != 0 || !(1..=16).contains(&slots) || info > r.data.len() as i64 - (8 + parameters * 8) {
        return Ok(None);
    }
    let table = if textures > 0 { r.pointer(shader + 16, textures * 8)? } else { 0 };
    let mut refs = Vec::new();
    for p in 0..parameters {
        let parameter = info + 8 + p * 8;
        let binding = r.u32(parameter + 4)?;
        let slot = ((binding >> 2) & 255) as i64;
        if (binding & 3) != 0 || slot >= textures || r.u64(table + slot * 8)? == 0 {
            continue;
        }
        let texture = r.pointer(table + slot * 8, 80)?;
        if r.u32(texture + 16)? != 0x980
            || r.u32(texture + 20)? != 0
            || r.u64(texture + 24)? != 0
            || r.u64(texture + 40)? != 0
            || r.u32(texture + 56)? != 0
            || r.u16(texture + 60)? != 1
            || r.u16(texture + 62)? != 1
            || r.u8(texture + 64)? != 255
            || r.u64(texture + 48)? == 0
        {
            continue;
        }
        let asset = r.pointer(texture + 48, 16)?;
        if asset < 16
            || r.u32(asset - 16)? != 6
            || r.u32(asset - 12)? != 0
            || r.u32(asset - 4)? != 0
            || r.u64(asset)? != 0x5000_0000 + asset as u64 - 11
            || r.u64(asset + 8)? == 0
        {
            continue;
        }
        refs.push(TextureRef {
            parameter_hash: r.u32(parameter)?,
            texture_index: slot as i32,
            system_offset: texture as i32,
            asset_descriptor_offset: asset as i32,
            opaque_id64: r.u64(asset + 8)?,
            opaque_id32: r.u32(asset - 8)?,
            tag: 6,
        });
    }
    Ok(Some(Material {
        shader_index: i as i32,
        shader_hash: hash,
        preset_hash: preset,
        textures: refs,
        render_bucket: Some(r.u8(shader + 57)?),
        render_bucket_mask: Some(r.u32(shader + 60)?),
    }))
}
