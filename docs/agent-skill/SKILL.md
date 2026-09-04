---
name: iaam
description: IAAM keeps the books on one owner's own money. Use it when he asks how much he spent and on what, where his money went, what he holds and what it is worth, how his money has done — or when he has a statement or an export from a bank or a broker that needs sorting out and recording.
---

# IAAM — personal accounting

## Bootstrap

This file explains **meaning**, and holds no address of any kind. What a running
instance serves is answered by the instance and never from here: this file once
claimed implemented routes were unimplemented, and work the system could do went
undone.

Three steps, in order:

1. `/.well-known/api-catalog` (RFC 9727) returns a linkset (RFC 9264). Its
   `service-desc` link addresses the machine-readable contract; its `status`
   link addresses the health resource. Its `related` links address the rest of
   the way in: the outstanding-work queue, the scopes a report is computed over,
   and the four questions this system answers about money — each tagged with the
   `goal` name the queue's items and every report's confidence register also use,
   so a caveat naming a goal leads to the resource that answers it. Take
   addresses from that document, never from this file, which holds none.
2. The document behind `service-desc` is the contract: the routes, methods,
   request and response schemas and status codes this instance actually serves.
   Read it instead of asking a human which routes exist, which fields are
   required, or what a refusal will look like.
3. The actions operation declared in that contract answers what **this**
   instance needs next, computed from its own state. Each item carries the
   operation to call, its address resolved from the same contract, the fields
   already decided, and the fields still missing together with who must supply
   each. Work that queue; do not reconstruct an order of setup from memory.

   **An item whose `state` is `settled` wants nothing, and it is not work.** The
   owner decided something; the item says what his decision left standing — the
   records it keeps out of every report, and why — and the only call it publishes
   is the withdrawal of that decision, which is his to send. Show such an item
   when he asks what has been decided. Do not raise it as work, do not collect a
   field for it, and do not go looking for what it is holding up: nothing is. It
   is not the word for «no call in this API touches this», which publishes no way
   out at all.

   **Read `requiredScope` on the resolution, not only on the item.** An item with
   several ways out publishes one per resolution, and they need not agree. The
   item's own is the narrowest — «there is at least one call here you may make»,
   never «you may make them all» — so filter the queue by the item's and choose
   among the resolutions by theirs. It is a floor and not a promise: a call you
   may make can still be refused for what the request says or for what the
   journal holds. `providedBy` on a missing field is a separate question — who
   holds the value, not who may transmit it.

   **A field the owner must fill in arrives with the question to put to him, and
   that question is the whole of what you show him.** Each missing field marked
   as his carries a `prompt` with two parts: `ask`, what he is being asked and
   why, and `consequence`, what is different depending on how he answers. Relay
   both. Never put the field's pointer to him, and never read him the contract's
   field descriptions: those are written for whoever implements a client.

   A missing field of his that carries no `prompt` is not an invitation to
   compose one. It means this instance has not decided whether a person should be
   asked for that field at all; ask about the fields that do carry one, and leave
   that one to him or to whoever runs the instance. There is at present no such
   field.

   **Never read a `preset` value out to him.** `preset` is the request already
   filled in from what the instance worked out — an address in the route, a
   composition, an identifier a document printed — and none of it is a question.
   Send it as it stands.

   **The fields of one call may be put to him in one breath.** `missing` is one
   call's fields, in the order to ask them, and you hold all of it before you say
   anything to him: show them together and send one request. Each keeps its own
   question — do not fuse them into one sentence you then have to take apart —
   but its own question is not its own exchange. Asking them one after another is
   not more careful, only twice as long.

   **A field the call is accepted without says so, and you offer him a way past
   it.** Read `optional` beside every missing field. Where it is set, the call
   goes through with the field left out, and the question's `consequence` says
   what leaving it out costs — put both to him, so that «I don't know» and «skip
   it» are answers he can give. Where it is not set, the call is refused without
   the field and there is nothing to offer: that is not a field to invent a value
   for. Optional does not mean unimportant.

   **An item may carry an answer he can give once for several of them.** A
   missing field may publish a `proposal`: a value this instance worked out, the
   question to put to him about the whole set, and `covers` — the items that one
   answer fills. Read the question out; nothing is recorded until he agrees, and
   if he does not, you ask the field's own question item by item instead. If he
   agrees, send one call per item named in `covers`, each with **its own**
   `value`: one decision does not mean one value. `covers` is complete — an item
   that cannot take the answer is not in it, so never carry the answer to an item
   the offer does not name. Two institutions' names are two sets and two
   questions.

A credential is not obtained through the API. It is issued at the console by
whoever runs the instance and handed to you; no call produces one. If a call is
refused for want of one, say so — there is no other route to try.

Everything below is what those three steps cannot tell you.

## The overriding rule

**Arithmetic of your own is forbidden.** Every number in your answer must be
present verbatim in the API's answer. Do not add amounts together, do not
compute percentages, do not convert currencies, do not estimate a return
"roughly".
A number that is not in the API's answer is an error — even when it is correct.

If the API refused to compute a quantity, the answer says exactly that: the
system cannot compute it, and here is why. Replacing a refusal with an estimate
of your own is the most expensive mistake that can be made here.

**A number can be verbatim and still be the wrong thing said. Read
`reading-the-reports.md` before you quote any figure of a report back to him** —
when a cash figure is a balance and when it is not, whose money a report counted,
what the return figure may be called, what an unconfirmed posting means, and
which facts may be quoted as they stand and which values may not.

## The agent is an external client

The agent is not part of the system and has no access to its storage. It does
not write to the journal directly: a record is the outcome of passing ingest,
not a separate action. It does not create accounts or contours: the portfolio's
boundary is drawn by the owner. It does not rule on what is already recorded —
retracting a fact the owner holds is his act, and his credential is what the
system will accept for it. The one exception is narrow and is set out under «A
mistake is retracted, not erased» in `correcting.md`: an agent may take back an
import it declared, while nothing has been built on it. And it does not **read**
the owner's statements. It may carry one to his instance, which is the ordinary
way an import starts; what reads it is the instance, and what the agent knows
about the contents is what the API answered.

From this follows the thing that is easiest to violate out of the best
intentions: a missing value is asked of the owner, not filled in. A guess that
has reached the journal is indistinguishable from a fact — every report will
read it as one, and only the owner, who knows what actually happened, can
retract it.

**Before you propose undoing, retracting or re-sending anything already
recorded, read `correcting.md`.** It holds why append-only does not mean
irrevocable, whose act a correction is, the one import an agent may take back
and the bound that is checked rather than trusted on it, and why re-sending a
corrected row writes nothing at all.

**The boundary he draws, and the vocabulary that goes with it, are in
`the-money-and-the-perimeter.md`. Read it before you answer anything about what
he holds or about what a figure covers, and before you act on «this product is
closed, take it out».** It holds what a contour is and what its version means,
why a closed product is retired rather than dropped and what the retirement does
and does not change, what a category is and what it cannot change, how a string
naming one of his accounts is read on every channel, and how an instrument's
external code resolves as of a date.

## What is published is what to convey; the words are yours

Everything this system publishes for the owner — a field's question and what
turns on the answer, an alternative and what it does to his report, an item's
reason — is **source material and not a script**. It fixes what must reach him:
what is being decided, what it is for, and what is different depending on how he
answers. The wording is yours, and it is owed to him: his language, his register,
and no suggestion that a machine is speaking. This file is written in English
because everything written down about this system is; it says nothing about the
language you speak to a person in.

**A relay is one sentence.** Name what is being decided, in his words, with what
turns on it — and stop, ready to say more if he asks. Not a transcript, not a
status report, and not our sentence in quotation marks. Quoting is the failure at
one end and composing a question of your own is the failure at the other: the
obligations are the content and the freedom is only the wording, so a relay that
drops what the choice changes has failed in whatever language it was written.

**One sentence per decision, not per line of his statement.** Thirty lines naming
one shop are one decision, and what is the same decision is published beside the
question rather than left to you to work out. Put it to him once.

**Nothing of the machinery reaches him.** Not the words an answer is sent as, not
an item's state or its urgency, not a value already filled in, not the fact that
a field is optional — tell him instead that he can leave it and what leaving it
costs — and not the numbers this project files its own decisions under. A
client's control flow is not a conversation.

**Do not narrate what you did to find out.** Which state an item is in, how
something is classified now, whether anything is blocked: that is your work, and
never the answer to a question he asked. Tell him what you found, or ask him what
you could not work out.

**Before you put a question about one of his rows to him, read «A question is a
thing, not a sentence» in `importing.md`.** What has to reach him about such a
question — the day and the sum that name the row he is being asked about, the
answers it admits, and what each of them does to the figures of his year — is
fixed there, and none of it is yours to compose.

**And the line the freedom stops at.** The wording is yours; the decision is not.
Never answer in his place, never read silence as a value, and never narrow what
he is being asked because a shorter sentence reads better — where two answers
land in different figures of his year, he hears both. Rendering a question is
yours and answering it is his, which is the same boundary as an agent conveying a
document it may not interpret, one level up.

## Where an import begins: carry the document, do not read it

An import begins with a document the owner has — a statement, an export, a file
his institution gave him. There are two acts you can perform on it, and the
difference between them is the whole rule.

**You may convey it.** Handing the document to his own instance is the ordinary
way an import starts; the contract names the operation. The instance reads it
through a profile written for that institution and document type, and what you
get back is a session holding rows.

**You may not interpret it.** Do not parse it, do not summarise its rows, do not
tabulate it, and do not decide what a row was — not its direction, not its kind,
not whose account is on the other side, not its category. This is not a rule
about secrecy: the amounts, the dates and the counterparties reach you anyway,
through the assessment and through every question the session raises. It is a
rule about a format having one reader. A reading of your own is a second
implementation of that institution's rules, it does not fail loudly, and what it
produces is an import that files the wrong operations with nothing saying so.
Which reader read a row is recorded on the fact, for as long as the fact exists.

**If you cannot reach the document, say so.** An agent that does not run on the
machine holding the file cannot convey it, and no reading of this lets you
interpret it instead. What is left is poorer and you name it as poorer to him:
ask him for the values, submit them as what the source stated — the shape is «A
row you cannot classify is submitted as such» in `importing.md` — and conclude
nothing on his behalf. Every row that no rule of his already matches becomes a
question he has to answer.

**An empty instance does not know his accounts, and it will tell you which it
needs.** A statement names the account each record is on in the institution's own
words, and a name this instance holds no account for refuses that record — so a
first import looks like a wall of refusals and is not. Convey the document to a
session that declares no account, and the response summarises those refusals as
the distinct account names the document asked for, each with the number of
records it accounts for. Create an account for each, giving it the printed string
as the identifier its source prints for it — not as the title, which is his and
which he may change — and convey the same document again. The row keys are over
the document and the line, so nothing imports twice. The outstanding-work queue
publishes the same names once a document has been read, so you need not provoke
the refusals again.

You do not have to guess a name and you must not. Creating an account whose title
you invented does not make the records import; it makes a second account he did
not ask for.

**Before you send any string that names one of his accounts, read «An account is
named by an identifier, and every channel reads the same ones» in
`the-money-and-the-perimeter.md`.** Three vocabularies are read, in one order, on
every channel and whether the row arrived as a document or as a request body —
and one of the three you must not send at all.

**But do not ask him name by name what he can answer once.** Each name is its own
item, because an account created for one settles no other — and the items from
one institution's documents carry, on each of the two fields they ask, one answer
he may give for all of them: they are all held at the institution that printed
them, and they are called what the statement calls them. That is the `proposal`
of the bootstrap section. Put those two questions first, and what is left is
whatever he wants to say differently.

**And some of those names are not his accounts at all** — a party he paid, an
account that is not his. For those, creating an account is the wrong act and not
one you may take on his behalf. The queue's item offers two ways out and not one:
create the account, or record his statement that the name is nobody's account of
his, with the reason he gives you. Put both to him. Do not leave the item
standing because neither looked like something you could act on: while it stands
it is graded as work between him and every report he asks for, and it will be
there again next month with the same name on it.

**Never a credential but your own**, whatever the document is. No broker token,
no encryption key. An import that would need you to fetch the statement out of
the institution yourself is not one you can do.

**Everything that happens after the document is conveyed is in `importing.md`,
and you read it before you feed a row, before you put a question to him, and
before you commit anything.** It holds the shape a row you have not concluded
about is submitted as and when not to use it, what a question is and how one is
answered and how far one answer reaches, what a first import can settle without
asking him about every line, how an import is held open and how it is ended, how
an amount and a transfer between two of his own accounts are stated, what an
idempotency key names, and what may be asserted for a position older than the
journal.

## The four files beside this one

This file is the process. Everything else written down about this system is in
four files in this directory, each read when the work reaches it — not at the
start, and not never. What fires before you know which part of the domain you
are in is already above; what is in these is what you cannot guess once you are
in it.

- **`importing.md` — read it before you feed a row, put a question to him, or
  commit anything.** The shape a row you cannot classify is submitted as, and
  when not to use it; a question as a durable thing, the answers it admits, the
  one answer that reaches many rows, and what a first import can settle without
  asking him line by line; an import held open, and both ways it ends; how an
  amount and a transfer between two of his own accounts are stated; what an
  idempotency key names; what may be asserted for a position older than the
  journal.
- **`the-money-and-the-perimeter.md` — read it before you answer what he holds,
  what a figure covers, or what his money was for.** What a contour is and what
  its version means; why a closed product is retired rather than dropped, what
  the retirement changes and what it never changes; what a category is and what
  it cannot change; how a string naming one of his accounts is read; how an
  instrument's external code resolves as of a date.
- **`correcting.md` — read it before you propose to undo, retract or fix
  anything already recorded.** Why append-only does not mean irrevocable; whose
  act a correction is; the one import you may take back, and the bound checked
  on it; why re-importing a corrected row writes nothing.
- **`reading-the-reports.md` — read it before you quote any figure of a report
  back to him.** What makes a confirmation independent; when a cash figure is a
  balance and when it is a movement from an unasserted start; the population a
  figure was folded over, and what `whole` does not claim; what the return figure
  may be called; what an unconfirmed posting does and does not mean; which facts
  may be quoted and which values may not.

## What the system does not do

It does not compute taxes, does not compute TWR, a value series or a return
over an arbitrary sub-interval, does not implement the economics of shorts,
margin and derivatives, and does not recover a lost encryption key from a
single database. The price and the FX rates for a calculation must be supplied
by the input data or by the owner.

What the system can do **now** is a question for the system itself, not for
this file: the contract and the action queue answer it.
