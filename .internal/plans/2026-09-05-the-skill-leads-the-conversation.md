# The skill leads the conversation — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** `docs/agent-skill/` becomes one `SKILL.md` that tells a financial assistant how to open and lead a conversation about the owner's money, and everything it used to explain about payloads is either deleted as a copy or moved onto the contract description that carries it.

**Architecture:** Three passes, in this order. First the conversation sections land in `SKILL.md`, because they are new text that deletes nothing and they are the defect the owner reported. Then, file by file, each paragraph is classified by the spec's three questions and either deleted (the response already carries it), moved onto a `utoipa` doc comment in `crates/iaam-server/src/dto.rs` (a field can carry it), or kept. Last, the four companion files are removed and a fourth refusal is added to `scripts/check-agent-skill.sh` so the document cannot narrate a payload again.

**Tech Stack:** Rust 2024 (`rustc 1.98.0`), `axum` + `utoipa` 5 for the contract, `serde_json` assertions in `crates/iaam-server/tests/contract.rs`, POSIX shell for the guards, `nix develop` for every command.

**Spec:** `.internal/specs/2026-09-05-the-skill-leads-the-conversation-design.md`
**Brainstorming bead:** `iaam-1pfo`

## Global Constraints

- Everything written is **English** — prose, doc comments, test names, commit messages. Domain terms come from `docs/glossary-ru-en.md`; a missing term is added there before it is used.
- **No owner data anywhere**, including examples. `./scripts/check-no-personal-data.sh` must pass on every commit; invented names only (`Main`, `Savings`, `Shop One`).
- `scripts/check-agent-skill.sh` refuses, in **every** markdown file under `docs/agent-skill/`: a versioned route path, an HTTP method written as an instruction, an HTTP status code. Nothing this plan writes may reintroduce one.
- Every command runs inside `nix develop`; the gate is `make check`.
- Work on a branch, not on `main`. One commit per task, the bead id in the subject.
- A paragraph is deleted **only in the same commit that lands its carrier**. Never before.

---

### Task 1: The conversation lands in `SKILL.md`

**Files:**
- Modify: `docs/agent-skill/SKILL.md` (add four sections; extend `## What the system does not do`)

**Interfaces:**
- Consumes: nothing.
- Produces: the section headings later tasks move surviving text into — `## Who you are to him`, `## How a session opens`, `## How the conversation goes`, `## What he never hears`.

**Acceptance Criteria:**
- `SKILL.md` states that the session opens on where his money stands: what can be shown now and what is missing for the rest, in terms of his money.
- It forbids offering him a choice of where to start, in as many words.
- It names, as a list, what never reaches him: an item's state or urgency, identifiers, import sessions, counts of outstanding items, this project's decision numbers, the system's own vocabulary for a report, and anything about the container, the build or the schema.
- It says the system does not plan a budget and holds no limits.
- `./scripts/check-agent-skill.sh` and `./scripts/check-no-personal-data.sh` pass.
- Nothing is deleted in this task.

- [ ] **Step 1: Add `## Who you are to him` immediately after the frontmatter's opening paragraph**

Text to write (adapt wording, keep every obligation):

```markdown
## Who you are to him

You are the assistant for one person's own money, and the two of you answer four
questions about it: what he holds, where the money went, what it earned, and
whether the books agree with what his institutions say. Those four are the whole
of what this system knows, and everything you say to him is one of them or a step
towards one.

He is not an operator of this system. He did not choose its words, he does not
know what it calls things, and he has no reason to learn: the parts, their names
and their states are yours to hold and never his to hear.
```

- [ ] **Step 2: Add `## How a session opens` after it**

```markdown
## How a session opens

Read the instance before you say anything. What it needs and what it can already
answer are computed from its own state, and both come back before you have said a
word.

Then open on his money, not on your reading of it: what you can show him now, and
what is missing for the rest — said as the money it is about, and not as the
work it is. *«Spending for August I can show you; what it earned I cannot yet —
one account's August is not sorted out.»* Where nothing is missing, open with the
short look instead: what he holds and where the money went, quoted under the
rules for quoting a figure.

**Never offer him a choice of where to start.** The instance returned what is
outstanding in the order to work it, so which thing comes first is a question it
has already answered. «Where shall we start» is a question it did not publish,
and the reason it reads as courtesy is that the work of choosing has been handed
back to the person who came here to be led.
```

- [ ] **Step 3: Add `## How the conversation goes` after it**

```markdown
## How the conversation goes

You lead. Take what is most urgent first, put one decision to him at a time, and
carry on to the next without asking his leave to continue. The questions you put
are the ones the instance published and no others: a question of your own about
the shape of the session is the failure that composing a question of your own
about his money would be, one level up.

A session ends on what is left — in his terms, and briefly. Not on a report of
what you did, which is your work and never the answer to anything he asked.
```

- [ ] **Step 4: Add `## What he never hears`, replacing the clause now inside `## What is published is what to convey`**

```markdown
## What he never hears

None of the machinery reaches him. Not an item's state and not how urgent it is;
not an identifier of any kind; not that an import is held open or which one; not
how many things are outstanding; not the words this system files its own
decisions under; not the name of a report in our vocabulary rather than his; and
nothing whatever about the container, the build, the schema or how you reached
the instance.

Nor how you found out. Which state something was in, what you had to read, what
you had to try twice: that is your work. Tell him what you found, or ask him what
you could not work out.
```

Delete the sentence «Nothing of the machinery reaches him. Not the words an
answer is sent as, not an item's state or its urgency…» from `## What is
published is what to convey; the words are yours`, and the paragraph «Do not
narrate what you did to find out» beneath it — both are now this section. This
is a move within one file, not a deletion of knowledge.

- [ ] **Step 5: Extend `## What the system does not do`**

Add as the second sentence of that section:

```markdown
It does not plan a budget and holds no limits: the four questions are about what
happened, so an assistant that offers to plan is promising what nothing here
implements.
```

- [ ] **Step 6: Re-read the frontmatter `description` against the four questions**

It is the only string a model reads to decide whether to load the skill at all,
so it has to fire on what the owner actually says about his money — «how much did
I spend», «what do I hold», «I have a statement». Check the current one names all
four questions in his words and no part of this system's vocabulary. Leave it
alone if it holds; decision 0037 already rewrote it once.

- [ ] **Step 7: Run the guards**

```bash
nix develop -c ./scripts/check-agent-skill.sh
nix develop -c ./scripts/check-no-personal-data.sh
```

Expected: `Agent documents checked: 5.` and `Personal data checked.`

- [ ] **Step 8: Commit**

```bash
git add docs/agent-skill/SKILL.md
git commit -m "docs(skill): the assistant opens on his money and leads from there (<bead-id>)"
```

---

### Task 2: The paragraph inventory

**Files:**
- Create: `.internal/specs/2026-09-05-agent-skill-inventory.md`

**Interfaces:**
- Consumes: Task 1's section headings (class-3 destinations).
- Produces: the table Tasks 3–6 work from. Columns: `file`, `heading`, `paragraph` (first six words), `class` (1, 2 or 3), `carrier` (for class 2: `Dto::field` in `crates/iaam-server/src/dto.rs`), `destination` (for class 3: a `SKILL.md` heading).

**Acceptance Criteria:**
- Every heading of all five files in `docs/agent-skill/` appears in the table.
- Every class-2 row names a struct and field that exist — checked by the script in Step 3.
- Every class-3 row names a heading that exists in `SKILL.md`.
- Class 1 rows say which field or object of the response already carries the paragraph.
- No paragraph is left unclassified. A paragraph with no carrier and no destination is written into a `## No home yet` list at the end, with what a carrier for it would have to be — it is not silently dropped.

- [ ] **Step 1: List every heading and paragraph opener**

```bash
for f in docs/agent-skill/*.md; do
  awk -v f="$f" '/^#{2,3} /{h=$0} /^[^ #|-]/&&NF{print f"\t"h"\t"substr($0,1,60)}' "$f"
done > "${TMPDIR:-/tmp}/skill-paragraphs.tsv"
wc -l "${TMPDIR:-/tmp}/skill-paragraphs.tsv"
```

- [ ] **Step 2: Classify each row and write the table**

Apply the spec's three questions in order. Two worked examples, to fix the standard:

- `importing.md` «Every alternative says what answering it does to his money-flow
  report» → **class 1**. `AnswerAlternativeDto::consequence`
  (`crates/iaam-server/src/dto.rs:1270`) is documented as «What answering this
  word does to the owner's money-flow report», always present, one sentence per
  word. The paragraph is a copy.
- `importing.md` «Name the row by the day the source dated it and the amount the
  source printed» → **class 2**. `ImportQuestionDto::row`
  (`crates/iaam-server/src/dto.rs:8912`) carries a row number and its description
  says only «The row in the session the question is about.» The obligation has a
  field and no description on it.

- [ ] **Step 3: Verify every carrier named actually exists**

```bash
awk -F'\t' '$4=="2"{print $5}' .internal/specs/2026-09-05-agent-skill-inventory.md \
  | sed 's/.*:://' | sort -u | while read -r field; do
      grep -q "pub $field" crates/iaam-server/src/dto.rs \
        || echo "MISSING CARRIER: $field"
    done
```

Expected: no output.

- [ ] **Step 4: Commit**

```bash
git add .internal/specs/2026-09-05-agent-skill-inventory.md
git commit -m "docs(spec): every paragraph of the agent skill classified against its carrier (<bead-id>)"
```

---

### Task 3: `importing.md` — carriers written, copies deleted

**Files:**
- Modify: `crates/iaam-server/src/dto.rs` (doc comments on the fields the inventory names)
- Modify: `crates/iaam-server/tests/contract.rs` (one test asserting the descriptions are published)
- Modify: `docs/agent-skill/importing.md` (delete class-1 and class-2 paragraphs)
- Modify: `docs/agent-skill/SKILL.md` (class-3 paragraphs move in)

**Interfaces:**
- Consumes: the inventory table from Task 2.
- Produces: the test helper `fn property_description<'a>(spec: &'a Value, schema: &str, property: &str) -> &'a str`, reused by Tasks 4–6.

**Acceptance Criteria:**
- `ImportQuestionDto::row` publishes that the number is what is sent back and that the owner is told the day the source dated the row and the amount it printed, with the sign it printed.
- Every other class-2 paragraph of `importing.md` has its description on the field the inventory named.
- Every class-1 paragraph is gone from `importing.md`.
- `cargo test -p iaam-server --test contract` passes.
- `./scripts/check-agent-skill.sh` passes.

- [ ] **Step 1: Write the failing test**

Add to `crates/iaam-server/tests/contract.rs`:

```rust
/// A description a caller reads instead of a document it may not have.
///
/// Read out of the published document rather than off the Rust type: what a
/// client can learn is what the document says, and an assertion against the
/// struct would pass just as happily with the description stripped.
fn property_description<'a>(
    spec: &'a serde_json::Value,
    schema: &str,
    property: &str,
) -> &'a str {
    spec["components"]["schemas"][schema]["properties"][property]["description"]
        .as_str()
        .unwrap_or_else(|| panic!("{schema}.{property} publishes no description"))
}

/// The row number is what a machine sends back, and the day and the sum are what
/// a person recognises the line by. A caller told only «row 14» reads a list to
/// the owner and counts down it, and an owner counting down a list is eventually
/// off by one — which settles the wrong row, may become a standing rule, and is
/// never asked about again.
#[tokio::test]
async fn a_question_says_how_the_row_is_named_to_the_owner() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let described = property_description(&spec, "ImportQuestionDto", "row");
    for owed in ["day", "amount", "sign"] {
        assert!(
            described.contains(owed),
            "ImportQuestionDto.row does not say the owner is told the {owed}: {described}"
        );
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
nix develop -c cargo test -p iaam-server --test contract a_question_says_how_the_row_is_named_to_the_owner
```

Expected: FAIL — the description is «The row in the session the question is about.» and contains none of the three words.

- [ ] **Step 3: Write the description**

Replace the doc comment on `ImportQuestionDto::row` (`crates/iaam-server/src/dto.rs:8912`):

```rust
    /// The row in the session the question is about, and the number to send
    /// back with the answer.
    ///
    /// **Not what the owner is told.** He recognises a line by the day the
    /// source dated it and the amount it printed, with the sign it printed, and
    /// those are in the session's own reading of the row. Several rows of one
    /// month can carry the same word, name nobody and be identical in every
    /// other respect, so an owner matching questions to rows by counting down a
    /// list is eventually off by one — and a wrong answer settles the row, may
    /// become a standing rule of his, and is never asked about again.
    pub row: u32,
```

- [ ] **Step 4: Run the test again**

```bash
nix develop -c cargo test -p iaam-server --test contract a_question_says_how_the_row_is_named_to_the_owner
```

Expected: PASS.

- [ ] **Step 5: Repeat Steps 1–4 for every other class-2 paragraph the inventory assigns to `importing.md`**

One test per carrier, named for the obligation and not for the field. Each names the words the description must contain, as above.

- [ ] **Step 6: Delete the covered paragraphs from `importing.md`**

Delete every class-1 paragraph and every class-2 paragraph whose carrier landed in Steps 3–5. Move every class-3 paragraph into the `SKILL.md` section the inventory names. `importing.md` is not deleted in this task — Task 7 removes the file once it is empty of everything but headings.

- [ ] **Step 7: Run the guards and the suite**

```bash
nix develop -c cargo test -p iaam-server --test contract
nix develop -c ./scripts/check-agent-skill.sh
nix develop -c ./scripts/check-no-personal-data.sh
```

- [ ] **Step 8: Commit**

```bash
git add crates/iaam-server/src/dto.rs crates/iaam-server/tests/contract.rs docs/agent-skill/importing.md docs/agent-skill/SKILL.md
git commit -m "feat(contract): the question says how the row is named to the owner (<bead-id>)"
```

---

### Task 4: `reading-the-reports.md` — carriers written, copies deleted

**Files:**
- Modify: `crates/iaam-server/src/dto.rs` (`ConfidenceDto`, `CaveatDto`, `CaveatSubjectDto`, `JournalConfidenceDto`, and the reference-fact DTOs the inventory names)
- Modify: `crates/iaam-server/tests/contract.rs`
- Modify: `docs/agent-skill/reading-the-reports.md`, `docs/agent-skill/SKILL.md`

**Interfaces:**
- Consumes: `property_description` from Task 3.
- Produces: nothing new.

**The cycle, for every carrier in this task:** the five steps shown in full in
Task 3 — a failing test named for the obligation and not for the field, a run
that proves it fails, the description, a run that proves it passes, the deletion
of the paragraph it replaced in the same commit. The schema and property come
from Task 2's inventory, which is why they are named per carrier below rather
than listed here.

**Acceptance Criteria:**
- What makes a confirmation independent, when a cash figure is a balance and when it is a movement from an unasserted start, the population a figure was folded over, what `whole` does not claim, and what an unconfirmed posting does and does not mean are each published on the field or object that carries the figure they qualify.
- «A fact can be quoted, a derived value cannot» survives as class 3 — an assistant that recomputes has produced no request for a description to sit on — and moves into `SKILL.md`'s arithmetic rule rather than being deleted.
- `cargo test -p iaam-server --test contract` passes.

- [ ] **Step 1: Write the failing test for the first carrier**

```rust
/// A cash figure that nothing anchors is a movement and not a balance, and a
/// caller that quotes it as «what he has» has told him something false about
/// his own money using a number the system computed correctly.
#[tokio::test]
async fn a_cash_figure_says_whether_anything_anchors_it() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let described = property_description(&spec, "ConfidenceDto", "caveats");
    assert!(
        !described.trim().is_empty(),
        "ConfidenceDto.caveats publishes no description"
    );
}
```

Refine the assertion to the exact carrier the inventory names before writing it: the schema and property above are the shape, and the inventory decides which of `ConfidenceDto`, `CaveatDto` or the report's own cash field holds this obligation.

- [ ] **Step 2: Run it and watch it fail**

```bash
nix develop -c cargo test -p iaam-server --test contract a_cash_figure_says_whether_anything_anchors_it
```

Expected: FAIL.

- [ ] **Step 3: Write the description on the carrier, in the register decision 0035 requires**

A sentence or two, saying what the reader must not conclude — never an essay. The text comes from the paragraph being replaced.

- [ ] **Step 4: Run the test again**

Expected: PASS.

- [ ] **Step 5: Repeat Steps 1–4 for every remaining class-2 paragraph of this file**

- [ ] **Step 6: Delete the covered paragraphs; move class-3 into `SKILL.md`**

- [ ] **Step 7: Run the suite and the guards**

```bash
nix develop -c cargo test -p iaam-server --test contract
nix develop -c ./scripts/check-agent-skill.sh
```

- [ ] **Step 8: Commit**

```bash
git add crates/iaam-server/src/dto.rs crates/iaam-server/tests/contract.rs docs/agent-skill/reading-the-reports.md docs/agent-skill/SKILL.md
git commit -m "feat(contract): a report's caveats carry what may not be concluded from the figure (<bead-id>)"
```

---

### Task 5: `the-money-and-the-perimeter.md` — carriers written, residue placed

**Files:**
- Modify: `crates/iaam-server/src/dto.rs` (`AccountRetirementDto`, `RecordAccountRetirementRequest`, the category and instrument DTOs the inventory names)
- Modify: `crates/iaam-server/tests/contract.rs`
- Modify: `docs/agent-skill/the-money-and-the-perimeter.md`, `docs/agent-skill/SKILL.md`
- Possibly modify: `docs/import-boundary.md` (for meaning that is neither a payload's nor an assistant's)

**Interfaces:**
- Consumes: `property_description` from Task 3.
- Produces: nothing new.

**The cycle, for every carrier in this task:** the five steps shown in full in
Task 3 — a failing test named for the obligation and not for the field, a run
that proves it fails, the description, a run that proves it passes, the deletion
of the paragraph it replaced in the same commit. The schema and property come
from Task 2's inventory, which is why they are named per carrier below rather
than listed here.

**Acceptance Criteria:**
- What a retirement changes and what it never changes, and that a retirement never hides money, are published on the retirement operation's own schemas.
- What a category cannot change, and how a string naming an account is read, are published on the schemas that refuse a bad one.
- The spec names this file as the one that will not classify uniformly: any paragraph that is meaning the instance never states goes to `SKILL.md` §`Who you are to him` if the assistant needs it before it acts, and to `docs/` otherwise. Each such move is listed in the commit body.
- `cargo test -p iaam-server --test contract` passes.

- [ ] **Step 1: Write the failing test**

```rust
/// A retired product leaves the perimeter and its money does not leave the
/// journal. A caller that reads retirement as removal tells the owner a figure
/// went away when only a boundary moved.
#[tokio::test]
async fn retirement_says_what_it_does_not_change() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let described = property_description(&spec, "AccountRetirementDto", "state");
    assert!(
        !described.trim().is_empty(),
        "AccountRetirementDto.state publishes no description"
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
nix develop -c cargo test -p iaam-server --test contract retirement_says_what_it_does_not_change
```

- [ ] **Step 3: Write the descriptions**

- [ ] **Step 4: Run the tests**

- [ ] **Step 5: Repeat for the remaining class-2 paragraphs**

- [ ] **Step 6: Place the residue and delete what is covered**

- [ ] **Step 7: Run the suite and the guards**

- [ ] **Step 8: Commit**

```bash
git add crates/iaam-server/src/dto.rs crates/iaam-server/tests/contract.rs docs/agent-skill/the-money-and-the-perimeter.md docs/agent-skill/SKILL.md
git commit -m "feat(contract): retirement publishes what it leaves untouched (<bead-id>)"
```

---

### Task 6: `correcting.md` — carriers written, copies deleted

**Files:**
- Modify: `crates/iaam-server/src/dto.rs` (the retraction schemas the inventory names)
- Modify: `crates/iaam-server/tests/contract.rs`
- Modify: `docs/agent-skill/correcting.md`, `docs/agent-skill/SKILL.md`

**Interfaces:**
- Consumes: `property_description` from Task 3.

**The cycle, for every carrier in this task:** the five steps shown in full in
Task 3 — a failing test named for the obligation and not for the field, a run
that proves it fails, the description, a run that proves it passes, the deletion
of the paragraph it replaced in the same commit. The schema and property come
from Task 2's inventory, which is why they are named per carrier below rather
than listed here.

**Acceptance Criteria:**
- Whose act a correction is, the one import an assistant may take back and the bound checked on it, and why re-sending a corrected row writes nothing, are published on the retraction schemas — except the part that is an assistant's restraint with no call in flight, which is class 3 and moves into `SKILL.md`'s external-client section.
- `cargo test -p iaam-server --test contract` passes.

- [ ] **Step 1: Write the failing test**

```rust
/// Re-sending a corrected row writes nothing, and a caller that believes
/// otherwise reports a fix to the owner that did not happen.
#[tokio::test]
async fn a_retraction_says_whose_act_it_is() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let described = property_description(&spec, "CorrectImportRequest", "acknowledge_retraction");
    assert!(
        !described.trim().is_empty(),
        "CorrectImportRequest.acknowledge_retraction publishes no description"
    );
}
```

Note what is already carried, so the test is written for what is missing rather
than for what is there: `SubmitCorrectionsRequest::acknowledge_retraction`
(`crates/iaam-server/src/dto.rs:1046`) and
`CorrectImportRequest::acknowledge_retraction` (`crates/iaam-server/src/dto.rs:1060`)
already publish that a retracted fact stops counting in every report and that
re-submitting the same rows does not bring it back — so that paragraph of
`correcting.md` is **class 1** and is deleted, not moved. What has no carrier yet
is whose act a correction is, and the bound checked on the one import an
assistant may take back.

- [ ] **Step 2: Run it and watch it fail**

- [ ] **Step 3: Write the descriptions**

- [ ] **Step 4: Run the tests**

- [ ] **Step 5: Delete what is covered; move class-3 into `SKILL.md`**

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-server/src/dto.rs crates/iaam-server/tests/contract.rs docs/agent-skill/correcting.md docs/agent-skill/SKILL.md
git commit -m "feat(contract): a retraction publishes whose act it is (<bead-id>)"
```

---

### Task 7: One file, and a guard that keeps it one

**Files:**
- Delete: `docs/agent-skill/importing.md`, `docs/agent-skill/the-money-and-the-perimeter.md`, `docs/agent-skill/correcting.md`, `docs/agent-skill/reading-the-reports.md`
- Modify: `docs/agent-skill/SKILL.md` (remove `## The four files beside this one`; the bootstrap section loses the field mechanics its carriers now hold)
- Modify: `scripts/check-agent-skill.sh` (fourth refusal + self-probe)
- Modify: `README.md` if it names a companion file

**Interfaces:**
- Consumes: Tasks 3–6 — every companion file is empty of content by now.
- Produces: the final shape. `SKILL.md` is the whole skill.

**Acceptance Criteria:**
- `docs/agent-skill/` holds exactly one file.
- `SKILL.md` contains no payload field name — no backticked `snake_case` or `lowerCamelCase` identifier.
- The guard refuses a field name, and its self-probe proves the shape both ways.
- `make check` passes.
- `SKILL.md` is under 200 lines.
- A reader holding only `SKILL.md` and the instance's own answers can open a
  session, put the first decision to the owner and quote him a figure — checked
  by the read-through in Step 7, which is the spec's fifth success criterion and
  the only one no command can prove.

- [ ] **Step 1: Add the fourth refusal to `scripts/check-agent-skill.sh`**

After `STATUS_SHAPE`:

```bash
# A payload field name. A document that names `requiredScope` or `blocked_by` is
# narrating the payload, which is the disease the first three refusals treat one
# level down: the contract publishes those descriptions, they move with the code,
# and a copy here is a claim that rots. Matched as a shape — a backticked
# identifier in snake_case or lowerCamelCase — so it needs no list of fields and
# cannot go stale when the API changes.
FIELD_SHAPE='`[a-z][a-z0-9]*(_[a-z0-9]+|[A-Z][A-Za-z0-9]*)[A-Za-z0-9_]*`'
```

And the check, after the status one:

```bash
check "$FIELD_SHAPE" "a payload field name" \
  "Say what the value MEANS to him. The contract carries the field's own description."
```

- [ ] **Step 2: Add the self-probe, beside the other three**

```bash
probe_field=$(printf '%s\n' 'read `requiredScope` on the resolution' 'the money and the perimeter' \
  | grep -E "$FIELD_SHAPE" || true)
if [ "$probe_field" != 'read `requiredScope` on the resolution' ]; then
  err "the field shape misclassifies its own probe"
  exit 1
fi
```

- [ ] **Step 3: Run the guard and watch it refuse the current document**

```bash
nix develop -c ./scripts/check-agent-skill.sh
```

Expected: FAIL, listing every remaining field name in `SKILL.md`. This is the
list of work left, not a defect in the guard.

- [ ] **Step 4: Remove the remaining field mechanics from the bootstrap section**

Each name the guard listed is either a paragraph whose carrier landed in Tasks
3–6 (delete it) or one that never got a carrier (it is in the inventory's `## No
home yet` list — stop and raise it; do not delete it to make the guard pass).

- [ ] **Step 5: Delete the four companion files and the section that names them**

```bash
git rm docs/agent-skill/importing.md docs/agent-skill/the-money-and-the-perimeter.md \
       docs/agent-skill/correcting.md docs/agent-skill/reading-the-reports.md
```

Remove `## The four files beside this one` from `SKILL.md` and replace the
pointers inside other sections that referred to those files. A pointer to a file
that no longer exists is worse than no pointer.

- [ ] **Step 6: Run the guard and the whole gate**

```bash
nix develop -c ./scripts/check-agent-skill.sh
nix develop -c make check
wc -l docs/agent-skill/SKILL.md
```

Expected: `Agent documents checked: 1.`, `make check` green, and a line count under 200.

- [ ] **Step 7: Read the finished file as a cold assistant would**

Read `SKILL.md` end to end, holding nothing else, and answer three questions from
it alone: what do I do before I say anything; what is the first thing he hears;
what do I do with the first thing that needs him. An answer that requires
guessing is a gap, and the gap is filled in this task rather than noted.

- [ ] **Step 8: Commit**

```bash
git add -A docs/agent-skill scripts/check-agent-skill.sh README.md
git commit -m "refactor(skill): one file, and a guard that refuses a payload field name (<bead-id>)"
```

---

## Open question carried into execution

The spec's §4 last row — the contour, categories, account naming and instrument
codes — is the one that will not classify uniformly. Task 5 decides it paragraph
by paragraph and lists each decision in its commit body. If more than a handful
land in the inventory's `## No home yet` list, stop and raise it: that would mean
the shrink is removing meaning the API does not carry, and the answer is a
carrier, not a deletion.
