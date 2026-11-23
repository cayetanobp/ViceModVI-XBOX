use super::*;

fn pack(q: [u64; 3]) -> u64 {
    (q[0] >> 8) << 16 | (q[0] & 0xff) | (q[1] >> 8) << 32 | (q[1] & 0xff) << 8 | q[2] << 48
}

/// A unit quad in the z = 0 plane: positions, normals (+z) and UV set 0.
fn quad_vertex_block(axis: u32) -> Vec<u8> {
    let mut b = vec![0u8; 0xc0];
    b[..4].copy_from_slice(VERTEX_MAGIC);
    let slot = |b: &mut Vec<u8>, slot: usize, kind: u32, at: u32| {
        b[4 + 4 * slot..8 + 4 * slot].copy_from_slice(&((at / 2) << 8 | kind).to_le_bytes());
    };
    slot(&mut b, 1, 0x21, 0xa0);
    slot(&mut b, 2, 0x28, 0xb0);
    for (i, v) in [0f32, 0.0, 0.0].iter().enumerate() {
        b[0x60 + 4 * i..][..4].copy_from_slice(&v.to_le_bytes());
    }
    b[0x6c..0x70].copy_from_slice(&axis.to_le_bytes());
    for (i, v) in [1.0 / 16_777_216.0f32, 1.0 / 16_777_216.0, 1.0 / 65_536.0, 4.0].iter().enumerate() {
        b[0x70 + 4 * i..][..4].copy_from_slice(&v.to_le_bytes());
    }
    let max = 0xff_ffff;
    for (i, (x, y)) in [(0, 0), (max, 0), (max, max), (0, max)].into_iter().enumerate() {
        b[0x80 + 8 * i..][..8].copy_from_slice(&pack([x, y, 0]).to_le_bytes());
        let n: u32 = 512 | 512 << 10 | 1 << 31;
        b[0xa0 + 4 * i..][..4].copy_from_slice(&n.to_le_bytes());
        let (u, v) = (if x == 0 { 0i16 } else { 8192 }, if y == 0 { 0i16 } else { 8192 });
        b[0xb0 + 4 * i..][..2].copy_from_slice(&u.to_le_bytes());
        b[0xb2 + 4 * i..][..2].copy_from_slice(&v.to_le_bytes());
    }
    b
}

fn quad_index_block() -> Vec<u8> {
    let mut b = Vec::new();
    for w in [0x8000_0001u32, 2, 4, 0, 0, 0, 0, 0x8080, 0x182] {
        b.extend_from_slice(&w.to_le_bytes());
    }
    for r in [0u16, 1, 2, 3] {
        b.extend_from_slice(&r.to_le_bytes());
    }
    b
}

#[test]
fn decodes_a_quad() {
    let vb = quad_vertex_block(2);
    let block = vertex_block(&vb).unwrap();
    assert_eq!(block.count, 4);
    let p = block.positions(&vb);
    assert!((p[2][0] - 1.0).abs() < 1e-6 && (p[2][1] - 1.0).abs() < 1e-6 && p[2][2] == 0.0);
    assert_eq!(block.uvs(&vb, 2).unwrap()[2], [1.0, 1.0]);
    let n = block.normals(&vb).unwrap();
    assert!(n.iter().all(|v| v[2] > 0.99));
    let ib = index_block(&quad_index_block(), 4).unwrap();
    assert_eq!(ib.triangles, [[0, 1, 2], [2, 3, 0]]);
    assert_eq!(ib.slack, 0);
    let agreement = normal_agreement(&p, &ib.triangles, &n).unwrap();
    assert!(agreement.median > 0.99 && agreement.same_side == 1.0, "{agreement:?}");
}

#[test]
fn axis_field_permutes_components() {
    // Axis 0: the stored components are (z, y, x), so the quad lies in x = 0.
    let vb = quad_vertex_block(0);
    let p = vertex_block(&vb).unwrap().positions(&vb);
    assert!(p.iter().all(|v| v[0] == 0.0));
    assert!((p[1][2] - 1.0).abs() < 1e-6);
}

#[test]
fn rejects_inconsistent_blocks() {
    let mut vb = quad_vertex_block(2);
    vb[0x6c] = 3;
    assert!(vertex_block(&vb).is_err());
    let mut vb = quad_vertex_block(2);
    vb[4 + 4 * 2..8 + 4 * 2].copy_from_slice(&((0xb4u32 / 2) << 8 | 0x28).to_le_bytes());
    assert!(vertex_block(&vb).is_err());
    let mut ib = quad_index_block();
    ib[28] = 0x05; // local index 5 in a meshlet of 4 vertices
    assert!(index_block(&ib, 4).is_err());
    assert!(index_block(&quad_index_block(), 3).is_err());
}

#[test]
fn normal_precision_follows_bit_31() {
    // +x with 10-bit fields (u = 1023, v = 511.5) and with 8-bit fields.
    let ten = decode_normal(1023 | 512 << 10 | 1 << 31);
    let eight = decode_normal(255 | 128 << 8);
    assert!(ten[0] > 0.99 && eight[0] > 0.99, "{ten:?} {eight:?}");
    // The same low bits read with the other precision point elsewhere.
    assert!(decode_normal(1023 | 512 << 10)[0] < 0.9);
}

#[test]
fn octahedral_axes() {
    let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
    assert!(close(octahedral(0.5, 0.5), [0.0, 0.0, 1.0]));
    assert!(close(octahedral(1.0, 0.5), [1.0, 0.0, 0.0]));
    assert!(close(octahedral(0.0, 0.0), [0.0, 0.0, -1.0]));
}

#[test]
fn glb_layout() {
    let vb = quad_vertex_block(2);
    let block = vertex_block(&vb).unwrap();
    let positions = block.positions(&vb);
    let triangles = index_block(&quad_index_block(), 4).unwrap().triangles;
    let mesh = Mesh {
        normals: block.normals(&vb), uv0: block.uvs(&vb, 2), uv1: None, positions, triangles,
        shader: 3, meshlets: 1, all_vertices_used: true, bounds_match: None, normal_agreement: None,
    };
    let out = glb::write("quad \"test\"", &[glb::Lod { name: "LOD0".into(), meshes: vec![&mesh] }]);
    assert_eq!(&out[..4], b"glTF");
    assert_eq!(u32::from_le_bytes(out[8..12].try_into().unwrap()) as usize, out.len());
    let json_len = u32::from_le_bytes(out[12..16].try_into().unwrap()) as usize;
    assert_eq!(json_len % 4, 0);
    let json = std::str::from_utf8(&out[20..20 + json_len]).unwrap();
    assert!(json.contains("\"name\":\"quad \\\"test\\\"\""));
    assert!(json.contains("\"shader_3\""));
    let bin_len = u32::from_le_bytes(out[20 + json_len..24 + json_len].try_into().unwrap()) as usize;
    assert_eq!(&out[24 + json_len..28 + json_len], b"BIN\0");
    assert_eq!(28 + json_len + bin_len, out.len());
}
