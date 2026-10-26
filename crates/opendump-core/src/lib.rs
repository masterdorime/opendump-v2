//! Deterministic classification core for opendump.
//!
//! This crate is a deliberate re-implementation of the TypeScript classifier in
//! `opendump` v0.5.0, not a port of the UI. It carries the same safety
//! invariants and should keep the same test expectations:
//!
//! 1. Matchers on one rule are **ANDed**.
//! 2. A candidate reports **exclusive** bytes — its own size minus nested
//!    candidate groups — so totals never double-count.
//! 3. Protection is **monotonic downward**: a `KEEP` or `LOCKED` folder protects
//!    everything beneath it, and such a parent is reported as blocked rather
//!    than removable.
//! 4. `display` rules colour a map, protect nothing, and are never candidates.

pub mod rules;
pub mod scan;
pub mod tokens;
pub mod types;

pub use rules::{compile, find_candidates, match_rule, Candidate, CompiledRule};
pub use scan::{scan, ScanReport};
pub use tokens::{default_tokens, expand, TokenMap};
pub use types::{DirEntry, Rule, RuleMatch, Tier};
