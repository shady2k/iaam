//! Tests that **must not compile** (§15.1).
//!
//! The first validation layer consists of types that make the error unrepresentable. Without
//! this test, the layer relies on trust alone: a commented-out line
//! in a regular test verifies nothing because it is never compiled.
//!
//! The expected compiler output is in the adjacent `.stderr` file. When the
//! toolchain version changes, diagnostic text changes; update it with
//! `TRYBUILD=overwrite cargo test -p iaam-core --test ui` and **read
//! the diffs**: a change like “the error is gone” means that the safeguard
//! has disappeared, not that the test is outdated.

#[test]
fn errors_that_must_not_compile() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
