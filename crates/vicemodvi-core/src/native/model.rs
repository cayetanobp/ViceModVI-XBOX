//! Decoded model geometry shared by the drawable reader, the OBJ reader,
//! the viewer and the exporters.

use std::fmt::Write as _;

use super::dictionary::DictionaryTexture;

pub type Vec2 = [f32; 2];
pub type Vec3 = [f32; 3];

#[derive(Clone, Debug, PartialEq)]
pub struct Mesh {
    pub name: String,
    pub positions: Vec<Vec3>,
    /// Three vertex indices per triangle.
    pub indices: Vec<i32>,
    pub tex_coords: Option<Vec<Vec2>>,
    /// Packed RGBA8 per vertex, when the vertex layout has colors.
    pub vertex_colors: Option<Vec<u32>>,
    /// Material (shader) of the mesh, when the drawable maps one.
    pub shader_index: Option<i32>,
}

/// A texture a shader parameter points to by native key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureRef {
    pub parameter_hash: u32,
    pub texture_index: i32,
    pub system_offset: i32,
    pub asset_descriptor_offset: i32,
    pub opaque_id64: u64,
    pub opaque_id32: u32,
    pub tag: u32,
}

impl TextureRef {
    pub fn name_hash(&self) -> u32 {
        self.opaque_id32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Material {
    pub shader_index: i32,
    pub shader_hash: u32,
    pub preset_hash: u32,
    pub textures: Vec<TextureRef>,
    pub render_bucket: Option<u8>,
    pub render_bucket_mask: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub name: String,
    pub meshes: Vec<Mesh>,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub is_fragment: bool,
    pub has_bone_bindings: bool,
    pub drawable_offset: i32,
    pub presentation_note: Option<String>,
    pub materials: Option<Vec<Material>>,
    pub embedded_textures: Option<Vec<DictionaryTexture>>,
    pub embedded_texture_dictionary_offset: Option<i32>,
}

impl Model {
    pub fn new(name: String, meshes: Vec<Mesh>, bounds_min: Vec3, bounds_max: Vec3) -> Self {
        Self {
            name,
            meshes,
            bounds_min,
            bounds_max,
            is_fragment: false,
            has_bone_bindings: false,
            drawable_offset: 0,
            presentation_note: None,
            materials: None,
            embedded_textures: None,
            embedded_texture_dictionary_offset: None,
        }
    }

    pub fn vertex_count(&self) -> usize {
        self.meshes.iter().map(|m| m.positions.len()).sum()
    }

    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|m| m.indices.len() / 3).sum()
    }
}

/// Wavefront OBJ text with one object per mesh, the geometry and the UVs
/// that match the vertex count.
pub fn to_obj(model: &Model) -> String {
    let mut text = String::from("# Recovered native drawable geometry. Materials and textures are omitted.\n");
    if let Some(note) = &model.presentation_note {
        let _ = writeln!(text, "# {note}");
    }
    // OBJ indices are 1-based and global across objects.
    let (mut vertex_base, mut uv_base) = (1usize, 1usize);
    for mesh in &model.meshes {
        let _ = writeln!(text, "o {}", mesh.name.replace(['\r', '\n'], "_"));
        for [x, y, z] in &mesh.positions {
            let _ = writeln!(text, "v {x} {y} {z}");
        }
        let uv = mesh.tex_coords.as_deref().filter(|uv| uv.len() == mesh.positions.len());
        for [u, v] in uv.unwrap_or_default() {
            let _ = writeln!(text, "vt {u} {v}");
        }
        for face in mesh.indices.chunks(3) {
            text.push('f');
            for &corner in face {
                let corner = corner as usize;
                match uv {
                    Some(_) => _ = write!(text, " {}/{}", vertex_base + corner, uv_base + corner),
                    None => _ = write!(text, " {}", vertex_base + corner),
                }
            }
            text.push('\n');
        }
        vertex_base += mesh.positions.len();
        uv_base += uv.map_or(0, <[Vec2]>::len);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obj_lists_vertices_uvs_and_faces() {
        let mesh = Mesh {
            name: "part\n1".into(),
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.5, 0.0]],
            indices: vec![0, 1, 2],
            tex_coords: Some(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            vertex_colors: None,
            shader_index: None,
        };
        let mut model = Model::new("box".into(), vec![mesh.clone(), mesh], [0.0; 3], [1.0, 1.5, 0.0]);
        model.presentation_note = Some("note".into());
        let obj = to_obj(&model);
        assert!(obj.starts_with("# Recovered"));
        assert!(obj.contains("# note\n"));
        assert!(obj.contains("o part_1\n"));
        assert!(obj.contains("v 0 1.5 0\n"));
        assert!(obj.contains("f 1/1 2/2 3/3\n"));
        assert!(obj.contains("f 4/4 5/5 6/6\n"));
        assert_eq!((model.vertex_count(), model.triangle_count()), (6, 2));
    }
}
