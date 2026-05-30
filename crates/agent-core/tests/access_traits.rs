//! Trait-shape compile-tests for Read<T> / Write<T> / Iter<T>.
//! Synthetic impls verify the API surface compiles cleanly.

use agent_core::spine::{Iter, MemValue, Read, Write};
use agent_core::spine::error::MemError;

/// Synthetic handle that holds a u32 in-process (no FFI).
#[derive(Debug, Clone, Copy)]
struct FakeHandle(u32);

impl Read<u32> for FakeHandle {
    fn read(&self) -> Result<u32, MemError> {
        Ok(self.0)
    }
}

impl Write<u32> for FakeHandle {
    fn write(&self, _value: u32) -> Result<(), MemError> {
        // FakeHandle's value can't be mutated by &self; the trait shape only
        // requires the call to compile and return Ok. Real impls (MemAddr,
        // FieldAddr) take &self because address-based writes don't need &mut.
        Ok(())
    }
}

#[test]
fn read_trait_compiles_and_returns_value() {
    let h = FakeHandle(42);
    let v: u32 = h.read().unwrap();
    assert_eq!(v, 42);
}

#[test]
fn write_trait_compiles_and_returns_ok() {
    let h = FakeHandle(0);
    assert!(h.write(99u32).is_ok());
}

/// Synthetic iter handle yielding three fixed values.
struct ThreeInts;

impl Iter<u32> for ThreeInts {
    type Iter = std::vec::IntoIter<u32>;
    fn iter(&self) -> Self::Iter {
        vec![1u32, 2, 3].into_iter()
    }
}

#[test]
fn iter_trait_yields_items_lazily() {
    let h = ThreeInts;
    let collected: Vec<u32> = h.iter().collect();
    assert_eq!(collected, vec![1, 2, 3]);
}

#[test]
fn iter_can_be_chained_with_combinators() {
    let h = ThreeInts;
    let doubled: Vec<u32> = h.iter().map(|x| x * 2).collect();
    assert_eq!(doubled, vec![2, 4, 6]);
}

/// Generic function using a `Read<T>` bound — proves composition contract.
fn read_anything<H: Read<u32>>(h: &H) -> Result<u32, MemError> {
    h.read()
}

#[test]
fn generic_read_bound_works() {
    let h = FakeHandle(7);
    assert_eq!(read_anything(&h).unwrap(), 7);
}

#[test]
fn frame_ring_iter_zero_length_yields_nothing() {
    use agent_core::protocol::FrameRing;
    use agent_core::spine::Iter;

    let ring = FrameRing::new(64, 4096);
    let count = (&ring).iter().count();
    assert_eq!(count, 0);
}
