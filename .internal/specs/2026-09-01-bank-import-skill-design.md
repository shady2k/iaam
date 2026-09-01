# Importing a bank export without teaching the code about the bank

Brainstorming bead: iaam-mksb. Date: 2026-09-01.

This design answers one question raised by the owner while the August T-Bank
export was already on disk: **how a bank's file becomes operations in the
journal without any bank ever appearing in the code.**

It is the input half of `.internal/specs/2026-09-01-money-flow-design.md`,
whose R8 states the constraint this document must satisfy:

> **No code per bank, ever.** A bank's identity is data — a column mapping, a
> channel, a set of rules — never a branch in the code and never an assumption
> in the design.

## The task, in the owner's words

> Ну как я думаю это должно выглядеть: у нас свой формат добавления операций.
> Парсер для разных банков мы потому конечно добавим. Но должен быть способ
> добавлять без парсера. Как я себе это вижу: я хочу импортировать csv из
> какого-нибудь банка, отдаю это агенту. Агент используя наш API получает
> список полей, которые необходимо передавать, а затем пишет скрипт, который
> отправит данные нам в необходимом виде. После чего из этого делается скилл.

Three things are being asked for at once, and all three are requirements:

- there is **one submission format of our own**, and it is discoverable **from
  the API itself**, not from a document that drifts;
- there is **always a way in without a parser** — a new institution must never
  be a blocked task waiting on a release;
- what the agent writes **does not evaporate**. It becomes an **import skill**,
  which is per-institution knowledge held outside the codebase.

## What was rejected, and why it is recorded

Before the owner stopped it, the agent had proposed writing a throwaway script
in a scratch directory to rewrite the export into the canonical CSV. That is
withdrawn, and the reason is worth keeping because it will be proposed again:

**A throwaway rewriter is the worst of the options.** It *is* a column
mapping — the thing the money-flow design says must be data — but it lives as
untracked code in a temporary directory, with no version, no owner and no
second month. It has the maintenance cost of code and none of the durability,
and it makes the ingest path claim a reproducibility it does not have.

The owner's shape fixes exactly this: the same script, promoted to an artefact
that survives the session and is re-run next month.

## Requirements

- **I1.** The set of fields an operation needs is answered **by the running
  system**, not by prose. An agent asks the API and gets the contract.
- **I2.** Institution knowledge lives in an **import skill**: which column is
  the date, how the sign decides the operation kind, which quirks the export
  has. Never in a crate.
- **I3.** A skill contains **no owner data** — no account UUIDs, no account
  names, no amounts. It resolves accounts by name against the live API at run
  time. A skill that embeds the owner's accounts has moved his data into a file
  that gets committed.
- **I4.** Re-importing the same export **changes nothing**. The row key rule is
  stated once, centrally, and every skill obeys it; a skill that invents its own
  key produces duplicates that only the control balance would catch, a month
  later.
- **I5.** The bank's own category value is preserved **verbatim** into
  `source_category`. It is the raw material for the category rules (money-flow
  design §3), and a channel that drops it makes "чтобы вручную их не
  проставлять" impossible on the first import.
- **I6.** A deterministic per-institution parser may be added later (money-flow
  design §6, P3). Nothing here may block that, and nothing here may be *required*
  by it: the skill path stays available for institution number three forever.

## What already exists

Verified in the tree:

- **The contract is already served, and served unauthenticated.**
  `/v1/openapi.json` is registered on the outer router, outside the
  `protected` group — `crates/iaam-server/src/lib.rs:173`. `utoipa` registers
  nested schemas transitively, so `SubmitOperationsRequest → OperationDto →
  OperationKindDto` is present in full. **I1 needs no new endpoint.**
- **The submission route takes a declared source.** `POST
  /v1/ingest/operations` builds `SourceId::declared(owner, account, channel)`
  — `crates/iaam-server/src/routes.rs:1416`, `crates/iaam-core/src/ids.rs:99`.
  Resubmission of the same rows does not duplicate them.
- **`source_category` is already carried through** the DTO to the domain
  operation — `crates/iaam-server/src/dto.rs`, `OperationDto`.
- **Accounts are resolvable by name.** `GET /v1/accounts` returns the
  directory, which is what I3 relies on.
- **Duplicate detection has two layers** —
  `crates/iaam-store/src/events.rs:200`: first `(owner, source,
  source_operation_id)`, then `(owner, idempotency_key)`.

## The model

### 1. The mechanism

```
owner ──── bank export ────▶ agent
                              │
                              ├── GET /v1/openapi.json   → what fields an operation needs
                              ├── GET /v1/accounts       → account ids, by name
                              │
                              └── writes a script ──▶ POST /v1/ingest/operations
                                        │                  source: {account, channel}
                                        │
                                        └──▶ promoted to an import skill
```

The institution is known only to the skill: no crate learns the word "T-Bank",
and no crate learns the owner's account names. That is R8, and it holds.

**What does not hold is the stronger claim that nothing in `crates/` changes.**
Three gaps were found in the tree while writing this design, and all three are
prerequisites for a useful August rather than nice-to-haves. They are listed in
§6.

### 2. What a skill holds, and what it must never hold

An import skill is **checked into this repository**. What an institution prints
in its export is public knowledge — anyone with an account at that bank sees the
same columns — so it belongs with the software that reads it, and a second bank
is then a second skill rather than a change to a crate. What passes through the
skill at run time is a different matter entirely, and §2.1 and the "never holds"
list below draw that line.

**Holds** — everything that is true about the institution and false about the
person whose accounts these are:

- the column mapping: which column is the date, the amount, the description,
  the institution's own category;
- the date format and the decimal separator;
- how the operation kind is decided — for a card export, the sign of the amount
  distinguishes `deposit` from `withdrawal`;
- the export's quirks. T-Bank emits an internal transfer as **two rows, one per
  leg**; submitting both double-counts it, so the skill submits one and drops
  the other by an explicit rule, not by luck of ordering.

**Never holds** — anything that identifies a person or their money: account
UUIDs, account names, balances, counterparty names, amounts, or a row of any
real export. The file is public the moment it is pushed, and a value published
in a commit outlives the commit that removes it. Accounts are resolved through `GET /v1/accounts` at run time (I3). A
skill is checked into the repository; the owner's data is not.

### 2.1 The perimeter is an input, not skill knowledge

Dropping one leg of a transfer pair is only correct when **both** accounts are
inside the contour. When the counterparty is outside it — the children's cards
below — the same two rows mean something else entirely: one leg is a real
`withdrawal` across the boundary, and the other must not be loaded at all.

Which accounts are inside is **owner data**, so by I3 it cannot live in the
skill. It arrives as an explicit input to the run: a list of account names the
agent is handed, alongside the export.

**The API cannot answer this today.** Contours are write-only over HTTP —
`create_contour_version` is the only registered contour route
(`crates/iaam-server/src/lib.rs:125`); there is no `GET /v1/contours`. A skill
therefore cannot derive the perimeter and must be told it. That is acceptable
and honest, but a read route would let the skill check what it was told against
what the system believes, which is strictly better. Bead iaam-mp5e is filed;
this design does not block on it.

### 3. The row key, stated once

`crates/iaam-store/src/events.rs:200` deduplicates on
`(owner, source, source_operation_id)` first and on `(owner, idempotency_key)`
second. The two fields mean different things and must not be confused:

- **`source_operation_id` is the identifier the institution stated.** Where the
  export has none, synthesising one and putting it here records an invention in
  the journal. The T-Bank export has no such column — its 17 columns were
  checked — so for it this field stays absent.
- **`idempotency_key` is ours.** It is what makes a re-import idempotent when
  the source names nothing.

The rule every import skill obeys, and the reason for each part:

```
idempotency_key = "<account-id>/<channel>/<sha256(raw row text)>/<ordinal within the day>"
```

- **`account-id` and `channel` are in the key** because
  `find_duplicate` matches `idempotency_key` **globally per owner**, not per
  source. Two institutions producing an identical row text would otherwise
  collide, and the second one would silently vanish as a duplicate.
- **The hash is of the raw row text** so that the same file re-imported
  reproduces the same key.
- **The ordinal** disambiguates two genuinely identical purchases on one day,
  which the store deliberately refuses to treat as one fact. It is counted
  within `(account, day, row hash)` rather than within `(account, day)`: a
  per-day counter would renumber every later row of a day the moment an
  overlapping export added one, and §6 of the money-flow design expects exactly
  such overlapping exports. Counted per identical row text, an added row changes
  no other row's key.

**The stated cost of the hash.** If the institution re-exports the same
operation with any field changed — a status moving from pending to settled, a
description edited — the key changes and the row lands a second time. That is
detected by the control balance, on the day it happens. Making the key narrower
(date + amount) would trade a visible duplicate for a silently swallowed real
operation, which is worse.

### 4. One route, and what happens to the other

Import skills submit to `POST /v1/ingest/operations`. `POST /v1/ingest/csv` is
**not** the import path, for two reasons found in the tree:

- it mints `SourceId::new_random()` per request —
  `crates/iaam-server/src/routes.rs:1532` — the exact defect that iaam-5jhq
  fixed for the operations route and left here, so a re-import duplicates
  everything;
- its `Row` — `crates/iaam-ingest/src/csv_source.rs:216` — has no
  `source_category` field, so it cannot satisfy I5.

Until both are addressed, the csv route is what it actually is: the canonical
hand-edited format, not an import channel. Beads iaam-ewcl (random source) and
iaam-5yv3 (missing `source_category`) are filed; this design does not depend on
either.

### 5. Provenance is honest about itself

The skill channel is the **paste channel** of money-flow design §6 under a more
durable name. Its parse is performed by an agent, so its version describes a
model and a prompt rather than code, and it **never claims independence**: the
§10.3 criterion (`parser_version != other && document != other`) is meaningless
for it. Its integrity comes from R5 — the control balance the owner states for
the account.

This is not a breach of ADR-0001, which governs where *we* parse a protocol we
implement. The agent is an external client (founding design §2, "ИИ — внешний
клиент"). When a deterministic parser for an institution is written, it belongs
in its own adapter crate by ADR-0001's reasoning, and it replaces this channel
for that institution without disturbing the others (I6).

### 6. What the system is missing, found while writing this

Each of these was verified in the tree, and each blocks something the money-flow
design already promised.

**6.1 A category cannot be created at all.** `POST /v1/categories` takes a group
identifier (`CategoryRequest.group`, `crates/iaam-server/src/dto.rs:3503`) and
returns 404 when the group does not exist. `create_group` exists in the
application layer — `crates/iaam-app/src/scenarios/categories.rs:49` — and is
called from no route. There is no `POST /v1/category-groups`. Without it the
owner's category list cannot be started, so the whole of money-flow §3 is
unreachable over HTTP.

**6.2 Rules on the description can never match.** `CategoryMatcher::
DescriptionContains` is implemented and tested in core
(`crates/iaam-core/src/category.rs:68`), but the subject that reaches it is
built with `counterparty: None, description: None` —
`crates/iaam-app/src/scenarios/categories.rs:274` — because the event carries
neither. `Provenance` (`crates/iaam-core/src/event/provenance.rs:52`) has no
such field and `OperationDto` accepts none.

This is level 3 of the precedence in money-flow §3, and August needs it. The
T-Bank export puts **74 rows** under one source category, `Переводы`, and those
rows mean at least four different things — money to the children, money to the
spouse, a utility payment, and a transfer to the owner's own account at another
bank. No rule on the source category can separate them; only the description
can.

The fix follows the precedent of `source_category` itself, which was added to
`Provenance` as an `Option` with `#[serde(default)]` precisely so that events
already in the append-only journal stay readable.

**6.3 A hand-made decision about one row is unreachable for a source without
identifiers.** The `Row` matcher keys off `subject.row_key`, which is populated
from `event.provenance.source_operation_id()`
(`crates/iaam-app/src/scenarios/categories.rs:275`). §3 above establishes that a
source which states no identifier of its own leaves that field absent and
carries its identity in `idempotency_key` instead. For every such source — the
T-Bank export among them — the strongest precedence level of money-flow §3 and
R12 therefore cannot be exercised at all.

The row key must fall back to `idempotency_key` when the source stated no
identifier of its own. This keeps the two fields' meanings intact: the
provenance still records that the source named nothing.

## The first skill, and the shape of the decisions it needs

The first import skill covers the T-Bank operations export.

**Nothing about the owner belongs in this document.** No account name, no card
number, no counterparty, no amount. What belongs here is the *kind* of decision
the perimeter needs, because those decisions are not recoverable from any export
and a later reader will otherwise assume the file answered them. The owner's own
values live in his database and in the run-time maps the skill is handed; they
are not committed anywhere.

Three decisions of that kind arose, and each changes the report materially:

- **A card the owner funds for someone else** — a child, a family member — is
  formally his account, and the bank labels the top-up a transfer between the
  owner's own accounts. Whether the *top-up* or the later *purchase* is the
  expense is the owner's call, not the bank's, and the consequence is stated
  rather than hidden: with the card outside the contour the report shows one
  line and does not show what was bought, and money still sitting on the card at
  month end already counts as spent.
- **An account whose statement is not loaded** enters the contour by balance
  alone (money-flow design §5). Transfers into it are visible from the other
  side; whatever its balance change the known transfers do not explain is
  reported as a named delta for that account.
- **A counterparty who is in fact the owner's own account at another bank.** The
  export cannot tell that from a payment to a stranger, and reading it as an
  arrival turns a movement of one's own money into income. In the first real
  month this single case was worth more than the entire rest of the month's
  inflow, which is why it is a first-class input (§2.1) rather than an
  afterthought.

**What the report is expected to show, and what would be a wrong conclusion.**
The acceptance criterion for entering August is the **discrepancy line**, not a
pleasing total. If it is not zero, the report has done its job and names the
account to look at; the response is to look at that account, not to adjust the
report. Likewise a large `not_decomposed` share means the rule set is thin, not
that the report is lying.

## Order of work

1. **The three gaps of §6**, in that order: the category-group route, the
   description on an operation, the row-key fallback. Each is code in `crates/`
   and each is testable on its own.
2. **Accounts and the contour** — create the accounts above through
   `POST /v1/accounts` and one contour version through `POST /v1/contours`.
3. **The import skill** — written against `/v1/openapi.json` and
   `/v1/accounts`, carrying the T-Bank column mapping, the leg-pairing rule and
   the row key of §3, and developed against a **synthetic** export fixture that
   contains none of the owner's data.
4. **August, entered by running the skill.** Running it is the test: a skill
   that only ever worked as a hand-driven script has not been proven.
5. **A second run of the same file**, which must change nothing. This is the
   proof of I4, and it is cheap.
6. **The control balance** for each account on 31.08, then the flow report.
7. **Categories** — map T-Bank's ~23 category values into the owner's own list
   through `POST /v1/category-rules`, and separate the `Переводы` rows with
   description rules, with the intervals of R11.

## What this design does not cover

- **The deterministic file importer with a stored column mapping (P3).** It
  remains the eventual answer for an institution imported every month, and I6
  keeps the road to it open. It is a separate epic with no plan yet.
- **The MTS Bank paste.** It enters as an account with a stated balance and no
  operations; its own spending will surface as a named undecomposed delta until
  the owner pastes it.
- **The owner's category list.** It is his to define; this design only
  guarantees the raw material (I5) reaches the rules intact.
