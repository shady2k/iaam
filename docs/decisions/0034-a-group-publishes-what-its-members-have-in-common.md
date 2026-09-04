# 0034. A group publishes what its members have in common, and one answer settles it

Date: 2026-09-05 · Status: proposed · Beads: `iaam-cixz`

## Context

The owner ran his first real import. Asked to show him what a set of transfers
actually were, the caller could not do it with this API and read his raw
statement file instead. That is this project's own boundary — «ИИ — внешний
клиент», an agent may convey a document and may not interpret one (decision
0022) — crossed for want of a view, and it is the second time the same pressure
produced it.

His own statement of what he wanted is the measure everything below is judged
by:

> He does not need every record. Show one of the group and ask what it was. Most
> of them share every attribute except the day, the time and the amount.

Wave Y holds all of it. `OpenQuestion::printed` publishes each question's row as
typed values, `alike` names the other open rows raising the same decision,
`pair` names the other leg of one movement, `OfferedRule` groups by the word the
source filed rows under, and `AnswerReach` lets one answer reach a stated set.

**What nothing publishes is the group as a thing.** Every one of those fields is
a relation from a row to other rows. None of them says what the members have in
common, how many there are, or what to put in front of a person. A caller
therefore has two moves and both are failures: list every member — the wall
`alike` was added to end — or invent a summary of them, which is interpreting a
document with this engine's own output as the document. The one that happened
was neither.

## Decision

### 1. A group is published, and it publishes what its members agree on

`Interpretation` gains `groups: Vec<RowGroup>`, reaching the wire as
`InterpretationDto.groups`. A `RowGroup` carries its members' row numbers, what
every one of them states alike (`SharedRow`), the spread of what they do not
(`DaySpan` and `AmountSpan`), the sentence to put to a person about the whole of
it, and the reach one answer must state to settle it.

**The shared values are read off the members and never derived from what made
them a group.** A decision group agrees about its account, the party the source
named and the direction the source stated, because `QuestionSubject` equality
says so; whether it also agrees about the currency, the word it was filed under
or its description is a fact about the document that has to be looked at. One
fold answers for both, and that is what lets one shape carry a set whose members
agree about nearly everything and a set whose members agree about nearly
nothing.

**The count is the member list's length and is not a field.** A count beside the
list of the things counted is one fact in two places, and the list is what a
caller needs anyway — it is how it finds the members among `open_questions`.

**A spread and not an endpoint.** «Between these two days» tells the owner which
page of a statement to open; «the earliest» tells him where to start looking.
The amounts keep the signs the source printed, exactly as `PrintedRow` does, so
that a range agrees with the lines in front of him; the cost is that for a group
of outflows the *smallest* signed amount is the largest sum that left, and the
alternative — a span of absolute values — would agree with nothing he can see.
An amount span is published only where the members share a currency, because a
range taken over two is a pair of numbers with no unit.

**Once per assessment and not once per question**, which is decision 0032 §3's
argument unchanged: a group of twenty rows hung on each of its members would be
published twenty times, and the field relating a question to its group is
already on the question. A caller going the other way compares the row against
the group's `rows`.

**It is stamped into the session revision.** Not because the grouping can move
under an unchanged set of questions — it cannot, and that is decision 0032's
reason for leaving `offered_rules` out — but because a group names its account
with the title the owner reads, and he can rename an account without touching
the session. That is the same line `answer_accounts` is stamped by.

### 2. No representative row, and it was the obvious carrier

The owner said «show one of the group». The obvious implementation is a
`representative` field naming a member, and it is refused.

A real member is recognisable — he is matching it against a line in front of him
— but it is also a **particular**: its day and its amount are true of it and of
nothing else in the group. A caller showing it *as* the group shows him one line
and takes an answer about twenty, and the answer it takes is the one this whole
surface exists to avoid: a decision made about a set from evidence about one of
them.

Two things make the field unnecessary as well as unsafe, and both had to be true
before it could be refused.

- **Every member is published in full beside the group**, keyed by row, in
  `open_questions[].printed`. A caller that wants a line takes one out of `rows`;
  `rows` is in row order, so `rows[0]` is a deterministic member and a
  `representative` field would be that value under a second name.
- **The sentence is written here.** `RowGroup::question` is an `OwnerQuestion`
  built out of the shared values, so what is put to him describes the group
  instead of standing in for it. That is what makes «show one of the group and
  ask what it was» answerable without showing him one.

The second is the load-bearing half, and it is decision 0027's finding rather
than a preference: a surface that publishes typed values and leaves the sentence
to whoever relays them gets a sentence composed out of field names. Wave Y's
per-row prompts are on the rows, and not one of them is about the group.

### 3. Which groupings take this shape, and the one that does not

Three groupings exist. Three shapes for the three would be exactly the drift
this module refuses everywhere else, so there is one shape, and it covers two of
them.

**The decision (`alike`) and the movement (`pair`) take it.** They are opposite
cases of one description: a decision group agrees about the account, the party
and the direction and differs in the day and the amount, which is the owner's
sentence verbatim; a pair agrees about almost nothing, because a departure on
one account and the arrival on the other differ in exactly the two fields a
decision group shares. One fold describes both, and the pair is the falsification
that the shape is not secretly the decision group's shape under a general name.

The sets are read off `alike` and `pair` themselves — a decision group is
`{row} ∪ alike`, a movement group is the rows sharing one `pair` — and not
recomputed from `QuestionSubject`. A second reading of what makes two questions
one decision is a second answer to that question, and both would look right;
read this way a group and the `alike` list beside it cannot disagree.

**A set of one is not a group**, which is decision 0033 §2 one surface down. A
question already stands alone, and «here is a group of one» would make a caller
take a group apart to find that out. A pair whose other leg an answer has
already settled is exactly that case, and it is published as one question again.

**The word the source filed rows under does not take it, and the reason is that
nothing answers it as one.** It is a grouping for a *condition*: decision 0032
fixed the group as the word precisely because the condition and the group have
to be the same question, and a word covering a single `RowShape` still covers a
hundred parties — so its rows raise a hundred decisions and there is no one
answer to put to him about it. The only call that acts on the word whole is the
rule route, which decides rows nobody has looked at. Publishing it in this shape
would be publishing a group with no answer, which is §4's whole subject. It
keeps `OfferedRule` and `WithheldOffer`, and the join costs one comparison: a
group whose members agree on the word publishes it.

### 4. A group publishes how it is answered as one

`iaam-q5og` gave the answering call a stated reach and made a wider answer
refuse whole. What was missing is that nothing said which reach settles which
set, so a group published as a set was still a set a caller had to work out how
to answer — and **a group published with no way to answer it as one is the same
wall in better clothes.**

`RowGroup::settles` is the word `POST …/answer` takes.
`every_like_row_in_this_session` for a decision group, whose members are exactly
the rows that reach is defined over, so the group and the reach cannot disagree
about who is in it. `this_row` for a movement group, and that is not the weaker
answer: the two legs are one movement, so an answer naming the other row's
account settles both from either side (decision 0031), and a wider reach would
be claiming something about rows that are not this movement.

`AnswerReach` gains `code`, so the word a group publishes and the word the
answering call parses have one spelling. Before this they existed only inside
the transport's parser, where nothing else could name them.

### 5. The description belongs to a group and not to a row

Decision 0032 kept the description off `PrintedRow` on `row_mark`'s grounds —
the row's whole text, of unbounded length, written by the source — and added a
second ground of its own: every other field it published was already inside some
question's sentence, so those fields disclosed nothing the prose did not.

**Both grounds are about a field beside every one of hundreds of questions, and
both still hold there.** Nothing here puts a description on a row.

A description shared by every member of a group is a different object, and three
things make it one. It is one string for a set rather than one per row. It is
published only where the source itself said the same thing about every member,
so it is a property of the group and not the text of any line in it. And a group
is never a set of one, so there is no group whose description is one row's text
under another name.

**The decisive argument is that the exclusion cost more disclosure than the
inclusion does.** Asked what a set of rows actually was, a caller holding no
field that says could not answer out of this API and read the owner's raw
statement instead — every description of every row, unbounded, read outside the
system altogether. This is the field that answers «what were these», and
withholding it is what produced the larger reading.

It stays out of the sentence. `row_mark`'s rule is about what a person is read —
a day and an amount identify a line well enough to point at, and a source's
whole text pasted into a sentence is how a statement's words end up in a queue
item, a log line and an agent transcript — and that rule is untouched. The
description is a field beside the sentence, and the sentence never quotes it.

### 6. An account a group names carries its title

`SharedRow::account` is an `AccountCandidate` — identifier, title, institution —
and not a bare identifier. An account published for a person to read is one
shape in this API (conventions §3.3), and a second shape for the same thing is
the drift this wave settled once for every surface that publishes one.

It is absent where the members are on different accounts, which is what a
movement group is, and where this instance's directory no longer holds the
account they share: a group named by an identifier nobody can read is not a
group anybody can be asked about, and the sentence drops the clause rather than
quoting an empty name.

## Non-vacuity

`a_group_publishes_what_its_members_agree_on_and_the_spread_of_what_they_do_not`
is the owner's sentence made into an assertion: three lines to one party over one
month, and the group is asserted to publish the account, the party, the word, the
direction and the currency as shared, and the day and the amount as spans. A
version of this that published only the count would pass a test that checked
existence, which is `iaam-3nqt`'s lesson and the reason every field is named.

`a_group_publishes_the_reach_that_settles_it_and_a_pair_settles_from_either_side`
is §4 and §3's pair case at once, and it is the one that fails if the shape is
secretly the decision group's: it asserts that a movement group shares **no**
account and **no** direction and still publishes a currency and a span, which is
what makes the two legs comparable at all.

`every_reach_a_group_can_publish_is_a_word_the_answering_call_takes` runs each
published reach back through the answering request's own parser. A group offering
a word the answer route refuses would be this response telling a client to send
something the server then rejects.

`a_description_every_member_carries_is_the_groups_and_one_that_differs_is_nobodys`
holds both halves of §5, including that the shared text does not reach the
sentence.

`a_set_of_one_is_no_group_because_one_row_is_one_question_already`,
`a_group_whose_members_are_in_two_currencies_publishes_no_amount_range`,
`a_group_whose_rows_this_build_cannot_read_is_not_published_at_all` and
`a_word_the_source_filed_rows_under_is_offered_as_a_condition_and_is_no_group`
are the four falsifications: each describes a state in which a group must **not**
be published, or must be published poorer, and each is written against a state
made in the test rather than asserted about the code.

`a_group_is_asked_in_his_words_and_says_what_answering_it_once_decides` runs the
group's sentence through the same mechanical check the offer beside it answers
to, and additionally requires it to say how many lines one answer decides — which
is the half of the consequence he would otherwise discover afterwards.

**What no test holds.** Whether the sentence a group publishes is the sentence he
would want to be asked is not decidable by a rule. The acceptance test is the
owner, as decision 0027 already says, and a group that satisfies every guard here
and still sends a caller to his statement file has failed.

## Consequences

**A caller can put one sentence naming a set of rows in front of a person and
take one answer**, where it previously needed one sentence per row or a summary
it wrote itself. For a first import whose repeats are a handful of parties, that
is a handful of sentences in place of hundreds.

`GET /v1/import-sessions/{session}/assessment` gains one optional list on the
interpretation. Nothing is removed, and a client that ignores it reads exactly
what it read before.

**The session revision changes for one more reason**: the title of an account a
group names.

**No migration.** Everything here is folded out of what the assessment already
holds, plus one field of the stored rows.

## What this does not settle

- **A word the owner adopts an offered rule on does not settle this session's own
  open rows.** The offer's consequence says one answer «settles all of them at
  once»; a rule created afterwards is read by the planner, so those rows produce
  planned facts, while their stored questions stay open and the commit goes on
  refusing over them. The sentence overstates what the call does, and the repair
  is a decision of its own — a question a rule now settles has to stop being open
  everywhere, including in the refusal that counts unanswered questions — so
  nothing here changes either the sentence or the filter.
- **Nothing folds a group away once it is answered.** The members disappear from
  `open_questions` one by one and the group shrinks with them, which is the
  visible behaviour decision 0033 chose for the queue and is kept here for the
  same reason.
- **A group carries no alternatives of its own.** They are on every member, read
  from what was stored when the question was asked, and a group with its own copy
  would be a fifth publisher of one stored list (`iaam-ulib`). Whether a decision
  group should assert that its members' stored lists agree is not decided.
- **Two groups that overlap are not related to each other.** A word's rows and a
  decision's rows can be the same rows, and nothing says so beyond the row
  numbers both publish.
