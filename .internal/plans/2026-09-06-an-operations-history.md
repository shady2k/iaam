# An operation's history — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** The owner can ask about one operation and get its life — what it was when it arrived, every change since with the date and what changed, and what it is now — and a row settled by his own answer that also minted a rule stops reading as a row no rule ever touched.

**Architecture:** Seven tasks in three layers. Task 1 is `iaam-core` alone: a third value on `RuleSettlement`. Tasks 2–4 carry that value from the answer to the fact and into the readers that already ask "which rule is behind this row" (`iaam-store`, `iaam-app`). Tasks 5–7 are the new surface: a store walk over the correction chain, a scenario that turns it into acts, and the route with its contract test.

**Tech Stack:** Rust 2024 (`rustc 1.98.0`), `rusqlite` with numbered SQL migrations under `crates/iaam-store/migrations/`, `axum` + `utoipa` 5 for the contract, `serde_json` assertions in `crates/iaam-server/tests/contract.rs`, `nix develop` for every command.

**Spec:** `.internal/specs/2026-09-06-an-operations-history-design.md`

## Global Constraints

- **No owner data, ever.** Fixtures are invented from scratch (`Main`, `Savings`, `Shop One`). No amount written with a thousands separator and a decimal comma. `./scripts/check-no-personal-data.sh` must pass on every commit.
- Everything written is English — identifiers, test names, doc comments, `#[error(...)]` texts, API descriptions — per `CLAUDE.md`. Domain terms come from `docs/glossary-ru-en.md`.
- **A doc comment on a DTO field is the published schema description.** A new field with no doc comment is a defect, not a nit. A `#[serde(flatten)]` field publishes nothing, so nothing may be documented only there.
- **The journal is append-only.** Nothing in this plan rewrites a fact, and nothing back-fills. New columns are nullable and `#[serde(default)]` applies to every additive payload field.
- Every command runs inside `nix develop`; the gate is `make check`. **A subagent never runs `make check`** — the controller runs it. A subagent runs only the specific tests its task names.
- Work on the branch `wave-ai/an-operations-history`, not on `main`. One commit per task, the bead id in the subject.

---

### Task 1: A third thing a rule can have done to a row

**Files:**
- Modify: `crates/iaam-core/src/event/provenance.rs` (`RuleSettlement`, ~line 52)
- Test: the same file's `mod tests`

**Interfaces:**
- Produces: `RuleSettlement::AnsweredMintingRule { rule: ClassificationRuleId, version: u32 }`, wire code `answered_minting_rule`. `RuleSettlement::rule()` returns `Some((rule, version))` for it. Tasks 3, 4 and 6 read both of these and add no second spelling.

**Why this shape.** The enumeration already says that its two values plus its own absence are three states. This adds a fourth, and the four are four different claims about who decided:

- `Rule` — a rule already standing filed the row, and the owner was not asked.
- `AnsweredMintingRule` — the owner answered, and that answer also became the rule.
- `NoRule` — a reading ran, none of his rules matched, and the answer minted nothing.
- absence — nothing was recorded, which is evidence for none of the three.

`rule()` returns the rule for the new value because every caller of `rule()` is asking "which rule is behind this row", and for this value the answer is a rule. `code()` keeps them apart because every caller of `code()` is asking "who decided".

**Acceptance Criteria:**
- `code()` returns `answered_minting_rule` and no other value returns it.
- `rule()` returns the rule and version for the new value.
- The value survives a serde round trip under the existing `#[serde(tag = "settled_by")]` representation.
- A fact carrying it deserialises in a build that has it; the doc comment states plainly that a build without it cannot read one, which is why the schema version moves in Task 2.
- The type's doc comment states all four states and says that absence is never evidence of any of them.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn an_answer_that_minted_a_rule_is_neither_a_standing_rule_nor_no_rule() {
    let rule = ClassificationRuleId(uuid::Uuid::from_u128(7));
    let minted = RuleSettlement::AnsweredMintingRule { rule, version: 1 };

    assert_eq!(minted.code(), "answered_minting_rule");
    assert_ne!(minted.code(), RuleSettlement::Rule { rule, version: 1 }.code());
    assert_ne!(minted.code(), RuleSettlement::NoRule.code());
}

/// Every caller of `rule()` asks which rule is behind the row, and for this
/// value there is one: the group a single decision of his reached is exactly
/// the rows the answer settled plus the rows the rule filed afterwards.
#[test]
fn an_answer_that_minted_a_rule_names_that_rule() {
    let rule = ClassificationRuleId(uuid::Uuid::from_u128(7));
    let minted = RuleSettlement::AnsweredMintingRule { rule, version: 4 };

    assert_eq!(minted.rule(), Some((rule, 4)));
}

#[test]
fn an_answer_that_minted_a_rule_survives_the_wire() {
    let rule = ClassificationRuleId(uuid::Uuid::from_u128(7));
    let minted = RuleSettlement::AnsweredMintingRule { rule, version: 4 };

    let json = serde_json::to_value(minted).expect("serialisable");
    assert_eq!(json["settled_by"], "answered_minting_rule");
    assert_eq!(
        serde_json::from_value::<RuleSettlement>(json).expect("readable"),
        minted
    );
}
```

- [ ] **Step 2: Run them and watch them fail**

```
nix develop -c cargo test -p iaam-core provenance
```
Expected: FAIL — no variant `AnsweredMintingRule`.

- [ ] **Step 3: Add the variant**

Add to the enum, with a doc comment saying what it claims. Extend `code()` with the new arm and `rule()` to return `Some((*rule, *version))` for it. Both matches are exhaustive already, so the compiler names every other place that must decide — do not add a wildcard arm anywhere to silence it.

- [ ] **Step 4: Rewrite the type's doc comment**

It currently says "Two values, and the absence of the whole thing is a third state". Make it four, in the order of the list above, and keep the sentence that absence is never read as any of them.

- [ ] **Step 5: Run the tests, then the crate**

```
nix develop -c cargo test -p iaam-core
```
Expected: PASS.

- [ ] **Step 6: Commit**

```
git commit -m "feat(core): a row settled by the answer that minted a rule says so (iaam-XXXX)"
```

---

### Task 2: The schema moves, and the journal gains a way to look forward

**Files:**
- Create: `crates/iaam-store/migrations/0030_operation_history.sql`
- Create: `crates/iaam-store/tests/migration_0030.rs`
- Modify: `crates/iaam-core/src/event/mod.rs` (`SCHEMA_VERSION`, line 224)
- Modify: `crates/iaam-store/migrations/` registration wherever the list of migrations is declared

**Interfaces:**
- Produces: index `events_by_relation_target`; columns `import_observations.answer_rule` (TEXT) and `import_observations.answer_rule_version` (INTEGER); `SCHEMA_VERSION == 16`.

**Why the schema version moves.** Task 1 added a payload value a previous build cannot read. `0029` moved 14 → 15 for the same reason, and the migration comment there is the model to follow.

**Why the index.** `relation` points backwards only. Assembling an operation's chain means asking "which fact names this one", and today that is a full scan. Partial, on `relation_target IS NOT NULL`, because a standalone fact is the majority and is never what this index is asked about — exactly `events_by_settled_by_rule`'s reasoning one handle along.

**Why the columns are on `import_observations`.** `resolution_of` reads the answer off the observation, so that is where the minted rule has to be for the fact to carry it. `import_questions.rule` is untouched: attaching the minted rule to sibling questions would change what the assessment says about each of them, and the owner would be told once per row that a rule was written from one answer.

**Acceptance Criteria:**
- The migration applies to a database at 0029 and leaves every existing row readable.
- `SCHEMA_VERSION` is 16 and the events written by the test suite carry it.
- A test proves the new index is used for a lookup by `relation_target` (`EXPLAIN QUERY PLAN` naming the index), so the walk in Task 5 cannot silently become a scan.
- Both new columns are nullable and nothing is back-filled.

- [ ] **Step 1: Write the failing migration test**

`crates/iaam-store/tests/migration_0030.rs`, following `migration_0012.rs` for shape: open a store, insert an event with a `relation_target`, assert the index is chosen.

```rust
#[test]
fn a_lookup_by_relation_target_uses_the_index_rather_than_scanning() {
    let store = /* an open store at the current schema */;
    let plan: String = store
        .conn()
        .query_row(
            "EXPLAIN QUERY PLAN
             SELECT id FROM events WHERE owner = ?1 AND relation_target = ?2",
            params!["owner", "target"],
            |row| row.get(3),
        )
        .expect("a query plan");

    assert!(
        plan.contains("events_by_relation_target"),
        "the chain walk must not scan the journal: {plan}"
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

```
nix develop -c cargo test -p iaam-store migration_0030
```
Expected: FAIL — no such index.

- [ ] **Step 3: Write the migration**

Write `0030_operation_history.sql` with a comment block in the house style: say what each object is for and why, and say what NULL means for each new column. Register it where the other migrations are registered.

- [ ] **Step 4: Bump `SCHEMA_VERSION` to 16**

- [ ] **Step 5: Run the store crate and the core crate**

```
nix develop -c cargo test -p iaam-store -p iaam-core
```
Expected: PASS. Fix any assertion pinned to 15 — grep for `15` near `schema_version` rather than trusting a search for the constant's name.

- [ ] **Step 6: Commit**

---

### Task 3: The minted rule reaches the fact

**Files:**
- Modify: `crates/iaam-store/src/import_session.rs` (a new write, beside `attach_import_question_rule` at ~line 604; the observation read that fills `ImportObservationView`)
- Modify: `crates/iaam-app/src/ports.rs` (`ImportObservationView` ~line 1307; the port method and its default)
- Modify: `crates/iaam-app/src/scenarios/import_session.rs` (`FactBasis` ~line 5443, `FactBasis::rule_settlement`, `resolution_of` ~line 8315, the minting block ~line 1673)
- Test: `crates/iaam-app/src/scenarios/import_session.rs` `mod tests`, and `crates/iaam-app/tests/classification_rules.rs`

**Interfaces:**
- Consumes: `RuleSettlement::AnsweredMintingRule` (Task 1); the columns from Task 2.
- Produces:
  - `pub struct AnswerRule { pub rule: ClassificationRuleId, pub version: u32 }` in `crates/iaam-app/src/ports.rs`, and `ImportObservationView.answer_rule: Option<AnswerRule>`.
  - Port method `async fn attach_import_answer_rule(&self, owner: OwnerId, session: ImportSessionId, rows: &[u32], rule: ClassificationRuleId, version: u32) -> Result<(), AppError>`.
  - `FactBasis::AnsweredMintingRule { rule: ClassificationRuleId, version: u32 }`.

**Why a typed pair and not two `Option`s.** `iaam-r0qk` is the precedent: a rule held as anything a caller can get half-right creates a state with no truthful answer. A rule without its version, or a version without its rule, is that state; the pair makes it unrepresentable.

**Why the reach's rows too.** The whole value of the change is that the group one answer settled is findable. Stamping only the row the caller addressed leaves the group split in two and the defect half-fixed in a way that reads as fixed.

**Acceptance Criteria:**
- Answering with a reach of `EveryLikeRowInThisSession` where the answer generalises stamps the minted rule and its version on the addressed row and on every row the reach settled.
- Committing such a session writes `RuleSettlement::AnsweredMintingRule` on every one of those facts, naming the rule and the version that was minted — not the version the rule has at commit.
- An answer that mints no rule (an agent's answer that may not generalise; an answer whose shape does not generalise) still commits as `NoRule`, and no row carries a rule it did not get.
- `FactBasis::Answered` remains for exactly that case, and the assessment's published vocabulary gains the new basis with a description.
- `import_questions.rule` is written exactly where it was written before.

- [ ] **Step 1: Write the failing test**

In `crates/iaam-app/tests/classification_rules.rs` — the owner's own case, end to end through the app layer:

```rust
/// He answers once for a group. The answer becomes a rule, and the rows the
/// answer itself reached must belong to that rule's group — otherwise «show me
/// what this decision did» answers with everything except the rows he decided.
#[tokio::test]
async fn the_rows_one_answer_settled_belong_to_the_rule_that_answer_minted() {
    let harness = /* a session with three alike classification questions */;
    let minted = answer_with_reach_over_the_session(&harness).await;
    let facts = commit(&harness).await;

    assert_eq!(facts.len(), 3);
    for fact in &facts {
        assert_eq!(
            fact.provenance.rule_settlement(),
            Some(&RuleSettlement::AnsweredMintingRule {
                rule: minted.rule,
                version: minted.version,
            }),
            "every row the one answer settled names the rule it minted"
        );
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

```
nix develop -c cargo test -p iaam-app the_rows_one_answer_settled
```
Expected: FAIL — the facts carry `NoRule`.

- [ ] **Step 3: Add `AnswerRule` and the port method**

Add the struct and the field to `ImportObservationView`; add `attach_import_answer_rule` to the store trait with a default that returns `AppError::NotConfigured`, matching how the other rule methods do it; implement it in the SQLite adapter and in the in-memory fake.

- [ ] **Step 4: Write the rule onto the observations at minting time**

In the minting block (~line 1673), after `create_rule` returns, call `attach_import_answer_rule` with the addressed row and every target row. The rows are already in hand: the addressed question's row and `targets`.

- [ ] **Step 5: Extend `FactBasis` and `resolution_of`**

Add `FactBasis::AnsweredMintingRule { rule, version }`. In `resolution_of`, the arm that reads `observation.answer` chooses between it and `Answered` on whether the observation carries an `answer_rule`. Extend `FactBasis::rule_settlement` with the arm that returns the new `RuleSettlement`. Both matches stay exhaustive.

- [ ] **Step 6: Document the new basis wherever the vocabulary is published**

`FactBasis` is published by the import assessment. The new value needs a wire word and a description that says what it claims, in the same register as its neighbours.

- [ ] **Step 7: Run the app crate**

```
nix develop -c cargo test -p iaam-app
```
Expected: PASS.

- [ ] **Step 8: Commit**

---

### Task 4: The readers that ask "which rule is behind this row"

**Files:**
- Modify: `crates/iaam-app/src/scenarios/correction.rs` (`standing_rule_for` ~line 300, and the `still_filed` count)
- Test: the same file's `mod tests`, and `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: `RuleSettlement::rule()` (Task 1).

**Why this is small.** `standing_rule_for` matches on `RuleSettlement::Rule` explicitly (`crates/iaam-app/src/scenarios/correction.rs:304`) rather than going through `rule()`, so Task 1 does not reach it on its own. The same is true of the `still_filed` count at line 277. Both are asking "which rule is behind this row", which is exactly what `rule()` answers, so both route through it and the explicit match on one variant goes away.

**Acceptance Criteria:**
- Correcting any row of the group an answer reached names the rule that answer minted, its version, and how many rows it still files.
- `still_filed` counts rows under both values — the rows the answer settled and the rows the rule filed afterwards — because retiring the rule is by rule.
- A row whose fact records `NoRule`, and a row whose fact records nothing, both still name no rule, and the two stay different states.
- A contract test proves it over the route, not only in the unit.

- [ ] **Step 1: Write the failing tests**

In `correction.rs` `mod tests`, beside `a_corrected_row_a_rule_filed_names_the_rule_and_what_it_still_files`:

```rust
/// His own case, and the one the previous wave left uncovered: the row he
/// answered is part of the group, so correcting it must name the standing
/// decision exactly as correcting a row the rule filed later does.
#[test]
fn a_corrected_row_his_answer_minted_a_rule_from_names_that_rule() {
    let rule = ClassificationRuleId(uuid::Uuid::from_u128(11));
    let corrected = settled_by(1, RuleSettlement::AnsweredMintingRule { rule, version: 1 });
    let filed_later = filed_by_rule(2, rule, 1);
    let journal = vec![corrected.clone(), filed_later];
    let effective = resolve(&journal).expect("a journal that resolves");

    let standing = standing_rule_for(&corrected, &effective, &BTreeSet::from([corrected.id]))
        .expect("the rule his answer minted is still standing");

    assert_eq!(standing.rule, rule);
    assert_eq!(standing.still_filed, 1, "the row the rule filed afterwards");
}
```

- [ ] **Step 2: Run and watch it fail**

```
nix develop -c cargo test -p iaam-app correction
```

- [ ] **Step 3: Route both readers through `rule()`**

- [ ] **Step 4: Add the contract test**

In `crates/iaam-server/tests/contract.rs`, prove `GET /v1/journal/events?settled_by_rule=R` returns both kinds of row and that each says which kind it is, per spec criterion 6.

- [ ] **Step 5: Run the app and server crates; commit**

---

### Task 5: The store can walk a correction chain

**Files:**
- Modify: `crates/iaam-store/src/events.rs`
- Modify: `crates/iaam-app/src/ports.rs` (the port method, the fake)
- Test: `crates/iaam-store/tests/`

**Interfaces:**
- Produces:
  ```rust
  /// One fact of a chain, with the moment this instance wrote it down.
  pub struct RecordedEvent {
      pub event: iaam_core::event::Event,
      /// RFC 3339, from the store's own clock at insert. Not the effective date
      /// and never published as one.
      pub recorded_at: String,
  }

  async fn event_chain(
      &self,
      owner: OwnerId,
      event: EventId,
  ) -> Result<Vec<RecordedEvent>, AppError>;
  ```
  Ordered oldest first. Empty where the owner has no such event.

**Why `recorded_at` does not go on `Event`.** It is a fact about when this instance wrote something down, not part of the fact that was written. The core type stays what the journal holds; the store row type carries the store's own observation.

**Why walk in both directions.** Any identifier in the chain must return the same history — the original, the current state, or a reversal written along the way. Walk backwards by `relation_target` to the head, then forwards by the new index to the tail.

**Acceptance Criteria:**
- Entering by the original, by the current replacement, or by a reversal returns the same sequence.
- An untouched fact returns a chain of one.
- A chain of N corrections is `O(N)` index lookups and never scans the journal.
- A cycle — which the core refuses to write but a corrupt database could hold — terminates rather than looping, and the refusal says so.
- An event belonging to another owner is not found.

- [ ] **Step 1: Write the failing tests** (the five criteria above, one test each)
- [ ] **Step 2: Run and watch them fail**
- [ ] **Step 3: Implement the walk**
- [ ] **Step 4: Run the store crate; commit**

---

### Task 6: The scenario turns a chain into acts

**Files:**
- Modify: `crates/iaam-app/src/scenarios/journal.rs`
- Test: the same file's `mod tests`

**Interfaces:**
- Consumes: `event_chain` (Task 5), `JournalEventView` and `journal_event_view` (existing).
- Produces:
  ```rust
  pub enum HistoryAct { Arrived, Corrected, Retracted }

  pub enum ChangedAspect { Kind, Amount, Account, Dates, Counterparty, Confidence }

  pub struct HistoryStep {
      pub act: HistoryAct,
      /// When the act was recorded. Not the effective date.
      pub at: String,
      /// The fact as it stands after this act. Absent after a retraction,
      /// because nothing stands after one.
      pub state: Option<JournalEventView>,
      /// Which aspects this act made different from the previous state. Empty
      /// on `Arrived`, which changed nothing, and after a retraction, which
      /// left no state to compare.
      pub changed: Vec<ChangedAspect>,
      pub reversal: Option<EventId>,
      pub replacement: Option<EventId>,
  }

  pub struct OperationHistory {
      pub steps: Vec<HistoryStep>,
      /// The fact that counts now; absent where the operation ends retracted.
      pub current: Option<EventId>,
  }

  pub async fn read_operation_history(
      store: &dyn Store,
      owner: OwnerId,
      event: EventId,
  ) -> Result<OperationHistory, AppError>;
  ```

**Why acts and not facts.** One correction is a reversal and a replacement. Publishing them raw shows him two entries for one thing he did and makes him reconstruct the pairing. The pairing is unambiguous — both name the same target, and `resolve` refuses a second replacement of an event — so this does it once, here.

**What `changed` is not.** It is not a rendered before-and-after: both states are published in full, and rendering the difference as well would be a second answer that comes to disagree with the first. It is the names of the aspects that differ, computed from the two published states.

**The category is not an aspect, on purpose.** A category is not on the fact; it is decided by the owner's category rules when a report is computed. "I filed this under the wrong category" is therefore not a correction of an operation and this history will not show it. The `ChangedAspect` doc comment must say so, because an empty `changed` that quietly means "we do not record that" is worse than the gap.

**Acceptance Criteria:**
- Spec criteria 1–4, each as a test.
- A reversal written by a whole-import retraction reads as `Retracted` exactly as a single-row reversal does.
- `AppError::NotFound` for an event of another owner, matching what the listing gives an idempotency key that addresses nothing.

- [ ] **Step 1: Write the failing tests** (one per acceptance criterion)
- [ ] **Step 2: Run and watch them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run the app crate; commit**

---

### Task 7: The route

**Files:**
- Modify: `crates/iaam-server/src/routes.rs` (beside `list_journal_events` ~line 5175)
- Modify: `crates/iaam-server/src/dto.rs`
- Test: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: `read_operation_history` (Task 6).
- Produces: `GET /v1/journal/events/{event}/history` → `OperationHistoryDto`.

**Acceptance Criteria:**
- 200 with steps oldest first; 404 for an identifier that addresses no event of his; 422 for one that cannot be read.
- No pagination, and the doc comment says why: a page boundary in the middle of a history hides the thing it exists to show.
- Every field of every new DTO carries a doc comment, because that comment is the published description. No new field is behind `#[serde(flatten)]`.
- A contract test walks the whole owner's-case story: a group answered once, one row corrected, the history of that row read back naming the standing rule, the date, and what changed.

- [ ] **Step 1: Write the failing contract test**
- [ ] **Step 2: Run and watch it fail**
- [ ] **Step 3: Add the DTOs and the handler; register the route and the schema**
- [ ] **Step 4: Run the server crate; commit**

---

## Closing the wave

After Task 7: the controller runs `make check`, dispatches one read-only review over the whole branch, fixes what it blocks on, and merges.

Two follow-ups this plan deliberately does not do, and which become beads:

- The history does not merge two operations a transfer pairing decided are two halves of one movement. That is a different relation from replacement.
- `docs/decisions/0028` §4 says the opposite of the code and was never amended (carried over from the previous wave).
