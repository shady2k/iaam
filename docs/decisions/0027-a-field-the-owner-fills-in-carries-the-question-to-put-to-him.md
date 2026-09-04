# 0027. A field the owner fills in carries the question to put to him

Date: 2026-09-04 · Status: proposed · Bead: `iaam-ytvf`

## Context

The queue's `create_account_named_by_document` item was relayed to the owner by
an agent, and what he was shown was `provider_account_id`, `provider` and
`title`, with this API's own schema descriptions beside them — «Whatever the
source prints for this account», «The client's own label for the source… it
scopes the identifier below».

The agent was not wrong to do that. Those were the only strings the surface gave
it.

`MissingInput` published a pointer, a `provided_by`, account candidates and, for
a field with a closed vocabulary, its alternatives. **`ProvidedBy::Owner` says
who supplies a value and never says what to ask him.** An agent that has to put a
field to a person therefore had exactly one string per field, and it was the JSON
pointer.

The hole is provable from inside the codebase. `InputAlternative::consequence`
exists for precisely this purpose — a sentence shown to the owner beside one
value, read from `AnswerShape::consequence` rather than written at the queue, so
that the queue and the import session say the same words about the same thing —
and it is filled **only** for a field with a closed vocabulary. A field with an
open value, a title, a label, a balance, a date, gets nothing. Those are exactly
the fields only he can fill.

`RequestPlan.preset` is a second, smaller instance of the same thing: a bare map
of wire name to value, with nothing saying whether an entry is plumbing the owner
must never see or a value he should confirm. In this item `provider_account_id`
is plumbing — decision 0004 presets the printed string as the *identifier* and
deliberately refuses to preset it as the title, because a title can be renamed
and would silently stop a statement importing — and that reasoning was invisible
to the caller, so the agent read a filled-in field out to him.

### The rule the owner then wrote

Told what the fields were, he stated the register himself, and it is now the
specification:

> «каждый вопрос пользователю должен задаваться так, чтобы он понял, без наших
> внутренних терминов с объяснением для чего это и как это решение повлияет»

Three obligations, and a prompt keeping two of them is still wrong:

1. **No internal vocabulary.** Not a field name, and not a word that exists only
   because of how this is built.
2. **What it is for**, in terms of something he already recognises.
3. **What his choice changes** — not that the field is his, not that he may
   change it later, but what actually differs between answering one way and
   another, including «nothing differs, and here is the one case where it
   would».

The third is the one that gets dropped, and he has now asked for it twice. An
agent, given better prose, told him a title was his own and that he could rename
it whenever he liked. That keeps 1 and 2 and fails 3, and his reply was that he
had no idea what the question was or what it affected.

### The schema is part of the same defect

In `CreateAccountRequest`, `provider`, `provider_account_id` and
`negative_balance_expectation` carried doc comments; `title` — the request's only
required field, and the only one a person fills in — carried none, and neither
did `institution`, `cash_class` or `aliases`. Every field that was explained was
one the machinery needs. The agent built its question for `title` out of the only
prose within reach, which was decision 0004's paragraph about
`provider_account_id`, and the question came out about printed strings instead of
about the name he will read on his reports.

## Decision

### 1. The question lives beside `provided_by`, on both types

`MissingInput` and `RequiredInput` each gain `prompt: Option<OwnerPrompt>`, and
it reaches the wire as `MissingInputDto.prompt` / `RequiredInputDto.prompt`.

**Beside `provided_by` and not inside `ProvidedBy::Owner`.** The payload was
tempting: it would make «the owner supplies this and nobody wrote a question»
unrepresentable. It is refused for the reason `ProvidedBy` refuses a fourth word
— each of its three names *who supplies a value*, and a variant that also carried
what to ask him would make the enum answer two questions, which is the argument
already written on that type and the one that kept a fourth code being
rediscovered. It is also a published vocabulary of three codes, and one code
carrying a payload is not that.

**`RequiredInput` needed one too, and its own doc comment nearly said
otherwise.** «It carries no alternatives» is a statement about a second closed
choice and says nothing about prose. Its one field today is `/account`, one of
the owner's own accounts chosen from `candidates`, and «which of your accounts»
is as much a question for a person as a title is. A pointer reading `/account` is
exactly as mute as one reading `/title`.

### 2. Read from one place per field, where a field is a field of a call

`OwnerPrompt` is a closed vocabulary. Each variant knows its own pointer
(`pointer()`) and its own call (`asked_by()`), and `MissingInput::asked` derives
the pointer from the question rather than taking both.

**Keyed by the pair, because a pointer is not an identity.** `/title` is an
account's title on one route and a group of accounts' title on another; a table
keyed by the pointer alone would answer one of them with the other's sentence.
Deriving the pointer from the question is what makes reusing the wrong variant
visible rather than merely wrong: the pointer changes with it, and the request
built from it no longer addresses the field the item meant.

**Written per item was rejected.** Three items ask for an account title. Three
free strings would be three answers to «what is a title for», and the queue would
be publishing that this system holds two opinions about what it wants. This is
`AnswerShape::consequence`'s arrangement and it is here for its reason: two
publishers of one sentence eventually disagree.

**The item's `reason` was rejected, and this was already settled once.**
`iaam-tt71` found that a mapping from field to question, gathered into one prose
sentence, has to be taken apart again by the caller that must show the owner
**one** field. That is `docs/api/conventions.md` §5, and it is written a second
time on `InputAlternative::consequence`.

### 3. The question has two parts, and only the second may vary by item

`OwnerQuestion` carries `ask` and `consequence`.

Obligation 3 is a separate value because it is the one that gets lost. Folded
into the end of a sentence that already reads as finished, it is what a relayer
trims, and trimming it is exactly what produced «what does this question even
affect». As two values, a client that shows only one has visibly shown only one,
and an empty consequence is an empty string rather than a sentence that stops a
clause early.

It is also the half that legitimately varies. What a name is *for* is the same
wherever it is asked. What a rename **costs** is not:
`AccountNames::candidates` matches a uuid, then an identifier a source printed,
then a case-folded title, returning early at each tier. On an account created
from a document the printed identifier is preset, tier two fires, the title is
never reached, and a rename is free. On an account carrying no printed
identifier the title is the only thing a statement line can find it by, and a
rename is not free. So `OwnerPrompt::AccountTitle` carries what the item knows —
the printed string, or its absence — and both wordings still live in one place.
An item hands over a datum; it does not write a second sentence.

`consequence` is the word `InputAlternative` already uses, for the same idea one
noun away: what is different depending on how this is answered.

### 4. `preset` stays a plain map, and the document says what it is

Rejected: making a preset entry an object carrying a reason. `preset` is the
shape the write route accepts, so a client copies it into the request body
unchanged, and `docs/api/conventions.md` §5 is the rule that makes that a
property worth keeping. Wrapping each value would oblige every client to unwrap
it, forever, to carry prose nobody is meant to read out.

What was missing was never a structure. It was the sentence saying that a preset
value is the request already filled in and is not a question, and that none of it
is put to the owner. That sentence is now on `RequestPlanDto.preset`, where it
reaches the contract, and in the agent skill, where it reaches the reader who was
relaying items to a person.

### 5. What obliges the question to exist, and the two other ways to satisfy it

`MissingInput::plain` no longer takes a `ProvidedBy`. It takes `NobodyIsAsked`,
which is that vocabulary minus `Owner`, so a field the owner fills in can only be
built by `MissingInput::asked`, which cannot be called without a question, or by
`MissingInput::asked_without_a_question`, which says so in its name. A guard
would catch the pair afterwards; a parameter type leaves nothing to catch.

`every_field_the_owner_fills_in_carries_the_question_to_put_to_him` sweeps the
queue for the half a signature cannot hold, and it is **satisfiable three ways**.
That is the decision here, not an accident of the implementation.

- **The field carries a question.**
- **The field stops being the owner's**, because a value this instance can work
  out is worked out instead of asked for.
- **The field is registered in `QUESTIONS_UNDER_REVIEW`**, which names the bead
  deciding whether it should be asked at all.

A guard satisfiable only by writing prose would push the next author into writing
a *fluent* question for a question that should not exist, and the proof is that
it would have done so here. Told that `provider`'s only property is that it
differs between sources, the owner asked why it is being asked for at all, and
then said what should have been asked instead: **"then it should have asked me
what the bank is called."**

That is the general form, and it is worth more than this one field:

> A request field that exists for the implementation's sake is not automatically
> a question. Ask the question a person can answer, and compute the one the
> implementation needs.

«What bank is this?» needs no teaching. «What word shall we scope the printed
identifiers by?» cannot be answered without first explaining printed identifiers,
scoping and collisions — which is the conversation that produced this bead.
`CreateAccountRequest` already carries `institution` beside `provider`, so the
item was publishing the derived field and leaving the answerable one unasked.

`iaam-9i83` owns both halves of that — whether the label is minted rather than
asked for, and whether the question becomes the institution. `/provider` is
therefore the single entry in `QUESTIONS_UNDER_REVIEW`, and it carries no
wording, deliberately.

### 6. Every existing owner field is filled

Twenty questions across eleven calls, covering every `ProvidedBy::Owner` site the
queue publishes. An item that gained the field and left it empty would have moved
the defect rather than fixed it.

## Non-vacuity

`iaam-3nqt` exists because a guard checked only existence, and wave V's
`the_offer_scan_reads_resolutions_and_not_prose_or_fixtures` is the shape a proof
takes here: the rule is made against an input written in the test, where what the
answer should be is not in question, and the *reason* is asserted rather than the
refusal.

`a_question_for_a_person_is_not_a_field_name` does that against the strings this
defect actually produced. `provider_account_id` is refused as a wire name;
`/title` as a pointer; this API's own «Whatever the source prints for this
account» as not a question — fluent English, no field name in it, and still
nothing a person can answer, which is the point; «the title is yours to decide»
and «you can change it later» are refused as saying nothing that turns on the
answer, because both are true of nearly every field in the vocabulary. Each
specimen differs from a passing question in exactly one way, so the rule that
fires is the rule being proved.

`the_sweep_sees_every_question_this_vocabulary_has` is the other half, in the
shape `the_offer_scan_finds_the_resolutions_the_crate_builds` takes: the fields
the queue actually asks the owner for are compared with the fields this
vocabulary declares, in both directions. A question no item publishes is prose
nothing asks; an owner field the heap never reaches is a field the guard never
ran on.

**What no test holds.** Whether a person who has never read this codebase can
answer a question is not decidable by a rule, and the checks above are the
mechanical half of the owner's register and nothing more. The acceptance test is
the owner. A prompt that satisfies every guard here and still leaves him asking
«на что это влияет» has failed.

## Consequences

A client relaying an item to the owner now has, per field he must fill in, a
question in his words and a statement of what his answer changes — and a
statement, in the contract, that a preset value is not to be read out to him. The
pointer and the schema descriptions go back to being what they always were:
material for whoever writes the client.

`GET /v1/actions` gains one optional object per missing field. Nothing is
removed, and a client that ignores `prompt` reads exactly what it read before.

`iaam-9i83` remains open on `/provider`, and this decision deliberately does not
pre-empt it: the register entry is what keeps the field visible while it is
decided, instead of a comfortable sentence closing the question by making it
painless to leave.
