//! Shader parameters that hold color (diffuse) textures.
//!
//! Parameters are identified by the Jenkins one-at-a-time hash of their
//! lowercase name. The table lists the color slots in preview priority:
//! the main diffuse map first, then layer maps, then secondary maps.

use super::model::{Material, TextureRef};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorTexture {
    pub reference: TextureRef,
    pub parameter_name: &'static str,
    /// The slot belongs to a layered (blend) shader.
    pub layered: bool,
    /// The slot is not the primary color map of its shader.
    pub secondary: bool,
    /// The name was inferred from the hash rather than confirmed.
    pub name_inferred: bool,
}

struct Slot {
    hash: u32,
    name: &'static str,
    layered: bool,
    secondary: bool,
    inferred: bool,
}

const fn slot(hash: u32, name: &'static str, layered: bool, secondary: bool, inferred: bool) -> Slot {
    Slot { hash, name, layered, secondary, inferred }
}

/// Color slots in priority order.
const SLOTS: [Slot; 16] = [
    slot(3370697346, "diffusetex", false, false, false),
    slot(756347250, "diffusetexture", false, false, false),
    slot(0x3463_94EA, "lyr0diffusetex", true, false, true),
    slot(0x7D4F_12E2, "lyr1diffusetex", true, true, true),
    slot(3464205966, "diffusetexture_layer0", true, false, false),
    slot(3632507550, "diffusetexture_layer1", true, true, false),
    slot(851042057, "diffusetexture_layer2", true, true, false),
    slot(1024521143, "diffusetexture_layer3", true, true, false),
    slot(3853173992, "diffusetexture_layer4", true, true, false),
    slot(2464948076, "diffusetexture_layer5", true, true, false),
    slot(3389132183, "diffusetexture_layer6", true, true, false),
    slot(3547013225, "diffusetexture_layer7", true, true, false),
    slot(234123336, "diffusetex2", false, true, false),
    slot(3831897388, "diffusetex3", false, true, false),
    slot(3489704073, "diffusetexture2", false, true, false),
    slot(1380592926, "diffusetexture3", false, true, false),
];

/// Jenkins one-at-a-time hash of the lowercase name.
pub const fn joaat(name: &str) -> u32 {
    let bytes = name.as_bytes();
    let mut hash: u32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        hash = hash.wrapping_add(bytes[i].to_ascii_lowercase() as u32);
        hash = hash.wrapping_add(hash << 10);
        hash ^= hash >> 6;
        i += 1;
    }
    hash = hash.wrapping_add(hash << 3);
    hash ^= hash >> 11;
    hash.wrapping_add(hash << 15)
}

/// Color texture references of a material, ordered by slot priority and
/// then by material order.
pub fn color_textures(material: &Material) -> Vec<ColorTexture> {
    let mut found: Vec<(usize, ColorTexture)> = material
        .textures
        .iter()
        .filter_map(|reference| {
            let (rank, slot) = SLOTS.iter().enumerate().find(|(_, slot)| slot.hash == reference.parameter_hash)?;
            Some((
                rank,
                ColorTexture {
                    reference: reference.clone(),
                    parameter_name: slot.name,
                    layered: slot.layered,
                    secondary: slot.secondary,
                    name_inferred: slot.inferred,
                },
            ))
        })
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, texture)| texture).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_hashes_match_the_shader_parameters() {
        assert_eq!(joaat("diffusetex"), 3370697346);
        assert_eq!(joaat("DiffuseTexture"), 756347250);
        assert_eq!(joaat("lyr0diffusetex"), 0x346394EA);
        assert_eq!(joaat("lyr1diffusetex"), 0x7D4F12E2);
        assert_eq!(joaat("diffusetexture_layer0"), 3464205966);
        assert_eq!(joaat("diffusetexture_layer7"), 3547013225);
        assert_eq!(joaat("diffusetex2"), 234123336);
        assert_eq!(joaat("diffusetexture3"), 1380592926);
    }

    #[test]
    fn slots_sort_by_priority() {
        let reference = |hash| TextureRef { parameter_hash: hash, texture_index: 0, system_offset: 0, asset_descriptor_offset: 0, opaque_id64: 1, opaque_id32: 2, tag: 6 };
        let material = Material {
            shader_index: 0,
            shader_hash: 0,
            preset_hash: 0,
            textures: vec![reference(joaat("diffusetex2")), reference(0x1234), reference(joaat("diffusetex"))],
            render_bucket: None,
            render_bucket_mask: None,
        };
        let names: Vec<_> = color_textures(&material).iter().map(|t| t.parameter_name).collect();
        assert_eq!(names, ["diffusetex", "diffusetex2"]);
    }
}
