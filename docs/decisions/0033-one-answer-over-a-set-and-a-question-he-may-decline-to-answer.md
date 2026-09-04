# 0033. One answer over a set, the fields of one call together, and a question he may decline to answer

Date: 2026-09-04 · Status: proposed · Beads: `iaam-hdr7`, `iaam-zxc6`, `iaam-4fsw`

## Context

The owner ran his first real import. Seven printed account names became roughly
fifteen exchanges: one item per name, each item asking its two fields one after
the other, and a caller walking them in sequence. In the middle of it he stopped
answering questions and answered a set instead — every name in the document was
from one institution, and every account was to be called what the statement
called it — and afterwards he wrote the rule: the agent is to be proactive and
to put as few questions as it can, because two confirmations were standing in for
fifteen questions and both of them were derivable from what the instance already
held.

Three defects produce those fifteen exchanges, and each is a separate absence.
One of them is a defect decision 0027 introduced.

### The queue publishes items and he decides over sets

The unit of the item is right. `create_account_named_by_document` is raised per
printed name because completion is per name: an account created for one string
does not settle another, and folding them would publish one item for seven
accounts.

What was missing is a way to say **this answer, for these items**. `iaam-q5og`
established exactly that shape one level down, inside an import session: the
caller states the reach, the system publishes what the wider answer would touch,
and an answer that cannot be recorded for one of them is refused whole. The
mechanism was argued a wave ago and stops at the session boundary.

And the reach alone would not have been enough. A set of items sharing a field
is not a sentence anybody can put to a person. «Here are four items each asking
where an account is held» is not answerable; «this export is from that
institution, so all four accounts are held there — yes?» is answerable in a word,
and the difference between the two is a **proposal**: the value, and the ground
it stands on.

### The fields of one call were serialised because nothing said they need not be

The same item asked what to call the account and then, separately, where it is
held. Both are fields of one call, written by one request.

Decision 0027 gave every owner-facing field its own question, and it was right to
— `iaam-tt71` found that a mapping from field to question, gathered into one
prose sentence, has to be taken apart again by the caller that must show the
owner one field. But «each field keeps its own words» is not «each field is a
separate exchange», and only the first was ever written down. So the safe reading
was the slow one, and a well-behaved caller doubled the length of a first import
by being careful.

### The queue could not say a field was skippable

`MissingInput` was `pointer`, `provided_by`, `candidates`, `alternatives`,
`prompt`. Nothing in it said whether the call is refused without the field.

`institution` on `create_account` is `Option<String>`. The account is created
without it, no figure reads it, and nothing is matched against it — and the queue
published it beside the title with nothing to tell the two apart. The owner was
stopped for it as though the account could not exist otherwise, by an agent that
had just told him, correctly under decision 0027's third obligation, that no
figure depends on it. It asked anyway, because the item gave it no way to offer
skipping.

## Decision

### 1. A missing field may carry an answer offered over a set of items

`MissingInput` gains `proposal: Option<Proposal>`, and it reaches the wire as
`MissingInputDto.proposal`. A `Proposal` is a `ProposedAnswer` — a closed
vocabulary that knows its field, its call, the value it proposes and the words to
put to him — together with `covers`, the ids of every item that one answer fills.

**A rendering and not a call, and the route was already there.**
`create_account` is called once per account and goes on being called once per
account: nothing about a wider answer changes what is written or how. What was
absent was only that nothing published the items as a set with the field and the
proposal they share, so a caller could not offer them together. Adding a call
that created several accounts at once would have been a new write route for a
problem that is entirely in what the queue says.

**The value is per item, and the shape had to admit that.** «They are all held at
that institution» is one answer writing one value into every request. «Call them
what the statement calls them» is one answer writing a *different* value into
each. A shape carrying a single value for the set would have covered the first
and left the seven names asked one at a time, which is eight of the fifteen
exchanges. So `covers` is shared and the value is the item's own.

**A proposal is a question and not a guess**, and the distinction is already
load-bearing here: `matcher_for` proposes a rule from a row and the owner adopts
it, and until he does there is no rule. The agent skill's rule — a missing value
is asked of the owner, not filled in — is kept exactly when the proposal is
published *as* the question and the recorded answer is his. What that rule
forbids is a value written without him, and a confirmed proposal is not one.

**A proposal is not a preset under another name.** Decision 0027 §4 has a preset
value never read out to him, and decision 0030 refused to preset `institution`
from the profile's issuer for that reason: presetting would have filled in his
answer and hidden it in the same act. This is the door that decision left open.
The same value, published as a question with the ground beside it, is read out —
and a guard asserts that neither proposed value ever reaches `preset`.

### 2. The set is the reading's institution, and the alternatives were weighed

Membership is: an item of this kind, raised for a name a document of **that**
institution printed, that still asks the field.

**Not the document reading.** The fold above already merges two statements of one
bank naming one unknown account into a single item (decision 0030 §1), so a set
keyed on the reading would be a set an item belongs to twice.

**Not the item kind.** An owner who conveys two institutions' statements before
working his queue would then be asked one question over both, and «they are all
from that bank» would be false of half of it. Two sentences settling four names
is the right answer; one sentence that is wrong about two of them is not. Two
institutions are two sets, and the second question is worth asking.

**Not something the caller names.** The caller cannot see the ground. What every
member of this set has in common is a claim the reading recorded — these strings
were printed by that institution — and a set assembled by a client would be a
client deciding which of the owner's accounts are alike, which is the judgement
this whole surface refuses to make for him.

**A name the owner has declared is not his is not a member**, and this is not an
exclusion but the membership rule doing its work: that item asks for neither
field. Its one resolution is the withdrawal of his own statement (decision 0030
§4), so there is no title and no institution on it for one answer to fill.
Membership is asking the field, not sharing the kind.

**Never a set of one.** `ActionTarget::from_options` normalises a single
resolution out of a list for this reason, and it holds a level down: one item is
one question already, and «here is a set of one» would make a caller take a set
apart to find what it had — and would put a sentence about several accounts to
the owner about one.

### 3. Refused whole, and what that means for a publication

`iaam-q5og` required that a wider answer reach everything it names or nothing.
Here nothing is written by the publication, so the refusal is a property of what
is published rather than of a call: **`covers` is complete, and an item that
cannot take the answer is in no set at all rather than quietly left out of one.**

The reachable case is a name whose kept document this instance can no longer
place. It carries no institution, so there is no ground; decision 0030 already
presets neither half of an identity for it rather than one, and the same refusal
applies twice over here, because folding it into a neighbour's set would put a
claim about one bank's document to him over a name that came from nobody knows
where — inside an answer he gives in one word. So it joins no set, gets no
proposal, and goes on being asked on its own. The item that could not take the
answer is the item the offer does not name, and a caller applying the answer
beyond `covers` has gone outside the offer rather than been misled by it.

**Rejected: publishing a withheld offer that names the item that blocked it.**
It was the literal reading of «the refusal names which», and it is refused for
two reasons. Splitting by ground serves the owner better — two sentences settling
four names, rather than none — so the state a withheld offer would describe is
one this decision does not want to reach. And a wire shape whose only content is
unreachable is decoration: every case that would fill it is a case where a
smaller, true set exists.

### 4. The fields of one call may be put to him together

`RequestPlan.missing` is already an ordered list of one call's fields, so the
fact was published. What was absent was the sentence, and it is written in three
places and no more: on `RequestPlan::missing`, on `MissingInputDto` where it
reaches the contract, and in the agent skill, where it reaches the reader that
was serialising them.

**Nothing is added, and that is the finding.** The check the bead asked for was
made — nothing anywhere implied the opposite, and a rule stated twice is what
this project keeps paying for. Decision 0027's obligation is per field and this
is per exchange; they are about different things, and stating that plainly is the
whole of the fix.

### 5. A field the call is accepted without says so, and is offered with a way past

`MissingInput` gains `optional: bool`, absent on the wire when false, so a client
that ignores it reads exactly what it read before and reads the safe answer.

**It is a fact about the call, not a grade of the question.** True means the route
accepts the request with the field absent. It does not mean the field is
unimportant, and it is not the item saying it would rather not know: what
skipping costs is in the question's `consequence`, where decision 0027's third
obligation already puts it. For `institution` that sentence is now «nothing now,
and a year from now nothing will say where this account is», which is exactly the
sentence that rule asks for — and it is what makes the flag an offer rather than a
fact nobody can act on. An optional question is put to him with a way past it.

**`false` is not «the schema requires it», and the guard is one-directional
because of it.** A route may refuse a request for a field its own schema marks
optional: `/reason` is required for one disposition and refused for another, and
a balance carrying neither cash nor positions is refused outright. Both are
`Option` in the shape and neither is skippable. So
`every_action_request_schema_required_input_is_advertised_as_missing` is extended
in the direction that is decidable — a field published as skippable must be a
field of that body which the schema does not require, and a path parameter is
never skippable — and a sweep insisting that every schema-optional field be
advertised as optional would be demanding a lie. The other half is held where the
queue is built from a state written out in the test.

**Rejected: a vocabulary rather than a flag.** `ProvidedBy` is three words on an
axis a reader had to be taught, and its whole argument is that a code with no
sentence beside it gets misread. «Is this call refused without the field» is two
states with no third, and the sentence that explains it belongs on the field
rather than in a published enumeration nobody has to decode.

**Rejected: putting it on `RequiredInput` too.** That type's name is the answer:
it is a field one alternative *requires*, and a required input the call is
accepted without would be a contradiction rather than a state.

## Non-vacuity

`iaam-3nqt` exists because a guard checked only existence, and the shape a proof
takes here is decision 0027's own: the rule is made against an input written in
the test, and what fired is asserted rather than that something did.

`a_field_the_call_is_accepted_without_says_so_and_the_one_beside_it_does_not`
asserts both halves on the pair the owner actually met — one field the account
cannot be created without and one it can, published side by side — because a
queue that marked every field optional would be as mute as one that marked none.

`a_proposal_is_a_question_and_says_what_answering_it_once_decides` runs the
proposals through `puts_a_question_to_a_person`, the same check every other
question answers to rather than a laxer one written for them, and additionally
requires each to say how many accounts one answer decides for — which is the half
of the consequence that is new, and the half he would otherwise discover
afterwards.

`a_proposed_value_is_read_out_to_him_and_never_preset` is the one that keeps
§1's distinction from eroding: the whole difference between this and the thing
decision 0030 refused is which side of `preset` the value is on.

`a_name_with_no_institution_behind_it_joins_no_set_rather_than_a_neighbours` and
`a_name_he_has_declared_is_not_his_is_in_no_set` are §2 and §3 made against
states written out in the test, and
`two_institutions_are_two_sets_and_neither_answers_for_the_other` is the case
that decides the set.

`every_proposal_names_the_field_it_fills_and_the_items_it_reaches` is the sweep,
in the shape decision 0027's is: the field and the call are checked against the
proposal rather than taken on trust, and the heap it runs over was widened to
raise two printed names, because a heap holding one would let the sweep run over
a queue in which nothing was ever offered over a set.

**What no test holds.** Whether two confirmations are the two he would want to be
asked is not decidable by a rule. The acceptance test is the owner, as decision
0027 already says, and a proposal that satisfies every guard here and still makes
him answer seven times has failed.

## Consequences

**A first import of seven printed names from one institution costs two
confirmations and whatever he wants to say differently**, in place of roughly
fifteen exchanges. Two of the three defects account for that on their own: the
fields of one call are shown together, so seven items are seven exchanges rather
than fourteen, and one answer over the set collapses those seven to two. The
third removes an exchange the owner could not have known was optional.

**It is not zero, and it must not be.** Both proposals are questions and both may
be answered no, and a name whose institution this instance cannot state is asked
about on its own.

**`GET /v1/actions` gains one optional flag and one optional object per missing
field.** Nothing is removed. A client that reads neither reads exactly what it
read before, and reads `optional` as absent, which is the safe answer.

**No migration.** Everything here is computed from what the queue already reads:
the set is a fold over the names one call already returns, and the flag is a
statement about a route.

## What this does not settle

- **Only one item kind offers an answer over a set.** The shape is general — a
  field, a ground, and the items that share it — and `account_scope_undecided`
  is the obvious next one, since the accounts created from this item all land in
  no contour and he is likely to want the same one for all of them. It is not
  done here because the ground is different: which perimeter an account joins is
  his judgement and not a fact any reading recorded, so an offer would have to
  propose a value nothing worked out. That is a decision of its own.
- **Only one field is published as skippable.** The category is real and there
  are probably others; each needs the route read rather than the schema, which is
  §5's whole point, and none was done speculatively.
- **A proposal is not published on `RequiredInput`.** Its fields are required by
  the alternative that names them, and no alternative's field is asked across
  items. If one ever is, the shape transfers unchanged.
- **Whether the queue should fold a set into one item after he confirms** is not
  decided. It should not: the items are what say which accounts are still to be
  created, and one answer settling seven of them is visible as seven items
  disappearing.
