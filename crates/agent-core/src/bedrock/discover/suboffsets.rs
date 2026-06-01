//! Derive sub-offsets by classifying a found container's own slots — never by
//! assuming a number. Returns (method_pointer_off, method_klass_off, method_name_off).

use crate::bedrock::mem::MemView;

pub fn derive_method_suboffsets(mem: &dyn MemView, mi: usize, klass: usize)
    -> (Option<usize>, Option<usize>, Option<usize>)
{
    let (mut mp, mut mk, mut mn) = (None, None, None);
    let mut j = 0usize;
    while j < 0x60 {
        if let Some(w) = mem.read_u64(mi + j) {
            let wu = w as usize;
            if mk.is_none() && wu == klass { mk = Some(j); }
            else if mp.is_none() && wu >= 0x10_0000 && mem.is_exec(wu) { mp = Some(j); }
            else if mn.is_none() {
                if let Some(s) = mem.read_cstr(wu) { if s.len() >= 2 && s.len() < 64 { mn = Some(j); } }
            }
        }
        j += 8;
    }
    (mp, mk, mn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::mem::MockMem;
    #[test]
    fn derives_pointer_klass_name_slots() {
        let mut m = MockMem::new();
        let (mi, klass, code, name) = (0x30_000usize, 0x10_000usize, 0x6f00_0000usize, 0x50_000usize);
        m.mark_exec(code, 0x1000);
        m.put_u64(mi + 0x00, code as u64);   // RX → method_pointer_off
        m.put_u64(mi + 0x18, name as u64);   // cstr → method_name_off
        m.put_cstr(name, "Pow");
        m.put_u64(mi + 0x20, klass as u64);  // ==klass → method_klass_off
        assert_eq!(derive_method_suboffsets(&m, mi, klass), (Some(0x0), Some(0x20), Some(0x18)));
    }
}
