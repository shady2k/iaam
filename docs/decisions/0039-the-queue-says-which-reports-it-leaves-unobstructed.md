# 0039. The queue says which reports it leaves unobstructed, and names what stands in the way of the rest

Date: 2026-09-05 · Status: proposed · Beads: `iaam-i3nx`

## Context

An agent that knows nothing about this system is handed an address and a token
and must become a competent interface to it. Four things make it one: what this
instance is, what can be done now, what needs doing, and where things stand.

Three of the four it can assemble today. The discovery document names the
questions this system answers about money; the contract says what may be called;
the outstanding-work queue says what needs doing, item by item, with the call
that closes each. The fourth it cannot assemble, and it is the one a person asks
first:

> Which of these reports can you show me now, and what is in the way of the
> others?

**The data was already here and the fold was not published.** Every item the
queue grades as required work declares which of the four reports its outstanding
work stands between the owner and — `asset_snapshot`, `money_flow`, `returns`,
`reconciliation`, the same four names each report's confidence register publishes
for itself. Nobody published the inverse. So an agent holding the queue could say
«there are five things to do» and could not say «I can show you where the money
went and what you hold; how it has done needs these two first», which is the
sentence a person wants and the one it was asked for.

A caller can of course fold the list itself, and the reason that is not an answer
is the same reason this whole line of work exists. It has to know that an item
declaring no goal at all can still stand in the way of every one of them — and
nothing told it that. The one such item is the one every new instance starts
with.

## Decision

### 1. The fold is published beside the items, on the same response

`GET /v1/actions` returns `{"items": […], "reports": […]}`. `reports` holds all
four report names, each with what it answers and the identity of every item in
`items` standing in its way.

**Not a route of its own.** The agent fetches the queue anyway; a second address
is a second thing to find, and not finding things is the disease this whole line
of work is about — a reviewer once ran an entire import without finding the
session assessment and wrote out as a wishlist the seven sections it already
answered. It is folded from the items that are about to be published, in the same
reading, so the two halves of one response cannot describe two states of the
instance.

### 2. The envelope comes back, and §1.4a is why rather than why not

`docs/api/conventions.md` §1.4a removed this route's wrapper. The wrapper held a
`policy_version` that was the literal `1`, derived from nothing and bumped by
nothing, and §1.5's question — is there a fact about the answer as a whole that
no item can carry? — had the honest answer «no».

It now has the honest answer «yes», and the reason is exactly the shape §1.4 gives
for `population`: **the fact is about what is not there.** A report with nothing
outstanding is stated by no item appearing, and an absence is the one thing an
item cannot carry. A response of items alone can say that five things are
outstanding; it cannot say that a sixth thing is not.

So this is §1 working twice rather than §1 being reversed. What changed is not the
rule but the answer to its question.

### 3. Named, and no flag beside the names

`blocked_by` carries item identifiers, in the queue's own order, so the first
named is the most urgent.

**Named and not counted.** A report that says «blocked by two items» and does not
say which two makes the caller scan the queue for them, and the identifiers are in
the same response already. It is decision 0034's defect one surface along: a group
that publishes its members and not what they share, seen from the other end.

**And no `answerable` flag.** It would be `blocked_by.is_empty()` published a
second time, which is 0034's rule about the count beside the list it counts —
two statements of one fact, in one response, that can come to disagree. What a
flag would have added is not a fact but a word, and the word is the next section.

### 4. A blocking item stands in the way of all four

An item stands between the owner and a report when its category is
`required_for_goal` and names that report, **or** when its category is
`blocking`.

**The second clause is what keeps the first honest, and there is a specimen.** A
freshly claimed instance's whole queue is one item, `create_first_account`. It is
`blocking`, so it names no goal — deliberately: `ActionKind::goals` grades what a
kind's *work is required for*, and a blocking item stops the next call rather
than any one report. Folded on the first clause alone, an instance holding no
account, no scope and no fact would answer «which reports can you produce for
me?» with «all four». That is the worst sentence this surface can produce, and it
would have been the first sentence every new instance produced.

**This is not a widening of `ActionKind::goals` to `ReportGoals::ALL`.** That
table stays the single statement of what a kind's work is required for, and
`Action::new` still refuses a required item that names no goal. Blocking is a
different fact — the system will not accept another act until this is done, so it
stands in the way of every report by standing in the way of everything — and
writing it into the table would grade a stop as required work and quietly delete
that refusal.

A recommendation and a statement of fact stand in the way of nothing, which is
what those two words already mean.

### 5. An empty list is the narrow claim, and the narrow claim is deliberate

**`blocked_by: []` says nothing in this queue stands in the way of this report.
It does not say the report is ready.**

The stronger word was available and is refused. A report states what it is silent
or partial about in its own confidence register — accounts outside every scope, a
cash figure accumulated from a start nothing asserts, movements the flow
quantities do not decompose — and the queue neither reads that register nor
summarises it. The two join on the goal's name and neither contains the other. A
queue that promised «ready» would be promising something the report it points at
can immediately contradict, and a promise the system contradicts a moment later
is worse than no promise, because it is believed once.

So the sentence a caller may put to a person is *«nothing outstanding stands in
the way of this one»*, and never *«this one is ready»*. That obligation is written
where the caller reads it — on the published description of `blocked_by` — and not
only here.

The wording is also what the sweep of an empty instance proves is needed: with the
blocking clause, four empty lists are unreachable from an instance holding
nothing. Without the narrow wording, four empty lists on a well-set-up instance
would still be over-claiming, because a set-up instance can hold a report full of
caveats and a queue with nothing outstanding.

### 6. The report's own sentence, published once and moved to the core

Under decision 0035, nothing published to be read out to him is stated only in
this system's own vocabulary. `money_flow` is our word: a caller offering a person
a choice of reports cannot offer him `money_flow`.

Such a sentence already existed — one line per goal, written for the discovery
catalog, where a cold client reads the four names and decides where to go. It was
**moved to `ReportGoal` in the core** and both surfaces now publish it. A second
set of sentences would be two answers to «what is `returns`?» from one system,
differing by however much two authors differ, which is the divergence
`ReportGoal::code` was moved to the core to prevent, one column over.

**It fixes what must be conveyed and not the words to convey it in**, which is
decision 0036 §6: a caller that says this in the owner's language and register is
using the sentence correctly. It is source material, not a script.

### 7. On the whole queue and on no slice of it

Three other responses embed the same items bound to what was asked of them: the
reconciliation answer's account and range, the broker sync outcome's own verdicts,
the money-flow report's own diagnostics. None of them gains a `reports` block.

«Nothing stands in the way of this report», folded out of a slice that was never
the queue, is precisely the false reassurance this system must never give by
accident. The fold therefore takes an already-computed queue rather than being
returned by `frontier`, and it is called at the one place the queue is published
whole.

### 8. Rejected

- **A second route.** §1 of the context: an agent that cannot find a thing does
  not use it, and this fold's whole purpose is to be found by an agent that has
  already fetched the queue.
- **Publishing the standings as items in the queue.** «`asset_snapshot` is
  unobstructed» is not outstanding work, and the queue is a list of outstanding
  work. It would also make the emptiness unrepresentable in the one direction
  that matters: an item saying nothing is in the way is still an item, and a
  caller counting the queue would count it.
- **A widened `ActionKind::goals`**, and **an `answerable` flag** — §4 and §3.
- **A `report_standings` returned by `frontier`.** It would then be computed at
  the three call sites that publish a slice, and the temptation to publish it
  there is exactly what §7 refuses. Taking a slice of computed items makes the
  narrowness visible at the call site.

## Non-vacuity

`iaam-3nqt` exists because a guard checked only existence. Each test here is made
against an input written in the test, and the sweep is followed by named
witnesses, because a sweep over the properties alone is satisfied by a fold that
puts every item into every report.

`an_instance_whose_queue_is_one_blocking_item_publishes_no_unobstructed_report`
is the specimen §4 is about, and it asserts **the item's own goal set is empty**
before it asserts anything about the standings. Without that line the test would
pass on the first clause and prove nothing about the second; with it, the only
way to satisfy the test is the clause being proved.

`a_report_names_the_items_standing_in_its_way_and_no_others` fails in both
directions on purpose: two reports are obstructed and two are not, so a fold that
named everything everywhere breaks the empty pair and one that named nothing
breaks the full pair. The recommendation in its queue is the third witness — the
item that must appear in no standing at all.

`the_queue_says_where_each_report_stands_and_names_what_stands_in_the_way` checks
the wire in both directions item by item — an item's identity is in a report's
list **exactly when** the item is blocking or names that report — and then names
two witnesses, because the item-by-item equivalence is satisfied by a fold that
is the identity function on a queue where every item happens to name every goal.
The first witness is a fixture where the standings differ from each other; the
second is the empty instance, where the sole item declares no goal and must
nevertheless appear four times.

`every_goal_says_what_it_answers_without_using_this_systems_own_words` refuses the
three ways of publishing a sentence that gives a reader nothing: this system's
own words, the code repeated, and one sentence doing duty for two reports. It is
asserted against the codes and against the other sentences, never against four
strings written out in the test — a list of expected sentences stays green by
being edited to whatever the sentences became.

**What no test holds.** Whether the narrow claim in §5 is narrow enough is not
decidable by a rule; decision 0027 already says so about the register it belongs
to, and the acceptance test is the owner.

## Consequences

`GET /v1/actions` returns an object where it returned an array. This is a
breaking change to a published shape, and it is taken rather than adding a route:
the alternative publishes the same fact where the caller that needs it is not
looking. The list is under `items`, which is the key the earlier wrapper used and
§1.3's default, so a client that reads one key finds the queue where it was.

The three responses that embed these items are untouched, and a client reading
only `ActionDto` reads exactly what it read before.

`ReportGoal` gains one accessor and `iaam_server::api_catalog` loses the private
table it was the move of. The catalog's document is byte-for-byte what it was.

No migration. Nothing is stored: the standings are a fold over a queue that was
already computed from the store on every reading.
