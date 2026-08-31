# T3: an import attempt that refused rows cannot confirm what it refused

Bead: iaam-dbvu (core half). Epic: iaam-40vm.

## The defect

`sync_broker` returns `assertions: 0` whenever any row of the response was
quarantined, on the reasoning that a rejected row proves the response is not a
complete export. That is true about journal coverage and false about the
portfolio assertion, which is fetched independently. One nano-precision
commission therefore costs the account every control assertion, and the
channel provides no independent confirmation at all — the very thing §10.3
has it for.

## What was rejected, and why it matters here

The first answer was an interval-wide confidence cap. A codex review
overturned it on three grounds, all verified:

- **It is bypassable.** `raise` is monotonic `max`
  (`crates/iaam-core/src/reconciliation/mod.rs:614-627`), and both
  `merge_status` (`:629`) and `with_external_evidence` (`:267`) apply after
  `build_status`. A ceiling set during construction does not survive.
- **It has no recovery.** The journal is append-only. A fact meaning "this
  interval had refused rows" would poison the interval for ever, including
  after a later import accepts every row.
- **It asserts something false.** A refused row does not prove the journal is
  incomplete: the same operation may already be present from a broker report.

So the fact must disqualify **the attempt that refused the rows**, not the
interval. A later clean import, a depositary statement or a tax certificate
must still be able to confirm the same interval, and they will, because they
are different groups carrying no gap.

## The fact

A new `EventKind` variant. `docs/irreversible-core.md` permits new variants
freely; adding a field to `ControlAssertion` would be a journal migration.

```rust
/// An import attempt refused rows, so it cannot confirm on its own the
/// dimensions those rows would have moved.
///
/// It is not a statement about the interval: the same operations may already
/// be in the journal from another channel, and a later attempt that refuses
/// nothing carries no gap. It is a statement about this attempt.
ImportCoverageGap {
    period: AssertionPeriod,
    /// What this attempt cannot confirm. Never empty — a gap that taints
    /// nothing is not a fact.
    dimensions: BTreeSet<Dimension>,
    /// How many rows were refused. Carried for the owner, not for the rule.
    refused: u32,
},
```

`Dimension` already derives `Serialize`/`Deserialize`
(`crates/iaam-core/src/reconciliation/mod.rs:32`).

Everything a new variant must satisfy:

- `discriminant()` → `"import_coverage_gap"` (`event/kind.rs:264`, beside `"control_assertion"`).
- `flow_endpoints()` → the same no-money answer `ControlAssertion` gives
  (`event/kind.rs:291`).
- `validate_structure()` gains an arm requiring a well-formed period, a
  **non-empty** dimension set, and exactly zero legs — the shape
  `ControlAssertion` is validated against (`event/mod.rs:270` and `validate_control_assertion` at `:684`).
- `SCHEMA_VERSION` becomes 7, with its history line continued
  (`event/mod.rs:152-164`).
- The serde round-trip test over every variant
  (`crates/iaam-core/tests/serde_roundtrip.rs`) covers it.

Every exhaustive `match` on `EventKind` must be updated — in the projections,
in classification, in the store. Find them with the type gate, not by eye.

## The rule, and the correlation it depends on

**Read this before writing the correlation, because the obvious version does
not work.** `collect_groups` builds a group's channel with
`document: Some(event.provenance.raw_hash())` (`mod.rs:353-357`), and
`assertion_event` gives every claim its own synthetic hash derived from an
identity string that contains the claim itself
(`crates/iaam-app/src/scenarios/sync.rs`). So each API claim is already its
own singleton group, and two claims of one synchronisation never share a
`SourceChannel`. Matching a gap to a group by `SourceChannel` equality would
match nothing, silently, and every test asserting "the gap applies" would have
to be written to pass anyway.

The correlation is therefore **(account, period, source, parser version)** —
the channel identity without the document. State that in a comment where it is
written; it is exactly the kind of thing a later reader would "simplify" back
into a `SourceChannel` comparison.

The rule itself: a dimension named by a gap matching a group is removed from
what that group can contribute as evidence. The forcing function is the
signature — `confirmed_dimensions(outcomes)` (`mod.rs:559`) becomes
`confirmed_dimensions(outcomes, tainted)`, and the compiler names every
caller. Apply the subtraction at each of them; do not add it at only the one
you happened to read.

The subtraction removes the dimension from **every** ground that group
contributes to, not only from the independent ones. A refused cash row means
our cash projection may be missing an operation, and a group whose own data is
incomplete is not better evidence merely because the ground it feeds is a
weaker one.

What the rule must **not** do:

- Cap, clamp or lower a `DimensionStatus`. Nothing in `build_status`,
  `merge_status`, `raise` or `with_external_evidence` changes.
- Turn a `ClaimOutcome` into `NotComparable`. The claim **is** comparable, and
  saying otherwise would be false: the outcome stays `Matched` or
  `Discrepant`, truthfully, and only the evidence drawn from it is withheld.
- Consult anything outside the journal. `ReconciliationLedger::build` is a
  pure function of the events it is given (§3.1) and stays one.

## Acceptance

Proven on a seeded journal in `iaam-core`, without the application layer:

1. Two independent channels whose cash claims both match raise cash to
   `AcceptedIndependent` — the existing behaviour, unchanged, as the control.
2. The same journal plus an `ImportCoverageGap` naming `Cash` for one
   channel's account, period, source and parser version: cash is **not**
   raised to `AcceptedIndependent`.
3. In that same journal, `Positions` — a dimension the gap does not name —
   is still raised. This is the point of the whole task and it must fail if
   the subtraction is written as "any gap taints everything".
4. A gap whose source or parser version differs from the group's leaves that
   group's evidence intact: the gap belongs to an attempt, not to an interval.
5. A later group with no gap raises the same dimension over the same interval,
   proving there is no permanent poisoning.
6. A `Discrepant` outcome stays `Discrepant` and a `Matched` outcome stays
   `Matched` in the presence of a gap: outcomes are untouched.
7. An `ImportCoverageGap` with an empty dimension set is refused by
   `validate_structure`, and one with a leg is refused too.

## Out of scope

The channel half: classifying a quarantine reason into dimensions, recording
the fact, and removing the early return in `sync_broker`. That is T4 of this
epic (bead iaam-ep05) and it is what makes the rule observable end to end. Until T4 lands, no
`ImportCoverageGap` is ever written, and the `assertions: 0` early return
stays exactly as it is.
