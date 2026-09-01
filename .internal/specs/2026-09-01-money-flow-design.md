# The flow of money: where it came from, where it went, and into what

Brainstorming bead: iaam-ovq0. Date: 2026-09-01. Revised twice on 2026-09-01,
after the owner's pivot and then after he stated the concrete task.

This is the first design in the project that is not about investments. It comes
from the owner's statement that investments are important but not the point:
the system is meant to replace **Actual Budget** and **Snowball Income**, and
the first thing it must show is the flow of income and expenses.

That is not a new requirement. §1 of the founding design
(`.internal/specs/2026-08-22-investment-tracker-design.md`) names both tools as
the incumbents that fail, §2 records "учёт первичен" as the first accepted
decision, and defect 4 of §1 is exactly this:

> Граница портфеля определяется учреждением, а не владельцем. Перевод со
> вклада на брокерский счёт брокер считает пополнением, потому что вклад вне
> его периметра. Собственные пополнения выглядят как заработок.

§18 puts "Интеграция с Actual Budget" outside the perimeter. That excludes
integration, not replacement.

## The task, in the owner's words

On 2026-09-01, having said **"я понял, что не получаю ценности сейчас"**, he
stated what he is about to do:

> Я выгружу отчет из Т-Банка, скопирую операции со страницы МТС-Банка, а также
> остатки от брокеров. И я хочу увидеть сколько у меня денег пришло, сколько
> ушло и куда. Ну и категории поменять, чтобы вручную их не проставлять. А
> потом сверка с остатками.

Read literally, that fixes the first cut:

- **three input shapes at once** — a file, pasted text, and stated balances;
- **"и куда"** — a breakdown, not just two totals. That is categories, and it
  means the earlier revision of this document was wrong to defer them;
- **"чтобы вручную их не проставлять"** — categorization is automatic by
  default; the owner corrects, he does not fill in;
- **"а потом сверка с остатками"** — reconciliation closes the loop.

**This is a larger first cut than the previous revision proposed, and that is
stated plainly rather than quietly absorbed.** The previous revision promised
the report in days by cutting the importer and categories. Both are back,
because without them the report does not answer the question that was asked.
Scaling it back down is the owner's call, not the design's; §"Order of value"
below breaks the cut into steps that each deliver something on their own.

## The evidence

The owner's live Actual Budget, read from two screenshots:

- `All accounts <redacted>`, of which **`On budget` is only <redacted>** —
  two cards. `Off budget` is <redacted>: investments, two deposits, three
  broker accounts. Everything that is not a card is a dead balance the tool
  cannot reason about.
- Among the expense categories stands **`Переводы`**, <redacted> in March. A
  transfer to the owner's own deposit is recorded as an expense, because the
  deposit is off budget. Defect 4, mirrored: own saving looks like spending.
- `Income <redacted> / Expenses −<redacted> / Saved <redacted>` for March.
  Both sides are polluted by movements between the owner's own accounts, so the
  headline number means nothing.
- `Budgeted` is `0,00` in every row. Envelope budgeting is not used.
- Categories are grouped (`Usual Expenses` → `Техника`, `Медицина`, `ИИ`,
  `Маркетплейсы`, …), so two levels are used and are wanted here too.

The owner's own summary: the category view has been opened only a few times in
years and says nothing. **That is a consequence, not a preference.** The sum the
categories decompose is not a real number, so the decomposition cannot be
informative. Fix the sum and the decomposition becomes worth opening.

## Requirements, as agreed 2026-09-01

- **R1.** All accounts — cards, cash, deposits, broker accounts — are one
  owner-defined contour. A transfer between them is neither income nor expense.
- **R2.** The flow report answers honestly what came in from outside, what went
  out, what the capital earned on its own, what was moved into assets, what went
  to fees and taxes, and what that leaves — **and decomposes the outflow by
  category**.
- **R3.** Balances for every account, each carrying whether it has been
  reconciled and when. An account unverified for six months must not look like
  one verified yesterday.
- **R4.** Three input shapes are in the first cut: a **file** parsed by a column
  mapping, **pasted text** parsed by the agent, and **stated balances** for an
  account whose operations are not loaded at all.
- **R5.** Integrity is defended **primarily** by **control balances** rather than
  by cross-channel independence: the owner states the balance on a date and
  reconciliation compares it to the sum of operations. Independence is not
  forbidden — an account with both a file channel and a paste channel reaches it
  naturally — but nothing may depend on it, because most accounts will only ever
  have one.
- **R6.** **Categories are in the first cut.** Two levels: group → category. The
  list is **living**: categories are added, renamed, split, merged and retired
  over the years, and none of that may require a journal migration or break the
  comparability of past months.
- **R7.** No budgets, no planned amounts. `Budgeted` is unused in the tool being
  replaced.
- **R8.** **No code per bank, ever.** The owner's banks are MTS Bank and T-Bank
  today and "могут измениться или добавиться в любой момент". A bank's identity
  is data — a column mapping, a channel, a set of rules — never a branch in the
  code and never an assumption in the design.
- **R9.** **One mechanic: everything is an operation.** A line from a statement
  and a line reading "прочие траты за август, −120 000" are the same kind of
  event, differing only in the provenance recorded on each. There is no
  "aggregate mode" and no second code path. Granularity improves by replacing a
  coarse row with precise ones.
- **R10.** **Money reconciles; positions do not.** A cash balance is a fact an
  institution states and reconciliation can check. A position's value changes
  every second and cannot be checked against anything. They are two quantities
  with two different statuses and are never added into one number.
- **R11.** **A rule is valid over an interval of dates, never forever.** A
  merchant that sold pies and later sold umbrellas is the same problem the
  project already solved for instrument aliases, and it is solved the same way.
- **R12.** **Recomputation never happens silently, and a hand-made decision
  outranks a later blanket rule.**

## What already exists

Verified in the tree, not assumed:

- **Accounts are institution-agnostic.** `accounts (id, owner, title,
  institution, created_at)` — `crates/iaam-store/migrations/0001_initial.sql:58`.
  `institution` is nullable. A card, a cash wallet and a deposit are ordinary
  accounts; the schema does not change.
- **Contours are versioned and live in core.** `ContourDefinition { id,
  version, accounts }` — `crates/iaam-core/src/contour.rs:35`.
- **The three cash movements exist in the journal.** `cash_in` / `cash_out`
  cross the contour boundary; `cash_transfer` moves money between accounts
  inside it (founding design §4.9). `EventKind::flow_endpoints` already encodes
  which is which — `crates/iaam-core/src/event/kind.rs:300`.
- **Cash flows and balances are already projected.**
  `crates/iaam-core/src/projection/flows.rs` classifies events against the
  contour; `crates/iaam-core/src/projection/balances.rs` sums cash per account
  and currency.
- **A tax leg exists.** `LegKind::Tax` with a cash effect —
  `crates/iaam-core/src/event/leg.rs:24`. Tax withheld at source is already
  representable.
- **Classification already asks and learns.** `ClassificationSubject { account,
  counterparty, description, source_kind, movement }` and `RuleMatcher {
  counterparty_account, description_contains, kind }` —
  `crates/iaam-ingest/src/classification.rs:59`. An unresolved row yields
  `ClassificationResult::Ambiguous { question }` rather than a guess, and a rule
  that asks about nothing matches nothing, so "reclassify everything" cannot be
  written by accident (`classification.rs:66`). Editing a rule recomputes history
  through reversal and replacement without rewriting the journal (bead iaam-8t7).
- **Date-scoped resolution is an established pattern.** Instrument aliases carry
  an `AliasInterval` and are resolved for the row's date, not for today, with the
  reasoning spelled out at `crates/iaam-ingest/src/csv_source.rs:47`. R11 reuses
  it.
- **Control balances exist.** `ControlClaim::CashBalance { currency, amount, at }`
  with the reconciliation ledger, used today for broker accounts.
- **A structured ingest route exists.** `POST /v1/ingest/operations`, with a
  per-row verdict: one unreadable row does not reject the others (epic
  iaam-40vm).
- **Raw material is stored by hash.** The documents store keeps the bytes a fact
  was derived from, so provenance is real for a pasted blob too.

## The model

### 1. One contour, and what follows from it

A single contour holds every account: cards, cash, deposits, broker accounts. It
is versioned, so adding an account creates a new version and does not silently
move historical figures; the report names the version it used.

Two consequences carry the whole design:

- **A transfer to one's own deposit is `cash_transfer`.** Not income, not
  expense. The category `Переводы` stops existing as a spending category because
  it stops being spending.
- **Buying securities is not an expense.** Cash leaves a broker account and
  becomes a position; both ends are inside the contour.

How the system knows a transfer is internal: `Counterparty::OwnAccount` when the
receiving account is in the directory. When the counterparty is only a string,
classification asks a typed question and remembers the answer as a rule. This
mechanism is written and tested; it is reused, not rebuilt.

### 2. The report's shape, and the identity it must hold

The flow report over an interval, naming a contour version and a rule version,
is six quantities, one reference block, and a discrepancy:

| Quantity | What it is | Checkable |
|---|---|---|
| Came in from outside | salary, gifts, refunds | exactly |
| Went out to outside | what was actually spent | exactly |
| Earned by the capital | deposit interest, coupons, dividends, cashback | exactly |
| Moved into assets | securities bought less sold, net | exactly |
| Fees | brokerage, depositary, account maintenance | exactly |
| Taxes | withheld at source and self-paid | exactly |
| *Internal transfers* | *reference only, not in the total* | exactly |

```
Δ cash balances = came in − went out + earned by the capital
                  − moved into assets − fees − taxes
```

**A discrepancy is named, not hidden.** When the identity does not close, the
report states the amount and the account it belongs to. That is what replaces
the meaningless `Saved <redacted>`.

**"И куда":** the outflow is decomposed by category, two levels, group and
category. Rows the rules could not place are shown as **their own line, "not
decomposed", with the count and the amount** — never folded into "Прочее". A
silent catch-all bucket is how a decomposition stops being informative.

**Why "earned by the capital" is a separate block and not part of "came in".**
`EventKind::Income` — coupon, dividend, deposit interest — is deliberately
classified as `WithinAccount` rather than as an inbound external flow
(`crates/iaam-core/src/event/kind.rs:316`): a coupon is not a new contribution
of capital, and counting it as one overstates contributions and corrupts XIRR.
That classification is correct and stays. But for a household flow report the
same event *is* income, and reusing `FlowLog` unchanged would make it invisible
— neither in nor out — while the balance grows and the report fails to close.

The resolution is a **third block, not a reclassification**: the new projection
reads the same events from a different angle without moving any of them between
buckets. Returns are untouched. This is precisely the distinction Snowball
Income loses, where a contribution and an earning are indistinguishable.

**Taxes.** Tax withheld at source is already a `LegKind::Tax` leg. A self-paid
tax (property, transport, a filed return) is currently indistinguishable from
ordinary spending, so it needs its own event kind, modelled on the existing
`Fee { amount, origin }` — a cost not tied to a trade, classified
`WithinAccount`, surfaced as its own line. Without it, discretionary spending
stays overstated.

### 3. Categories: a living list, derived rather than stored

**Two levels.** A group holds categories; a category holds operations. Both are
ordinary reference records the owner edits, not an enum in code.

**The category of an operation is not a field on the event.** It is **derived
from versioned rules**. This is the whole reason the list can live: renaming,
splitting, merging or retiring a category is an edit to reference data and
rules, and the journal — which is append-only — is never touched. Had the
category been written onto the event, every reorganization would demand a
journal migration, and the owner would stop reorganizing.

**Where a rule's answer comes from, in order:**

1. **A hand-made decision about one specific row** — the strongest. Once the
   owner has said what a particular operation was, no later blanket rule
   overrides it (R12).
2. **A rule on the source's own category value.** The file and the page both
   carry the bank's category. It is retained separately from the owner's
   category and mapped into it by one rule per value — on the order of thirty
   answers per source, once, and automatic from then on. This is what makes
   "чтобы вручную их не проставлять" true on the first import.
3. **A rule on the description or counterparty**, for the rows where the source
   was wrong or silent.
4. **Nothing** — the row is *not decomposed*, and the report says so.

**T-Bank's category set and MTS Bank's category set are different sets.** Taking
either as the owner's own would make the two banks unsummable — "сколько ушло на
еду" could not be computed across them. The owner's list is his own, and each
source's vocabulary maps into it. This is also why the mapping is per source and
is data, consistent with R8.

**Every rule carries an interval of validity (R11).** A merchant that sold pies
in 2024 and umbrellas in 2026 is one string with two meanings, and a rule that
claims to hold forever misclassifies half of history. `AliasInterval` already
solves exactly this for instrument codes, with the reasoning written out at
`crates/iaam-ingest/src/csv_source.rs:47`; the same shape applies here. When a
rule is created, the system proposes an interval and the owner narrows it.

**Recomputation shows its work (R12).** When a new or edited rule would
reclassify operations that were already decomposed, the system reports what
changed before it stands: how many rows, in which months, and which amounts
moved between which categories. The owner's objection is exactly right — in a
year he will not remember the umbrellas — so the system does not rely on him
remembering. It relies on him seeing the numbers move at the moment they move.

`RuleMatcher` today is three fields with no interval and no category target
(`crates/iaam-ingest/src/classification.rs:59`). It gains a validity interval, a
category target, and a source-category matcher. That is a bounded change to an
existing, tested mechanism.

### 4. Balances, and why two numbers never become one

`GET /v1/reports/balances?as_of` returns, per account:

- **cash** — exact, with its reconciliation state: stated and matched; stated
  and off by an amount; never stated;
- **position value** — separately, as an estimate on a date with a named price
  source.

They are never summed into a single figure. A broker account's value changes
every second and there is nothing to reconcile it against; a cash balance is a
number an institution states. Presenting them as one is exactly what produces
`All accounts <redacted>` — a figure nobody checks. R3 and R10 exist so that
this does not recur.

**Currencies are kept apart**, converted to the report currency through the
existing mechanism with an explicit rate source. Everything is RUB today; the
system still will not add currencies silently.

### 5. An account whose operations are not loaded

The broker accounts enter August as **stated balances only** — the owner is not
loading their operations. That is legitimate under R9, and the report handles it
without inventing anything:

- the account contributes its stated balance to §4;
- transfers into it are visible from the *other* side, because the card's
  statement records them;
- whatever change in its balance the known transfers do not explain is reported
  as **an undecomposed delta for that account**, named as such. It is not
  silently called earnings, and it is not silently called spending.

When the owner later loads that account's operations, the coarse picture is
replaced by the precise one through the same mechanism as any other refinement
(R9). Nothing has to be undone.

### 6. Input

**The file channel.** One importer and a **column mapping the owner configures
once per account** — a date column, a description column, an amount column, a
source-category column, plus date format, delimiter, lines to skip and sign
inversion. A mapping is **data, not code** (R8) — the same argument the owner
accepted in epic iaam-d8b.2.2, which moved the broker operation-kind dictionary
into the database so that extending it needs no release. A mapping is
deterministic and reproducible, it has a real version, and re-importing the same
file yields the same result.

**The paste channel.** Where the owner copies rows from a web page, the agent
parses them and submits them through the documented structured route. That parse
is not reproducible — its version describes a model and a prompt, not code — so
the channel **declares itself as such and never claims independence**. The §10.3
criterion (`parser_version != other && document != other`) is meaningless for it.
Its integrity comes from R5. This is not a breach of ADR-0001, which governs
where *we* parse a protocol we implement: the agent is an external client
(founding design §2, "ИИ — внешний клиент"). A future deterministic parser for
some source does belong in its own adapter crate by ADR-0001's reasoning.

**The stated-balance channel** is §5 above.

**One blocking defect.** `POST /v1/ingest/operations` mints
`SourceId::new_random()` per request (`crates/iaam-server/src/routes.rs:1219`).
Every submission therefore becomes a new source, so nothing deduplicates across
submissions and re-importing after a correction creates duplicates instead of
replacing rows. A **declared source per (account, channel)** is required — not
per account: the file channel and the paste channel of the same account must
stay distinct source identities, or a pasted row would deduplicate against an
exported one instead of confirming it, and the independence described above
would collapse. This is bead iaam-5jhq, which this cut promotes from an
inconvenience to a blocker.

**Deduplication, which overlapping pastes make necessary.** The owner selects
January, then January–February. The store deliberately refuses "account + date +
amount" as a natural key, because two identical purchases in one day are
legitimate (`crates/iaam-store/src/events.rs`, `find_duplicate`). Two layers,
one preventing and one detecting:

1. the row carries a stable key — the source's own operation identifier when it
   shows one, otherwise a hash of the row text plus an ordinal within its day, so
   a repeated import of the same day reproduces it;
2. the control balance catches whatever slips through, on the day it happens
   rather than six months later.

### 7. Reconciliation closes the loop

For every account, a control balance on the closing date, compared against the
sum of its operations. Three outcomes, and all three are shown: matched; off by
a stated amount; never stated. The report's discrepancy line (§2) and the
per-account reconciliation state (§4) are the same fact seen at two altitudes.

This is what makes the coarse rows of R9 safe. A month entered as one lump is
still checked: if the lump is wrong, the balance disagrees.

## Order of value

The cut is large, so it is ordered so that each step is worth having on its own
and none of them is thrown away:

1. **Balances and reconciliation** — "сколько у меня есть и насколько я в это
   верю". Uses machinery that already exists.
2. **Input**: the declared source (iaam-5jhq), the column mapping and the file
   importer, the paste channel. August's operations land in the journal.
3. **The flow report without categories** — the six quantities, the identity and
   the discrepancy. This is already an honest number, and Actual Budget never
   produced one.
4. **Categories** — the two-level list, the source-category mapping rules, the
   description rules, intervals. This turns the number into "и куда".
5. **Recomputation that shows its work** — the diff when a rule changes.

Steps 1–3 answer "сколько пришло и сколько ушло". Step 4 answers "куда".

## Form factor

Founding design §2 decided "на старте UI не делаем — интерфейсом служит агент
через REST", and §18 puts the web UI at the last stage. That decision was made
for an investment system, where the agent does bulk work and the owner reads
reports: rare actions, large portions.

Expense tracking inverts it. Reviewing a transaction feed, answering "what was
this?" across dozens of rows, configuring a column mapping, checking balances —
this is high-frequency interactive work over a table, and a dialogue is a poor
medium for a table of two hundred rows.

**Decision (owner, 2026-09-01): the API first, a thin UI immediately after.**
Not E8 in full — three screens: import with column mapping, the transaction feed
with its questions, flow and balances. The REST layer is required under either
choice and the web layer is its client (§17.4), so nothing is wasted. August is
read through the agent, as a table in the terminal, because waiting for the UI
means waiting past the point of this cut.

This amends an accepted decision and should be recorded as an ADR; without one,
the departure from §2 will be unexplainable in six months.

## Out of scope

- **Budgets and envelope planning.** `Budgeted` is unused in the tool being
  replaced.
- **The Actual Budget migration.** Valuable — it would move history already
  corrected, so that August has something to be compared against — but its
  transfer pairing is a problem of its own and the owner's self-hosted export
  format has not been inspected. It follows this cut.
- **The thin web UI itself** — its own epic, immediately after this one.
- **§8 Вклады in full.** Here a deposit is an account with a balance and
  movements; projecting interest accrual from contract terms is its own epic.
- **Broker-grade reconciliation.** Control balances only.
- **Deterministic per-source parsers.** They follow the paste channel, one
  source at a time, and land as the same structured operations.
- **Revaluation of positions inside the identity.** The identity is over cash;
  position value is reported beside it as an estimate (R10).
