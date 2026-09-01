# The flow of money: income and expenses as the first goal

Brainstorming bead: iaam-ovq0. Date: 2026-09-01. Revised 2026-09-01 after the
owner's pivot.

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

## The pivot

The owner's words on 2026-09-01: **"я понял, что не получаю ценности сейчас"**.
August has just closed and he wants to enter it and see where his money went.

The earlier revision of this document described the whole replacement — a
mapping importer, deduplication, an Actual Budget migration, and a thin web UI.
That is weeks of work, and for all of those weeks the owner still sees nothing.

The diagnosis: **the bottleneck is not input, it is that there is nothing to
look at.** `POST /v1/ingest/operations` already accepts operations with a
per-row verdict; classification already asks "is this a transfer to yourself or
a spend?" and learns the answer; control balances already exist. What does not
exist, in any form, is a report of the flow of money. Data can be entered today
and cannot be seen by anything.

So the order is inverted:

1. **The flow report and balances** — days. August becomes visible.
2. **Deterministic file import with a column mapping** — what makes the habit
   monthly rather than one-off.
3. **The thin web UI** — by then it has something to show, and the owner knows
   which screen he actually opens.

August itself is entered **through the agent, once, as a bridge** — not as a
permanent channel. Its rows are marked with agent provenance, because that
parse is not reproducible. The control balance on 31.08 checks them anyway.

Rejected explicitly: building the web UI before the report. Its first screen
would show emptiness, and the owner would again get no value.

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
  `Маркетплейсы`, …), so hierarchy is used.

The owner's own summary: the category view has been opened only a few times in
years and says nothing. **That is a consequence, not a preference.** The sum the
categories decompose is not a real number, so the decomposition cannot be
informative.

## Requirements, as agreed 2026-09-01

- **R1.** All accounts — cards, cash, deposits, broker accounts — are one
  owner-defined contour. A transfer between them is neither income nor expense.
- **R2.** The flow report answers honestly what came in from outside, what went
  out, what the capital earned on its own, what was moved into assets, and what
  that leaves.
- **R3.** Balances for every account, each carrying whether it has been
  reconciled and when. An account unverified for six months must not look like
  one verified yesterday.
- **R4.** Operations arrive from four channels: a bank export file, text pasted
  from a bank's web page, manual entry, and a one-off migration from Actual
  Budget. Only the manual/agent channel is in the first cut.
- **R5.** Integrity is defended **primarily** by **control balances** rather than
  by cross-channel independence: the owner states the balance on a date and
  reconciliation compares it to the sum of operations. Independence is not
  forbidden — the mapping importer described under "Later stages" makes it
  reachable for an account that has two channels — but nothing may depend on it,
  because most accounts will only ever have one.
- **R6.** Categories are deferred by the owner's decision. Data that would feed
  them is retained from day one so that deferring costs nothing later.
- **R7.** No budgets, no planned amounts. `Budgeted` is unused in the tool being
  replaced.
- **R8.** **No code per bank, ever.** The owner's banks are MTS Bank and T-Bank
  today and "могут измениться или добавиться в любой момент". A bank's identity
  is data — a column mapping, a channel — never a branch in the code and never
  an assumption in the design. What one particular bank happens to export must
  not appear as a premise anywhere in this document.
- **R9.** **One mechanic: everything is an operation.** A line from a statement
  and a line reading "прочие траты за август, −120 000" are the same kind of
  event, differing only in the provenance recorded on each. There is no
  "aggregate mode", no second code path, and no migration between modes.
  Granularity improves by replacing a coarse row with precise ones.
- **R10.** **Money reconciles; positions do not.** A cash balance is a fact an
  institution states and reconciliation can check. A position's value changes
  every second and cannot be checked against anything. They are two quantities
  with two different statuses and are never added into one number.

## What already exists

Verified in the tree, not assumed. This is most of the foundation:

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
  `crates/iaam-ingest/src/classification.rs`. `Counterparty` is
  `OwnAccount(AccountId)` / `Named(String)` / `Unknown`; an unresolved row
  yields `ClassificationResult::Ambiguous { question }` rather than a guess. The
  owner's answer becomes a versioned rule, and editing a rule recomputes history
  through reversal and replacement without rewriting the journal (bead
  iaam-8t7).
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

The flow report over an interval, naming a contour version, is six quantities,
one reference block, and a discrepancy:

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

**Taxes.** Tax withheld at source is already a `LegKind::Tax` leg. A
self-paid tax (property, transport, a filed return) is currently
indistinguishable from ordinary spending, so it needs its own event kind,
modelled on the existing `Fee { amount, origin }` — a cost not tied to a trade,
classified `WithinAccount`, surfaced as its own line. Without it, "minus taxes"
cannot be separated from "went out", and discretionary spending stays
overstated.

### 3. Balances, and why two numbers never become one

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

### 4. Entering August

Through the agent, once. The existing `POST /v1/ingest/operations` takes the
rows; each is marked with agent provenance, because an agent's parse is not
reproducible and must never claim to be. Integrity comes from R5: the control
balance on 31.08 for every account.

Under R9 the granularity is the owner's choice per row. Where a statement is at
hand the rows are precise; where only a balance is known, one coarse row carries
the month. Both are operations; only the provenance differs.

**One blocking defect.** `POST /v1/ingest/operations` mints
`SourceId::new_random()` per request (`crates/iaam-server/src/routes.rs:1219`).
Re-submitting after fixing a mistake would therefore create duplicates instead
of replacing rows. The caller must be able to declare the source. This is bead
iaam-5jhq, which this cut promotes from an inconvenience to a blocker.

## Later stages, out of the first cut

### Input: one importer, a mapping per account, an agent where there is no export

**Not a parser per bank** (R8). The tool being replaced does not have one: it
has a single CSV importer and a **column mapping the owner configures once per
account** — a date column, a description column, an amount column, plus date
format, delimiter, lines to skip, and sign inversion.

A column mapping is **data, not code**. This is the same argument the owner
already accepted in epic iaam-d8b.2.2, which moved the broker operation-kind
dictionary into the database precisely so that extending it needs no release.
So: one importer, and a versioned mapping stored per account. A mapping is
deterministic and reproducible, it has a real version, and re-importing the same
file yields the same result.

**The agent channel is for accounts with no export**, where the owner copies
rows from a web page. Its parse is not reproducible — the version describes a
model and a prompt, not code — so this channel **declares itself as such and
never claims independence**. The §10.3 criterion (`parser_version != other &&
document != other`) is meaningless for it. Its integrity comes from R5.

This is not a breach of ADR-0001, which governs where *we* parse a protocol we
implement. The agent is an external client (founding design §2, "ИИ — внешний
клиент") using the documented structured route. A future deterministic bank
parser, however, does belong in its own adapter crate by ADR-0001's reasoning —
not in `iaam-app`.

**Independence is recoverable later.** The same account exported and parsed by
the mapping importer, versus pasted and parsed by the agent, are two channels
differing in both parser version and document. `accepted_independent` becomes
genuinely reachable for such an account. It is not reachable for an account with
only one channel, and that is a true statement rather than a gap.

**A source's own category column is a hint, not the owner's decision.** It is
retained separately from any future owner category, or the source silently
decides what a purchase was.

### Deduplication, which copy-paste makes hard

Overlapping pastes are the normal case: the owner selects January, then
January–February. Two obstacles, both real:

- **The declared source must be per (account, channel)**, not per account. The
  export channel and the agent channel of the same account must stay distinct
  source identities, or the independence described above collapses: two channels
  sharing one source could not be told apart, and a pasted row would deduplicate
  against an exported one instead of confirming it. This is iaam-5jhq, whose
  first half the first cut already needs.
- **There is no natural key.** The store deliberately refuses "account + date +
  amount" because two identical purchases in one day are legitimate
  (`crates/iaam-store/src/events.rs`, `find_duplicate`).

Two layers, one preventing and one detecting:

1. The row carries a stable key: the source's own operation identifier when it
   shows one; otherwise a hash of the row text plus an ordinal within its day,
   so a repeated import of the same day reproduces it.
2. The control balance catches whatever slips through. A double-counted row
   makes the balance disagree, on the day it happens rather than six months
   later.

### Migrating from Actual Budget

A one-off import that moves the history **already corrected**. Rows reading
`Между своими счетами` stop being expenses and become `cash_transfer`, so the
owner gets comparable prior months instead of an empty system with nothing to
compare against.

What moves: accounts including the off-budget ones; operations with dates,
descriptions and amounts; categories — **retained though unused**, per R6.

**Transfer pairing is the hard part.** The export shows both halves as separate
rows: `+1 300,00` at 17:13:12 and `−1 300,00` at 17:13:11. Pairing is by equal
magnitude, opposite sign, different accounts and proximity in time. Where a pair
is unambiguous it is joined; where two transfers of the same amount occur in one
day, the importer **asks rather than guesses**, consistent with how
classification already behaves.

The export format of the owner's self-hosted instance has not been inspected and
is not guessed at here; confirming it is the first task of that work.

### Form factor: the agent is not enough in the long run

Founding design §2 decided "на старте UI не делаем — интерфейсом служит агент
через REST", and §18 puts the web UI at the last stage. That decision was made
for an investment system, where the agent does bulk work and the owner reads
reports: rare actions, large portions.

Expense tracking inverts it. Reviewing a transaction feed, answering "what was
this?" across dozens of rows, configuring a column mapping, checking balances —
this is high-frequency interactive work over a table, and a dialogue is a poor
medium for a table of two hundred rows. No prompt fixes that.

**Decision (owner, 2026-09-01): the report first, the deterministic importer
next, a thin UI after both.** Not E8 in full — three screens: import with column
mapping, the transaction feed with its questions, flow and balances. The REST
layer is required under either choice and the web layer is its client (§17.4),
so nothing is wasted.

This amends an accepted decision and should be recorded as an ADR; without one,
the departure from §2 will be unexplainable in six months.

## Out of scope

**Out of the first cut** — flow report, balances, `EventKind::Tax`, a declarable
source, August entered through the agent:

- the deterministic file importer and its column mappings;
- deduplication by stable row key;
- the Actual Budget migration;
- the thin web UI;
- revaluation of positions inside the identity.

**Out of the epic entirely:**

- **Categories and the category report.** Deferred by the owner; the data that
  feeds them accumulates from day one.
- **Budgets and envelope planning.** `Budgeted` is unused in the tool being
  replaced.
- **§8 Вклады in full.** Here a deposit is an account with a balance and
  movements; projecting interest accrual from contract terms is its own epic.
- **Broker-grade reconciliation.** Control balances only; no independence levels
  for bank accounts beyond what the mapping importer makes reachable naturally.
- **Deterministic per-bank parsers.** They follow the agent channel, one account
  at a time, and land as the same structured operations.
