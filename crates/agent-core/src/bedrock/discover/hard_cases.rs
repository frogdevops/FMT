//! Hard-case discoverers: static_fields, type_def, valuetype bit.
//! These are the honest-Unresolved paths — the plan explicitly calls out that
//! static_fields returns NoDiscriminator on most real tables (most klasses have
//! null static_fields) and type_def returns NoMetadata without metadata access.

use crate::bedrock::{
    discover::foundation::is_klass_shape,
    fact::{DerivationMethod, Fact, Provenance, UnresolvedReason, Witness},
    mem::MemView,
};

/// Attempt to find the static_fields offset by looking for a candidate offset
/// whose slot is a unique non-exec (RW data) pointer on a near-unanimous
/// fraction of the supplied klasses.
///
/// Honest reality: most klasses have null static_fields, so NO candidate will
/// reach near-unanimity and this returns `Fact::Unresolved{NoDiscriminator}`.
/// The function does NOT fabricate a value.
///
/// `candidates` is a slice of byte offsets to test within each klass.
pub fn discover_static_fields(
    mem: &dyn MemView,
    klasses: &[usize],
    candidates: &[usize],
) -> Fact<usize> {
    if klasses.is_empty() || candidates.is_empty() {
        return Fact::Unresolved { reason: UnresolvedReason::NoDiscriminator };
    }

    // near-unanimity threshold: ≥80% of klasses must have a non-null, non-exec pointer
    let threshold = (klasses.len() * 80 + 99) / 100; // ceiling of 80%

    for &off in candidates {
        let mut hits = 0usize;
        let mut witnesses: Vec<Witness> = Vec::new();

        for &klass in klasses {
            if let Some(slot) = mem.read_u64(klass + off) {
                let addr = slot as usize;
                // Reject structural class-pointers (self / element_class / castClass /
                // parent): those are non-null on every klass and would falsely win.
                // A static-storage pointer points to a DATA block, not to a klass.
                if addr != 0 && addr >= 0x10_0000 && !mem.is_exec(addr) && !is_klass_shape(mem, addr) {
                    hits += 1;
                    witnesses.push(Witness {
                        method: DerivationMethod::Structural,
                        observed: off as u64,
                        signal: "non-exec, non-klass data ptr at candidate offset",
                    });
                }
            }
        }

        if hits >= threshold {
            return Fact::Resolved {
                value: off,
                provenance: Provenance {
                    witnesses,
                    sampled: klasses.len() as u16,
                },
            };
        }
    }

    Fact::Unresolved { reason: UnresolvedReason::NoDiscriminator }
}

/// Resolve the type_def offset only when metadata is available.
/// Without metadata access the value cannot be validated, so this returns
/// `Fact::Unresolved{NoMetadata}` — no fallback, no guessed constant.
///
/// `has_metadata`: true when the caller has confirmed out-of-band access to
/// Il2CppMetadata (e.g. via a mapped metadata file or FFI read).
/// `value`: the candidate offset for klass_type_def.
pub fn discover_type_def(has_metadata: bool, value: usize) -> Fact<usize> {
    if has_metadata {
        Fact::Resolved {
            value,
            provenance: Provenance {
                witnesses: vec![Witness {
                    method: DerivationMethod::OutOfBandAnchor,
                    observed: value as u64,
                    signal: "metadata present — type_def derivable",
                }],
                sampled: 1,
            },
        }
    } else {
        Fact::Unresolved { reason: UnresolvedReason::NoMetadata }
    }
}

/// Find the (byte-offset-within-klass, bit-index) that distinguishes value
/// types from reference types.
///
/// Scans the first 0x60 bytes of each klass at byte granularity and finds the
/// (byte_off, bit) where the bit is set in ALL `value_types` AND clear in ALL
/// `ref_types`. Returns `Resolved` if exactly one such pair is found;
/// `Unresolved{NoWitness}` otherwise.
pub fn discover_valuetype_bit(
    mem: &dyn MemView,
    value_types: &[usize],
    ref_types: &[usize],
    byval_off: usize,
) -> Fact<(usize, u8)> {
    if value_types.is_empty() || ref_types.is_empty() {
        return Fact::Unresolved { reason: UnresolvedReason::NoWitness };
    }

    let mut candidates: Vec<(usize, u8)> = Vec::new();

    for byte_off in 0..0x60usize {
        for bit in 0..8u8 {
            let mask = 1u8 << bit;

            let set_in_all_vt = value_types.iter().all(|&klass| {
                mem.read_u8(klass + byval_off + byte_off)
                    .map_or(false, |b| b & mask != 0)
            });
            let clear_in_all_ref = ref_types.iter().all(|&klass| {
                mem.read_u8(klass + byval_off + byte_off)
                    .map_or(false, |b| b & mask == 0)
            });

            if set_in_all_vt && clear_in_all_ref {
                candidates.push((byte_off, bit));
            }
        }
    }

    if candidates.len() == 1 {
        let (byte_off, bit) = candidates[0];
        let mut witnesses: Vec<Witness> = Vec::new();
        for _klass in value_types {
            witnesses.push(Witness {
                method: DerivationMethod::Structural,
                observed: ((byte_off as u64) << 8) | (bit as u64),
                signal: "valuetype bit set",
            });
        }
        for _klass in ref_types {
            witnesses.push(Witness {
                method: DerivationMethod::Structural,
                observed: ((byte_off as u64) << 8) | (bit as u64),
                signal: "valuetype bit clear",
            });
        }
        Fact::Resolved {
            value: (byte_off, bit),
            provenance: Provenance {
                witnesses,
                sampled: (value_types.len() + ref_types.len()) as u16,
            },
        }
    } else {
        Fact::Unresolved { reason: UnresolvedReason::NoWitness }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::mem::MockMem;

    // --- discover_static_fields ---

    #[test]
    fn static_fields_unresolved_when_most_null() {
        let mut m = MockMem::new();
        let klasses: Vec<usize> = (0..5).map(|i| 0x100_0000 + i * 0x1000).collect();
        // Only 1 of 5 klasses has a non-null, non-exec pointer at offset 0x50 → below threshold
        m.put_u64(klasses[0] + 0x50, 0x500_0000u64);
        // rest are null / zero
        let candidates = vec![0x50usize];
        assert!(matches!(
            discover_static_fields(&m, &klasses, &candidates),
            Fact::Unresolved { reason: UnresolvedReason::NoDiscriminator }
        ));
    }

    #[test]
    fn static_fields_resolved_when_near_unanimous() {
        let mut m = MockMem::new();
        let klasses: Vec<usize> = (0..5).map(|i| 0x100_0000 + i * 0x2000).collect();
        // 5 of 5 klasses have a non-null, non-exec data ptr at offset 0x50 → unanimous
        for &k in &klasses {
            m.put_u64(k + 0x50, 0x700_0000u64 + k as u64);
        }
        let candidates = vec![0x50usize];
        match discover_static_fields(&m, &klasses, &candidates) {
            Fact::Resolved { value, .. } => assert_eq!(value, 0x50),
            other => panic!("expected Resolved, got {:?}", other),
        }
    }

    // --- discover_type_def ---

    #[test]
    fn type_def_unresolved_without_metadata() {
        assert!(matches!(
            discover_type_def(false, 0x80),
            Fact::Unresolved { reason: UnresolvedReason::NoMetadata }
        ));
    }

    #[test]
    fn type_def_resolved_with_metadata() {
        match discover_type_def(true, 0x80) {
            Fact::Resolved { value, .. } => assert_eq!(value, 0x80),
            other => panic!("expected Resolved, got {:?}", other),
        }
    }

    // --- discover_valuetype_bit ---

    #[test]
    fn valuetype_bit_resolved_for_clean_split() {
        let mut m = MockMem::new();
        let vt = vec![0x100_0000usize, 0x200_0000usize];
        let rf = vec![0x300_0000usize, 0x400_0000usize];
        let byval_off = 0x10usize;
        // bit 2 of byte 0 is the flag: set in all vt, clear in all ref
        for &k in &vt {
            m.put_u64(k + byval_off, 0x04u64); // bit 2 set
        }
        for &k in &rf {
            m.put_u64(k + byval_off, 0x00u64); // bit 2 clear
        }
        match discover_valuetype_bit(&m, &vt, &rf, byval_off) {
            Fact::Resolved { value: (byte_off, bit), ref provenance } => {
                assert_eq!(byte_off, 0);
                assert_eq!(bit, 2);
                assert_eq!(provenance.sampled, 4);
            }
            other => panic!("expected Resolved, got {:?}", other),
        }
    }

    #[test]
    fn valuetype_bit_unresolved_when_no_single_bit() {
        let mut m = MockMem::new();
        let vt = vec![0x100_0000usize];
        let rf = vec![0x200_0000usize];
        let byval_off = 0x10usize;
        // Both have the same byte → no bit is set in vt and clear in ref
        m.put_u64(vt[0] + byval_off, 0xFFu64);
        m.put_u64(rf[0] + byval_off, 0xFFu64);
        assert!(matches!(
            discover_valuetype_bit(&m, &vt, &rf, byval_off),
            Fact::Unresolved { reason: UnresolvedReason::NoWitness }
        ));
    }

    #[test]
    fn valuetype_bit_unresolved_when_empty() {
        let m = MockMem::new();
        assert!(matches!(
            discover_valuetype_bit(&m, &[], &[0x100_0000], 0),
            Fact::Unresolved { reason: UnresolvedReason::NoWitness }
        ));
    }
}
