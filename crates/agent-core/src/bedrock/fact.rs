//! The discovery contract: a value either has ≥2 agreeing witnesses (`Resolved`,
//! carrying its full derivation in `Provenance`) or it is `Unresolved`. There is
//! deliberately no third "fallback" state — fail-closed is structural.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact<T> {
    Resolved { value: T, provenance: Provenance },
    Unresolved { reason: UnresolvedReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Every agreeing derivation and what it observed. The value documents its
    /// own derivation; the report/log is generated from this, never hand-written.
    pub witnesses: Vec<Witness>,
    /// Sample size the agreement held over (e.g. 12 → "12/12 klasses").
    pub sampled: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Witness {
    pub method: DerivationMethod,
    pub observed: u64,
    pub signal: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivationMethod { Structural, ReferenceCrossCheck, FfiCrossCheck, OutOfBandAnchor }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnresolvedReason { NoWitness, WitnessDisagreement, NoMetadata, NoDiscriminator }

impl<T: Copy> Fact<T> {
    pub fn get(&self) -> Option<T> {
        match self { Fact::Resolved { value, .. } => Some(*value), _ => None }
    }
    pub fn require(&self) -> Result<T, UnresolvedReason> {
        match self { Fact::Resolved { value, .. } => Ok(*value), Fact::Unresolved { reason } => Err(*reason) }
    }
    pub fn is_resolved(&self) -> bool { matches!(self, Fact::Resolved { .. }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(v: u64) -> Provenance {
        Provenance { witnesses: vec![Witness { method: DerivationMethod::Structural, observed: v, signal: "test" }], sampled: 1 }
    }

    #[test]
    fn resolved_get_and_require() {
        let f = Fact::Resolved { value: 0x98usize, provenance: prov(0x98) };
        assert_eq!(f.get(), Some(0x98));
        assert_eq!(f.require(), Ok(0x98));
    }

    #[test]
    fn unresolved_get_is_none_require_is_err() {
        let f: Fact<usize> = Fact::Unresolved { reason: UnresolvedReason::NoWitness };
        assert_eq!(f.get(), None);
        assert_eq!(f.require(), Err(UnresolvedReason::NoWitness));
    }
}
