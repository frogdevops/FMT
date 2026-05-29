//! Resolver unit tests are intentionally deferred.
//!
//! The dumper's il2cpp type resolver lives in the `agent` crate (Windows-only,
//! cross-compile required). The resolver function signature couples to
//! `RegionMap`, `Il2CppApi`, and `TypeMaps` — all agent-internal types whose
//! mocks would either:
//!
//!   - Drift from the real types (silent test rot), or
//!   - Require lifting the resolver into agent-core (large architectural
//!     change disproportionate to B-2a's scope).
//!
//! B-2a relies on live-game regression (PW + Highrise) for Fix A + B
//! correctness. See `docs/superpowers/plans/2026-05-30-b2a-honest-dumper-plan.md`
//! Task 7 for the manual verification matrix.
//!
//! A future brick that promotes the resolver into agent-core would naturally
//! land unit tests then.

#[test]
fn deferred_to_live_regression() {
    // Sentinel test — keeps the file compiled and discoverable.
}
