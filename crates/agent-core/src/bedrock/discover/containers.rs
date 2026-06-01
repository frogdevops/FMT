//! Container recognizers. A container is identified by intrinsic structure only —
//! no sub-offset is assumed (that is what makes it non-circular). Proven live:
//! methods and fields on PW + Highrise (12/12).

use crate::bedrock::mem::MemView;

/// MethodInfo-shaped: within its first 0x60 bytes it holds >=1 executable pointer
/// AND >=1 pointer equal to `klass` (the declaring-class back-pointer).
pub fn looks_methodinfo(mem: &dyn MemView, p: usize, klass: usize) -> bool {
    let (mut rx, mut back) = (false, false);
    let mut j = 0usize;
    while j < 0x60 {
        if let Some(w) = mem.read_u64(p + j) {
            let wu = w as usize;
            if wu == klass { back = true; }
            else if wu >= 0x10_0000 && mem.is_exec(wu) { rx = true; }
        }
        j += 8;
    }
    rx && back
}

/// klass+off → pointer-array whose first two entries are both MethodInfo-shaped.
pub fn recognize_methods(mem: &dyn MemView, klass: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut off = 0x40usize;
    while off < 0x108 {
        if let Some(arr) = mem.read_u64(klass + off) {
            let arr = arr as usize;
            let e0 = mem.read_u64(arr).unwrap_or(0) as usize;
            let e1 = mem.read_u64(arr + 8).unwrap_or(0) as usize;
            if e0 >= 0x10_0000 && e1 >= 0x10_0000
                && looks_methodinfo(mem, e0, klass)
                && looks_methodinfo(mem, e1, klass)
            {
                hits.push(off);
            }
        }
        off += 8;
    }
    hits
}

/// FieldInfo-shaped inline array: klass+off → first FieldInfo whose slot0 → cstr
/// (field name), contains a ptr == klass (parent), and NO executable ptr.
pub fn recognize_fields(mem: &dyn MemView, klass: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut off = 0x40usize;
    while off < 0x108 {
        if let Some(arr) = mem.read_u64(klass + off) {
            let fi = arr as usize;
            let name_ok = mem.read_u64(fi).and_then(|n| mem.read_cstr(n as usize)).map_or(false, |s| s.len() >= 2);
            if name_ok {
                let (mut back, mut rx) = (false, false);
                let mut j = 0usize;
                while j < 0x40 {
                    if let Some(w) = mem.read_u64(fi + j) {
                        let wu = w as usize;
                        if wu == klass { back = true; }
                        else if wu >= 0x10_0000 && mem.is_exec(wu) { rx = true; }
                    }
                    j += 8;
                }
                if back && !rx { hits.push(off); }
            }
        }
        off += 8;
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::mem::MockMem;

    // Build a klass whose methods array (klass+0x98) holds 2 MethodInfo*; each
    // MethodInfo has an RX code ptr at slot +0x0 and a back-ptr to klass at slot +0x20.
    // Addresses are in the high-user range so pointer-sanity guards (>= 1MB) pass.
    fn klass_with_methods() -> (MockMem, usize) {
        let mut m = MockMem::new();
        let klass = 0x100_0000usize;
        let arr   = 0x200_0000usize;
        let mi0   = 0x300_0000usize;
        let mi1   = 0x300_1000usize;
        let code  = 0x6f00_0000usize; // executable stub region
        m.mark_exec(code, 0x1000);
        m.put_u64(klass + 0x98, arr as u64);
        m.put_u64(arr,     mi0 as u64);
        m.put_u64(arr + 8, mi1 as u64);
        for mi in [mi0, mi1] {
            m.put_u64(mi + 0x00, code as u64);    // RX method pointer slot
            m.put_u64(mi + 0x20, klass as u64);   // back-ptr to declaring klass
        }
        (m, klass)
    }

    #[test]
    fn finds_methods_offset_structurally() {
        let (m, klass) = klass_with_methods();
        assert_eq!(recognize_methods(&m, klass), vec![0x98]);
    }

    #[test]
    fn rejects_non_method_arrays() {
        // array of pointers to structs with NO rx ptr + NO klass backptr → not methods
        let mut m = MockMem::new();
        let klass = 0x100_0000usize;
        m.put_u64(klass + 0x98, 0x200_0000);
        m.put_u64(0x200_0000, 0x400_0000u64);
        m.put_u64(0x200_0008, 0x400_1000u64);
        // structs at 0x400_0000 / 0x400_1000 have no data → looks_methodinfo false
        assert_eq!(recognize_methods(&m, klass), Vec::<usize>::new());
    }
}
