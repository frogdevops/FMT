//! Pure parser for the il2cpp `global-metadata` format.
//! Operates on a decrypted byte blob; no FFI, fully host-testable.

use crate::model::{Dump, DumpedClass, DumpedField};

/// `Il2CppGlobalMetadataHeader.sanity`. In a little-endian blob the first four
/// bytes are `AF 1B B1 FA`.
pub const METADATA_MAGIC: u32 = 0xFAB1_1BAF;

/// Byte layout for one il2cpp metadata version. `h_*` are byte positions of a
/// field within the header; the `*_index`/`*_start`/`*_count` are byte offsets
/// within their definition struct; `*_size` are struct sizes in bytes.
#[derive(Debug, Clone, Copy)]
pub struct MetadataLayout {
    pub h_string_offset: usize,
    pub h_string_size: usize,
    pub h_type_defs_offset: usize,
    pub h_type_defs_size: usize,
    pub h_fields_offset: usize,
    pub h_fields_size: usize,
    pub h_images_offset: usize,
    pub h_images_size: usize,
    pub image_size: usize,
    pub image_name_index: usize,
    pub image_type_start: usize,
    pub image_type_count: usize,
    pub type_size: usize,
    pub type_name_index: usize,
    pub type_namespace_index: usize,
    pub type_field_start: usize,
    pub type_field_count: usize,
    pub field_size: usize,
    pub field_name_index: usize,
}

pub(crate) fn read_u32(bytes: &[u8], pos: usize) -> Option<u32> {
    let end = pos.checked_add(4)?;
    let s = bytes.get(pos..end)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub(crate) fn read_i32(bytes: &[u8], pos: usize) -> Option<i32> {
    read_u32(bytes, pos).map(|v| v as i32)
}

pub(crate) fn read_u16(bytes: &[u8], pos: usize) -> Option<u16> {
    let end = pos.checked_add(2)?;
    let s = bytes.get(pos..end)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

/// Read a NUL-terminated string starting at `pos`. Bounded by the slice; never panics.
pub(crate) fn read_cstr(bytes: &[u8], pos: usize) -> String {
    if pos >= bytes.len() {
        return String::new();
    }
    let mut end = pos;
    while end < bytes.len() && bytes[end] != 0 {
        end += 1;
    }
    String::from_utf8_lossy(&bytes[pos..end]).into_owned()
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MetaHeader {
    pub string_offset: u32,
    pub type_defs_offset: u32,
    pub fields_offset: u32,
    pub images_offset: u32,
    pub images_size: u32,
}

pub(crate) fn parse_header(bytes: &[u8], layout: &MetadataLayout) -> Option<MetaHeader> {
    if read_u32(bytes, 0)? != METADATA_MAGIC {
        return None;
    }
    Some(MetaHeader {
        string_offset: read_u32(bytes, layout.h_string_offset)?,
        type_defs_offset: read_u32(bytes, layout.h_type_defs_offset)?,
        fields_offset: read_u32(bytes, layout.h_fields_offset)?,
        images_offset: read_u32(bytes, layout.h_images_offset)?,
        images_size: read_u32(bytes, layout.h_images_size)?,
    })
}

/// Parse a decrypted global-metadata blob into a `Dump` (class/namespace/field
/// names + hierarchy). Returns `None` if the magic or a required read fails.
/// Out-of-range table indices are skipped, never panicked on.
pub fn parse_metadata(bytes: &[u8], layout: &MetadataLayout) -> Option<Dump> {
    let h = parse_header(bytes, layout)?;
    // Reject fake candidates (random memory or a static-data copy that happens
    // to contain the magic). A real header's table offsets lie within the blob
    // and the images table must fit.
    let n = bytes.len();
    if (h.string_offset as usize) >= n
        || (h.type_defs_offset as usize) >= n
        || (h.fields_offset as usize) >= n
        || (h.images_offset as usize) >= n
        || h.images_size == 0
        || (h.images_offset as usize).saturating_add(h.images_size as usize) > n
    {
        return None;
    }
    let string_base = h.string_offset as usize;
    let read_name = |idx: u32| read_cstr(bytes, string_base.wrapping_add(idx as usize));

    let num_images = (h.images_size as usize) / layout.image_size.max(1);
    let mut classes = Vec::new();

    for i in 0..num_images {
        let img = h.images_offset as usize + i * layout.image_size;
        let type_start = match read_i32(bytes, img + layout.image_type_start) {
            Some(v) if v >= 0 => v as usize,
            _ => continue,
        };
        let type_count = read_u32(bytes, img + layout.image_type_count).unwrap_or(0) as usize;

        for t in 0..type_count {
            let tdef = h.type_defs_offset as usize + (type_start + t) * layout.type_size;
            let name_idx = match read_u32(bytes, tdef + layout.type_name_index) {
                Some(v) => v,
                None => continue,
            };
            let ns_idx = read_u32(bytes, tdef + layout.type_namespace_index).unwrap_or(0);
            let field_start = read_i32(bytes, tdef + layout.type_field_start).unwrap_or(-1);
            let field_count = read_u16(bytes, tdef + layout.type_field_count).unwrap_or(0) as usize;

            let mut fields = Vec::new();
            if field_start >= 0 {
                let fs = field_start as usize;
                for f in 0..field_count {
                    let fdef = h.fields_offset as usize + (fs + f) * layout.field_size;
                    if let Some(fname_idx) = read_u32(bytes, fdef + layout.field_name_index) {
                        fields.push(DumpedField {
                            name: read_name(fname_idx),
                            type_name: String::new(), // phase 2
                        });
                    }
                }
            }

            classes.push(DumpedClass {
                namespace: read_name(ns_idx),
                name: read_name(name_idx),
                fields,
            });
        }
    }

    Some(Dump { classes })
}

/// Byte positions in `bytes` where the metadata magic appears (little-endian).
pub fn find_magic_offsets(bytes: &[u8]) -> Vec<usize> {
    let magic = METADATA_MAGIC.to_le_bytes();
    if bytes.len() < 4 {
        return Vec::new();
    }
    (0..=bytes.len() - 4)
        .filter(|&i| bytes[i..i + 4] == magic)
        .collect()
}

/// Scan `bytes` for metadata-magic candidates and return the first that parses
/// into a non-empty `Dump`. `layout_for` maps a metadata version (the u32 at the
/// candidate's byte offset +4) to its layout; candidates whose version is
/// unsupported or whose blob fails validation are skipped.
pub fn find_and_parse(
    bytes: &[u8],
    layout_for: impl Fn(u32) -> Option<MetadataLayout>,
) -> Option<Dump> {
    for off in find_magic_offsets(bytes) {
        let version = match read_u32(bytes, off + 4) {
            Some(v) => v,
            None => continue,
        };
        let layout = match layout_for(version) {
            Some(l) => l,
            None => continue,
        };
        if let Some(dump) = parse_metadata(&bytes[off..], &layout) {
            if !dump.classes.is_empty() {
                return Some(dump);
            }
        }
    }
    None
}

/// Map a metadata version number to its byte layout. Real layouts are filled in
/// later (transcribed from Il2CppDumper); until then this returns `None`, so the
/// scanner finds nothing rather than misparsing. Add `29 => Some(LAYOUT_V29),`
/// etc. as each layout lands.
pub fn layout_for_version(_version: u32) -> Option<MetadataLayout> {
    match _version {
        // 29 => Some(LAYOUT_V29),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) const TEST_LAYOUT: MetadataLayout = MetadataLayout {
    h_string_offset: 8,
    h_string_size: 12,
    h_type_defs_offset: 16,
    h_type_defs_size: 20,
    h_fields_offset: 24,
    h_fields_size: 28,
    h_images_offset: 32,
    h_images_size: 36,
    image_size: 16,
    image_name_index: 0,
    image_type_start: 4,
    image_type_count: 8,
    type_size: 16,
    type_name_index: 0,
    type_namespace_index: 4,
    type_field_start: 8,
    type_field_count: 12,
    field_size: 8,
    field_name_index: 0,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_ints() {
        let bytes = [0xAF, 0x1B, 0xB1, 0xFA, 0x05, 0x00];
        assert_eq!(read_u32(&bytes, 0), Some(0xFAB1_1BAF));
        assert_eq!(read_u16(&bytes, 4), Some(5));
        assert_eq!(read_u32(&bytes, 4), None);
    }

    #[test]
    fn reads_nul_terminated_string() {
        let bytes = b"Game\0Player\0";
        assert_eq!(read_cstr(bytes, 0), "Game");
        assert_eq!(read_cstr(bytes, 5), "Player");
        assert_eq!(read_cstr(bytes, 100), "");
    }

    fn test_header() -> Vec<u8> {
        let mut h = vec![0u8; 40];
        h[0..4].copy_from_slice(&METADATA_MAGIC.to_le_bytes());
        h[4..8].copy_from_slice(&29u32.to_le_bytes());
        let put = |h: &mut [u8], pos: usize, v: u32| h[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut h, 8, 40);
        put(&mut h, 12, 24);
        put(&mut h, 16, 80);
        put(&mut h, 20, 16);
        put(&mut h, 24, 96);
        put(&mut h, 28, 16);
        put(&mut h, 32, 64);
        put(&mut h, 36, 16);
        h
    }

    #[test]
    fn parses_header_and_rejects_bad_magic() {
        let h = test_header();
        let parsed = parse_header(&h, &TEST_LAYOUT).unwrap();
        assert_eq!(parsed.string_offset, 40);
        assert_eq!(parsed.images_offset, 64);
        assert_eq!(parsed.images_size, 16);

        let mut bad = h.clone();
        bad[0] = 0;
        assert!(parse_header(&bad, &TEST_LAYOUT).is_none());
    }

    fn test_blob() -> Vec<u8> {
        let mut b = test_header();
        b.extend_from_slice(b"Game\0Player\0health\0mana\0");
        assert_eq!(b.len(), 64);
        let mut img = vec![0u8; 16];
        img[0..4].copy_from_slice(&0u32.to_le_bytes());
        img[4..8].copy_from_slice(&0i32.to_le_bytes());
        img[8..12].copy_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&img);
        assert_eq!(b.len(), 80);
        let mut ty = vec![0u8; 16];
        ty[0..4].copy_from_slice(&5u32.to_le_bytes());
        ty[4..8].copy_from_slice(&0u32.to_le_bytes());
        ty[8..12].copy_from_slice(&0i32.to_le_bytes());
        ty[12..14].copy_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&ty);
        assert_eq!(b.len(), 96);
        let mut f0 = vec![0u8; 8];
        f0[0..4].copy_from_slice(&12u32.to_le_bytes());
        let mut f1 = vec![0u8; 8];
        f1[0..4].copy_from_slice(&19u32.to_le_bytes());
        b.extend_from_slice(&f0);
        b.extend_from_slice(&f1);
        b
    }

    #[test]
    fn parses_full_blob_to_dump() {
        use crate::model::{DumpedClass, DumpedField};
        let dump = parse_metadata(&test_blob(), &TEST_LAYOUT).unwrap();
        assert_eq!(
            dump.classes,
            vec![DumpedClass {
                namespace: "Game".to_string(),
                name: "Player".to_string(),
                fields: vec![
                    DumpedField { name: "health".to_string(), type_name: String::new() },
                    DumpedField { name: "mana".to_string(), type_name: String::new() },
                ],
            }]
        );
    }

    #[test]
    fn rejects_fake_candidate_with_bad_offsets() {
        let mut b = test_blob();
        // Corrupt images_offset (header byte position 32) to point out of bounds.
        b[32..36].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert!(parse_metadata(&b, &TEST_LAYOUT).is_none());
    }

    #[test]
    fn finds_magic_offsets() {
        let mut data = vec![0u8; 10];
        data.extend_from_slice(&METADATA_MAGIC.to_le_bytes());
        data.extend_from_slice(&[1, 2, 3]);
        data.extend_from_slice(&METADATA_MAGIC.to_le_bytes());
        assert_eq!(find_magic_offsets(&data), vec![10, 17]);
        assert_eq!(find_magic_offsets(&[0u8; 4]), Vec::<usize>::new());
    }

    #[test]
    fn find_and_parse_locates_embedded_blob() {
        let mut region = vec![0u8; 16];
        region.extend_from_slice(&test_blob());
        let dump = find_and_parse(&region, |v| if v == 29 { Some(TEST_LAYOUT) } else { None }).unwrap();
        assert_eq!(dump.classes.len(), 1);
        assert_eq!(dump.classes[0].name, "Player");
    }

    #[test]
    fn find_and_parse_returns_none_without_magic() {
        let region = vec![0u8; 256];
        assert!(find_and_parse(&region, |_| Some(TEST_LAYOUT)).is_none());
    }

    #[test]
    fn layout_for_unknown_version_is_none() {
        assert!(layout_for_version(9999).is_none());
    }
}
