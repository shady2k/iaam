# One question described once — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** The queue and the session assessment stop asserting what the instance computes, a movement printed twice is one piece of work with one question, a leg the document holds no counterpart for says so, the T-Bank profile reads the columns that already hold the owner's answers and refuses a row the source did not complete, and the skill's machinery rule becomes a test rather than a list.

**Architecture:** Six tasks in three areas. Tasks 1–3 are the queue and the assessment (`crates/iaam-app`) and share one new derivation. Tasks 4–5 are the import profile (`crates/iaam-ingest`) — one needs a schema field, the other only configures a mechanism that already exists. Task 6 is `docs/agent-skill/SKILL.md` alone.

**Tech Stack:** Rust 2024 (`rustc 1.98.0`), `axum` + `utoipa` for the contract, `serde_json` assertions in `crates/iaam-server/tests/contract.rs`, JSON profiles under `crates/iaam-ingest/profiles/`, `nix develop` for every command.

**Spec:** `.internal/specs/2026-09-05-one-question-described-once-design.md`
**Brainstorming bead:** `iaam-1wg6`

## Global Constraints

- **No owner data, ever.** This work came out of a live session against the owner's own instance. No account title, counterparty, merchant, amount, card number, date or session identifier from it appears in code, tests, fixtures, bead text or commit messages. Fixtures are invented from scratch (`Main`, `Savings`, `Shop One`). `./scripts/check-no-personal-data.sh` must pass on every commit.
- **Column names are the source's schema and stay as the source prints them** — `Ваша категория`, `MCC`, `Статус`. Everything else written is English, per `CLAUDE.md`.
- `scripts/check-agent-skill.sh` refuses a route path, an HTTP method written as an instruction, a status code and a payload field name in every markdown file under `docs/agent-skill/`.
- Every command runs inside `nix develop`; the gate is `make check`.
- Work on a branch, not on `main`. One commit per task, the bead id in the subject.

---

### Task 1: One derivation behind both sentences

**Files:**
- Modify: `crates/iaam-app/src/scenarios/import_session.rs` (the new derivation, beside `Generalisation`; the group proposal's consequence at ~line 6846)
- Modify: `crates/iaam-app/src/actions.rs` (the item reason at ~line 3541)
- Test: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Produces: `pub enum GeneralisationProspect { WillStand, NeedsHisAdoption, NoneFromThisRow }` and `pub fn generalisation_ahead(subject: Option<&ClassificationSubject>, may_generalise: bool) -> GeneralisationProspect`, both in `import_session.rs`. Tasks 2 and 3 relay its sentence and do not re-derive it.

**Why this is small.** The queue already holds what it needs and ignores it. `ClassificationQuestion` (`crates/iaam-app/src/actions.rs:2160`) carries `generalisation` and `subject`, and its own doc comment states that `subject: None` is «the same row the generalisation calls `Impossible`». `Generalisation` itself cannot answer here — an unanswered question is `Unanswered` by construction — so the prospect is derived from the two facts that *are* known before an answer: whether the row can ground a matcher at all, and whether the caller may generalise.

**Acceptance Criteria:**
- Neither the queue's item reason nor the group proposal's consequence states unconditionally whether a rule is kept.
- The queue's reason says, for this caller: the answer will stand as a rule; or it settles the row and the rule is the owner's to adopt; or no rule can be built from this row.
- The group proposal keeps saying how many of this session's rows one answer settles, and stops answering the rule question in the same sentence.
- A contract test proves the two surfaces agree for one question under one authority.
- `make check` passes.

- [ ] **Step 1: Write the failing test**

In `crates/iaam-server/tests/contract.rs`:

```rust
/// Two surfaces described one act with opposite persistence, and a caller that
/// read both had to pick one — which is how an owner came to be told a rule
/// would not exist that would exist.
#[tokio::test]
async fn the_queue_and_the_assessment_agree_on_what_an_answer_keeps() {
    let harness = harness();
    let session = a_session_with_one_classification_question(&harness).await;

    let (status, queue) = call(&harness.router, get("/v1/actions", Some(OWNER))).await;
    assert_eq!(status, StatusCode::OK);
    let reason = item_reason(&queue, "answer_classification_question");

    let (status, assessment) = call(
        &harness.router,
        get(&format!("/v1/import-sessions/{session}/assessment"), Some(OWNER)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let consequence = group_consequence(&assessment);

    // The owner's token may generalise, so both surfaces owe the same answer.
    assert!(
        reason.contains("settles by itself next time"),
        "the queue does not say the rule will stand: {reason}"
    );
    assert!(
        !consequence.contains("no standing decision is kept"),
        "the group still denies what the queue promises: {consequence}"
    );
}
```

Write `a_session_with_one_classification_question`, `item_reason` and `group_consequence` as helpers in the same file if none exist; the file's existing helpers show the shape.

- [ ] **Step 2: Run it and watch it fail**

```bash
nix develop -c cargo test -p iaam-server --test contract the_queue_and_the_assessment_agree_on_what_an_answer_keeps
```

Expected: FAIL on the second assertion — the group's consequence contains «no standing decision is kept» today.

- [ ] **Step 3: Add the derivation**

In `crates/iaam-app/src/scenarios/import_session.rs`, beside `Generalisation`:

```rust
/// What answering will decide beyond this session, before it is answered.
///
/// [`Generalisation`] is the same question asked afterwards, and it cannot
/// answer this one: an unanswered question is `Unanswered` by construction. Two
/// things are known before the answer and they are the whole of it — whether the
/// row can ground a matcher at all, and whether the caller may generalise
/// (`iaam-hnod`).
///
/// **One derivation, because two sentences.** The queue's item and the
/// assessment's group proposal both have to say this, they said opposite things
/// while each was right about one case, and a reader had no way to tell which
/// case he was in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralisationProspect {
    /// The answer is written as a rule, and a row matching it settles by itself
    /// next time.
    WillStand,
    /// The answer settles this row and writes no rule, because the answerer may
    /// not generalise. The rule is the owner's to make stand.
    NeedsHisAdoption,
    /// No rule can be built from this row under any token: a matcher that asks
    /// nothing matches nothing.
    NoneFromThisRow,
}

#[must_use]
pub fn generalisation_ahead(
    subject: Option<&ClassificationSubject>,
    may_generalise: bool,
) -> GeneralisationProspect {
    match (subject, may_generalise) {
        (None, _) => GeneralisationProspect::NoneFromThisRow,
        (Some(_), false) => GeneralisationProspect::NeedsHisAdoption,
        (Some(_), true) => GeneralisationProspect::WillStand,
    }
}
```

- [ ] **Step 4: Derive the queue's sentence**

In `crates/iaam-app/src/actions.rs`, replace the final clause of the reason built at ~line 3541 — «The answer is written as a rule, so a row matching it settles by itself next time.» — with the sentence for `generalisation_ahead(question.subject.as_ref(), may_generalise)`:

```rust
let kept = match generalisation_ahead(question.subject.as_ref(), may_generalise) {
    GeneralisationProspect::WillStand => {
        "The answer is written as a rule, so a row matching it settles by itself next time."
    }
    GeneralisationProspect::NeedsHisAdoption => {
        "The answer settles this row and writes no rule: the rule it would have been is \
         published with the answer, and making it stand is the owner's own act."
    }
    GeneralisationProspect::NoneFromThisRow => {
        "The answer settles this row and nothing else: this row carries nothing a rule \
         could match on, so no later row settles by itself because of it."
    }
};
```

**The authority does not reach the queue today, and threading it is the one signature change in this task.** `frontier` (`crates/iaam-app/src/actions.rs:2206`) takes `owner`, the store and the rule store, and nothing else; the caller's scope stops at the route, which holds it as `Principal` (`crates/iaam-server/src/routes.rs:178`). Add the authority as a parameter of `frontier` and pass it from that one call site. Do **not** read a token inside `actions.rs`: the queue's business is to say what may be called, and who is asking is the caller's fact to supply.

- [ ] **Step 5: Take the persistence clause out of the group proposal**

In `crates/iaam-app/src/scenarios/import_session.rs` at ~line 6846, the consequence currently reads «…and decides nothing outside them: no line of a later statement is settled by it and no standing decision is kept.» Keep what is the group's own fact — one answer settles these `count` lines rather than one at a time — and replace the persistence half with the sentence for the same `GeneralisationProspect`. The group's in-session reach and what the answer keeps are two facts, and the old sentence welded them.

- [ ] **Step 6: Run the test and the suite**

```bash
nix develop -c cargo test -p iaam-server --test contract
nix develop -c make check
```

- [ ] **Step 7: Commit**

```bash
git commit -m "fix(actions): what an answer keeps is computed once and said the same on both surfaces (<bead-id>)"
```

---

### Task 2: Two legs of one movement are one item

**Files:**
- Modify: `crates/iaam-app/src/actions.rs` (item construction for classification questions)
- Modify: `crates/iaam-app/src/scenarios/import_session.rs` if the pairing has to reach the queue through it
- Test: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: `generalisation_ahead` from Task 1.
- Produces: nothing later tasks depend on.

**Acceptance Criteria:**
- Where the assessment pairs two rows as one movement, the queue publishes **one** item for the pair, not two.
- That item's alternatives still include «these are two different things»: `crates/iaam-ingest/src/mirror.rs` is explicit that a pair is a hypothesis and that refusing it must stay sayable.
- The item names both rows the way a row is named to the owner — the day the source dated it and the amount it printed — never by row number alone.
- Answering it settles both legs, and the queue no longer lists the far leg.
- `make check` passes.

- [ ] **Step 1: Write the failing test**

```rust
/// One movement a document printed twice is one decision. Two items grade it as
/// two pieces of work and leave open the act that records the movement twice.
#[tokio::test]
async fn a_movement_printed_on_both_of_its_accounts_is_one_item() {
    let harness = harness();
    // A document covering two of the owner's own accounts, one movement
    // between them: a departure on one and the arrival on the other, same day,
    // same amount, opposite signs. Invented, like every fixture here.
    let session = a_session_with_one_mirrored_movement(&harness).await;

    let (status, queue) = call(&harness.router, get("/v1/actions", Some(OWNER))).await;
    assert_eq!(status, StatusCode::OK);

    let items: Vec<_> = queue["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "answer_classification_question")
        .collect();
    assert_eq!(
        items.len(),
        1,
        "the two legs of one movement are published as {} items: {items:#?}",
        items.len()
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
nix develop -c cargo test -p iaam-server --test contract a_movement_printed_on_both_of_its_accounts_is_one_item
```

Expected: FAIL with two items — `crates/iaam-app/src/actions.rs` never consults the pairing.

- [ ] **Step 3: Bring the pairing to the queue**

The pairing is computed at reading time (`crates/iaam-ingest/src/mirror.rs`) and surfaces in the assessment as a `GroupBasis::OneMovement` group with `AnswerReach::ThisRow`, the far row settled as `NoFactReason::SecondLegOfOneMovement`. The queue builds from `ClassificationQuestion`s and sees none of it.

**The seam is already open.** `frontier` loads `observations` for every session that raised a question (`crates/iaam-app/src/actions.rs:2242`) — the same observations the pairing is computed over, loaded for the same loop that builds each `ClassificationQuestion`. Read the pairing there, through the scenario function the assessment uses, and carry it on the question the way `generalisation` and `subject` are carried beside it.

Do **not** re-derive the pairing in `actions.rs`. That is this plan's own defect one level up: two derivations of one fact, on two surfaces, that eventually disagree — which is what Task 1 exists to undo.

- [ ] **Step 4: Publish one item for the pair**

The item's identity must stay stable across readings so a caller deduplicating by id does not see it move; the existing per-question id rule (`actions.rs:3530`) is the shape to follow, applied to the pair.

- [ ] **Step 5: Add the refusal test**

```rust
/// A pair is a hypothesis. Two unrelated payments of one amount on one day
/// exist, and an item that could not be refused would be worse than two items.
#[tokio::test]
async fn one_item_for_a_pair_can_still_be_answered_they_are_two_different_things() {
    let harness = harness();
    let session = a_session_with_one_mirrored_movement(&harness).await;

    let (status, queue) = call(&harness.router, get("/v1/actions", Some(OWNER))).await;
    assert_eq!(status, StatusCode::OK);

    let alternatives = pair_item_alternatives(&queue);
    assert!(
        alternatives.iter().any(|alternative| alternative == "not_one_movement"),
        "the pair cannot be refused: {alternatives:?}"
    );
}
```

Use whatever code the answering vocabulary already publishes for that refusal rather than inventing `not_one_movement`; read it off the question before writing the assertion.

- [ ] **Step 6: Run the suite**

```bash
nix develop -c make check
```

- [ ] **Step 7: Commit**

```bash
git commit -m "fix(actions): a movement printed on both its accounts is one item and one question (<bead-id>)"
```

---

### Task 3: A leg with no counterpart says so, and says why when it knows

**Files:**
- Modify: `crates/iaam-ingest/src/mirror.rs` (report the unmatched leg, not only the pairs)
- Modify: `crates/iaam-app/src/scenarios/import_session.rs` (publish it on the row's question)
- Test: `crates/iaam-ingest/src/mirror.rs` unit tests; `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: Task 2's pairing path into the queue.

**Acceptance Criteria:**
- A row with the shape of a leg and no counterpart in the document publishes that fact.
- The published sentence says **in this document**, never that the far half does not exist: it may be in another statement, or on an account the owner did not put in his group.
- Where the document covered one account, the sentence says so — a document covering one account cannot hold the far half of a movement between two, by construction.
- The question it steers to is the existing one: an own-account movement with the far side unnamed (`iaam-fmih`, decisions 0012 and 0013).
- `make check` passes.

- [ ] **Step 1: Write the failing unit test in `mirror.rs`**

```rust
/// A departure with no arrival to match is not «nothing to report». Answered as
/// an ordinary row it records a movement whose other half does not exist, or
/// files an internal move as spending.
#[test]
fn a_departure_with_no_arrival_is_reported_as_unmatched() {
    let sides = vec![departure(Day(1), Amount(100), account("main"))];

    let read = read_mirrors(&sides);

    assert!(read.pairs.is_empty());
    assert_eq!(
        read.unmatched,
        vec![departure(Day(1), Amount(100), account("main"))],
        "an unmatched leg is dropped rather than reported"
    );
}
```

Follow the module's own fixture helpers rather than these names if it has them; `a_departure_and_an_arrival_of_one_amount_on_one_day_are_one_movement` (`mirror.rs:309`) shows the shape.

- [ ] **Step 2: Run it and watch it fail**

```bash
nix develop -c cargo test -p iaam-ingest mirror
```

Expected: FAIL — `mirrored()` returns `Vec<Mirror>` and has nowhere to put an unmatched leg.

- [ ] **Step 3: Report the unmatched legs**

Return the pairs **and** the legs that found none, rather than the pairs alone. Keep `mirrored()`'s existing refusals intact: an ambiguous row pairs with nothing and is not thereby «unmatched with a reason», it is ambiguous — say which it is rather than folding the two.

- [ ] **Step 4: Publish it on the question, with the careful wording**

```rust
/// Why this row has no other half here, said as narrowly as it is known.
///
/// **«In this document» and never «nowhere».** The far half may be in a
/// statement the owner has not brought yet, or on an account he did not put in
/// his group — the second is the ordinary case and it looks exactly like the
/// first from here.
///
/// Where the document covered one account the reason is known and is worth
/// saying: a movement between two accounts prints its halves on two accounts, so
/// a document holding one of them never held the other. That is a different
/// conversation from a bare «no counterpart», because it tells him what to do.
```

- [ ] **Step 5: Add the contract test**

Assert the published sentence contains «in this document» and does not contain a claim that the far half does not exist.

- [ ] **Step 6: Run the suite**

```bash
nix develop -c make check
```

- [ ] **Step 7: Commit**

```bash
git commit -m "fix(ingest): a leg the document holds no counterpart for says so, and says why (<bead-id>)"
```

---

### Task 4: The T-Bank profile refuses a row the source did not complete

**Files:**
- Modify: `crates/iaam-ingest/profiles/tbank-operations-csv.json`
- Test: `crates/iaam-ingest/tests/tbank_profile_parity.rs`

**Why this is the smallest task in the plan.** The mechanism exists and is unused. `RowShape::status` (`crates/iaam-ingest/src/profile/mod.rs:281`) is documented «Whether the source says the row is a movement it completed. Absent, the engine reads every row as one — which is what a document that prints no such column has said», `StatusSource` is `{ column, tokens: BTreeMap<String, RowStatus> }`, and `engine.rs:555` **rejects a row whose word is not in the map**, naming the map in the rejection. So the repository holds three generic words, the profile names the source's, and an unknown value is disclosed rather than accepted. Nothing here needs new code.

**Acceptance Criteria:**
- The profile declares a `status` block over the `Статус` column, mapping the word the export prints for a completed movement to `completed`.
- A row whose status word is not in the map is refused as what the source stated, and the refusal names the map — this is existing engine behaviour and the test pins it.
- No status vocabulary is written into Rust: the profile is the only place a source's word appears.
- Fixtures are invented, not trimmed from an export.

- [ ] **Step 1: Write the failing test**

```rust
/// A row the source did not complete is not a fact about the owner's money. The
/// profile read every row as completed because it declared no status column.
#[test]
fn a_row_whose_status_is_not_the_completed_word_is_refused() {
    let profile = tbank_profile();
    let refused = read_one_row(&profile, row_with_status("НеОк"));

    let rejection = refused.expect_err("a row with an unknown status word was accepted");
    assert_eq!(rejection.field, "status");
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
nix develop -c cargo test -p iaam-ingest --test tbank_profile_parity
```

Expected: FAIL — with no `status` block the engine reads every row as completed and returns no rejection.

- [ ] **Step 3: Declare the status block**

```json
  "status": {
   "column": "Статус",
   "tokens": {
    "Ок": "completed"
   }
  },
```

Only the word we have seen is mapped. **This is deliberate and is the safe direction**: every other word the export may print is refused loudly and named, so the map grows from evidence rather than from guessing, and no row is silently taken as completed because somebody assumed what a bank prints.

- [ ] **Step 4: Run the tests**

```bash
nix develop -c cargo test -p iaam-ingest
nix develop -c make check
```

- [ ] **Step 5: Commit**

```bash
git commit -m "fix(profile): a T-Bank row states whether the movement completed, and an unknown word is refused (<bead-id>)"
```

---

### Task 5: The profile reads the owner's own category and the code that grounds a rule

**Files:**
- Modify: `crates/iaam-ingest/src/profile/mod.rs` (`RowShape`), `crates/iaam-ingest/src/profile/load.rs`, `crates/iaam-ingest/src/profile/engine.rs`
- Modify: `crates/iaam-ingest/profiles/tbank-operations-csv.json`
- Modify: wherever an observation carries `source_category`, so the two new values travel the same way
- Test: `crates/iaam-ingest/src/profile/load.rs` tests, `crates/iaam-ingest/tests/tbank_profile_parity.rs`, `crates/iaam-server/tests/contract.rs`

**Acceptance Criteria:**
- The profile transcribes `Ваша категория` — the owner's own category, already decided at his institution — and `MCC`.
- Both are **transcribed and never interpreted** (decision 0028): the profile writes down what the source claims, and a rule may ask what the source filed the row under, which is decision 0026's shape already built for `source_category`.
- A classification rule can match on either.
- The owner's own category grounds a question asked **once per distinct value**, not once per row: it is his decision in his bank's vocabulary, and what it maps to here is one question per value.
- `MCC` is empty on some rows, so nothing may require it.
- `make check` passes.

- [ ] **Step 1: Write the failing test**

```rust
/// The statement carries the owner's own category — his decision, already made —
/// and the profile ignored it, so the instance asked him for what he had already
/// told his bank.
#[test]
fn the_profile_transcribes_the_owners_own_category_and_the_code() {
    let profile = tbank_profile();
    let read = read_one_row(&profile, a_row_with_an_owner_category_and_a_code())
        .expect("the row is readable");

    assert_eq!(read.owner_category.as_deref(), Some("Invented Category"));
    assert_eq!(read.source_code.as_deref(), Some("0000"));
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
nix develop -c cargo test -p iaam-ingest the_profile_transcribes_the_owners_own_category
```

Expected: FAIL to compile — no such fields.

- [ ] **Step 3: Add the two fields to the shape and the loader**

`RowShape` gains two `Option<String>` column names beside `source_category`, loaded with the same `column_block` helper (`load.rs:248` shows the five that already share it). Name them for what they are — the owner's own category as the source recorded it, and the source's standardised code — not for the column headings, which are one institution's.

- [ ] **Step 4: Carry them through the engine and the observation**

`engine.rs:720` shows how `source_category` is transcribed; follow it exactly. Both new values travel the same way and reach the same place, so a rule can ask about them.

- [ ] **Step 5: Declare them in the T-Bank profile**

```json
  "owner_category": { "column": "Ваша категория" },
  "source_code": { "column": "MCC" }
```

- [ ] **Step 6: Make the category question one per distinct value**

The owner's category is his decision in his bank's words. What it is called here is one question per distinct value, and the answer reaches every row carrying that value — which is the reach decision 0032 and 0034 already give one answer over a set. Do not ask it per row.

- [ ] **Step 7: Run the suite**

```bash
nix develop -c make check
```

- [ ] **Step 8: Commit**

```bash
git commit -m "feat(profile): the statement's own category and code are transcribed and can ground a rule (<bead-id>)"
```

---

### Task 6: What he never hears is a test, not a list

**Files:**
- Modify: `docs/agent-skill/SKILL.md` (`## What he never hears`)

**Acceptance Criteria:**
- The section leads with a test: what is his is his money and his decisions; everything you went through to reach them is yours; if you could not say it to a person keeping his books by hand, it is machinery.
- The existing enumeration stays as **examples of the test**, not as the rule.
- It gains the four that were missing and were said to the owner in a live session: an address, the way a call is made, the name of a field or of any part of an answer, and what a credential may or may not do.
- The credential clause distinguishes mechanism from effect: what turns on it is his business and is said in his terms; how it works is not.
- The replacement is published with the prohibition — saying «I'll ask iaam» is what to say instead.
- `scripts/check-agent-skill.sh` passes: no route path, no method, no status code, no payload field name, so each is named by what it is rather than by an example of it.

- [ ] **Step 1: Rewrite the section**

```markdown
## What he never hears

What is his is his money and his decisions. Everything you went through to reach
them is yours. The test is one question: could you say this to a person who kept
his books by hand? If not, it is machinery, and machinery does not reach him.

So: not an address, and not the way a call is made; not the name of a field or of
any part of an answer; not what your credential is and is not allowed to do — what
turns on it is his business and you say that in his words, but how it works is
not; not the words an answer is sent as; not an item's state and not how urgent
it is; not an identifier of any kind; not that an import is held open, or which
one; not how many things are outstanding; not a value already filled in; not that
a field is optional — tell him instead that he can leave it and what leaving it
costs; not the numbers this project files its own decisions under; not the name
of a report in our vocabulary rather than his; and nothing whatever about the
container, the build, the schema or how you reached the instance. A client's
control flow is not a conversation.

Nor how you found out. Which state something was in, what you had to read, what
you had to try twice: that is your work. Tell him what you found, or ask him what
you could not work out.

**And what to say instead**, because a prohibition with nothing beside it is
broken by whoever still has to say something: you are asking his own instance,
and «let me ask iaam» is the whole of it. That is true, it is his word for the
thing, and it needs no part of how the asking is done.
```

- [ ] **Step 2: Run the guards**

```bash
nix develop -c ./scripts/check-agent-skill.sh
nix develop -c ./scripts/check-no-personal-data.sh
wc -l docs/agent-skill/SKILL.md
```

Expected: `Agent documents checked: 1.`, `Personal data checked.`, and a file still under 200 lines — this replaces a section rather than adding one.

- [ ] **Step 3: Commit**

```bash
git commit -m "docs(skill): the machinery rule is a test, and it publishes what to say instead (<bead-id>)"
```

---

## Ordering and parallelism

Tasks 1 → 2 → 3 are sequential: 2 consumes 1's derivation, and 3 consumes 2's pairing path. Task 4 is independent of everything. Task 5 is independent of 1–3 and touches the same crate as 4, so it follows 4 rather than running beside it. Task 6 touches one markdown file and can run at any time.

**Do not dispatch tasks that share a file in parallel.** Tasks 1–3 all touch `crates/iaam-app`; 4 and 5 both touch `crates/iaam-ingest/profiles/tbank-operations-csv.json`. The safe fan-out is one of {1,2,3}, one of {4,5}, and 6.
