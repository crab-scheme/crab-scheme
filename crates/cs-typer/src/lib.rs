//! Optional / gradual type checker for CrabScheme.
//!
//! Inspired by Typed Racket (Tobin-Hochstadt + Felleisen, POPL'08):
//! migratory per-function opt-in via `: Type` annotations, Local
//! Type Inference (Pierce-Turner) for bidirectional checking,
//! union types, occurrence typing, and erasure at runtime.
//!
//! Annotations are optional on every `define` / `lambda` /
//! `letrec` binding. Unannotated code typechecks vacuously (all
//! types are `Any`); annotated code gets static checking at
//! compile time and feeds the JIT / AOT pipelines as
//! `param_type_hints`.
//!
//! The crate is **additive**: an unannotated program parses,
//! expands, compiles, and runs exactly as before this crate
//! existed. Type checking only triggers when at least one
//! annotation is present in the expanded `CoreExpr`.
//!
//! Phased per `docs/milestones/typer-plan.md`:
//!
//! - Phase 1 (this iter): Annotation syntax + parser skeleton.
//! - Phase 2: Bidirectional checking — atomic types.
//! - Phase 3: Union types + procedure types.
//! - Phase 4: Occurrence typing.
//! - Phase 5: JIT / AOT integration.
//! - Phase 6: Polish + CLI surface.
//! - Phase 7 (optional): Polymorphism.

pub mod annotate;
pub mod extract;
pub mod parse_ann;
pub mod types;

pub use annotate::{
    AnnotationTable, LambdaAnnotation, LetrecAnnotation, TopLevelAnnotation, TypeAlias,
};
pub use extract::extract_annotations;
pub use parse_ann::{parse_type_ann, TypeAnn, TypeAnnError, TypeDatum};
pub use types::{ProcType, Type};
