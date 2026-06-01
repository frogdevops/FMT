//! Type-code discriminator discovery. Finds the (read_at, shift) pair that
//! extracts the Il2CppTypeEnum byte from a klass's byval_arg region by round-
//! tripping all supplied known-primitive anchors — no offset is assumed.

use crate::bedrock::{
    fact::{DerivationMethod, Fact, Provenance, UnresolvedReason, Witness},
    mem::MemView,
};

/// Try to find the (read_at_offset_in_klass, byte_shift) that extracts the
/// type-code byte from every entry in `known`.
///
/// `known` entries are `(klass_or_byval_arg_ptr, expected_tc)` pairs for
/// well-known primitive types.
///
/// For each candidate `read_at` ∈ {0, 8, 16} and `shift` ∈ {0, 8, 16, 24}:
///   read the u64 at `ptr + read_at`; shift right by `shift`; mask to 0xFF.
///   If that equals `expected_tc` for ALL entries → Resolved with one Witness
///   per anchor (method=Structural, observed=packed read_at+shift, signal=
///   "tc round-trip"). Otherwise Unresolved{NoWitness}.
pub fn find_discriminator(
    mem: &dyn MemView,
    known: &[(usize, u8)],
) -> Fact<(usize, u8)> {
    if known.is_empty() {
        return Fact::Unresolved { reason: UnresolvedReason::NoWitness };
    }

    for read_at in [0usize, 8, 16] {
        for shift in [0u8, 8, 16, 24] {
            let mut all_match = true;
            let mut witnesses: Vec<Witness> = Vec::new();

            for &(ptr, expected_tc) in known {
                match mem.read_u64(ptr + read_at) {
                    Some(chunk) => {
                        let got = ((chunk >> shift) & 0xFF) as u8;
                        if got != expected_tc {
                            all_match = false;
                            break;
                        }
                        witnesses.push(Witness {
                            method: DerivationMethod::Structural,
                            // pack read_at and shift into the observed field for traceability
                            observed: ((read_at as u64) << 32) | (shift as u64),
                            signal: "tc round-trip",
                        });
                    }
                    None => {
                        all_match = false;
                        break;
                    }
                }
            }

            if all_match {
                return Fact::Resolved {
                    value: (read_at, shift),
                    provenance: Provenance {
                        witnesses,
                        sampled: known.len() as u16,
                    },
                };
            }
        }
    }

    Fact::Unresolved { reason: UnresolvedReason::NoWitness }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::mem::MockMem;

    /// Build three synthetic primitive-klass stubs where read_at=8, shift=0
    /// yields tc bytes 0x08, 0x0E, 0x1C respectively.
    fn build_primitive_klasses() -> (MockMem, Vec<(usize, u8)>) {
        let mut m = MockMem::new();
        // Three distinct klass addresses — just need the byval_arg chunk at +8
        let k1 = 0x100_0000usize;
        let k2 = 0x200_0000usize;
        let k3 = 0x300_0000usize;
        // tc at read_at=8, shift=0 (byte 0 of the u64 at klass+8)
        m.put_u64(k1 + 8, 0x08u64); // tc = 0x08 (Int32)
        m.put_u64(k2 + 8, 0x0Eu64); // tc = 0x0E (String)
        m.put_u64(k3 + 8, 0x1Cu64); // tc = 0x1C (Boolean)
        let known = vec![(k1, 0x08u8), (k2, 0x0Eu8), (k3, 0x1Cu8)];
        (m, known)
    }

    #[test]
    fn finds_discriminator_read8_shift0() {
        let (m, known) = build_primitive_klasses();
        match find_discriminator(&m, &known) {
            Fact::Resolved { value: (read_at, shift), ref provenance } => {
                assert_eq!(read_at, 8);
                assert_eq!(shift, 0);
                // one witness per anchor
                assert_eq!(provenance.witnesses.len(), 3);
                assert_eq!(provenance.sampled, 3);
            }
            other => panic!("expected Resolved, got {:?}", other),
        }
    }

    #[test]
    fn no_discriminator_when_values_disagree() {
        let mut m = MockMem::new();
        let k1 = 0x100_0000usize;
        let k2 = 0x200_0000usize;
        // tc bytes are in different positions for the two klasses — no single
        // (read_at, shift) pair can satisfy both
        m.put_u64(k1 + 0, 0x0800_0000_0000_0000u64); // tc=0x08 only at read_at=0, shift=56
        m.put_u64(k2 + 8, 0x0000_0000_0000_0E00u64); // tc=0x0E only at read_at=8, shift=8
        let known = vec![(k1, 0x08u8), (k2, 0x0Eu8)];
        assert!(matches!(
            find_discriminator(&m, &known),
            Fact::Unresolved { reason: UnresolvedReason::NoWitness }
        ));
    }

    #[test]
    fn empty_known_returns_no_witness() {
        let m = MockMem::new();
        assert!(matches!(
            find_discriminator(&m, &[]),
            Fact::Unresolved { reason: UnresolvedReason::NoWitness }
        ));
    }
}
