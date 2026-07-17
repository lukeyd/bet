//! `lsp` — bet's language server, consuming the `frontend` crate for a single
//! unambiguous parse/typecheck source.
//!
//! Not yet implemented. This crate currently only reserves the name and the
//! `lsp -> frontend` dependency edge (see `graph-allowlist.toml`); no server,
//! protocol handling, or LSP framework has been chosen yet.

// Holds the `frontend` edge open until the server is written, without which
// `unused_crate_dependencies` would flag the dependency as rot.
use frontend as _;
