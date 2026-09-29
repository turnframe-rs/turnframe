//! `turnframe-macros` — reserved procedural-macro crate for Turnframe.
//!
//! No macros are shipped in the 0.1 series. Turnframe deliberately avoids a
//! proc-macro DSL until at least three real domain implementations have proven
//! the exact boilerplate worth removing (see ADR-011 and the implementation
//! rules in the architecture guide). The crate exists so the name stays
//! reserved in the `turnframe-*` family and so downstream manifests can
//! already depend on it without churn once macros land.
#![forbid(unsafe_code)]
