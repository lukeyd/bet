//! `midir` — bet's mid-level IR (types, builder, textual `.mir` format, validator).
//!
//! Contract crate ★ (`bootstrap-plan.md §1a`). The frontend lowers surface `bet` to this
//! IR; the backend lowers this IR to LLVM. `midir` also hosts the bet-specific passes
//! (arena lifetime, `holla` generation-check hoisting/fusion, SoA crib layout) — though
//! those land later; Step 1a delivers the **representation** plus a builder, a textual
//! `.mir` format (parser + printer), and a validator.
//!
//! Shape is MIR-style: typed locals, places, basic blocks, and terminators, with no phi
//! nodes (see [`ir`]). `spec/midir.md` is rationale; **this crate is normative.**

pub mod build;
pub mod ir;
pub mod text;
pub mod validate;

pub use build::FuncBuilder;
pub use ir::*;
pub use text::{ParseError, parse, print};
pub use validate::{ValidationError, validate};
