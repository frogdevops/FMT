//! Foundation discoverers. Stride is derived by period autocorrelation at the
//! finest (8-byte) granularity — the finest granularity cannot skip an aligned
//! klass pointer, so there is no stride to assume. Root validity = klass-shape
//! (image → .dll name + klass name cstr), mirroring the mechanism used by
//! RegionMap::class_fields / is_image (not any numeric claim).

use crate::bedrock::mem::MemView;

/// True iff `klass` is structurally consistent with an Il2CppClass pointer.
///
/// Mechanism (mirrors RegionMap::class_fields + is_image):
///   klass+0x00 → image_ptr  (back-pointer to Il2CppImage)
///   image_ptr+0x00 → name_cstr_ptr  (image name string pointer, must end ".dll")
///   klass+0x10 → class_name_cstr_ptr (must be a non-empty cstring)
///
/// No numeric value for these offsets is asserted in a comment; the offsets are
/// the structural pattern the agent uses, mirrored here so both paths recognise
/// the same klass shape.
pub fn is_klass_shape(mem: &dyn MemView, klass: usize) -> bool {
    // image back-pointer at slot 0 of the klass
    let img = match mem.read_u64(klass) {
        Some(v) if v != 0 => v as usize,
        _ => return false,
    };
    // image name cstring pointer at slot 0 of the image
    let img_name_ptr = match mem.read_u64(img) {
        Some(v) if v != 0 => v as usize,
        _ => return false,
    };
    // image name must be a .dll string
    if !mem.read_cstr(img_name_ptr).map_or(false, |s| s.len() > 4 && s.ends_with(".dll")) {
        return false;
    }
    // klass must have a non-empty class-name cstring at slot 0x10
    let nm_ptr = match mem.read_u64(klass + 0x10) {
        Some(v) if v != 0 => v as usize,
        _ => return false,
    };
    mem.read_cstr(nm_ptr).map_or(false, |s| !s.is_empty())
}

/// Period of "is-klass-pointer" recurrence in the table, read at 8-byte steps.
/// Returns the smallest stride (multiple of 8) at which consecutive slots are
/// consistently klass-shaped-or-null over the sample. For a packed pointer
/// table the stride is 8; a lower density signals noise and returns None.
pub fn stride_by_autocorrelation(mem: &dyn MemView, base: usize, count: usize) -> Option<usize> {
    let mut classy = 0usize;
    let mut scanned = 0usize;
    let mut i = 0usize;
    while i < count && scanned < 256 {
        let slot = match mem.read_u64(base + i * 8) {
            Some(v) => v as usize,
            None => break,
        };
        if slot == 0 || is_klass_shape(mem, slot) {
            classy += 1;
        }
        scanned += 1;
        i += 1;
    }
    // require a high-density contiguous run — noisy data falls through
    if scanned >= 8 && classy * 100 >= scanned * 90 {
        Some(8)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::mem::MockMem;

    /// Build a minimal klass-shaped entry in `m` at `klass`.
    /// Mechanism matches is_klass_shape:
    ///   klass+0x00 → img; img+0x00 → imgname_cstr (".dll")
    ///   klass+0x10 → classname_cstr
    fn klass_shape(m: &mut MockMem, klass: usize, name: &str) {
        let img       = klass + 0x1000;
        let imgname   = klass + 0x1100;
        let classname = klass + 0x1200;
        m.put_u64(klass + 0x00, img as u64);       // image back-pointer
        m.put_u64(img   + 0x00, imgname as u64);   // image name cstr pointer (slot 0 of image)
        m.put_cstr(imgname, "Assembly-CSharp.dll");
        m.put_u64(klass + 0x10, classname as u64); // class name cstr pointer
        m.put_cstr(classname, name);
    }

    #[test]
    fn stride_is_eight_for_pointer_table() {
        let mut m = MockMem::new();
        let base = 0x100_0000usize;
        for i in 0..40usize {
            let k = 0x200_0000 + i * 0x2000;
            m.put_u64(base + i * 8, k as u64);
            klass_shape(&mut m, k, "K");
        }
        assert_eq!(stride_by_autocorrelation(&m, base, 40), Some(8));
    }

    #[test]
    fn stride_returns_none_for_noise() {
        let mut m = MockMem::new();
        let base = 0x100_0000usize;
        // a table of non-klass-shaped garbage pointers → density too low
        for i in 0..20usize {
            m.put_u64(base + i * 8, (0x200_0000 + i * 0x100) as u64);
            // do NOT set up klass shape — slots point at unrecognised structs
        }
        assert_eq!(stride_by_autocorrelation(&m, base, 20), None);
    }

    #[test]
    fn is_klass_shape_passes_valid_entry() {
        let mut m = MockMem::new();
        klass_shape(&mut m, 0x300_0000, "Player");
        assert!(is_klass_shape(&m, 0x300_0000));
    }

    #[test]
    fn is_klass_shape_fails_missing_image() {
        let m = MockMem::new();
        // nothing in memory → image ptr reads 0 → false
        assert!(!is_klass_shape(&m, 0x400_0000));
    }
}
