//! `cs-lsp` — Language Server Protocol server for CrabScheme.
//!
//! Gives editors (VS Code, Neovim, Emacs, Helix) a uniform interface
//! for diagnostics, hover, go-to-definition, completion, and document
//! symbols on `.scm` files by reusing CrabScheme's existing front-end
//! (cs-parse / cs-expand / cs-diag) — no re-implementation of parsing
//! or macro expansion, and no codegen dependency.
//!
//! See `docs/milestones/lsp-server-plan.md` for the full six-phase
//! plan. This is Phase 1: the JSON-RPC skeleton. Subsequent iters add
//! real-time parse/expand diagnostics, then hover, go-to-def,
//! completion, and editor integrations.
//!
//! The server binary is `crabscheme-lsp` (`src/bin/crabscheme-lsp.rs`),
//! a stdio JSON-RPC loop built on `tower-lsp`.

pub mod builtins;
pub mod diagnostics;
pub mod hover;
pub mod references;
pub mod server;
pub mod symbols;
pub mod text;

pub use server::Backend;
