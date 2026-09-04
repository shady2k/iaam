# 0036. A state for an item that wants nothing, and a relay in the owner's voice

Date: 2026-09-05 · Status: proposed · Bead: `iaam-c143`, `iaam-09tn`

## Context

Two findings from one relay of a first import, and they are the same finding
seen from two ends: the queue told a caller something false, and the caller
passed the machinery on to the owner instead of the question.

### The queue had no word for «nothing is wanted here»

`ActionState` was `Ready`, `NeedsOwnerInput`, `Blocked`, and its doc comment said
what it was for: *whether an action can be invoked without asking the owner*.
That is a yes, a no, and a third word for «neither of those, because there is no
call at all». None of the three can say that nothing is wanted.

Decision 0030 had already produced an item in exactly that position. When the
owner declares that a printed name is no account of his, the item for that name
stays in the queue — the records printed under it are still refused and still in
no report, and the queue is the only surface saying so — and its category drops
to `Informational`, «a fact that requires no action». Its state stayed
`NeedsOwnerInput`, on the argument that the one call it still publishes, the
withdrawal of his own statement, is his and not an agent's.

That argument is about the **floor on a call**, which decision 0021 settled is a
property of the call and is read off the call. It was being spent a second time,
on the field that says what the item wants.

The cost was observed, and it was not theoretical. A caller that had just
recorded three such declarations read three items still marked as needing the
owner, went to find out what they were holding up, and reported that
investigation to him. They were holding up nothing. He was handed a description
of the queue's internal states and asked why he was the one working it out.

`Informational` alone does not fix it. A caller deciding what to raise reads the
field that exists to say what is wanted; requiring it to consult the urgency
first is one rule written in two places, which is the arrangement
`ActionCategory::goals` and `Action::required_scope` are both here to avoid.

### The published question was read out as a script

The same relay reached the owner as two engineers talking to each other rather
than as an assistant talking to a client. Three things arrived that should not
have.

**The published question was quoted verbatim, in English, attributed to the
system.** That is the predictable reading of what decision 0027 built. It put
`ask` and `consequence` on the surface so that a caller would stop inventing a
question out of JSON pointers, and nothing anywhere said whether the string is
source material or a script. The repository's language rule keeps the tree in
English; it is a rule about the tree and says nothing about the language a caller
speaks to a person in. The two were conflated, and nothing in the tree said they
were different.

**The machinery showed through**: the wire words an answer is sent as, item
states, values already filled in, the word for a field being optional, this
project's own decision numbers.

**The caller narrated its own reconnaissance** — what it checked, how something
was classified, whether anything was blocked. That is a caller describing how it
found out, which is never the answer to anything he asked. Partly its own habit;
partly ours, because the queue made the reconnaissance necessary, and the item
above is one instance of that.

## Decision

### 1. `ActionState` gains a fourth word, and it is `Settled`

Nothing is wanted. The item states a fact, and the fact still stands.

**A word rather than an absence.** `Option<ActionState>` was rejected: every
consumer would then hold two questions — is there a state, and what is it — where
the queue holds one, and «no state» is not a fact about an item. It is the same
argument decision 0027 made against folding a question into `ProvidedBy::Owner`:
a published vocabulary answers one question, and a variant that also carries
another is not that vocabulary.

**`Settled` and not `Recorded` or `Decided`.** The item's own prose already
called it settled before there was a word for it, and what settles it is a
decision of the owner's rather than a record this system wrote.

The enum's doc comment changes with it. It no longer asks whether an action can
be invoked without asking the owner; it says what is wanted of a reader, and from
whom. The old question is a yes/no with an escape hatch, and it is what left no
room for the answer «nothing».

### 2. The fourth word is threaded through the invariants, and adds two

`Action::new` already refuses five combinations. `Settled` is bound by the
existing `NonBlockedWithoutScope`, deliberately and not by exemption: an item
that wants nothing **and** publishes no way back is a fact nothing in this API
touches, and `Blocked` is already the word for that. What distinguishes a settled
item is precisely that the statement which settled it can be withdrawn.

Two refusals are added, and they say that urgency and what-is-wanted are not
independent of each other:

- **`InformationalNeedingInput`** — a category saying no action is required, with
  a state saying the owner must act. Whenever those appear together one of them
  is false, and the queue is the worst place for a contradiction between two
  fields: a caller reads one of them, so it is invisible from where it is acted
  on. It was acted on.
- **`SettledWithWork`** — a state saying nothing is wanted, with any category but
  `Informational`. This is what stops the new word being reached for as a way of
  quieting an item that is still work: an item required for a goal is short of
  something, and saying nobody is waiting does not supply it.

**Refused at construction rather than swept afterwards**, for the reason decision
0027 gives about `MissingInput::plain`: a guard catches the pair after it is
built, and a constructor leaves nothing to catch.

**Rejected: refusing `Informational` with `Ready` as well.** No producer builds
it and no argument was found that it is a contradiction — an item that states a
fact and also publishes a call an agent may make is odd but not dishonest. A
refusal with no instance is a rule invented ahead of its defect.

### 3. This is not the fourth state decision 0021 rejected

0021 rejected «a fourth `ActionState` for could-not-be-computed» on the ground
that the item in question was in an ordinary state — the owner must supply
something, and there is a call for it — and that what was unusual was the *kind*.
That argument holds and is untouched. It is the opposite of this one: here the
item is in no state the vocabulary has, and the unusual thing is not the kind but
what is wanted, which is nothing.

### 4. The sweep found one item, and the word is still worth minting

Every producer whose category is `Informational` was read. Three of them —
the unexplained residual, the external transfers that carry no category, and a
coverage gap whose period has since reached independent confirmation — are all
`Blocked`, correctly: no call in this API is addressed to any of them. The
declined printed name was the only item claiming input while asking for nothing.

One instance is enough, because the defect is in the vocabulary rather than in
the item: the item was graded honestly on the field that could express it and
dishonestly on the field that could not, and a second such item would have had
nowhere else to go either. The alternative — leaving the state false and telling
callers to cross-check the category — is the two-places arrangement this module
refuses.

### 5. What a caller does with it, said where a caller reads it

An item in this state is shown when the owner asks what has been decided, and is
never raised as work. That is on the variant, on the floor in
`iaam_app::ports::required_scope` where the owner-only call is explained, and in
the agent skill, which is where the caller that got this wrong was reading.

The value reaches the wire as `settled`. Additive: a client that switches on the
three it knows sees one more value on an item it previously saw as
`needs_owner_input`, and no field was removed or renamed.

### 6. A published question is source material, not a script

The `ask` and `consequence` of decision 0027, the alternatives of decision 0029
and what each does to his report, an item's `reason`: these fix **what must be
conveyed** — what is being decided, what it is for, and what is different
depending on how he answers. The caller owes the **voice**: the owner's language,
his register, and no attribution to the machinery.

**0027 is not weakened into «say it however you like».** The obligations are the
content; the freedom is the wording. A caller that drops what the choice changes
has failed whatever language it used, and 0027 already argues the other half —
that inventing a question in place of the published one is what produced field
names read out to a person.

The agent skill gains one short section rather than three rules, because the
three findings are one: a relay is one sentence naming what is being decided, in
his words, with what turns on it, and the readiness to say more if he asks. Not a
transcript, not a status report, not our sentence in quotation marks. It states,
in one clause, why a document written in English tells a caller to speak to him
in his own language: the language rule is about the tree.

It also draws the line the freedom stops at, in the same breath, because a
section telling a caller to speak freely without it is an invitation to decide
for him: the wording is his caller's, the decision is his. Never answer in his
place, never read silence as a value, and never narrow what he is being asked
because a shorter sentence reads better. That is the import boundary of decision
0022 one level up — a caller may render a question and may not answer one.

**Rejected: a wording the system publishes for him to be read out as-is.** It is
the shape that produced the finding. A sentence fixed in the tree is fixed in one
language and one register, and the person it is for is not the person who wrote
it; what the tree can fix is what the sentence must contain, which is what 0027
already fixes.

## Non-vacuity

`no_item_the_queue_publishes_asks_for_something_it_does_not_want` sweeps the heap
of every item the queue can reach and demands **both** witnesses rather than
assuming them: an item that wants nothing must be in the heap, or the sweep runs
over items that could never have shown the defect, and an item that does want
something must be too, or it proves only that `Informational` is absent. The heap
gained a declared name for exactly that reason.

The sweep cannot fail while every item is built through the producers, and that
is what it proves — that the producers are the only way these items are made. It
fails the day one is assembled around the constructor.

`an_item_that_requires_no_action_cannot_say_the_owner_must_act` and
`an_item_that_wants_nothing_cannot_be_graded_as_work` are made against specimens
written in the test and well formed in every other respect, so the refusal that
fires is the one being proved rather than a second defect in the fixture.
`a_settled_item_publishes_the_withdrawal_that_undoes_it` asserts that the fourth
word is bound by the invariant that was already there.

`a_declared_name_becomes_a_statement_of_fact_and_not_work` asserts the floor as
well as the state, because the two used to be the same assertion: the withdrawal
is still owner-scoped, and it is `Action::required_scope` that says so.

**What no test holds.** Whether a sentence put to a person reads as an assistant
speaking to a client is not decidable by a rule, exactly as 0027 recorded of the
questions themselves. The acceptance test is the owner.

## Consequences

A client reading the queue sees one new value of `state`. The item for a printed
name the owner has declared is not his moves from `needs_owner_input` to
`settled`; a client that filtered the queue for work and found that item in it
will now find one fewer, which is the finding.

A caller relaying anything to the owner has, for the first time, a written
statement that the published strings are what to convey and not what to say, and
a written line between rendering a question and answering it.

## What this does not settle

Whether `ActionDto.state` should publish its vocabulary in the contract rather
than as an undocumented string. It carries no doc comment today, so a client
author learns the four words by observing them, which is the shape of defect this
project has paid for elsewhere. It is a change to a file this work does not own
and is filed rather than made.
