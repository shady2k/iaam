//! Independent reference implementations for tests (§15.4).
//!
//! This crate exists so the check that “two methods produce the same result”
//! does not collapse into a tautology. It is **not** a dependency
//! of any production crate.

pub mod lots_reference;
