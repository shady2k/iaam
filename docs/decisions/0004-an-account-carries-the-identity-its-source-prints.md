# 0004. An account carries the identity its source prints

Date: 2026-09-03 · Status: proposed · Bead: `iaam-34f3`

## Context

An account in iaam is `{ id, title, institution }`. That is `AccountDto` and
`CreateAccountRequest` in `crates/iaam-server/src/dto.rs` and `AccountView` in
`crates/iaam-app/src/ports.rs`. Nothing in it says *which* account at the
institution it is. Three separate defects trace back to that single absence, and
a fourth is the workaround that hides them.

**The title is load-bearing, and it is a display name.** The only mechanism that
recognises a printed counterparty as one of the owner's own accounts is
`resolve_counterparty` in `crates/iaam-app/src/scenarios/import_session.rs`: it
compares the counterparty string against **account titles**, trimmed and
lowercased. That is the sole path to `Counterparty::OwnAccount`, and therefore
the sole path to an internal transfer derived without asking the owner a
question. The function's neighbour `counterparty_matches` exists only because two
accounts can share one title, and it refuses to pick between them — correctly.
So the fact that decides whether money left the perimeter hangs on a string the
owner may rename at any moment, with nothing recording that he did.

**The owner already answers a question nothing reads.** Before every import he is
asked which of his accounts money moves between this one and
(`resolve_transfer_relationships`, `crates/iaam-app/src/actions.rs`). Its answer
reaches exactly two places:
`grep -rn "list_account_transfer_statements" crates/ --include=*.rs` finds the
action queue that decides whether to keep asking, and the route that returns it.
It never reaches `import_session.rs`. The owner states the relationship and the
classifier does not consult it.

**There is no card anywhere.** `grep -rni "binding" crates/*/src --include=*.rs`
finds nothing. An external reviewer importing a real month hit two cards over one
underlying bank account. Modelled as one account, the client must merge the
source's two identifiers itself; modelled as two, that account's balance is
counted twice in any total.

**The workaround has the same defect as the thing it works around.** The import
tooling takes an `--account-map` supplied at run time, outside the repository
(`tools/tbank-csv-import/README.md`), and resolves the names in it against
`GET /v1/accounts` **by title**. So the map the owner maintains by hand breaks in
precisely the case it was built for: a rename at the source, or a rename in iaam.

One precedent matters, because it is this exact problem already solved one crate
away for a different noun. An instrument has `AliasRecord { namespace, value,
instrument, interval, source }` with `AliasNamespace` and
`AliasInterval { valid_from, valid_to }` in `crates/iaam-core/src/instrument.rs`.
iaam already knows how to give a thing a namespaced external identity with a
validity interval. It does it for securities and not for accounts.

## Decision

### 1. An account carries an external identity, supplied by the client

```
provider: String              // the client's own label for the source
provider_account_id: String   // opaque to iaam, unique within provider
```

`(owner, provider, provider_account_id)` is unique. Account creation upserts by
external identity: a create carrying an identity that already exists returns the
account created last time rather than minting a second one.

`provider_account_id` is **opaque to iaam**. It is not parsed, not shape-checked,
not validated against a register, and never rendered anywhere a title belongs.
The server's only operations on it are equality and uniqueness.

**iaam accepts whatever string the client sends. It does not require a
fingerprint, and it does not compute one.** This is the part the privacy
convention forces us to say out loud rather than assume.

The convention in `CLAUDE.md` governs *the repository*: nothing that identifies
the owner or his money is committed, and `scripts/check-no-personal-data.sh`
enforces it over tracked files and added lines. The owner's database is not the
repository. It lives on his machine, it already holds his entire journal, and no
guard here reaches it. So storing what a source prints is not forbidden the way a
fixture derived from a real export is forbidden — the two are different claims,
and conflating them would let a repository rule silently decide a product
question.

Both options are real, and each costs something the other does not:

- **The client sends what the source prints.** A mismatched import is debuggable
  by reading the value: the owner sees which account the importer thought it was
  addressing. The cost is that the identifier now sits in a database that also
  holds every fact about his money, and every path out of that database — a
  backup, a copy for support, a future export route — carries it. Nothing in iaam
  redacts it, and this decision does not add redaction.
- **The client sends a fingerprint it computes.** The printed identifier never
  exists inside iaam at all. The cost lands on the owner at the worst moment: a
  mismatched import shows him two opaque strings, and he cannot tell which
  account either belongs to without re-running the client's derivation. Worse, if
  the client ever changes how it derives the value, every account looks new and
  the next import silently mints duplicates — against which the first import's
  facts are already recorded.

We take the first as the contract and the second as the recommendation: iaam
accepts any string; the client tooling and the agent skill advise deriving a
stable value rather than sending the number itself, and advise that a change in
the derivation must change the `provider` label with it, so a re-derivation
presents as a new source rather than as new accounts. The advice lives with the
client because only the client can honour it — and because the debuggability cost
falls on the owner, so the choice has to be his and not the server's.

### 2. A card is an alias with a validity interval, and there is no binding lifecycle

An account carries further identifiers for the same account, each with the
interval shape instruments already use:

```
aliases: [{ value: String, valid_from: Date, valid_to: Option<Date> }]
```

Two cards over one underlying account are **one account with two aliases**, and
its balance is counted once. A card that stopped working is an alias whose
interval has closed. That records that a binding ended without making a card a
thing that exists in the model.

**What we lose by refusing the lifecycle, stated plainly.** iaam cannot report
spending by card, because a card is not a dimension. It cannot distinguish an
expired card from a reissued one, from a blocked one, or from one the owner
closed — all four are the same two facts: an alias closed, perhaps another
opened. It cannot answer "was this card active on that date" as anything more
than "was this string a known alias then".

We accept that because the reporting perimeter is accounts. A card is a way to
reach an account, and the money reached the account whichever way it travelled.
The one thing the missing model actually breaks today — a balance counted twice —
is fixed by the alias and not by the lifecycle. If the owner later wants spending
broken down by card, this decision is insufficient and must be revisited rather
than extended.

### 3. A cash account carries one optional label, and nothing branches on it

A cash account may declare what kind of cash it is — deposit, savings account,
card account, wallet — so a report can group balances without parsing titles. It
is optional, defaults to unset, is supplied by the owner, and is **never
inferred**.

Three conditions are part of the decision, not commentary on it:

- **Cash only.** `Balances` in `crates/iaam-core/src/projection/balances.rs`
  separates cash from positions structurally, so `brokerage` and
  `security_position` are not values here. A position on an instrument is what
  the journal records; it needs no declaration.
- **One consumer.** Report grouping reads it. No rule, no classification, no
  validation, no invariant and no refusal reads it. A later feature that wants to
  branch on it is evidence that the objection in `iaam-d41s` was right, and the
  answer then is to give that feature its own declaration — not to grow this one.
- **Unset is a value.** It groups as "not stated" and is never filled by a guess.

**Reconciling with `iaam-d41s`, which argues against exactly this.** That bead's
design argues against an account kind on the grounds that a kind enum invites
every later feature to branch on it, with `InstrumentKind` as the cautionary
precedent, and it records that the owner already corrected a stronger first draft
— from "this account cannot be overdrawn", a constraint the system would enforce,
down to "a negative balance here is unexpected", a warning it would report.

That argument stands, and it stands *for the need it was made about*: knowing
which negative balances are impossible. The correction is the instructive part.
What survived it was a value the owner supplies that produces a report line; what
was removed was a taxonomy the system reasons from. A grouping label is the
surviving shape, not the removed one: the owner supplies it and the only thing
that reads it renders a heading.

So the two coexist as **two values with two consumers**, and this decision
forbids the merge that would recreate the objection: the expectation "a negative
balance here is unexpected" must not be derived from the label. Deriving it —
savings accounts cannot be overdrawn, therefore warn — is precisely the branch
`iaam-d41s` refuses, and it would be wrong on the first ordinary technical
overdraft, which is the correction the owner already made once.

**Why this is in the same ADR as external identity.** It is the same field set on
the same struct, arriving from the same client in the same call. Deciding
identity without deciding whether an account also gains a class would leave the
next person holding a bead that says no and a change that needs a yes.

## Rationale

**Identity by title is not a weak identity; it is the wrong kind of thing.** A
title is what the owner reads. An identity is what a source repeats. Asking one
string to be both means every rename is a silent re-identification, and the code
already pays for this: `counterparty_matches` exists to count the collisions.

**The precedent is one crate away and is the same problem.** Instruments got
namespaced aliases with validity intervals because a ticker is a display name a
source may reuse. An account number is the same class of fact and got nothing.
Reusing that shape means the card question is answered by a field we already
know how to store, rather than by a new entity.

**Opacity is the property that makes this safe to store at all.** The moment iaam
parses `provider_account_id` — to check a length, to detect a card number, to
render it — it has taken a position on what the value means, and a rule
downstream will eventually depend on that position. Equality and uniqueness are
the entire contract, and they are enough for upsert.

**A label read by one report is not a taxonomy.** The test is not how many
variants it has; it is how many decisions consult it. One is the number this
decision fixes, and the condition is written above so a reviewer can check it
later against the code rather than against anyone's memory of intent.

## What we rejected

- **Encoding the source's identifier in `title`.** This is what the account map
  does today by other means. Rejected: the title is displayed, so the identifier
  leaks into every heading and every question the importer renders — and it is
  still one rename away from breaking.
- **A single `external_id` with no provider.** Rejected: uniqueness would have no
  scope, and two sources that both print short sequential identifiers would
  collide on values neither of them controls.
- **iaam computing the fingerprint.** Rejected: to hash the value the server must
  first receive it, which is the one thing the fingerprint existed to prevent —
  and the owner would still be left debugging opaque strings.
- **Enforcing that the value is a fingerprint** (refusing anything that looks
  like an account number). Rejected: it is a shape check on a field defined as
  having no shape, it would refuse legitimate identifiers from sources that print
  something number-shaped, and its only lasting effect would be teaching clients
  how to satisfy it.
- **A card entity with a lifecycle.** Rejected for now, with the losses listed
  above rather than dismissed.
- **Inferring the cash label** from the title or from a transaction pattern.
  Rejected: it is the guess this repository refuses everywhere else, and the
  place it would be wrong is a report total the owner is meant to trust.
- **Merging the cash label with `iaam-d41s`'s expectation into one enum.**
  Rejected above, explicitly, as the move that would prove that bead's objection.
- **Splitting the cash label into its own ADR.** Considered seriously and
  rejected: it is the same call on the same struct, and separating them would
  produce two decisions that each assume the other.

## Consequences

**What becomes true.** A re-import finds the account it created last time instead
of minting a second. Two cards over one account are one account, and a total
counts it once. `--account-map` shrinks from a mapping the owner maintains to, at
most, a provider label — the identifiers come from the export itself.

**What it costs.**

- A migration over accounts that all currently have no external identity. The
  uniqueness constraint must tolerate absence, and the upsert must never treat
  two identity-less accounts as the same account.
- Source-shaped identifiers enter the store for the first time. Every path out of
  that database now carries them, and none of those paths redacts anything today.
- The client becomes responsible for stability it was never responsible for
  before. A client that derives identifiers differently between two of its own
  versions mints duplicate accounts silently, and duplicates are worse than the
  map they replaced, because facts are already recorded against the first copy.
- Aliases need a surface of their own — adding one, closing one, listing them —
  which is a route, a DTO and a store table beyond the account change itself.
- One more optional value the owner is asked for, and therefore one more item the
  action queue must either raise or deliberately not raise.

**What this does not fix.** Classification still resolves counterparties by
title. External identity gives `resolve_counterparty` something better to match
on, and the transfer statements the owner already gives would bound which pairs
are plausible — but pointing the classifier at either is a separate change that
this decision only makes possible. Until it happens, the owner keeps answering a
question nothing reads.

**How we will know this was wrong.** Two signs, and both are checkable rather
than felt. Something other than a report groups or branches on the cash label —
then `iaam-d41s` was right and the label should have stayed out. Or the owner
finds himself maintaining a file that maps a source's identifier to a
`provider_account_id` — then this is the account map with a new name, and the
identity was never the source's after all.
