use crate::resource::*;
use crate::{identify_payload, Check, Format};

#[test]
fn joaat_matches_known_hashes() {
    assert_eq!(joaat(b""), 0);
    assert_eq!(joaat(b"adder"), 0xb779_a091);
    assert_eq!(joaat(b"ADDER"), 0xb779_a091);
}

#[test]
fn mip_chain_sizes() {
    // BC1 256x256, 7 levels down to 4x4.
    assert_eq!(mip_chain_len(256, 256, 1, 7, 71), Some(32768 + 8192 + 2048 + 512 + 128 + 32 + 8));
    // Uncompressed A8 4x4, one level; unknown formats give no size.
    assert_eq!(mip_chain_len(4, 4, 1, 1, 65), Some(16));
    assert_eq!(mip_chain_len(4, 4, 1, 1, 200), None);
}

fn put(b: &mut [u8], at: usize, bytes: &[u8]) {
    b[at..at + bytes.len()].copy_from_slice(bytes);
}

fn ptr(b: &mut [u8], at: usize, value: u64) {
    put(b, at, &value.to_le_bytes());
}

/// A texture dictionary laid out as observed in core_restofworld.rpf: root
/// at 0, texture at 0x40, page map at 0xf0, arrays at 0x110 and 0x118.
fn texture_dictionary(name: &[u8], hash: u32) -> Vec<u8> {
    let mut b = vec![0; 0x120];
    ptr(&mut b, 0x08, VIRTUAL_BASE + 0xf0);
    ptr(&mut b, 0x20, VIRTUAL_BASE + 0x118);
    put(&mut b, 0x28, &[1, 0, 1, 0]);
    ptr(&mut b, 0x30, VIRTUAL_BASE + 0x110);
    put(&mut b, 0x38, &[1, 0, 1, 0]);
    ptr(&mut b, 0x40 + 0x18, PHYSICAL_BASE);
    ptr(&mut b, 0x40 + 0x30, VIRTUAL_BASE + 0xb0);
    put(&mut b, 0x40 + 0x38, &[0, 1, 0, 1, 1, 0, 1, 71, 5, 0, 7]);
    put(&mut b, 0xb8, name);
    put(&mut b, 0xf8, &[1, 1]);
    ptr(&mut b, 0x110, VIRTUAL_BASE + 0x40);
    put(&mut b, 0x118, &hash.to_le_bytes());
    b
}

#[test]
fn texture_dictionary_is_structural_when_names_hash_to_keys() {
    let name = b"bodicecorset_nm";
    let b = texture_dictionary(name, joaat(name));
    let root = root(&b).unwrap();
    assert_eq!((root.virtual_pages, root.physical_pages), (1, 1));
    let Kind::TextureDictionary(t) = classify(&b) else { panic!("not a texture dictionary") };
    assert_eq!((t[0].width, t[0].height, t[0].mips, t[0].dxgi), (256, 256, 7, 71));
    assert_eq!(t[0].name, "bodicecorset_nm");
    let len = t[0].min_len.unwrap();
    assert!(check_texture_extents(&t, len).is_ok());
    assert!(check_texture_extents(&t, len - 1).is_err());

    let p = identify_payload(&b);
    assert_eq!((p.format, p.check), (Format::ResourceVirtual, Check::Structural));
}

#[test]
fn wrong_key_leaves_an_untyped_dictionary() {
    let b = texture_dictionary(b"bodicecorset_nm", 0x1234);
    assert!(matches!(classify(&b), Kind::Dictionary { count: 1 }));
}

#[test]
fn unsorted_keys_are_not_a_dictionary() {
    let mut b = texture_dictionary(b"a", joaat(b"a"));
    // Two keys in decreasing order.
    put(&mut b, 0x28, &[2, 0, 2, 0]);
    put(&mut b, 0x38, &[2, 0, 2, 0]);
    b.extend_from_slice(&[0; 16]);
    put(&mut b, 0x118, &[2, 0, 0, 0, 1, 0, 0, 0]);
    ptr(&mut b, 0x110, VIRTUAL_BASE + 0x120);
    ptr(&mut b, 0x120, VIRTUAL_BASE + 0x40);
    ptr(&mut b, 0x128, VIRTUAL_BASE + 0x40);
    assert!(dictionary(&b).is_none());
}

#[test]
fn payload_signatures() {
    assert_eq!(identify_payload(b"\xef\xbb\xbf<?xml version=\"1.0\"?><a/>").check, Check::Structural);
    assert_eq!(identify_payload(b"<?xml version=\"1.0\"?><a>").check, Check::Signature);
    assert_eq!(identify_payload(b"BaseCommon:sounds\0").format, Format::Text);
    assert_eq!(identify_payload(b"RBF0\xfb\xff\x01\x00\x00\x00\x17\x00CMovie\0").variant, "CMovie");
    assert_eq!(identify_payload(&[0u8; 64]).format, Format::Unknown);
}

#[test]
fn split_root_candidate_needs_a_page_map_past_the_unit() {
    let mut b = vec![0u8; 0x40];
    ptr(&mut b, 8, VIRTUAL_BASE + 0x100);
    assert!(split_root_candidate(&b));
    assert!(root(&b).is_none());
    ptr(&mut b, 8, VIRTUAL_BASE + 0x20);
    assert!(!split_root_candidate(&b));
    ptr(&mut b, 8, PHYSICAL_BASE);
    assert!(!split_root_candidate(&b));
}
