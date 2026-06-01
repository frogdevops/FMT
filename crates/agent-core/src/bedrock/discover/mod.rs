pub mod containers;
pub mod suboffsets;
pub mod foundation;
pub mod type_discrim;
pub mod hard_cases;

use crate::bedrock::{
    fact::{DerivationMethod, Fact, Provenance, UnresolvedReason, Witness},
    layout::Layout,
    mem::MemView,
};
use containers::{recognize_fields, recognize_methods};
use foundation::{is_klass_shape, stride_by_autocorrelation};
use hard_cases::{discover_static_fields, discover_type_def, discover_valuetype_bit};
use type_discrim::find_discriminator;

/// Minimum number of structurally-valid klass samples required before any
/// consensus verdict is considered meaningful.
const MIN_SAMPLES: usize = 12;

/// Orchestrator: walk the class table, sample structurally-valid klasses,
/// discover every layout offset by unanimous consensus across the sample,
/// and return a `Layout` where every field carries its full derivation provenance.
///
/// Truth-management contract:
///   - Every Resolved fact carries one Witness per sampled klass.
///   - UNANIMITY is required; disagreement → Unresolved{WitnessDisagreement}.
///   - Honest Unresolved paths are never papered over with fallbacks.
pub fn discover(mem: &dyn MemView, table_base: usize, table_count: usize) -> Layout {
    // ── Step 1: table_base + table_count are directly supplied (root locator) ──
    let tb_fact = Fact::Resolved {
        value: table_base,
        provenance: Provenance {
            witnesses: vec![Witness {
                method: DerivationMethod::OutOfBandAnchor,
                observed: table_base as u64,
                signal: "from root locator",
            }],
            sampled: 1,
        },
    };
    let tc_fact = Fact::Resolved {
        value: table_count,
        provenance: Provenance {
            witnesses: vec![Witness {
                method: DerivationMethod::OutOfBandAnchor,
                observed: table_count as u64,
                signal: "from root locator",
            }],
            sampled: 1,
        },
    };

    // ── Step 2: stride by autocorrelation ──
    let stride_opt = stride_by_autocorrelation(mem, table_base, table_count);
    let effective_stride = stride_opt.unwrap_or(8); // fall to pointer-width only for sampling; Fact stays Unresolved if None
    let class_table_step = match stride_opt {
        Some(s) => Fact::Resolved {
            value: s,
            provenance: Provenance {
                witnesses: vec![Witness {
                    method: DerivationMethod::Structural,
                    observed: s as u64,
                    signal: "autocorrelation density",
                }],
                sampled: table_count.min(256) as u16,
            },
        },
        None => Fact::Unresolved { reason: UnresolvedReason::NoWitness },
    };

    // ── Step 3: sample structurally-valid klasses ──
    let mut sampled_klasses: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < table_count && sampled_klasses.len() < 64 {
        let slot_addr = table_base + i * effective_stride;
        if let Some(ptr) = mem.read_u64(slot_addr) {
            let klass = ptr as usize;
            if klass != 0 && is_klass_shape(mem, klass) {
                sampled_klasses.push(klass);
            }
        }
        i += 1;
    }

    // ── Step 4: methods consensus ──
    let klass_methods = unanimity_usize(
        &sampled_klasses,
        |klass| {
            let hits = recognize_methods(mem, klass);
            if hits.len() == 1 { Some(hits[0]) } else { None }
        },
        "container recognized",
    );

    // ── Step 5: fields consensus ──
    let klass_fields = unanimity_usize(
        &sampled_klasses,
        |klass| {
            let hits = recognize_fields(mem, klass);
            if hits.len() == 1 { Some(hits[0]) } else { None }
        },
        "container recognized",
    );

    // ── Step 6: method sub-offsets consensus ──
    // For each sampled klass where we know the methods offset, read the first
    // MethodInfo pointer and derive sub-offsets.
    let methods_off_opt = klass_methods.get();
    let (method_pointer_off, method_klass_off, method_name_off) = if let Some(moff) = methods_off_opt {
        suboffset_consensus(&sampled_klasses, mem, moff)
    } else {
        (
            Fact::Unresolved { reason: UnresolvedReason::NoWitness },
            Fact::Unresolved { reason: UnresolvedReason::NoWitness },
            Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        )
    };

    // ── Step 7: locate known-primitive anchor klasses by name (for type discrim) ──
    // Walk the sampled klasses looking for "Int32", "Boolean", "String" by name
    // (no FFI — structural name lookup only).
    let anchors = locate_primitive_anchors(mem, &sampled_klasses);
    let type_discrim_fact = find_discriminator(mem, &anchors);
    let (type_discrim_read_at, discrim_shift) = match &type_discrim_fact {
        Fact::Resolved { value: (ra, sh), .. } => (
            Fact::Resolved {
                value: *ra,
                provenance: type_discrim_fact.provenance_clone(),
            },
            Fact::Resolved {
                value: *sh,
                provenance: type_discrim_fact.provenance_clone(),
            },
        ),
        _ => (
            Fact::Unresolved { reason: UnresolvedReason::NoWitness },
            Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        ),
    };

    // ── Step 8: valuetype bit ──
    let (value_type_klasses, ref_type_klasses) = split_vt_ref_klasses(mem, &sampled_klasses);
    let klass_valuetype = discover_valuetype_bit(mem, &value_type_klasses, &ref_type_klasses, 0);
    let (klass_valuetype_off, klass_valuetype_bit_fact) = match &klass_valuetype {
        Fact::Resolved { value: (boff, b), .. } => (
            Fact::Resolved {
                value: *boff,
                provenance: klass_valuetype.provenance_clone(),
            },
            Fact::Resolved {
                value: *b,
                provenance: klass_valuetype.provenance_clone(),
            },
        ),
        _ => (
            Fact::Unresolved { reason: UnresolvedReason::NoWitness },
            Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        ),
    };

    // ── Step 9: static_fields — honestly Unresolved on most tables ──
    let candidate_offs: Vec<usize> = (0x40..=0xf8).step_by(8).collect();
    let klass_static_fields = discover_static_fields(mem, &sampled_klasses, &candidate_offs);

    // ── Step 10: type_def — Unresolved without metadata (honest mock reality) ──
    let klass_type_def = discover_type_def(false, 0);

    // ── Step 11: namespace offset ──
    let klass_namespace = unanimity_usize(
        &sampled_klasses,
        |klass| {
            // namespace is the cstring pointer at klass+0x18 (mirrors class_fields mechanism)
            let ns_ptr = mem.read_u64(klass + 0x18).map(|v| v as usize)?;
            // it should be a readable cstring (empty string is valid for no-namespace types)
            mem.read_cstr(ns_ptr)?;
            Some(0x18)
        },
        "namespace cstr at structural offset",
    );

    // ── Assemble Layout ──
    Layout {
        table_base: tb_fact,
        table_count: tc_fact,
        class_table_step,
        klass_namespace,
        klass_fields,
        klass_methods,
        klass_static_fields,
        klass_type_def,
        klass_generic_class: Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        klass_valuetype_off,
        klass_valuetype_bit: klass_valuetype_bit_fact,
        type_discrim_read_at,
        discrim_shift,
        method_pointer_off,
        method_klass_off,
        method_name_off,
        method_param_count_off: Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        method_return_type_off: Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        method_parameters_off: Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        method_flags_off: Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        param_info_size: Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        param_info_type_off: Fact::Unresolved { reason: UnresolvedReason::NoWitness },
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Require unanimity of a usize result across `klasses`. Each klass is passed
/// to `probe`; if all return the same Some(value), return Resolved with one
/// Witness per klass. If any disagrees or returns None, return Unresolved.
fn unanimity_usize<F>(klasses: &[usize], probe: F, signal: &'static str) -> Fact<usize>
where
    F: Fn(usize) -> Option<usize>,
{
    if klasses.len() < MIN_SAMPLES {
        return Fact::Unresolved { reason: UnresolvedReason::NoWitness };
    }
    let mut agreed: Option<usize> = None;
    let mut witnesses: Vec<Witness> = Vec::new();
    for &klass in klasses {
        match probe(klass) {
            Some(v) => {
                if let Some(prev) = agreed {
                    if v != prev {
                        return Fact::Unresolved { reason: UnresolvedReason::WitnessDisagreement };
                    }
                } else {
                    agreed = Some(v);
                }
                witnesses.push(Witness {
                    method: DerivationMethod::Structural,
                    observed: v as u64,
                    signal,
                });
            }
            None => return Fact::Unresolved { reason: UnresolvedReason::NoWitness },
        }
    }
    match agreed {
        Some(v) => Fact::Resolved {
            value: v,
            provenance: Provenance {
                witnesses,
                sampled: klasses.len() as u16,
            },
        },
        None => Fact::Unresolved { reason: UnresolvedReason::NoWitness },
    }
}

/// Derive method sub-offsets by unanimous consensus across sampled klasses.
/// For each klass at `methods_off`, read the first MethodInfo pointer, then
/// call `derive_method_suboffsets`. Require all samples to return the same
/// (mp, mk, mn) triple.
fn suboffset_consensus(
    klasses: &[usize],
    mem: &dyn MemView,
    methods_off: usize,
) -> (Fact<usize>, Fact<usize>, Fact<usize>) {
    if klasses.len() < MIN_SAMPLES {
        let u = || Fact::Unresolved { reason: UnresolvedReason::NoWitness };
        return (u(), u(), u());
    }

    let mut mp_agreed: Option<usize> = None;
    let mut mk_agreed: Option<usize> = None;
    let mut mn_agreed: Option<usize> = None;
    let mut mp_witnesses: Vec<Witness> = Vec::new();
    let mut mk_witnesses: Vec<Witness> = Vec::new();
    let mut mn_witnesses: Vec<Witness> = Vec::new();
    let mut mp_ok = true;
    let mut mk_ok = true;
    let mut mn_ok = true;

    for &klass in klasses {
        let arr = match mem.read_u64(klass + methods_off) {
            Some(v) if v != 0 => v as usize,
            _ => { mp_ok = false; mk_ok = false; mn_ok = false; break; }
        };
        let mi0 = match mem.read_u64(arr) {
            Some(v) if v != 0 => v as usize,
            _ => { mp_ok = false; mk_ok = false; mn_ok = false; break; }
        };
        let (mp, mk, mn) = suboffsets::derive_method_suboffsets(mem, mi0, klass);

        // method_pointer_off
        if mp_ok {
            match mp {
                Some(v) => {
                    if let Some(prev) = mp_agreed { if v != prev { mp_ok = false; } }
                    else { mp_agreed = Some(v); }
                    if mp_ok { mp_witnesses.push(Witness { method: DerivationMethod::Structural, observed: v as u64, signal: "method_pointer_off" }); }
                }
                None => mp_ok = false,
            }
        }
        // method_klass_off
        if mk_ok {
            match mk {
                Some(v) => {
                    if let Some(prev) = mk_agreed { if v != prev { mk_ok = false; } }
                    else { mk_agreed = Some(v); }
                    if mk_ok { mk_witnesses.push(Witness { method: DerivationMethod::Structural, observed: v as u64, signal: "method_klass_off" }); }
                }
                None => mk_ok = false,
            }
        }
        // method_name_off
        if mn_ok {
            match mn {
                Some(v) => {
                    if let Some(prev) = mn_agreed { if v != prev { mn_ok = false; } }
                    else { mn_agreed = Some(v); }
                    if mn_ok { mn_witnesses.push(Witness { method: DerivationMethod::Structural, observed: v as u64, signal: "method_name_off" }); }
                }
                None => mn_ok = false,
            }
        }
    }

    let n = klasses.len() as u16;
    let make = |ok: bool, agreed: Option<usize>, witnesses: Vec<Witness>| -> Fact<usize> {
        if ok {
            if let Some(v) = agreed {
                return Fact::Resolved { value: v, provenance: Provenance { witnesses, sampled: n } };
            }
        }
        Fact::Unresolved { reason: UnresolvedReason::WitnessDisagreement }
    };

    (
        make(mp_ok, mp_agreed, mp_witnesses),
        make(mk_ok, mk_agreed, mk_witnesses),
        make(mn_ok, mn_agreed, mn_witnesses),
    )
}

/// Walk `sampled_klasses` and try to read the class name at klass+0x10.
/// Returns `(value_type_ptrs, ref_type_ptrs)` by looking for "Int32"/"Boolean"
/// (value types) and "String"/"Object" (reference types) by name.
/// This is purely structural — no FFI, no metadata required.
fn split_vt_ref_klasses(mem: &dyn MemView, klasses: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let mut vt = Vec::new();
    let mut rf = Vec::new();
    for &klass in klasses {
        if let Some(nm_ptr) = mem.read_u64(klass + 0x10).map(|v| v as usize) {
            if let Some(name) = mem.read_cstr(nm_ptr) {
                match name.as_str() {
                    "Int32" | "Boolean" | "Single" | "Int64" | "Byte" => vt.push(klass),
                    "String" | "Object" | "MonoBehaviour" => rf.push(klass),
                    _ => {}
                }
            }
        }
    }
    (vt, rf)
}

/// Walk `sampled_klasses` collecting `(klass, tc)` pairs for known primitives
/// (identified by class name) for the type-discriminator round-trip.
fn locate_primitive_anchors(mem: &dyn MemView, klasses: &[usize]) -> Vec<(usize, u8)> {
    let mut anchors = Vec::new();
    for &klass in klasses {
        if let Some(nm_ptr) = mem.read_u64(klass + 0x10).map(|v| v as usize) {
            if let Some(name) = mem.read_cstr(nm_ptr) {
                // Il2CppTypeEnum values for well-known primitives
                let tc: Option<u8> = match name.as_str() {
                    "Boolean"  => Some(0x02),
                    "Byte"     => Some(0x05),
                    "Int16"    => Some(0x06),
                    "Int32"    => Some(0x08),
                    "Int64"    => Some(0x0A),
                    "Single"   => Some(0x0C),
                    "Double"   => Some(0x0D),
                    "String"   => Some(0x0E),
                    _          => None,
                };
                if let Some(tc) = tc {
                    anchors.push((klass, tc));
                }
            }
        }
    }
    anchors
}

// ── Provenance helper ────────────────────────────────────────────────────────

/// Helper trait to clone Provenance out of a Fact without taking ownership.
trait FactProvenance {
    fn provenance_clone(&self) -> Provenance;
}

impl<T> FactProvenance for Fact<T> {
    fn provenance_clone(&self) -> Provenance {
        match self {
            Fact::Resolved { provenance, .. } => provenance.clone(),
            Fact::Unresolved { .. } => Provenance { witnesses: vec![], sampled: 0 },
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::mem::MockMem;

    /// Build a complete klass entry suitable for `discover()`:
    ///   - klass-shaped (image→.dll, name, namespace)
    ///   - methods array at klass+0x98 with 2 MethodInfo (RX+back-ptr)
    ///   - fields array at klass+0x80 with 1 FieldInfo (name cstr + back-ptr, no exec)
    ///   - MethodInfo: RX ptr@+0x0, name cstr@+0x18, klass back-ptr@+0x20
    ///   - namespace cstring at klass+0x18
    fn build_klass(m: &mut MockMem, klass: usize, name: &str, code: usize) {
        // Image shape (slot 0 of image → name cstr ending ".dll")
        let img       = klass + 0x10_000;
        let imgname   = klass + 0x11_000;
        let classname = klass + 0x12_000;
        let nsname    = klass + 0x13_000;
        m.put_u64(klass + 0x00, img as u64);
        m.put_u64(img   + 0x00, imgname as u64);
        m.put_cstr(imgname, "Assembly-CSharp.dll");
        m.put_u64(klass + 0x10, classname as u64);
        m.put_cstr(classname, name);
        m.put_u64(klass + 0x18, nsname as u64);
        m.put_cstr(nsname, "");

        // Methods array at klass+0x98
        let arr = klass + 0x14_000;
        let mi0 = klass + 0x15_000;
        let mi1 = klass + 0x15_100;
        let miname = klass + 0x16_000;
        m.put_u64(klass + 0x98, arr as u64);
        m.put_u64(arr,     mi0 as u64);
        m.put_u64(arr + 8, mi1 as u64);
        for mi in [mi0, mi1] {
            m.put_u64(mi + 0x00, code as u64);    // RX method pointer
            m.put_u64(mi + 0x18, miname as u64);  // name cstr
            m.put_u64(mi + 0x20, klass as u64);   // back-ptr to klass
        }
        m.put_cstr(miname, "Update");

        // Fields: klass+0x80 → first FieldInfo directly (recognize_fields reads
        // klass+off as a pointer to the FieldInfo itself, not an array header).
        let fi0    = klass + 0x17_000;
        let finame = klass + 0x19_000;
        m.put_u64(klass + 0x80, fi0 as u64);   // points directly to first FieldInfo
        m.put_u64(fi0 + 0x00, finame as u64);  // slot 0: field name cstr pointer
        m.put_cstr(finame, "m_value");
        m.put_u64(fi0 + 0x08, klass as u64);   // back-ptr to klass (no exec)
    }

    /// Build a MockMem with ≥12 klass entries and a table pointing at them.
    fn build_table(n: usize) -> (MockMem, usize, usize) {
        let mut m = MockMem::new();
        let code = 0x6f00_0000usize;
        m.mark_exec(code, 0x1000);

        let table_base = 0x100_0000usize;
        // Each klass gets a large address range: base 0x200_0000, spaced 0x20_000 apart
        // so their internal sub-allocations don't collide.
        for i in 0..n {
            let klass = 0x200_0000 + i * 0x20_000;
            m.put_u64(table_base + i * 8, klass as u64);
            build_klass(&mut m, klass, "MyClass", code);
        }
        (m, table_base, n)
    }

    #[test]
    fn discover_resolves_core_layout() {
        let (m, base, count) = build_table(14);
        let layout = discover(&m, base, count);

        // table provenance
        assert_eq!(layout.table_base.require(), Ok(base));
        assert_eq!(layout.table_count.require(), Ok(count));

        // stride
        assert_eq!(layout.class_table_step.require(), Ok(8));

        // container offsets
        assert_eq!(layout.klass_methods.require(), Ok(0x98), "klass_methods mismatch");
        assert_eq!(layout.klass_fields.require(), Ok(0x80), "klass_fields mismatch");

        // method sub-offsets
        assert_eq!(layout.method_pointer_off.require(), Ok(0x0), "method_pointer_off mismatch");
        assert_eq!(layout.method_klass_off.require(), Ok(0x20), "method_klass_off mismatch");
        assert_eq!(layout.method_name_off.require(), Ok(0x18), "method_name_off mismatch");

        // honest Unresolved: no metadata in mock → type_def stays Unresolved
        assert!(
            matches!(layout.klass_type_def, Fact::Unresolved { reason: UnresolvedReason::NoMetadata }),
            "klass_type_def should be NoMetadata"
        );

        // sampled >= 12 for all resolved container facts
        if let Fact::Resolved { ref provenance, .. } = layout.klass_methods {
            assert!(provenance.sampled >= 12, "klass_methods sampled={}", provenance.sampled);
        }
        if let Fact::Resolved { ref provenance, .. } = layout.klass_fields {
            assert!(provenance.sampled >= 12, "klass_fields sampled={}", provenance.sampled);
        }
        if let Fact::Resolved { ref provenance, .. } = layout.method_pointer_off {
            assert!(provenance.sampled >= 12, "method_pointer_off sampled={}", provenance.sampled);
        }
    }

    #[test]
    fn discover_requires_unanimity_methods() {
        // Build a table where klasses disagree on methods offset → WitnessDisagreement
        let mut m = MockMem::new();
        let code = 0x6f00_0000usize;
        m.mark_exec(code, 0x1000);
        let table_base = 0x100_0000usize;

        // First 7 klasses: methods at 0x98
        for i in 0..7usize {
            let klass = 0x200_0000 + i * 0x20_000;
            m.put_u64(table_base + i * 8, klass as u64);
            build_klass(&mut m, klass, "A", code);
        }
        // Next 7 klasses: methods placed at 0xA0 instead (different offset)
        for i in 7..14usize {
            let klass = 0x200_0000 + i * 0x20_000;
            m.put_u64(table_base + i * 8, klass as u64);
            // Build the standard klass shape but with methods at 0xA0 not 0x98
            let img     = klass + 0x10_000;
            let imgname = klass + 0x11_000;
            let cname   = klass + 0x12_000;
            let ns      = klass + 0x13_000;
            m.put_u64(klass + 0x00, img as u64);
            m.put_u64(img   + 0x00, imgname as u64);
            m.put_cstr(imgname, "Assembly-CSharp.dll");
            m.put_u64(klass + 0x10, cname as u64);
            m.put_cstr(cname, "B");
            m.put_u64(klass + 0x18, ns as u64);
            m.put_cstr(ns, "");
            // methods at 0xA0
            let arr = klass + 0x14_000;
            let mi0 = klass + 0x15_000;
            let mi1 = klass + 0x15_100;
            m.put_u64(klass + 0xA0, arr as u64);
            m.put_u64(arr,     mi0 as u64);
            m.put_u64(arr + 8, mi1 as u64);
            for mi in [mi0, mi1] {
                m.put_u64(mi + 0x00, code as u64);
                m.put_u64(mi + 0x20, klass as u64);
            }
            // fields at 0x80
            let farr  = klass + 0x17_000;
            let fi0   = klass + 0x18_000;
            let finame= klass + 0x19_000;
            m.put_u64(klass + 0x80, farr as u64);
            m.put_u64(farr, fi0 as u64);
            m.put_u64(fi0 + 0x00, finame as u64);
            m.put_cstr(finame, "m_x");
            m.put_u64(fi0 + 0x08, klass as u64);
        }

        let layout = discover(&m, table_base, 14);
        assert!(
            matches!(layout.klass_methods, Fact::Unresolved { reason: UnresolvedReason::WitnessDisagreement }),
            "expected WitnessDisagreement for disagreeing methods offset"
        );
    }

    #[test]
    fn discover_insufficient_samples_unresolved() {
        // Fewer than MIN_SAMPLES valid klasses → methods/fields Unresolved{NoWitness}
        let (m, base, _) = build_table(3); // only 3 klasses
        let layout = discover(&m, base, 3);
        assert!(
            matches!(layout.klass_methods, Fact::Unresolved { .. }),
            "should be Unresolved with only 3 samples"
        );
    }
}
