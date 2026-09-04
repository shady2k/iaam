# 0005. Two maps, two reasons, and only one of them is 0004's

Date: 2026-09-04 · Status: proposed · Bead: `iaam-5wh2`

## Context

Decision 0004 wrote its own falsification test:

> Or the owner finds himself maintaining a file that maps a source's identifier
> to a `provider_account_id` — then this is the account map with a new name, and
> the identity was never the source's after all.

He maintains two files. `--account-map` and `--counterparty-map` are passed to
`tools/tbank-csv-import/import.py` on every run, while the three-tier resolution
that decision built, the classification rules and the transfer statements sit on
the server unused — because the tool reaches its conclusions before it submits
anything.

That looks like the test tripping. It is not, and the four questions below say
why. Every answer is against the code; the function names are the citations.

### 1. What each map does today, and what would replace it

**`--account-map` has three jobs, and only the first is a mapping.**

- *It translates.* `resolve_accounts` reads `GET /v1/accounts` into
  `{title: id}` and looks the map's **values** up in it. So the file maps a name
  the export prints to an account **title**, and the resolution is by title.
  This is exactly the defect 0004 named in its context section: the workaround
  breaks on a rename at either end.
- *It is the contour.* In `build`, `accounts.get(row["Имя счёта"])` returning
  nothing is what makes a row `skipped_outside_contour`. The file decides which
  of the export's accounts are the owner's at all.
- *It supplies the row key.* `row_key` puts the resolved account identifier in
  front of the hash, so the key is stable across runs and cannot collide with
  another institution's identical row.

The server-side mechanism that replaces the first job already exists and is
0004's: `AccountDirectory::candidates` tries iaam's own identifier, then
`provider_account_id` and the aliases (`identifies`), then the title, stopping
at the first tier that matches. `resolve_declared` is the same tiering for a
batch's declaration. Nothing replaces the second and third jobs, and nothing
needs to: a contour is a fact about the owner's accounts, and the directory is
where it lives.

**`--counterparty-map` has one job, and it is not about identity at all.** It
maps a string the export prints in its description column to an account title,
and `transfer_to_own_account` turns that row into a transfer between two of the
owner's accounts. The question it answers is *what was this row*, not *which
account is this*. The server-side mechanism that replaces it is a classification
rule: `RuleMatcher { counterparty_account | description_contains | kind }` with
the outcome `Classification::InternalTransfer { to }`, stored through
`POST /v1/classification-rules`, matched in `classify`, and — unlike the file —
listed, versioned, retirable, and able to say what it would correct in history.

### 2. What the tool concludes, split by who could possibly know it

**Format, which the export alone supplies.** The signed amount and what its sign
means, the timestamp shape, the fact that the two legs of an internal transfer
are posted seconds apart. `pair_legs` is here: the source itself prints a word
meaning "between my own accounts", and the pairing joins two adjacent rows of
equal magnitude inside one file. That evidence exists only while the file is in
one process's hands, and `transfer_pairing::propose` deliberately refuses to
decide on weaker evidence across files. Format conclusions stay in a converter.

**Identity, which the owner used to be the only source of and is not any more.**
Which account the printed account name is. Decision 0004 moved this: the printed
identifier can be declared once on the account, as `provider_account_id` or as
an alias with a validity interval, and both the declaration and — after
`iaam-varx` — every row may name the account with it.

**Judgement, which no export can supply.** That a printed counterparty is in
fact the owner's own account somewhere else. That a positive row carrying a
spending category is a merchant returning money rather than money arriving.
That a positive row under the bank's own interest category is the balance
earning rather than an arrival from outside. These are the rules in
`EARNED_BY_CAPITAL`, `ARRIVES_FROM_OUTSIDE` and `--counterparty-map`, and the
server has a home for all three: a classification rule is a judgement written
once and applied to every later import.

### 3. Whether retiring them is possible today

**`--account-map`: yes, today.** Nothing is missing. The identity tier exists,
the declaration takes it, and `iaam-varx` — in this branch — makes a row take it
too. What the owner declares once in iaam replaces what he passes on every run,
and the declaration is better than the file in the way 0004 predicted: it
survives a rename in iaam, it carries an interval for a card that was replaced,
and it is visible in `GET /v1/accounts` instead of living in a file nothing
versions.

**`--counterparty-map`: no, and the blocker is larger than the one
`docs/import-boundary.md` named.** That document names two gaps — `Classification`
has no refund, and an observation resolved as income carries no income kind —
and another agent owns both this wave. Reading `classify` shows a third, and it
is the one that decides the question:

> A row whose counterparty is not recognised as the owner's own account, and
> which no rule matches, is `ClassificationResult::Ambiguous`. Always.
> `question_for` is total: every combination of counterparty and direction
> yields a question. There is no outcome meaning "nothing suggests otherwise,
> so this is ordinary external flow".

So a converter that submits observations instead of conclusions raises **one
question per row** for every row the owner has not already covered with a rule.
A month of ordinary shopping becomes a month of questions. Against that, a
converter that concludes raises none. This is the wave's theme in its sharpest
form: the honest path is not merely poorer, it is unusable at the scale an
import actually has.

The loop that was supposed to absorb this cost does not close either.
Answering a question mints a rule from `matcher_for`, and that matcher sets all
three fields — counterparty, the **whole** description, and the source's word —
joined with "and". A rule made from one shop's row therefore matches that shop's
row and nothing else. Answering a hundred questions produces a hundred rules and
the next import asks a hundred more.

Retiring `--counterparty-map` means moving the owner's judgement onto the
observation path, and the observation path cannot carry an ordinary month yet.
It is blocked, and it is blocked on three things rather than two.

### 4. What genuinely stays outside the repository

The rule in `CLAUDE.md` is about the repository, and neither retirement puts
anything into it. What moves is *where the owner's judgement is stored*: out of
two JSON files on his laptop that nothing versions, and into his own database,
which already holds every fact about his money. 0004 settled that this is not
the same claim as committing a fixture derived from a real export, and the
reasoning holds unchanged here.

What stays outside, permanently:

- **The export.** The file, its path, and everything in it. No route takes one
  and none should — see `docs/import-boundary.md` §8.
- **The values.** No account identifier, no counterparty string, no title and no
  amount is written into this repository by either retirement. The tool keeps
  taking every such value at run time; what changes is that it takes them from
  the API rather than from a file.
- **The agent's distance from all of it.** The owner runs the converter. The
  agent writes it and reads what he pastes back. Nothing here moves that line.

## Decision

**1. Decision 0004's falsification test has not tripped, and stands.**

Neither file is "a file that maps a source's identifier to a
`provider_account_id`". `--account-map` maps a source's identifier to a **title**
and predates the identity; it is the workaround 0004 was written to remove, and
0004 removes it. `--counterparty-map` maps a printed **counterparty** to an
account, which is a claim about what a row was and not about which account a
string names. Recording this matters as much as recording a failure would: the
two files were being read as one piece of evidence against 0004, and they are
evidence about two different things.

**2. `--account-map` is retired against the identity 0004 gave an account.**

The owner declares the string his export prints in its account column once, on
the account, as `provider_account_id` or as an alias. The converter resolves the
export's account names against that identity — `GET /v1/accounts`, matched on
`provider_account_id` and alias values — instead of against a file of titles.
`--account-map` remains accepted, documented as the pre-0004 fallback, because
an offline preview has no directory to read and because an owner mid-migration
needs both paths to work on the same day.

**3. `OperationDto.account` takes the identifier the source prints
(`iaam-varx`).** The declaration was widened two waves ago and a row was not,
which left one flow answering "which account is this" in two vocabularies. It is
one field, one tiering — `AccountDirectory`, the same one the declaration and a
row's counterparty go through — and one refusal wording under two field names.
This is what lets a converter that never read the directory submit rows at all,
which is the second converter `docs/import-boundary.md` §4 describes: an agent
holding rows the owner pasted.

**4. `--counterparty-map` is not retired, and the condition for retiring it is
written down rather than left to judgement.** Three things must be true:

- an observation can resolve as a **refund**, so the tool's refund rule has
  somewhere to go;
- an observation resolved as income can carry the **income kind** the source's
  own category states;
- a row that nothing distinguishes settles as ordinary external flow **without a
  question**, or a rule made from an answer generalises past the single row it
  was made from.

The first two are `docs/import-boundary.md` §6 and belong to another decision
this wave. The third is stated here for the first time and is the largest of the
three.

**5. Until then, the arrangement is the current one, stated rather than
implied.** The owner's converter concludes about *what a row was*, because that
is the only place it can be concluded cheaply. It no longer concludes about
*which account a row is on*, because that stopped being a conclusion when 0004
gave an account an identity.

## Rationale

**The falsification test was written about identity, and only one file is about
identity.** A test that fires on any file the owner passes to an importer would
condemn `--export` as well. The test names its shape precisely — a source's
identifier, mapped to a `provider_account_id` — and honouring that precision is
the whole value of having written it down in advance.

**Splitting the two files is what makes either one actionable.** Held together
they support one conclusion — "the maps are still here, so nothing worked" —
which is false and leads nowhere. Held apart, one is retirable this afternoon
and the other has three named preconditions.

**A rule beats a file for the same reason an identity beats a title.** The file
is invisible to the system that acts on it: nothing can list it, nothing records
when it changed, and a wrong line in it is discovered as a wrong month in a
report. A rule is listed, versioned, retirable, and `recompute_history` says
what changing it would correct. The judgement is the owner's either way; where
it is written decides whether he can ever find it again.

**The third blocker had to be found by reading `classify` rather than by reading
the documents.** `docs/import-boundary.md` §6 counts the expressiveness gap
between the two channels as exactly two outcomes. That is true of the
vocabulary and false of the cost: the conclusive channel also settles every
ordinary row for free, and the observation channel asks about each one. A
decision that had retired `--counterparty-map` on the strength of the two named
gaps would have shipped a converter that turns a month into a questionnaire.

## What we rejected

- **Retiring `--counterparty-map` now by writing one rule per source category.**
  It does work for ordinary rows — a rule on `kind` with the outcome
  `external_flow` settles them, because the direction comes from the sign the
  source printed. It fails exactly on the rows the map exists for: a positive
  row under a spending category would settle as an arrival, which is the refund
  defect, and the report would gain income that never arrived. Rejected because
  it is correct for the easy rows and wrong for the ones that motivated the
  file.
- **Teaching the server the export's format so no converter is needed.** Already
  rejected in `docs/import-boundary.md` §8 and not reopened here.
- **Keeping `--account-map` as the only path because the tool must read the
  directory anyway.** True today, and it argues for the wrong thing: the read is
  for the contour and the row key, and neither wants a file of titles.
- **Widening `--account-map` into "at most a provider label", as 0004's
  consequences predicted.** Considered and found to be a smaller change than the
  evidence supports: with the identity declared on the account there is nothing
  left for the file to carry, and a file that carries one constant is a file the
  owner still has to remember to pass.
- **Making the converter's leg pairing a server concern.** `transfer_pairing`
  refuses to decide that two rows are one movement, on stated grounds, and the
  converter's pairing rests on evidence the server does not have: the source's
  own word for the movement and the two rows' adjacency inside one file.
  Rejected as a contradiction of a decision that is working.
- **Recording nothing, because 0004 turned out to be right.** Rejected: the
  appearance of a tripped falsification test is itself a finding, and the next
  reader will hit the same two files and reach the same wrong conclusion unless
  the split is written down.

## Consequences

**What becomes true.** The owner stops maintaining a file that maps his bank's
account names to iaam titles, and stops re-passing it on every run. An account
renamed in iaam no longer breaks an import. A card replaced at the source is an
alias whose interval closed, which the resolution already reads. A row may name
its account the way its statement prints it, so a converter holding pasted rows
and no directory can submit them.

**What it costs.**

- `OperationDto.account` is a string rather than a `Uuid` in the published
  schema. JSON always carried it as a string, so a client sending an iaam
  identifier is unaffected on the wire; what changes is that a malformed value
  is now one **rejected row** carrying `field: "account"`, where it used to fail
  the whole request at deserialisation.
- The converter's dry run stops being offline whenever it is asked to resolve by
  identity, because the identity lives in the directory. It still writes
  nothing. The offline path remains, through `--account-map`, and that is what
  the synthetic proof keeps using.
- An export account name the owner has not yet declared an identity for is
  silently outside the contour, where a missing line in the map used to be a
  hard refusal. The converter reports each unrecognised name on stderr, and that
  report has to be read before submitting.
- Two ways to name an account in a row now exist, and a caller may mix them
  within one batch. That is deliberate — the declaration has admitted both since
  it was widened — and the agreement check between a row and its declaration is
  made on the resolved account, so a batch cannot be split across two accounts
  by spelling.

**What this does not fix.** Everything in decision point 4. The tool still
concludes what every row was, still holds `--counterparty-map`, and still
submits to the conclusive route. `docs/import-boundary.md` §7's open questions
stay open, minus the one about the account map.

## How we will know this was wrong

Three signs, all checkable.

- **The owner ends up declaring identities he does not otherwise want**, purely
  to satisfy the converter — a made-up string per account, entered once and
  never printed by any source. Then the identity is not the source's after all,
  it is the map in the database, and this decision moved a file into a table.
- **An import lands on the wrong account** because a row named it by a string
  two accounts answer to. Resolution refuses an ambiguity rather than picking,
  so this can only happen through a tier being widened later; if it happens, the
  row-level widening was the wrong place to spend the tolerance.
- **`--counterparty-map` is still in use after all three conditions in decision
  point 4 are met.** Then the file is not what this decision says it is, and the
  reason it survives is something nobody has named yet.
