# M9 R6RS Stdlib Completion — Design

> Status: **Draft** (M9 iter 1)
> Companion: `requirements.md`

## Overview

M9 is implementation-spread, not architecture-novel. Each subsystem follows the same pattern:

1. Add the conformance test file in `tests/conformance/foundation/<subsystem>.scm`.
2. Implement the builtins in `crates/cs-runtime/src/builtins/mod.rs` (and any helper data types in `crates/cs-core/src/value.rs`).
3. Register them with both walker (`install_into`) and VM tiers.
4. Wire the conformance file into `crates/cs-runtime/tests/vm_conformance.rs` so both tiers' pass counts get tracked.

No new crates. No new ADR entries unless a subsystem turns out invasive.

## Per-subsystem dispatch shape

The runtime already has the procedure dispatch infrastructure:

- `pure_builtins()` for fn-pointer builtins.
- `higher_order_builtins()` for builtins that need EvalCtx (rare).
- `make_vm_builtin` for VM-tier registration.
- `make_builtin_pure` for walker-tier registration.

For data types that need a new `Value` variant (e.g., `Value::EnumSet`), add the variant in `cs-core/src/value.rs`, add a Trace impl, register parents in the GC root closure if mutable.

For data types that can ride existing variants (e.g., enumerations as a struct stored in a Vector + Symbol-list pair), use the existing data — fewer touchpoints.

## Iter 2 (FR-1 `(rnrs enums)`) sketch

Choice point: dedicated `Value::EnumSet` variant vs ride-existing.

- **Dedicated variant**: clean, type-safe, integrates with `enum-set?` predicate naturally. Cost: new variant in cs-core, Trace impl, hashtable equivalence, etc. (~200 LOC of plumbing.)
- **Ride existing**: encode an enum-set as `(cons <universe-list> <bitset>)` where bitset is a fixnum (or bigint for >63 universe). `enum-set?` becomes a tag check + structural predicate. Less invasive but exposes the encoding.

Pick: **dedicated variant** for cleanliness; the type-safety win at the dispatch boundary is worth the LOC.

```rust
// cs-core/src/value.rs (sketch)
pub struct EnumSet {
    /// Shared universe — the ordered list of symbols defined by
    /// the originating make-enumeration call. Multiple enum-sets
    /// derived from the same universe share this Rc, so set ops
    /// can fast-path on Rc::ptr_eq.
    pub universe: Rc<Vec<Symbol>>,
    /// Bitset over universe positions. u64 covers up to 64-symbol
    /// universes; bigger universes upgrade to a Vec<u64>.
    pub bits: u64,
}
```

R6RS §13 requires that union / intersection / difference operations on enum-sets from the same universe produce an enum-set on that universe. `Rc::ptr_eq(self.universe, other.universe)` is the validation.

`enum-set-projection` is the cross-universe op: project an enum-set's members into a different universe. Symbols not in the target universe are dropped.

## Iter 3 (FR-2 procedural records) sketch

The syntactic `define-record-type` already builds the underlying machinery — see `crates/cs-runtime/src/builtins/mod.rs` `define-record-type` handling. The procedural API is a wrapper:

- `make-record-type-descriptor` returns the same RTD value the syntactic form builds.
- `record-predicate` / `-accessor` / `-mutator` are accessors over an RTD.

Should be a small iter (maybe 1-2 days) since the underlying types exist.

## Iter 4+ (FR-3 conditions, FR-4 libraries, FR-5 ports, FR-6 programs)

Not yet sketched in this design. Each will get a per-iter design note when picked up.

## Testing strategy

Per subsystem:

1. Test file in `tests/conformance/foundation/<name>.scm` written *first* (NFR-3 from requirements).
2. Pre-implementation: file errors at the unbound builtin; harness records 0 passes.
3. Implementation iter: builtin + test wiring. Pass count flips to N.
4. Post-implementation: vm_conformance.rs + cli conformance.rs both pick up the file via `assert_eq!(walker_pass, vm_pass)`.

## Risks

- **Scope creep**: M9 lists many subsystems. Iters should ship one subsystem at a time, exit-as-you-go. Resist the urge to "audit everything before implementing anything."
- **R6RS vs R7RS divergence**: `(rnrs enums)` (R6RS) doesn't exist in R7RS. Some users want R7RS-only behavior. Document the dual conformance and let the user pick the import.
- **Library form breadth**: full R6RS library system (versions, phases) is a substantial project on its own. M9 may end up scoped to "what's needed for the stdlib subsystems we ship" rather than "complete library system." Document as a sub-deferred item.
