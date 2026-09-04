# HTTP API conventions

This document is about **IAAM's own HTTP API**, the one rooted at `/v1` and
described by `/v1/openapi.json`. The contracts under `tinkoff-invest/` are
somebody else's API, read here as a reference; nothing in this file applies to
them.

The reader is a client author. Each section states a rule that every existing
route already follows, so that the shape of a route can be predicted before it
is called rather than discovered by calling it.

---

## 1. The shape of a list response

> **A list is a bare JSON array, unless the response carries something about the
> list itself — a page cursor, a version the items were computed under, a
> statement of what the list covered — in which case it is an object with
> `items` and that something beside it.**

### 1.1 Why the bare array is the default

A wrapper is not free. `{"items": […]}` costs the client one indirection on
every read, and it costs it forever: once published, the key cannot be removed
without breaking every caller. What a client buys for that price is a place to
put a fact that belongs to the answer as a whole. Where there is no such fact —
`GET /v1/accounts` returns the accounts, all of them, in one response, and there
is nothing true of the set that is not true of each member — the wrapper is an
empty box, and an empty box that can never be thrown away.

So the bare array is not laziness and the wrapper is not ceremony. The shape
reports whether the answer has something to say about itself.

### 1.2 Why a wrapper, when there is one

**`GET /v1/journal/events` → `{"rows": […], "next": …}`.** The journal is read a
page at a time, and the position to resume from is a property of the page, not
of any row in it. It is also the one field that cannot be reconstructed from the
rows: an absent `next` means "this was the last page", which no row states.

The same reasoning already appears in the code, at the type that first needed it:
`BalancesReportDto` is an object rather than an array of account rows because
`negative_cash` is one fact about the whole answer, and `MarketPriceSeriesDto`
carries `complete_through` for the same reason — it is returned even when `rows`
is empty, which is the only way a client can tell "this instance holds nothing
for the series" from "the series is complete and holds nothing in this interval".

### 1.3 The field beside the list is named for what the list is

The rule fixes that there **is** a field holding the list, not one universal
spelling of it. `items` is the default and the name to reach for; a route whose
payload has a more precise word uses it — `rows` for the journal page and for
the market series, `accounts` for the balances report, `statuses` and `gaps` for
reconciliation, which carries three lists and could not have named any of them
`items` without lying about the rest.

A client should therefore read the shape as: array at the top level, or an object
whose documented list field it looks up once.

### 1.4 A statement of what the list covered is list-level information

`GET /v1/reports/balances` and `GET /v1/reports/returns` both carry a
`population` block: the accounts the report covered and the known accounts it did
not. This is the third case named in the rule, and it is the clearest of them.
It cannot be a field on a row, because the accounts it reports on are precisely
the ones that have no row — the report left them out, and that silence is what
the block breaks. A report over part of the owner's money that did not say so
would read as an answer about all of it.

So `population` is exactly the kind of fact the wrapper exists for.

### 1.4a A wrapper that turned out to hold nothing: `GET /v1/actions`

`GET /v1/actions` wrapped, and it was cited here as the clearest case: the items
are computed under a policy, and a policy has a version, so `policy_version` sat
beside `items` as a fact about the whole computation.

It never was one. The field was the literal `1`, written at the single place the
response was built; nothing derived it and nothing bumped it, and it never
changed across any release. A client told to compare it between two responses
would have found them equal forever, which is worse than having nothing to
compare: it reads as evidence that the rules did not change, and it was never
evidence of anything. The three other responses that carry the same items — the
reconciliation answer, the broker sync outcome, the money-flow report — carried
no version at all, so the promise was not even made consistently.

The route is a bare array now. This is §1 working rather than an exception to
it: the question in §1.5 is whether there is a fact about the answer as a whole
that no item can carry, and the honest answer for this route was no. A
`policy_version` may come back, and if it does it comes back derived from
something that moves and published everywhere `ActionDto` is published — not
beside one of the four.

### 1.4b A count is named as a count

Where a response does publish a count — because the thing counted is not in the
response and the client therefore cannot count it — the field is named so that it
cannot be read as a list. `ImportSessionContentsDto.row_count` and
`SourceInventoryDto.row_count` are both `row_count` and not `rows`, and the
suffix was bought with a client's mistake: `rows` sat beside `questions`, a list
of one-row-shaped items, and an external agent wrote `len(rows)` against it twice
before reading the field description.

The rule has a companion in §1.3: `rows` is the right name for a list of rows,
and `POST /v1/import-sessions/{session}/commit` uses it for exactly that. One
word cannot be both, and the count is the one that gives way — a client that
indexes into a list it was given is doing the ordinary thing.

### 1.5 What this means when a new list route is added

Decide the shape when the route is published, because it cannot be changed
afterwards without breaking callers. The question to answer is not "might this
list grow?" but "is there, or will there be, a fact about the answer as a whole
that no item can carry?" A page cursor, a version, a coverage statement, a
completeness boundary — those are the facts. A count is not one: a client that
holds the whole array can count it.

An example, invented: a route returning the owner's accounts at `Bank One` and
`Bank Two` is a bare array. The same route, if it reported which banks it managed
to reach and which it could not, is an object — because "Bank Two did not answer"
is not something an account row can say.

---

## 2. The lists as they stand

Every route whose success response is, or contains, a list of the owner's
things. Read it as the lookup table for §1.

| Route | Response | Shape | Why |
|---|---|---|---|
| `GET /v1/accounts` | `[AccountDto]` | bare array | whole list, nothing about the set |
| `GET /v1/instruments` | `[InstrumentDto]` | bare array | whole catalogue |
| `GET /v1/categories` | `[CategoryDto]` | bare array | whole history; each item carries its own retirement |
| `GET /v1/category-groups` | `[CategoryGroupDto]` | bare array | whole list |
| `GET /v1/category-rules` | `[CategoryRuleDto]` | bare array | whole history; the version is per rule, not per list |
| `GET /v1/classification-rules` | `[ClassificationRuleDto]` | bare array | whole history; the version is per rule, not per list |
| `GET /v1/tokens` | `[TokenDto]` | bare array | whole list, revoked included |
| `GET /v1/broker-access` | `[BrokerAccessDto]` | bare array | whole list, revoked included |
| `GET /v1/contours` | `[ContourDto]` | bare array | whole list; each contour carries its own version |
| `GET /v1/import-sessions` | `[ImportSessionDto]` | bare array | whole list, newest first |
| `GET /v1/actions` | `[ActionDto]` | bare array | the whole queue; nothing true of the set that is not true of each item (§1.4a) |
| `GET /v1/journal/events` | `JournalPageDto` | object, `rows` | `next` — the position to resume the page from |
| `GET /v1/market/prices` | `MarketPriceSeriesDto` | object, `rows` | `complete_through` — how far the series is known |
| `GET /v1/market/fx` | `MarketFxSeriesDto` | object, `rows` | `complete_through` |
| `GET /v1/market/key-rate` | `MarketKeyRateSeriesDto` | object, `rows` | `complete_through` |
| `GET /v1/reconciliation` | `ReconciliationResponseDto` | object, `statuses` | three lists — `statuses`, `gaps`, `actions` — none of them a property of another's rows |
| `GET /v1/transfer-pairings` | `CrossSourceMatchingDto` | object, `candidates` | `without_counterpart` — the legs nothing paired with, which no candidate can carry |
| `GET /v1/accounts/{id}/transfer-partners` | `AccountTransferPartnersDto` | object, `partners` | `stated` — whether the owner has ruled at all, which an empty array cannot say |
| `GET /v1/import-sessions/{session}` | `ImportSessionContentsDto` | object, `questions` | the session it belongs to, `row_count`, and how many questions are unanswered |
| `GET /v1/reports/balances` | `BalancesReportDto` | object, `accounts` | `negative_cash`, `population` |
| `GET /v1/reports/returns` | `ReturnsAnswerDto` | object | not a list at the top level; `population` sits beside the report's own figures |
| `GET /v1/reports/flow` | `MoneyFlowReportDto` | object, `currencies` | the interval, the scope version, `population`, `actions` |
| `POST /v1/ingest/operations` | `[VerdictDto]` | bare array | one verdict per submitted row, in the caller's own order |
| `POST /v1/ingest/journal-events` | `[VerdictDto]` | bare array | one verdict per submitted row |
| `POST /v1/ingest/csv` | `[VerdictDto]` | bare array | one verdict per parsed row |
| `POST /v1/corrections` | `[VerdictDto]` | bare array | one verdict per submitted correction |
| `POST /v1/reconciliation/balance` | `OwnerBalanceOutcomeDto` | object, `statuses` | `control_assertions` — whether each claim reached the journal or was already held there, which no status row can say |
| `POST /v1/import-sessions/{session}/rows` | `[ImportRowDto]` | bare array | one outcome per fed row; nothing was recorded |
| `POST /v1/documents` | `DocumentDto` | object, `rows` | the document hash, source, parser version and period |
| `POST /v1/brokers/{broker}/sync` | `SyncOutcomeDto` | object, `recorded` | the duplicate and assertion counts for the run, and `actions` |
| `POST /v1/import-sessions/{session}/commit` | `ImportCommitDto` | object, `rows` | the session and the revision the commit was planned from |
| `POST /v1/classification-rules` | `ClassificationRuleChangeDto` | object, `plan.corrections` | `applied: false` — that the plan was not carried out |
| `DELETE /v1/classification-rules/{id}` | `RecomputePlanDto` | object, `corrections` | `applied: false` |

A batch response — a verdict per submitted row — is a bare array for the same
reason a full list is: the caller supplied the batch, so it already knows what
the array is about, and each verdict carries the row number that ties it back.
Where a batch response is an object, it is because the run produced a fact of its
own: a document hash, a set of counts, a revision.

---

## 3. Naming a thing the owner named

> **Wherever the API prints the identifier of a thing the owner named, it prints
> his own name for it beside it: `title`, and `institution` too for an account
> he gave one for. A name is never accepted as input.**

### 3.1 Why the name travels with the identifier

The whole interaction this API serves is the system asking the owner something
and the owner answering. `GET /v1/actions` is the clearest case: it returns one
item per account, and the items of a kind differ from one another in nothing but
a UUID. An agent that had run an import and asked what remained received a dozen
`record_owner_balance` items and could not tell which bank any of them was about
— not because the fact was unavailable, but because it was in a different
response. Reading the queue meant fetching `GET /v1/accounts` and joining.

A join every client must write is a join every client can get wrong, and a
question the owner cannot read is a question he cannot answer. The account
already holds everything needed to ask it properly — he supplied the title and
the institution once, when he created it — so making him supply the connection
again, every time, is asking twice.

The same reasoning is already written in the core, at the first type that needed
it. `PopulationAccount` carries a title beside the identifier because "the
manifest exists to be read: an owner asked to rule on an account cannot act on a
bare UUID, and a caller that had to fetch the names separately would be free to
render the manifest without them." An action item is that sentence with the same
reader.

`ImportQuestionDto.accounts` is the same reasoning reaching the last place on
the import path that had not had it. Two of the six answers to a classification
question name one of the owner's own accounts, and the question published only
`needs_account: true` — so a client answering one called `GET /v1/accounts` and
joined, which is the join §3.1 exists to remove. The candidates are the
question's own: the account the row is already on is not among them, because an
account is not the other side of itself, and they are built by the function that
builds the action queue's `/account` field so that the two cannot offer
different lists for one question.

`institution` travels with `title` for the case a title alone cannot settle. Two
accounts the owner calls `Savings`, at two banks, are one word apart in a list
and are not the same question. It is absent, never null and never guessed, when
he has not said where the account is held — an invented institution would tell
two accounts apart by a fiction.

### 3.2 Why a name is never accepted as input

A name is not an identity. The owner renames an account when the bank renames a
product; two of his accounts may carry one title; a title is a string a client
can autocomplete, mistype, translate or invent. A request that resolved an
account by name would have no way to fail on a plausible wrong answer — it would
address the wrong account and succeed, which is worse than a refusal.

The identifier has the properties the name does not: opaque, unique, stable
across every rename. And a client that took it from a response cannot have made
it up, which is the property that matters most when the client is a language
model. Every identifier a request carries was copied out of an earlier response.

There is a second cost. If a title were addressable, renaming an account would
break every stored request that named it, and the owner would be choosing
between a name that reads well and a name nothing depends on.

### 3.3 Why the asymmetry is the protection

Reading and writing are not the same act, and the rule points the same way in
both.

Output is read by a person deciding something. It must be legible, so it carries
the name.

Input is composed by a machine acting on his behalf. It must be unambiguous, so
it carries the identifier and nothing else.

Made symmetric in either direction the pair loses one of those. Accept names as
input and the ambiguity of names — renames, duplicates, near-misses — reaches
the one place in the system that must have none. Withhold names from output and
the owner is asked to rule on something he cannot read. So the asymmetry is not
an inconsistency to be tidied away: it is what lets a client obey a single rule
with no exceptions — **it never composes an identifier and never resolves a
name.**

### 3.4 The name is paired with the identifier where the answer is made

The pair is built in the component that computed the answer, not joined onto it
by the transport. This is §1.4a's sibling: a second, independently-derived copy
of a fact is a fact that can disagree with itself.

`ActionSubjectDto::Account` is filled from `iaam_app::actions::AccountSubject`,
which the action policy builds at the moment it builds the item, out of the same
account list it wrote the item's `reason` sentence from — and that sentence names
the account too. Joined at the transport, the name beside the identifier would
come from a second reading of the store, and one response could name one account
two ways. The transport copies; it does not look up. `ConfidenceDto::from_domain`
carries the same note for the same reason: "a register assembled here could
disagree with the answer printed beside it."

That decision has a visible consequence. The diagnostic functions over a
reconciliation ledger and a money flow report are given the owner's accounts as
an argument, because a ledger holds identifiers and no names; and if they name
an account the list does not hold, they refuse the whole answer rather than
publish an item the owner cannot act on. Nothing deletes an account, so the
refusal reports the store contradicting itself and nothing else.

### 3.5 What carries the name, and what does not

Every published type that prints an identifier of a thing the owner named.

| Published type | Identifier | Name beside it |
|---|---|---|
| `AccountDto` | `id` | `title`, `institution` |
| `AccountCandidateDto` | `id` | `title`, `institution` |
| `ActionSubjectDto::Account` | `id` | `title`, `institution` |
| `AccountScopeDto` | `account` | `title`, `institution` |
| `PopulationAccountDto` | `account` | `title` |
| `ImportQuestionDto` | in `prompt`, and `accounts[].id` | the titles, in the sentence; `title` and `institution` on each candidate |
| `ContourDto` | `contour` | `title` |
| `CategoryDto`, `CategoryGroupDto` | `id` | `title` |
| `InstrumentDto` | `id` | `symbol`, `title` |
| `TokenDto` | `id` | `label` |
| `BrokerAccessDto` | `id` | `broker` |

And the types that print a bare identifier on purpose.

| Published type | Identifier | Why no name |
|---|---|---|
| `ActionSubjectDto::Event` | `id` | nothing the owner said names an event; the identifier is the whole of its identity and the item's `reason` states what it was |
| `AccountBalanceDto`, `NegativeCashDto`, `AssetAccountDto`, `CashClassTotalDto`, `NotDecomposedAccountDto`, `AccountResidualDto`, `EarningSourceAmountDto`, `CaveatSubjectDto` | `account` | the answer these sit in carries `population`, whose `covered` and `outside` name every account it mentions and every account it left out. The join table is in the same response, computed by the same fold, and one report row cannot disagree with it |
| `ReconciliationStatusDto`, `TaintDto` | `account` | the caller named the account in the request; the response answers about that one and no other |
| `JournalEventReadDto`, `JournalLegDto`, `OperationDto`, `VerdictDto` | `account` | a row-level echo of what the caller submitted or asked for |
| `BatchTotalDto`, `ControlComparisonDto`, `PlannedOriginDto` | `account` | the import assessment they sit in carries `account_resolution`, whose `resolved` and `missing` name every account the rows are on and say which of them the owner's directory holds. The join table is in the same response, computed by the same fold. It is also why a name cannot simply be printed: a total over rows on an account the directory has never heard of is precisely what these sections must be able to publish |
| every request body | — | §3.2 |

Two things follow for a client. Where a response carries a `population` block, it
is the name table for every account named anywhere in that response — look the
account up there rather than calling `GET /v1/accounts`. Where a response carries
neither the name nor a population, the account is the one the request named.

### 3.6 Where the rule is not yet kept

Named here rather than left to be discovered, in the spirit of §1.4a. Each is a
surface that asks the owner to act on an account, a contour or a category and
prints only its identifier, with no name table beside it in the same response:

- `AccountTransferPartnersDto` and `AccountTransferPartnersStatementDto` —
  `account` and `partners` are bare, and `partners` is precisely a list of the
  owner's own accounts he is being asked to confirm.
- The import session assessment — `SourceInventoryDto.accounts`,
  `AccountResolutionDto.resolved` and `.missing`, `ScopeAssessmentDto`'s three
  lists, `PlannedFactDto.account`, `TransferLegDto.account`. The session's
  questions do keep the rule; the assessment printed beside them does not.
- Contour references: `AccountScopeDto.contours`, `ContourDto.accounts`,
  `ContourVersionDto.accounts`, `PopulationDto.contour`,
  `MoneyFlowReportDto.contour`, `AppliedRulesDto.contour`.
- Category references: `CategoryDto.group`, `CategoryRuleDto.category`,
  `CategoryAmountDto.category`, `ClassifiedAsDto.to`, `CategoryMoveDto`.
- Instrument references outside the catalogue: `HoldingValueDto`,
  `PositionQuantityDto`, `CaveatSubjectDto::Instrument` and the position types.
  An instrument is named by a catalogue rather than by the owner, so this is the
  weakest of the five; it is listed because the reader's difficulty is the same.

Each of these is a shape change to a published type, so each is its own decision
about breaking a client, and none of them is a reason to publish a new type that
prints an identifier alone. A type added after this section carries the name.

---

## 4. What gates an act

> **What gates an act is what it does to the owner's decisions, not which
> endpoint it arrived at. Two acts that reach the same consequence are gated the
> same way, even when one of them is a side effect of a route named after
> something else — and one route that performs two acts of different consequence
> is gated twice, once per act.**

### 4.1 Why the endpoint is the wrong unit

There are three scopes — owner, agent, read-only — and the temptation is to read
them as three lists of routes. That reading fails in both directions, and it had
failed in both by the time this section was written.

It fails downward, by leaving a hole. `POST /v1/classification-rules` was
owner-only, because a standing rule classifies rows nobody has looked at yet.
Answering an import question was open to the agent, because an import is
mechanics. But answering also wrote a classification rule, so the decision the
agent could not make directly it made through a route whose name does not
mention rules. The agent-facing document forbade it in prose — *never answer one
yourself; relay what he says* — and prose is not a gate. The endpoint list said
the rule route was protected while the thing it protected was reachable
elsewhere.

It fails upward, by refusing what it has no reason to refuse.
`POST /v1/corrections/imports` was owner-only, and the reason recorded in the
code was that "a reversal rewrites what every downstream report says, and the
agent is an external client that does not decide the portfolio's shape". The
first clause is true. The second was already false when it was written:
committing an import session is open to the agent, and it rewrites every
downstream report just as thoroughly. The agent does decide the portfolio's
shape — by adding to it. The gate closed the safer of the two directions and
left the other open, and the practical result was an agent that discovered by
control total that it had written nonsense and could do nothing but wake the
owner to undo the agent's own mistake.

Both defects have one cause. Scope was being drawn around routes, and a route is
a name, not a consequence.

### 4.2 The rule

Ask what the act does to decisions the owner has made or will have to live with.

- **It disposes of something the caller itself submitted, and nothing else
  changes.** Agent. Recording a row, settling a row, committing an import,
  abandoning a session.
- **It states a standing decision that will apply to things nobody has looked
  at.** Owner. A classification rule, a category rule, a contour version, an
  account and its declarations, a token.
- **It rules on what is already in the journal — which of the owner's facts
  should stop counting, or what should have stood instead.** Owner.
- **It returns the journal to the state before the caller acted, reversing no
  decision of the owner's, because he made none about it.** Agent, under a bound
  narrow enough to be checked rather than trusted. Today this is exactly one
  act: §4.5.

Read-only is the absence of all four.

### 4.3 What each scope may do

| Act | Owner | Agent | Read-only |
|---|---|---|---|
| Read any report, the journal, the action queue | yes | yes | yes |
| Submit rows, open and feed an import session | yes | yes | no |
| Settle one row by answering its question | yes | yes | no |
| Generalise that answer into a standing rule | yes | **no** | no |
| Commit or abandon an import session | yes | yes | no |
| Synchronise a broker or the market reference | yes | yes | no |
| Repair custody, upload and reparse a document | yes | yes | no |
| Retract an import the caller declared, untouched | yes | **yes** | no |
| Retract any other import | yes | no | no |
| Reverse or replace a named journal event | yes | no | no |
| Confirm a transfer pairing (writes two corrections) | yes | no | no |
| Record a control balance | yes | no | no |
| Write classification, category and account rules | yes | no | no |
| Create accounts, contours, categories, instruments | yes | no | no |
| Issue and revoke tokens and broker access | yes | no | no |

Two rows of that table are the ones §4.1 got wrong, and each is stated below with
the line drawn where it is.

### 4.4 Answering a question, and generalising the answer

Answering an import question does two things of different consequence, and they
are now gated separately inside one route.

Settling the row is mechanics: it disposes of one line the caller already
submitted, and the portfolio changes by exactly that line. Writing the
classification rule generalises the settlement into a decision that will classify
rows nobody has looked at — from months not yet imported, and including rows that
will never be shown to anyone, because a row a rule matches is never asked about.
That is the same act `POST /v1/classification-rules` performs, so it is gated the
same way, and the agent's answer now settles the row and writes no rule.

The route was not closed to the agent, because the agent's half of it is
legitimate and closing it would stop the import. What comes back instead of a
bare `rule` identifier is `generalisation`, which names its own state:
`recorded` when the answer created a rule, `available` when one was possible and
the answerer may not write it, `impossible` when the row offers nothing a matcher
could match on, and `unanswered` while the question is open.

The identifier alone could not separate the middle two. Both were an absent
field, and only a client could tell which applied, because only a client knows
which token it holds. The owner reading a session back could not — he saw a
question answered and no rule — and he is the one for whom the difference is
actionable.

`available` therefore carries the rule itself, in the exact body
`POST /v1/classification-rules` takes, so adopting the agent's settlement is one
call with an object copied unedited. The cost of the split is still real — the
same counterparty is asked about again next month until the owner adopts it — but
it is a call rather than a reconstruction, and what he gets is a decision in his
own vocabulary that he can read back, edit and retire.

Nothing links the adopted rule back to the question. `generalisation` says what
*this answer* wrote, so it goes on saying `available` after the owner posts the
proposal: the rule he created is his own act, and it is read back from
`GET /v1/classification-rules` like any other.

The proposal is also an item in the action queue, kind
`adopt_classification_rule`, one per answered question that has one. A state the
system reports truthfully with no act that resolves it is a dead end dressed as
information, and `available` was one: the owner is the only principal who may
generalise, the queue is where he is told what only he can do, and nothing in it
mentioned the rule waiting for him. The item is `recommended` — the row it came
from is settled and no report is short of anything — it names `owner` as the
required scope, and its target is `POST /v1/classification-rules` with the
proposal preset as the body and no missing field. What is missing is his
decision, which is why the item's state is `needs_owner_input` rather than
`ready`.

Because the question goes on saying `available`, the item's completion is read
from his rules instead: it disappears once a standing rule of his classifies a
row like that one the way he answered — whether he sent the proposal as it stood,
narrowed it first, or had written something covering it last month. An item whose
completion could not be observed would never leave the queue, which is how a
queue is learned to be ignored.

The condition a proposal asks about is one field, not every field the row
printed — see decision 0008. A client showing the proposal to the owner should
show him that condition, because it is the part he may want to change before he
sends it.

### 4.5 Retracting an import, and retracting anything else

Retracting an import the caller declared is a return to the state before it
acted. No decision of the owner's is reversed, because he made none: the rows
appeared because the agent put them there. Refusing it protects nothing and costs
the owner an interruption to undo somebody else's mistake — while the far more
consequential act of *writing* those rows was never gated at all.

Retracting anything else is a judgement about the owner's history: which of the
facts he holds should stop counting. There the old refusal is right and stands.

The line between the two is a bound, and a bound is only worth as much as the
evidence behind it. An agent may retract an import when all four hold:

1. it named a **label** — the account, channel and label it submitted under. A
   request without a label reaches the rows that named no import at all, and
   those are nobody's declaration to take back;
2. **every** row the target covers was submitted under this very credential — not
   under some agent token. An import both parties fed is one the owner has a
   stake in;
3. every row of it is still effective — none reversed, none replaced. One that
   is not is one somebody has already ruled on, and sweeping the remainder would
   finish a judgement the agent did not make;
4. no control assertion of the owner's covers those rows. An assertion was
   compared against the projection over an interval; retracting rows inside it
   changes what the comparison was made against, and a reconciliation status he
   has already read becomes a statement about a journal that no longer exists.

Three consequences a client should expect. The refusal names which condition
failed, because "no" without the reason sends an agent to ask the owner for
something it could have fixed itself. Retracting twice is refused by condition 3,
and that refusal is also the answer to "did my first call land". And the
acknowledgement is still required and the reversal is still a journal fact, so
the owner sees what was done either way.

### 4.6 What the bound rests on, and what it does not know

Condition 2 needs an answer to "did this caller declare that import", and a scope
cannot give it: a scope says what a caller may do and never what it did. Nothing
else recorded it either — provenance answered which source, which parser and
which import, never which principal — so the honest state of affairs was that the
system could not know.

It records it now. Every submission stamps the credential it arrived under into
the event's provenance, at the one place every caller-driven input already passes
through for its permission check, so an input that forgets to stamp cannot write
anything. Provenance is the right home for it and not a table beside the journal:
it already answers where a fact came from, and a second place recording that is a
second place that can disagree with the fact.

What this does **not** know is worth stating, because each is a way the bound is
narrower or looser than it looks:

- **It is the token, not the client.** Two programs handed one token are one
  principal, and either may retract what the other declared. That is a
  consequence of how the owner keeps his keys, and the system cannot see past it.
  It cuts the same way for the owner: rows he submits under the agent's token are
  the agent's to retract.
- **A rotated token forgets.** Issue the agent a new token and it can no longer
  retract what it declared under the old one. The retraction becomes the owner's
  again, which is the safe direction.
- **Facts recorded before the field existed name no declarer**, and every rule
  reading it refuses on absence rather than assuming. No import that predates
  this change is retractable by an agent, ever. That is fail-closed by
  construction, and the schema version is what tells a reader that "no principal"
  means "written before anyone was recorded" rather than "written by nobody".

### 4.7 Where the check goes

A gate that depends on the journal belongs inside the operation that reads the
journal, not in a step before it.

Every one of the four conditions above is a predicate over the same events the
reversal is computed from, so the cost is one pass over a slice already in
memory, not one query. That is the smaller reason. The larger one is that a check
performed beside the operation runs against its own read of the journal, and
between that read and the write the owner can record the very assertion condition
4 exists to protect. The reversal would then be written against a journal that no
longer satisfies the precondition it was checked under. One read, one decision.

The transport keeps only the floor — the scope that cannot be right under any
journal, which for a retraction is read-only. Everything narrower is decided
where the evidence is.

### 4.8 What is not changed by any of this

Corrections do not ride the ingest transport, and that is unaffected. Submission
admits an agent, so a relation field on an ingest row would turn every intake
handler into a retraction surface guarded by a per-row check one input could
forget. A separate route with its own gate is the right shape; §4.5 changed the
gate and not the shape.

And the rule cuts both ways by design. A new route that turns out to write a
standing decision as a side effect is gated on that decision, not on its own
name — which is the whole of §4.1 read forwards instead of backwards.

---

## 5. A structure is never sent as a string

> **A field whose value is a structure is a JSON object on the wire, in both
> directions. It is never a JSON document encoded inside a JSON string, and the
> shape a route prints is a shape the route that writes it accepts unchanged.**

### 5.1 Why the read shape and the write shape must be one

Every other rule in this file can be learned from documentation. This one cannot,
because the client is a language model and the way a language model discovers a
write shape is by copying the shape it just read. A field that reads as one thing
and is written as another has no signal a client can follow: it has to guess, and
each guess costs a rejected request.

`POST /v1/classification-rules` used to take `matcher` and `outcome` as **strings
containing** the JSON that the listing prints — and an external client needed two
attempts to compose the write shape after reading the read shape. Both are now
objects on both sides, and `RuleMatcherDto` and `ClassifiedAsDto` are the same
types in the request body and in the response, so the round trip is a property of
the type rather than of two definitions that agree today.

### 5.2 Why a string is worse than an object even when it round-trips

A structure inside a string does round-trip, in the narrow sense: copy the string
back and the server parses it. What it does not do is say what may be put in it.
An object publishes its members in the specification; a string publishes
`type: string`, and the members exist only in prose the client has to find. It
also puts the encoding in the client's hands — escaping, key order, whether a
member may be omitted or must be `null` — none of which the client can verify
before sending.

The store keeps such values as opaque text on purpose: `import_sessions` and the
rule tables document exactly that, and the reason is that the store must not know
the classifier's vocabulary. That is a storage decision, and it stops at the
storage boundary. What reaches the wire is the parsed structure, read by the same
function the classifier reads it with — one reader, so a rule cannot be printed
in a vocabulary it may not be written in.

### 5.3 Where the rule is not yet kept

`GET /v1/category-rules` prints `matcher` as a string holding the stored JSON,
while `POST /v1/category-rules` documents an object. The write side does also
accept the string, in seven spellings, so a client that copies what it read is
accepted — the asymmetry is in what the two sides *say*, and a client that
follows the specification writes a shape it will never read back. Straightening
it means choosing one spelling of a category matcher and retiring the other six,
which is its own decision about breaking a client and is not made here.
