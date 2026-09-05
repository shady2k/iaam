# The skill leads the owner in conversation, and the API keeps what it already answers

Bead: `iaam-1pfo` · Prior: `iaam-arad`, `iaam-zu6m`, epic `iaam-l5y9` (E9) ·
Decisions: 0035, 0036, 0037, 0039 · Date: 2026-09-05

## What this is for

A live assistant, handed a fully working instance, opened a session by listing
what was in the queue and asking the owner where to start. The session before it
asked him how to import a month. Both are questions the instance had already
answered before either was asked, and both were asked because the document the
assistant reads is written for whoever implements a client, not for whoever
talks to the owner.

`iaam-arad` fixed the entry point — the file now opens with an act — and the
defect survived it, because the register of the remaining three hundred lines is
an operator's manual and register is what an assistant imitates. The owner's own
diagnosis is the one this spec is built on: **this is an instruction for a
financial assistant, not an administrator's guide to the API.**

Two things follow, and the second is the larger one.

1. The skill has to describe the **conversation**: what the owner hears first,
   how the assistant leads him through what needs him, and what he never hears.
   None of that is written down anywhere today.
2. Most of what *is* written down is a second copy of what a response already
   carries. `docs/agent-skill/` is 1275 lines across five files, and the
   assistant needs a small fraction of it before it makes a call. E9 settled the
   principle — the API's answers direct the agent, so no skill has to — and this
   spec applies it to the file E9 left behind.

Distribution of the skill is **out of scope** and deferred: the image ships the
binary alone, nothing packages `docs/agent-skill/`, and a skill small enough to
hand over whole makes that question smaller rather than answering it.

## 1. The criterion: three questions, in this order

Every paragraph of the five files is put to these, in order, and the first that
answers decides it:

1. **Does the response already carry this?** → delete it. Specimen:
   «the words and their effects travel with the question on every surface that
   publishes one … if you find yourself hunting for them elsewhere, you are
   reading something stale» (`importing.md`). A paragraph that describes a
   payload which describes itself is a copy that can go stale, and one of those
   told agents for weeks that implemented routes were unimplemented.
2. **Is there a field or a response whose published description can carry it?**
   → move it there, onto that carrier. The project already does this: decision
   0039 put the obligation to say *«nothing outstanding stands in the way of
   this one»* and never *«this one is ready»* onto the published description of
   `blocked_by`, «where the caller reads it, and not only here». A running
   instance serves its own contract, so a rule that lives there needs no
   packaging and cannot drift from the code it describes.
3. **Can the failure happen when no call is in flight?** → it stays in the
   skill. This is the whole of what may occupy context at turn one.

The third class is small and irreducible. The assistant that adds two figures of
its own has produced no request for a description to sit on; so has the one that
reads the owner's statement itself, uses a credential that is not its own,
answers in his place, or opens with a menu. There is no response to attach any of
those to, because the failure is the absence of one.

## 2. The order of work: the carrier lands before the paragraph goes

A paragraph is deleted **only once something published answers it.** Not as
caution — as the rule `iaam-zu6m` closed under: «a section is removed when
something computed answers it, not before». Reversing the order loses knowledge
that was paid for in defects, and the loss is silent.

Consequently the work is sequenced per carrier, not per file: each contract
description is written, and the paragraph it replaces is removed in the same
change, so the two halves cannot ship apart.

## 3. The new `SKILL.md`

One file, no companions, about 120–150 lines. Sections in order:

1. **Frontmatter.** The `description` fires on what the owner actually says
   about his money — the sentence that decides whether the skill is loaded at
   all. Decision 0037 already made this point and the current string is close;
   it is re-read against the four questions and left alone if it holds.
2. **Who you are to him.** An assistant for one person's money, and the four
   questions the two of you answer, in his words: what he holds, where the money
   went, what it earned, whether the books agree with what his institutions say.
   He is not an operator of this system and none of its parts are his business.
3. **How a session opens.** Before saying anything, read the instance. The first
   thing he hears is where his money stands: what can be shown him now and what
   is missing for the rest, in terms of his money and not of the queue's items —
   *«spending for August I can show you; what it earned I cannot yet, one
   account's August is not sorted out»*. Where nothing is missing, a short look
   at what he holds and where the money went, quoted under the rules for quoting
   a figure. **A menu is forbidden in as many words**: the instance returned an
   order, and «where shall we start» is a question it did not publish.
4. **How the conversation goes.** You lead. Most urgent first, one decision at a
   time, only the questions the instance published, and never a question of your
   own about the shape of the session. A session ends on what is left, said in
   his terms, and not on a report of what you did.
5. **What he never hears.** The machinery, named as a list so it is checkable by
   a reader: an item's state or its urgency, identifiers of any kind, import
   sessions, counts of outstanding items, this project's own decision numbers,
   the vocabulary the system files things under, and anything about the
   container, the build or the schema. Today this is one clause in the middle of
   a paragraph, and it was violated wholesale in the session that prompted this
   work.
6. **No arithmetic of your own.** Kept as it stands: every number in an answer is
   present verbatim in the API's answer, and a refusal to compute is relayed as
   a refusal.
7. **You are an external client.** You do not read his documents — you convey
   them, and what reads them is the instance. You never hold a credential but
   your own. You never answer for him, never read silence as a value, and never
   fill in a missing value yourself: a guess that reaches the journal is
   indistinguishable from a fact.
8. **What the system does not do.** Kept, with one line added: it does not plan
   a budget or hold limits. The four questions are analysis of what happened,
   and an assistant that offers planning is promising what nothing implements.
9. **Where everything else is.** The instance's own contract and its
   outstanding-work queue. No file names, because there are none.

## 4. What moves into the contract, and onto what

Classified at section granularity; the per-paragraph pass happens as each is
implemented, under §1's three questions.

| Now in | Carrier |
| --- | --- |
| The question to a row, its admissible answers, and what each does to his year (`importing.md`) | The published question itself — it already carries them |
| The accounts an answer may name | The question — already carried |
| Naming the row by the day and the sum the source printed | The question's own description |
| Row shapes for a row that cannot be classified, transfers as one row, how an amount is stated | The request schemas that accept them |
| A held session, how it ends, idempotency | The session resource's description |
| What retirement changes and what it never changes; a retirement never hides money | The retirement operation's description |
| A cash figure anchored or not; the population a figure was folded over; `whole`; an unconfirmed posting | The report's confidence register, which already carries caveats and their remedies |
| Reference facts quotable, derived values not; completeness boundary; the three price bases | The reference responses that carry the values |
| What a contour is, what a category cannot change, how an account name is read, how an instrument code resolves | Split under §1: the parts a request refuses belong on the refusal; the parts that are meaning the instance never states belong in `SKILL.md` §2 or in `docs/`, not in the skill |

The last row is the one that will not be uniform, and it is named as such rather
than assumed away.

## 5. The guard

`scripts/check-agent-skill.sh` refuses a versioned route path, an HTTP method
written as an instruction, and a status code, in every markdown file of the
directory. A fourth refusal is added: **the name of a payload field.**

A document that names `requiredScope`, `preset`, `covers`, `prompt` or
`consequence` is narrating the payload, which is the disease the first three
refusals treat, one level down. The shape is checkable without a list of fields —
a backticked identifier in `lowerCamelCase` or `snake_case` — so the guard cannot
go stale when the API changes, which is the property the existing three were
built for.

It is added **last**, when the moves are done: today the file contains about a
dozen such names, so the guard would refuse the document it is meant to protect.
Adding it at the end is also what proves the work finished.

## 6. Risks

- **Knowledge lost in the deletion.** Mitigated by §2's ordering and by nothing
  else. If a paragraph has no carrier and no place in the new file, it is not
  deleted — it is raised, and the answer may be that a carrier has to be built.
- **The contract inflates.** Descriptions are read by whoever is already reading
  that field, so the cost falls where the benefit does; but a description that
  becomes an essay has moved the problem rather than solved it. Each stays a
  sentence or two, in the owner-readable register decision 0035 requires of
  anything published to be read out.
- **The register slips back.** This is the second attempt — `iaam-arad` was the
  first — and prose alone did not hold. The guard in §5 is what makes the
  outcome checkable rather than a matter of taste, and it is the reason §5 is in
  scope rather than a follow-up.
- **`docs/import-boundary.md` and the skill are still unchecked against each
  other** (`iaam-xc01`, open). Untouched here; the shrink makes the surface it
  covers smaller.

## 7. Success criteria

1. `SKILL.md` is one file, no companions, and contains no payload field name, no
   route, no method and no status code — the guard proves it.
2. It states, in as many words, that the session opens on where his money stands
   and that offering him a choice of where to start is forbidden.
3. Every paragraph removed from the five files is either answered by a payload
   that already carried it, or answered by a contract description that landed in
   the same change.
4. `make check` passes, the existing three refusals included.
5. A cold assistant reading only the new file and the instance's own answers can
   open a session, put the first decision to the owner, and quote him a figure
   without reading anything else.
