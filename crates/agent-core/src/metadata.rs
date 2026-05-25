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
    /// Index of this type's `Il2CppType` in the codegen types array.
    /// Also known as `byvalTypeIndex`, `typeIndex`, or `byvalTypeIndex` in Il2CppDumper.
    pub type_byval_type_index: usize,
    pub type_field_start: usize,
    pub type_field_count: usize,
    pub field_size: usize,
    pub field_name_index: usize,
    pub field_type_index: usize,
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
            let byval_type_idx = read_u32(bytes, tdef + layout.type_byval_type_index).unwrap_or(0);
            let field_start = read_i32(bytes, tdef + layout.type_field_start).unwrap_or(-1);
            let field_count = read_u16(bytes, tdef + layout.type_field_count).unwrap_or(0) as usize;

            let mut fields = Vec::new();
            if field_start >= 0 {
                let fs = field_start as usize;
                for f in 0..field_count {
                    let fdef = h.fields_offset as usize + (fs + f) * layout.field_size;
                    if let Some(fname_idx) = read_u32(bytes, fdef + layout.field_name_index) {
                        let field_type_idx = read_u32(bytes, fdef + layout.field_type_index);
                        fields.push(DumpedField {
                            name: read_name(fname_idx),
                            type_name: String::new(),
                            type_index: field_type_idx,
                        });
                    }
                }
            }

            classes.push(DumpedClass {
                namespace: read_name(ns_idx),
                name: read_name(name_idx),
                fields,
                methods: Vec::new(),
                type_index: byval_type_idx,
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
    find_and_parse_with_offset(bytes, layout_for).map(|(_, d)| d)
}

/// Like `find_and_parse`, but also returns the byte offset within `bytes` where
/// the matching metadata blob was found. The offset is relative to `bytes[0]`.
pub fn find_and_parse_with_offset(
    bytes: &[u8],
    layout_for: impl Fn(u32) -> Option<MetadataLayout>,
) -> Option<(usize, Dump)> {
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
                return Some((off, dump));
            }
        }
    }
    None
}

// ── Struct field definitions ─────────────────────────────────────
// Each entry describes one field of a C# struct used in Il2CppDumper.
// `size` is the byte width, `min_ver`/`max_ver` are the version range
// (inclusive) where this field exists.  `None` means unbounded.

#[derive(Debug, Clone, Copy)]
struct FieldDef {
    size: usize,
    min_ver: Option<u32>,
    max_ver: Option<u32>,
}

fn ok_for_version(version: u32, f: &FieldDef) -> bool {
    f.min_ver.map_or(true, |v| version >= v)
        && f.max_ver.map_or(true, |v| version <= v)
}

fn struct_size_named(version: u32, fields: &[(&str, FieldDef)]) -> usize {
    let mut acc = 0usize;
    for (_, fd) in fields {
        if ok_for_version(version, fd) {
            acc += fd.size;
        }
    }
    acc
}

/// Compute the byte offset of a named field (case-sensitive) within a
/// struct for the given `version`.  Returns `None` if the name is unknown
/// or if the field is absent for this version.
fn field_offset(version: u32, fields: &[(&str, FieldDef)], name: &str) -> Option<usize> {
    let mut off = 0usize;
    for (n, fd) in fields {
        let ok = fd.min_ver.map_or(true, |v| version >= v)
            && fd.max_ver.map_or(true, |v| version <= v);
        if *n == name {
            return if ok { Some(off) } else { None };
        }
        if ok {
            off += fd.size;
        }
    }
    None
}

// ── Header struct: Il2CppGlobalMetadataHeader ────────────────────
// (u32 offset, i32 size) pairs.  sanity (4) + version (4) comes first,
// then the alternating pairs.
const HEADER_FIELDS: &[(&str, FieldDef)] = &[
    ("_sanity", FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("_version", FieldDef { size: 4, min_ver: None, max_ver: None }),
    // Actual header data sections (each is offset+size = 8 bytes):
    ("stringLiteral", FieldDef { size: 8, min_ver: Some(16), max_ver: None }),
    ("stringLiteralData", FieldDef { size: 8, min_ver: Some(16), max_ver: None }),
    ("string",              FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("events",              FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("properties",          FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("methods",             FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("parameterDefaultValues", FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("fieldDefaultValues",  FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("fieldAndParameterDefaultValueData", FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("fieldMarshaledSizes", FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("parameters",          FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("fields",              FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("genericParameters",   FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("genericParameterConstraints", FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("genericContainers",   FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("nestedTypes",         FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("interfaces",          FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("vtableMethods",       FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("interfaceOffsets",    FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("typeDefinitions",     FieldDef { size: 8, min_ver: None, max_ver: None }),
    // rgctxEntries removed after 24.1
    ("rgctxEntries",        FieldDef { size: 8, min_ver: None, max_ver: Some(24) }),
    ("images",              FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("assemblies",          FieldDef { size: 8, min_ver: None, max_ver: None }),
    ("metadataUsageLists",  FieldDef { size: 8, min_ver: Some(19), max_ver: Some(24) }),
    ("metadataUsagePairs",  FieldDef { size: 8, min_ver: Some(19), max_ver: Some(24) }),
    ("fieldRefs",           FieldDef { size: 8, min_ver: Some(19), max_ver: None }),
    ("referencedAssemblies", FieldDef { size: 8, min_ver: Some(20), max_ver: None }),
    // attributesInfo/attributeTypes removed after 27.2 (v27 as integer)
    ("attributesInfo",      FieldDef { size: 8, min_ver: Some(21), max_ver: Some(27) }),
    ("attributeTypes",      FieldDef { size: 8, min_ver: Some(21), max_ver: Some(27) }),
    // attributeData/attributeDataRange added in 29
    ("attributeData",       FieldDef { size: 8, min_ver: Some(29), max_ver: None }),
    ("attributeDataRange",  FieldDef { size: 8, min_ver: Some(29), max_ver: None }),
    ("unresolvedVirtualCallParameterTypes", FieldDef { size: 8, min_ver: Some(22), max_ver: None }),
    ("unresolvedVirtualCallParameterRanges", FieldDef { size: 8, min_ver: Some(22), max_ver: None }),
    ("windowsRuntimeTypeNames", FieldDef { size: 8, min_ver: Some(23), max_ver: None }),
    ("windowsRuntimeStrings", FieldDef { size: 8, min_ver: Some(27), max_ver: None }),
    ("exportedTypeDefinitions", FieldDef { size: 8, min_ver: Some(24), max_ver: None }),
];

// ── Data structs (fields we care about) ──────────────────────────

const IMAGE_FIELDS: &[(&str, FieldDef)] = &[
    ("nameIndex",       FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("assemblyIndex",   FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("typeStart",       FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("typeCount",       FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("exportedTypeStart", FieldDef { size: 4, min_ver: Some(24), max_ver: None }),
    ("exportedTypeCount", FieldDef { size: 4, min_ver: Some(24), max_ver: None }),
    ("entryPointIndex", FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("token",           FieldDef { size: 4, min_ver: Some(19), max_ver: None }),
    ("customAttributeStart", FieldDef { size: 4, min_ver: Some(24), max_ver: None }),
    ("customAttributeCount", FieldDef { size: 4, min_ver: Some(24), max_ver: None }),
];

const TYPE_FIELDS: &[(&str, FieldDef)] = &[
    ("nameIndex",       FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("namespaceIndex",  FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("customAttributeIndex", FieldDef { size: 4, min_ver: None, max_ver: Some(24) }),
    ("byvalTypeIndex",  FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("byrefTypeIndex",  FieldDef { size: 4, min_ver: None, max_ver: Some(24) }),
    ("declaringTypeIndex", FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("parentIndex",     FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("elementTypeIndex", FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("rgctxStartIndex", FieldDef { size: 4, min_ver: None, max_ver: Some(24) }),
    ("rgctxCount",      FieldDef { size: 4, min_ver: None, max_ver: Some(24) }),
    ("genericContainerIndex", FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("delegateWrapperFromManagedToNativeIndex", FieldDef { size: 4, min_ver: None, max_ver: Some(22) }),
    ("marshalingFunctionsIndex", FieldDef { size: 4, min_ver: None, max_ver: Some(22) }),
    ("ccwFunctionIndex", FieldDef { size: 4, min_ver: Some(21), max_ver: Some(22) }),
    ("guidIndex",       FieldDef { size: 4, min_ver: Some(21), max_ver: Some(22) }),
    ("flags",           FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("fieldStart",      FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("methodStart",     FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("eventStart",      FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("propertyStart",   FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("nestedTypesStart", FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("interfacesStart", FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("vtableStart",     FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("interfaceOffsetsStart", FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("method_count",    FieldDef { size: 2, min_ver: None, max_ver: None }),
    ("property_count",  FieldDef { size: 2, min_ver: None, max_ver: None }),
    ("field_count",     FieldDef { size: 2, min_ver: None, max_ver: None }),
    ("event_count",     FieldDef { size: 2, min_ver: None, max_ver: None }),
    ("nested_type_count", FieldDef { size: 2, min_ver: None, max_ver: None }),
    ("vtable_count",    FieldDef { size: 2, min_ver: None, max_ver: None }),
    ("interfaces_count", FieldDef { size: 2, min_ver: None, max_ver: None }),
    ("interface_offsets_count", FieldDef { size: 2, min_ver: None, max_ver: None }),
    ("bitfield",        FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("token",           FieldDef { size: 4, min_ver: Some(19), max_ver: None }),
];

const FIELD_FIELDS: &[(&str, FieldDef)] = &[
    ("nameIndex",       FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("typeIndex",       FieldDef { size: 4, min_ver: None, max_ver: None }),
    ("customAttributeIndex", FieldDef { size: 4, min_ver: None, max_ver: Some(24) }),
    ("token",           FieldDef { size: 4, min_ver: Some(19), max_ver: None }),
];

/// Build a `MetadataLayout` for a given metadata version by computing
/// all byte offsets from the struct definition tables above.
pub fn compute_layout(version: u32) -> Option<MetadataLayout> {
    if version < 16 || version > 31 { return None; }

    let h_off = |name| field_offset(version, HEADER_FIELDS, name);
    let s_sz = |fields| struct_size_named(version, fields);

    Some(MetadataLayout {
        h_string_offset:     h_off("string")?,
        h_string_size:       h_off("string")? + 4,
        h_type_defs_offset:  h_off("typeDefinitions")?,
        h_type_defs_size:    h_off("typeDefinitions")? + 4,
        h_fields_offset:     h_off("fields")?,
        h_fields_size:       h_off("fields")? + 4,
        h_images_offset:     h_off("images")?,
        h_images_size:       h_off("images")? + 4,

        image_size:          s_sz(IMAGE_FIELDS),
        image_name_index:    field_offset(version, IMAGE_FIELDS, "nameIndex")?,
        image_type_start:    field_offset(version, IMAGE_FIELDS, "typeStart")?,
        image_type_count:    field_offset(version, IMAGE_FIELDS, "typeCount")?,

        type_size:              s_sz(TYPE_FIELDS),
        type_name_index:        field_offset(version, TYPE_FIELDS, "nameIndex")?,
        type_namespace_index:   field_offset(version, TYPE_FIELDS, "namespaceIndex")?,
        type_byval_type_index:  field_offset(version, TYPE_FIELDS, "byvalTypeIndex")?,
        type_field_start:       field_offset(version, TYPE_FIELDS, "fieldStart")?,
        type_field_count:       field_offset(version, TYPE_FIELDS, "field_count")?,

        field_size:          s_sz(FIELD_FIELDS),
        field_name_index:    field_offset(version, FIELD_FIELDS, "nameIndex")?,
        field_type_index:    field_offset(version, FIELD_FIELDS, "typeIndex")?,
    })
}

/// Map a metadata version number to its byte layout.
pub fn layout_for_version(version: u32) -> Option<MetadataLayout> {
    compute_layout(version)
}

/// Compute the total count of type definitions from a decrypted metadata blob.
/// Returns `None` if the header can't be parsed or type_defs_size is not a
/// multiple of `type_size`.
pub fn compute_type_count(bytes: &[u8], layout: &MetadataLayout) -> Option<u32> {
    parse_header(bytes, layout)?;
    let type_defs_size = read_u32(bytes, layout.h_type_defs_size)?;
    if layout.type_size == 0 {
        return None;
    }
    let count = type_defs_size / layout.type_size as u32;
    if count == 0 { None } else { Some(count) }
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
    type_size: 20,
    type_name_index: 0,
    type_namespace_index: 4,
    type_byval_type_index: 8,
    type_field_start: 12,
    type_field_count: 16,
    field_size: 8,
    field_name_index: 0,
    field_type_index: 4,
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
        put(&mut h, 8, 40);  // string offset
        put(&mut h, 12, 24); // string size
        put(&mut h, 16, 80); // type_defs offset
        put(&mut h, 20, 20); // type_defs size (one 20-byte type)
        put(&mut h, 24, 100);// fields offset (40 + 24 + 16 + 20 = 100)
        put(&mut h, 28, 16); // fields size
        put(&mut h, 32, 64); // images offset
        put(&mut h, 36, 16); // images size
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
        let mut ty = vec![0u8; 20];
        ty[0..4].copy_from_slice(&5u32.to_le_bytes());   // nameIndex
        ty[4..8].copy_from_slice(&0u32.to_le_bytes());   // namespaceIndex
        ty[8..12].copy_from_slice(&0u32.to_le_bytes());  // byvalTypeIndex = 0
        ty[12..16].copy_from_slice(&0i32.to_le_bytes()); // fieldStart
        ty[16..18].copy_from_slice(&2u16.to_le_bytes()); // field_count
        b.extend_from_slice(&ty);
        assert_eq!(b.len(), 100);
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
                    DumpedField { name: "health".to_string(), type_name: String::new(), type_index: Some(0) },
                    DumpedField { name: "mana".to_string(), type_name: String::new(), type_index: Some(0) },
                ],
                methods: vec![],
                type_index: 0,
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

    #[test]
    fn compute_layout_v24_offsets_are_sane() {
        let l = compute_layout(24).expect("v24 should produce a layout");
        // sanity(4) + version(4) + stringLiteral(8) + stringLiteralData(8) = 24
        assert_eq!(l.h_string_offset, 24);
        // ... + string(8) + events(8) + properties(8) + methods(8)
        // + parameterDefaultValues(8) + fieldDefaultValues(8)
        // + fieldAndParameterDefaultValueData(8) + fieldMarshaledSizes(8)
        // + parameters(8) = 24 + 8*9 = 96
        assert_eq!(l.h_fields_offset, 96);
        // + fields(8) + genericParameters(8) + genericParameterConstraints(8)
        // + genericContainers(8) + nestedTypes(8) + interfaces(8)
        // + vtableMethods(8) + interfaceOffsets(8) = 96 + 8*8 = 160
        assert_eq!(l.h_type_defs_offset, 160);
        // + typeDefinitions(8) + rgctxEntries(8) = 160 + 16 = 176
        assert_eq!(l.h_images_offset, 176);
        assert_eq!(l.h_images_size, 180); // 176 + 4
        // Il2CppFieldDefinition for v24: nameIndex(4)+typeIndex(4)+customAttributeIndex(4)+token(4)=16
        assert_eq!(l.field_size, 16);
        assert_eq!(l.field_name_index, 0);
    }

    #[test]
    fn compute_layout_v27_removes_rgctx() {
        let l = compute_layout(27).expect("v27 should produce a layout");
        // v27 has no rgctxEntries, so images shifts up
        assert_eq!(l.h_images_offset, 168); // 160 + 8 (no rgctxEntries)
        // field_size: v27 has no customAttributeIndex (max_ver=24)
        // nameIndex(4)+typeIndex(4)+token(4)(min_ver=19) = 12
        assert_eq!(l.field_size, 12);
    }

    #[test]
    fn compute_layout_v29_adds_attributes() {
        let l = compute_layout(29).expect("v29 should produce a layout");
        assert_eq!(l.h_images_offset, 168); // same as v27 (no rgctxEntries)

        // v29 fields: nameIndex(4)+typeIndex(4)+token(4)(min=19) = 12
        assert_eq!(l.field_size, 12);
    }

    #[test]
    fn compute_layout_v16_produces_minimal_header() {
        let l = compute_layout(16).expect("v16 should produce a layout");
        // stringLiteral has min_ver=16, so it IS included
        // sanity(4)+version(4)+stringLiteral(8)+stringLiteralData(8) = 24
        // then string section: h_string_offset = 24
        assert_eq!(l.h_string_offset, 24);
        // v16: nameIndex(4)+typeIndex(4)+customAttributeIndex(4)(max=24) = 12
        assert_eq!(l.field_size, 12);
    }
}
