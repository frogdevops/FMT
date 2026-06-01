//! The memory-access seam. Discoverers depend only on this trait, so they are
//! pure logic (agent-core, Linux-testable). The agent crate impls it on RegionMap
//! (VirtualQuery-backed, never-fault). All reads return Option (None = unmapped).

pub trait MemView {
    fn read_u64(&self, addr: usize) -> Option<u64>;
    fn read_u32(&self, addr: usize) -> Option<u32>;
    fn read_u8(&self, addr: usize) -> Option<u8>;
    fn read_cstr(&self, addr: usize) -> Option<String>;
    /// True iff `addr` is in a committed executable page.
    fn is_exec(&self, addr: usize) -> bool;
}

#[cfg(test)]
pub struct MockMem {
    bytes: std::collections::HashMap<usize, u8>,
    exec: Vec<(usize, usize)>,
}

#[cfg(test)]
impl MockMem {
    pub fn new() -> Self { Self { bytes: Default::default(), exec: Vec::new() } }
    pub fn put_bytes(&mut self, addr: usize, b: &[u8]) { for (i, x) in b.iter().enumerate() { self.bytes.insert(addr + i, *x); } }
    pub fn put_u64(&mut self, addr: usize, v: u64) { self.put_bytes(addr, &v.to_le_bytes()); }
    pub fn put_u32(&mut self, addr: usize, v: u32) { self.put_bytes(addr, &v.to_le_bytes()); }
    pub fn put_cstr(&mut self, addr: usize, s: &str) { self.put_bytes(addr, s.as_bytes()); self.bytes.insert(addr + s.len(), 0); }
    pub fn mark_exec(&mut self, addr: usize, len: usize) { self.exec.push((addr, addr + len)); }
}

#[cfg(test)]
impl MemView for MockMem {
    fn read_u64(&self, a: usize) -> Option<u64> {
        let mut b = [0u8; 8];
        for i in 0..8 { b[i] = *self.bytes.get(&(a + i))?; }
        Some(u64::from_le_bytes(b))
    }
    fn read_u32(&self, a: usize) -> Option<u32> {
        let mut b = [0u8; 4];
        for i in 0..4 { b[i] = *self.bytes.get(&(a + i))?; }
        Some(u32::from_le_bytes(b))
    }
    fn read_u8(&self, a: usize) -> Option<u8> { self.bytes.get(&a).copied() }
    fn read_cstr(&self, a: usize) -> Option<String> {
        let mut s = Vec::new();
        let mut i = a;
        loop {
            let c = *self.bytes.get(&i)?;
            if c == 0 { break; }
            if s.len() > 256 { return None; }
            s.push(c); i += 1;
        }
        String::from_utf8(s).ok()
    }
    fn is_exec(&self, a: usize) -> bool { self.exec.iter().any(|&(s, e)| a >= s && a < e) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mockmem_reads_and_classifies() {
        let mut m = MockMem::new();
        m.put_u64(0x1000, 0xDEAD_BEEF);
        m.put_cstr(0x2000, "Player");
        m.mark_exec(0x3000, 0x100); // [0x3000,0x3100) executable
        assert_eq!(m.read_u64(0x1000), Some(0xDEAD_BEEF));
        assert_eq!(m.read_u64(0x9999), None);            // unmapped → None, never faults
        assert_eq!(m.read_cstr(0x2000).as_deref(), Some("Player"));
        assert!(m.is_exec(0x3050));
        assert!(!m.is_exec(0x1000));
    }
}
