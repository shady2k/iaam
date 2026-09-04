# 0030. An item asks what the owner knows, and offers what he may answer

Date: 2026-09-04 · Status: proposed · Beads: `iaam-9i83`, `iaam-mk1n`

## Context

One queue item is the subject of both halves of this decision, and it is not a
coincidence that both were found on it. `create_account_named_by_document` is
raised for every account name a kept document printed that the owner's directory
does not place (decision 0024). It is the item an owner meets first, on his first
import, seven times over — so a defect in what it asks and a defect in what it
offers are both visible there before they are visible anywhere else.

Decision 0027 left one of them open by name.

### The field he could not answer

The item published `provider` as a field marked `ProvidedBy::Owner`, with no
question beside it. It was the single entry in `QUESTIONS_UNDER_REVIEW`, the
register decision 0027 §5 created precisely so that a field whose *existence* is
in question is not settled by somebody writing a comfortable sentence for it.

The owner asked what the word was for, and the honest answer an agent could give
him was: nothing, except that two sources printing the same short string must not
collide.

**A value whose only property is that it must differ is a value this instance
should mint, not one a person invents.** He cannot get it wrong in an interesting
way, gains nothing by choosing it, and must then remember it forever —
`CreateAccountRequest::provider_account_id`'s own doc already obliges whoever
supplies it to change `provider` whenever it changes its derivation, which is a
rule no person can keep and a rule nobody had ever put to him.

Two things in the codebase said this out loud before anybody noticed. In
`CreateAccountRequest`, `institution` carried no doc comment at all until decision
0027 gave it one, while `provider` beside it always had a careful one — every
field that was explained was one the machinery needs. And
`docs/glossary-ru-en.md` had no row for the institution: the term appeared inside
three other definitions and was never itself defined.

### The answer he could not give

The same item published exactly one resolution, `create_account`, and
`account_named_by_document_completion` is `directory.resolve(printed).is_ok()`
and nothing else. So «this name is not an account of mine» was
**unrepresentable**, and the only act that closed the item was one the owner had
decided against.

A statement names accounts that are not his at all — another party's account, a
person he pays, an account belonging to somebody in his household. From his side
those records are an expense already visible from the account whose statement
they are on, and the named account is one he will never create.

The item is `ActionCategory::required_for(...)` over `ReportGoals::ALL` and
`ActionState::NeedsOwnerInput`. So each such name was permanent required work
against **every** report goal, and every report he asked for was flagged short on
account of a decision he had already made.

**The next function in the same file had already fixed this exact shape and
written out the argument.** `account_scope_action` publishes two options — put
the account in a contour, or state that it is outside the perimeter deliberately
— and its doc says why one was not enough: «the second way out was reachable only
by a caller who read the prose and then went looking through the specification
for a route no queue item mentioned. An agent that treats `target` as the
contract, which is what `target` is for, could put the account inside a contour
and do nothing else — including for an account that belongs in no contour at
all.» Replace «contour» with «directory» and it is this defect word for word.

**Observed, not theorised.** An agent working a real import reasoned its way to
the hole, asked how the items could be closed if the owner declines, found
nothing, and moved on — leaving the items standing without telling him.

## Decision

### 1. The label is minted, and the scope is the institution

`provider` moves from `missing` to `preset`. It is filled from the institution
the profile that read the document declares, so nothing is asked for it and
nothing is invented.

**It is a join and not a new fact.** The name was refused while a document was
being read; the reading recorded which document it was reading
(`document_unresolved_accounts.document_hash`); and the kept document already
records who printed it, because `read_document` writes the profile's own `issuer`
beside the bytes. Nothing had asked the two questions together.
`Store::list_unresolved_account_sources` is that join, run inwards from the names
so that an owner with a hundred kept statements and no unplaced name pays
nothing.

**The institution and not the profile id, and the choice is not obvious.** A
profile's `id` is declared «unique within an instance» and is the identity half of
its `ParserVersion`, so it survives the profile being corrected to a new version
— which is the argument for it. Against it: one institution ships two documents.
A card statement and a deposit statement are two profiles with two ids, and an
identifier scoped by the profile would say that one bank's short sequential
numbers are two vocabularies. They are one, and keeping *different* sources'
identifiers apart is the whole of what `provider` is for.

**The identity travels whole or not at all.** `create_account` refuses half of it
— a request stating half an identity would be stored as having stated none — so
where this instance cannot say which source printed a string it presets neither
half rather than one. That state is not reachable through any route this API
publishes, because a kept document is immutable and is written before the names
it could not place are; it is written out because a `None` that cannot be reached
still has to mean something, and the alternative is an item publishing a request
the route rejects on arrival.

**The fold key gains the institution with it.** Two statements of one bank naming
one unknown account are still one item; two *institutions* printing one string
are two items, because folding them would mint one label for both, which is the
collision `provider` exists to prevent reached from the other side.

### 2. What he is asked instead is where the account is held

`OwnerPrompt::AccountInstitution` is published on `/institution`, which
`CreateAccountRequest` was already carrying beside `provider`, undocumented and
unasked.

This is the owner's own correction and he narrowed it himself. Told the label
only had to differ, he said «then it should have asked me what the bank is
called» — and then: not «what bank», because a broker is not a bank and an
account may sit at neither, but **the bank, broker or organisation where the
account is held**. `docs/glossary-ru-en.md` gains the term, because the language
rule is that a term is added before it is used and this one was being used inside
three definitions without being one.

It is decision 0027 §5's general form, stated on the field that produced it:

> A request field that exists for the implementation's sake is not automatically
> a question. Ask the question a person can answer, and compute the one the
> implementation needs.

**`AccountInstitution` carries no datum**, and the contrast with
`OwnerPrompt::AccountTitle` is the reason rather than an inconsistency. That one
carries the printed string because what a rename *costs* differs between the two
states it can be asked in. What an institution is for does not differ, and
neither does what turns on it, so there is nothing for a datum to vary.

**`provider` stays in the request**, and this is not a deprecation. A source with
no profile — a pasted export, a broker, a converter of the owner's own — still
needs one, and `CreateAccountRequest` is unchanged. What changes is that an item
raised *by a profile reading* does not ask him for what the reading already
knows.

### 3. `QUESTIONS_UNDER_REVIEW` is emptied and kept

The register's only entry is gone, and it was not removed by writing the missing
sentence — which is the outcome decision 0027 §5 held it open for. The constant
stays, empty, because its worth is that the next author facing the same shape has
somewhere to put «this should not be asked at all» other than into a fluent
question. Deleting it would leave writing prose as the only way past
`every_field_the_owner_fills_in_carries_the_question_to_put_to_him`, which is
exactly the pressure that guard was built with three exits to avoid.

### 4. A printed name can be declared not to be his

`OperationKey::RecordAccountNameDisposition` and the route it names record, and
withdraw, the owner's statement that a name a document printed is not an account
of his. The item publishes it as a second resolution, after `create_account` —
ordered and not ranked, and the account comes first because a name printed on his
own statement is usually his.

**Three values with one refused**, which is `record_account_scope`'s shape one
route above. `mine` exists and the route says why it will not take it: that one of
his accounts answers to the name is said by giving that account the identifier its
source prints, which is what makes the statement lines resolve, and a flag
recorded here would resolve nothing. A route that silently accepted it would be a
second answer to «which account is this», and the one that changes nothing.
`undecided` is where every name starts and is how the statement is withdrawn.

**The reason is required**, for the reason `account_scope_exclusions.reason` is
required and with more force: a name ruled out without one is indistinguishable, a
year later, from a name nobody ever got round to — and here the records printed
under it stay refused on the strength of it. The rejected alternative was a bare
dismissal, and it is rejected because a dismissal that leaves no sentence is
indistinguishable from an agent clicking past an item it could not act on, which
is the failure this bead was filed on.

**Per owner and per printed string.** Not per document: the same counterparty is
printed again in next month's statement, and a statement keyed on the reading
would ask him about it every month. Not per institution either, and this is the
one place the two keys deliberately differ from the fold above: the item is scoped
per institution because a printed *identifier* is scoped to its source, which is a
statement about identity; the declaration is «no account of **mine** answers to
this», and his directory does not hold a different answer for one source than for
another. One declaration therefore settles every item on that string, which is
what he means when he makes it.

**A new write route is an `OperationKey`** rather than an entry in
`WRITE_ROUTES_WITHOUT_AN_OPERATION_KEY`, because it is an act the queue offers,
and decision 0025's guard is what would have caught it either way. Its floor is
`Scope::Owner`: it states a standing decision applying to statements nobody has
looked at yet, and it is the only thing keeping those records refused
deliberately rather than provisionally, which is not a distinction an agent may
draw for him.

### 5. The declared name stays in the queue, as a fact

The item does not disappear. It keeps its identity, its string and its counts,
and it changes from `RequiredForGoal(ALL)` to `Informational`.

Both halves are the decision.

**It stops being required**, because required work is what an owner has not done
and this is what he decided. Left as it was, every report he asked for would go on
being flagged short on account of a decision already made, which is the whole of
the bead.

**It does not disappear**, because two hundred records of his documents are in no
journal on account of this statement, and a queue that said nothing about them
would hide the consequence of his own decision from him. That is the silent drop
this module refuses everywhere else — `skipped_outside_contour` was rejected in
the parity port for being a silent drop of a month, and this would be a silent
drop of an account. So the queue goes on saying how many records are refused
under the name, and says why, in his own words.

`NeedsOwnerInput` and not `Ready`, and the two are not in tension:
`Informational` says nothing is being asked of him, and the one act the item still
offers is the withdrawal of his own statement, which an agent may not make because
it could not have made the statement.

**The way back is the withdrawal and not `create_account`.** Offering both would
publish, on an item that says the matter is settled, the very act he declined;
withdrawing puts the name back to being asked about, and the required item that
returns offers the account as it always did.

### 6. The directory beats the declaration, and completion is unchanged

`account_named_by_document_completion` is still `directory.resolve(printed)` and
nothing else, and the declaration is read nowhere near it.

The argument is the one already written on that function and in decision 0024 §3
about the stored *verdict*, and it transfers exactly: a statement says what the
owner decided, never what is true of his directory now. So an account he creates
afterwards that answers to the string removes the item outright, whether or not a
statement stands against it, and the row is not consulted while it does. A stale
declaration is harmless and is not cleaned up: it is a thing he said, and it
becomes relevant again the moment the account stops answering.

## What was rejected

**Writing a fluent question for `provider`.** It would have satisfied every guard
and settled `iaam-9i83` by making the wrong question painless to leave, which is
what decision 0027 §5 predicted and built the register to prevent.

**Scoping the identifier by the profile id.** §1. One institution's two documents
are two profiles, and the same account printed identically in both would scope
apart.

**Presetting `institution` from the issuer.** The issuer is what the profile calls
the institution; what he calls it is his, and a preset value is never read out to
him (decision 0027 §4), so presetting it would have filled in his answer and
hidden it in the same act.

**Deleting `QUESTIONS_UNDER_REVIEW` once it emptied.** §3.

**A bare dismissal, with no reason.** §4.

**Keying the declaration on the document.** §4. It would raise the same name again
every month.

**Making the declaration part of the completion.** §6. It is the stored verdict
decision 0024 refused, wearing a different noun.

**Letting the declared item disappear from the queue.** §5.

**Recording the declaration in the journal.** Decision 0024 §3's argument
unchanged: the journal holds facts about the owner's money, and «a name a document
printed is nobody's account of mine» is a fact about a reading and a decision
about what to ask him.

## Consequences

- **The item asks two questions and both are answerable.** What he calls the
  account, and where it is held. Nothing in it asks him for a value whose only
  property is that it must differ.
- **One migration, `0027`.** The schema head was 26 when this was written. A
  reserved number further out would have stranded 27 onwards on any database that
  ran this build first, because `migrate` advances `PRAGMA user_version`
  monotonically and skips every version at or below it (`iaam-0xn0`, and decision
  0024's consequences say the same thing after paying for it). A collision on 27
  is a merge conflict, which is visible; a gap is silent.
- **One new operation key and one new write route**, and both sweeps see it:
  `every_operation_key_is_offered_by_an_item_or_a_caveat` finds it offered by the
  item, and `every_write_route_is_a_key_or_says_why_it_is_not` finds it a key
  rather than a declared exception.
- **`GET /v1/actions` publishes two resolutions for this item where it published
  one.** A client that reads only `target.request` now reads nothing for it, which
  is the same change decision 0025 made for `start_account_import` and the reason
  `ActionTarget::Options` exists at all.
- **The queue pays two more reads, and only where it can produce an item.** Both
  are inside the branch that returns early when no document ever printed an
  unplaced name.

## What this does not settle

- **An issuer is catalogue prose and nothing constrains its spelling.** It is
  declared `"issuer": "the institution that prints this document"` in the source
  profile schema, for a human reading a catalogue, so two profiles of one
  institution may spell it two ways and scope apart, and two institutions may
  spell it alike and collide. Both are catalogue defects rather than run-time
  ones and both are visible in the profiles, which is why this decision derives
  from the issuer rather than waiting: promoting it to a declared identifier, or
  adding a scope field beside it, is a change to
  `crates/iaam-ingest/schema/source-profile-v1.json` and belongs to whoever owns
  that schema.
- **An account created from this item still carries no scope decision.** It lands
  in no contour, so `account_scope_undecided` is raised for it next, which is
  correct and is the item after this one.
- **Nothing here helps a name that only ever appears as a counterparty.** Those
  are `conflicting`'s neighbourhood and decision 0024's closing note: the far side
  of a movement is a guess about a string, and the account whose statement a
  record is on is not.
