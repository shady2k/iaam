# Every paragraph of the agent skill, classified against its carrier

Bead: `iaam-hya7` · Task 2 of the wave · Branch `wave-ac/skill-conversation` ·
Design: `.internal/specs/2026-09-05-the-skill-leads-the-conversation-design.md` ·
Date: 2026-09-05

## What this is

The five files under `docs/agent-skill/` are 1275 lines. Every paragraph of them
is put here to the design's three questions, in order, and the first that answers
decides it:

1. **Does the response already carry this?** → **class 1**, delete. The carrier
   column names the field or object of the response that already says it.
2. **Is there a field or a response whose published description can carry it?** →
   **class 2**, move. The carrier column names an exact `Struct::field` in
   `crates/iaam-server/src/dto.rs`, and the description is written there in the
   same change that removes the paragraph.
3. **Can the failure happen when no call is in flight?** → **class 3**, keep. The
   destination column names the `SKILL.md` heading it lands under.

Tasks 3–6 work from this table and from nothing else. A row is an instruction:
class 1 means *delete, write nothing*; class 2 means *write that description,
then delete*; class 3 means *keep, under that heading*.

### Conventions

- **Paragraph** is the first six words of a blank-line-separated block. A list
  item is its own block, because the obligations in these files are written as
  list items as often as as prose.
- **A paragraph carrying two separable obligations is split** into `a` and `b`
  rows rather than forced into one class. Folding them would lose the second, and
  losing knowledge is the one thing the ordering rule in §2 of the design exists
  to prevent.
- **†** marks a class-1 row whose paragraph is a **pointer or a restatement** —
  a cross-reference to one of the four companion files, or a preamble repeating a
  rule that already stands in `SKILL.md`. Its carrier column says what carries
  the thing it points at. Tasks 3–6 delete these and write nothing: the file the
  pointer names is being emptied, and the rule the preamble restates is already
  in the file it restates it from.
- **Class-2 carriers are `pub` fields** of `crates/iaam-server/src/dto.rs`, so
  that every one of them is checkable by grep. Where the only true carrier is an
  enum variant of a request schema — `OperationKindDto::UnresolvedDirection`,
  `::Transfer` — the carrier named is the **flattened field that publishes that
  schema**, `OperationDto::kind`, and the note says which variant the sentence
  belongs on.
- **Destinations are headings that exist in the current `SKILL.md`** (the one
  Task 1 left). One exception is marked and explained: the frontmatter is not a
  heading.

## `docs/agent-skill/SKILL.md`

| Heading | Paragraph | Class | Carrier | Destination | Note |
| --- | --- | --- | --- | --- | --- |
| `# IAAM — personal accounting` (frontmatter) | `name: iaam description: IAAM keeps the` | 3 | — | *(frontmatter — not a heading)* | The string that decides whether the skill is loaded at all. Design §3.1 re-read it and left it alone; Task 1 confirmed. The one class-3 destination that is not a heading, deliberately. |
| `# IAAM — personal accounting` | *(title only, no paragraph)* | 3 | — | `# IAAM — personal accounting` | The file's own title. |
| `## Who you are to him` | `You are the assistant for one` | 3 | — | `## Who you are to him` | The four questions in his words. No response states who the reader is to the owner. |
| `## Who you are to him` | `He is not an operator of` | 3 | — | `## Who you are to him` | |
| `## How a session opens` | `Read the instance before you say` | 3 | — | `## How a session opens` | The failure — speaking before reading — happens with no call in flight, by definition. |
| `## How a session opens` | `Then open on his money, and` | 3 | — | `## How a session opens` | |
| `## How a session opens` | `**Never offer him a choice of` | 3 | — | `## How a session opens` | The menu. There is no response to attach this to: the failure is a question the instance did not publish. |
| `## How the conversation goes` | `You lead. Take what is most` | 3 | — | `## How the conversation goes` | |
| `## How the conversation goes` | `A session ends on what is` | 3 | — | `## How the conversation goes` | |
| `## What he never hears` | `None of the machinery reaches him.` | 3 | — | `## What he never hears` | |
| `## What he never hears` | `Nor how you found out. Which` | 3 | — | `## What he never hears` | |
| `## Bootstrap` | `This file explains **meaning**, and holds` | 3 | — | `## Bootstrap` | Holding no address is a property of the file, checkable before any call. |
| `## Bootstrap` | `Three steps, in order:` | 3 | — | `## Bootstrap` | Scaffolding of the three steps below. |
| `## Bootstrap` | `1. \`/.well-known/api-catalog\` (RFC 9727) returns a` | 3 | — | `## Bootstrap` | Where to start cannot be published by a response nobody has fetched yet. |
| `## Bootstrap` | `2. The document behind \`service-desc\` is` | 3 | — | `## Bootstrap` | |
| `## Bootstrap` | `3. The actions operation declared in` | 3 | — | `## Bootstrap` | The imperative («work that queue, do not reconstruct an order from memory») is class 3. The enumeration inside it — the operation, its address, the fields decided, the fields missing with who supplies each — is `ActionTargetDto`, `RequestPlanDto::preset`, `RequestPlanDto::missing` and `MissingInputDto::provided_by`, and goes with the paragraph. |
| `## Bootstrap` | `**An item whose \`state\` is \`settled\`` | **2** | `ActionDto::state` | — | `state` has no doc comment at all. What `settled` means — nothing is wanted, the only call published is the owner's withdrawal, it is not «no call in this API touches this» — lives in a Rust comment in `routes.rs:250` that nothing publishes. |
| `## Bootstrap` | `**Read \`requiredScope\` on the resolution, not` | 1 | `ResolutionOptionDto::required_scope`; `MissingInputDto::provided_by` | — | The resolution's description already says «An item's own `required_scope` is a summary — the narrowest scope reaching *any* of its resolutions» and «A floor, not a promise the call will succeed». `providedBy` is a published vocabulary saying it names the holder and not the work. |
| `## Bootstrap` | `**A field the owner must fill` | 1 | `MissingInputDto::prompt`; `OwnerQuestionDto::ask`; `OwnerQuestionDto::consequence` | — | «Show this to him», «Show this to him too», and «`pointer` and the descriptions in this schema are for whoever writes a client, and showing either of those to the owner is the defect this field closes». |
| `## Bootstrap` | `A missing field of his that` | 1 | `MissingInputDto::prompt` | — | «Absent on an `owner` field means the question itself is under review … Do not write one for it.» The skill's «There is at present no such field» is a fact about today's instance and is exactly the kind of sentence that goes stale. |
| `## Bootstrap` | `**Never read a \`preset\` value out` | 1 | `RequestPlanDto::preset` | — | «Spread into the request body and never shown to the owner … none of it is a question.» |
| `## Bootstrap` | `**The fields of one call may` | 1 | `MissingInputDto` (struct description) | — | Carried verbatim, `iaam-zxc6`: «its own words» is not «its own exchange», `missing` is one call's fields in the order to ask them. |
| `## Bootstrap` | `**A field the call is accepted` | 1 | `MissingInputDto::optional` | — | Carried verbatim, `iaam-4fsw`, including «Optional does not mean unimportant» as «it is not the item saying the answer does not matter». |
| `## Bootstrap` | `**An item may carry an answer` | 1 | `ProposedAnswerDto` (struct); `ProposedAnswerDto::covers`; `ProposedAnswerDto::value` | — | Carried verbatim, `iaam-hdr7`, including «one decision does not mean one value» and that `covers` is complete. |
| `## Bootstrap` | `A credential is not obtained through` | 3 | — | `## The agent is an external client` | No call produces one, so no response can say so. Design §7. |
| `## Bootstrap` | `Everything below is what those three` | 3 | — | `## Bootstrap` | Scaffolding; it survives only as long as there is something below. |
| `## The overriding rule` | `**Arithmetic of your own is forbidden.**` | 3 | — | `## The overriding rule` | Design §6. The agent that adds two figures has produced no request for a description to sit on. |
| `## The overriding rule` | `If the API refused to compute` | 3 | — | `## The overriding rule` | `NotComputableCodeDto`'s vocabulary says «A refusal is an answer: pass it on, do not recompute it», but the failure being guarded against is the agent substituting its own estimate, which no response sees. |
| `## The overriding rule` | `**A number can be verbatim and` | 1 † | `ConfidenceDto::caveats`; `CashFigureDto` | — | A pointer to `reading-the-reports.md`. Everything it lists — when a cash figure is a balance, whose money was counted, what the return may be called, an unconfirmed posting — is the confidence register and the self-naming cash figure. |
| `## The agent is an external client` | `The agent is not part of` | 3 | — | `## The agent is an external client` | Design §7. Includes «it does not read the owner's statements — it may carry one». |
| `## The agent is an external client` | `From this follows the thing that` | 3 | — | `## The agent is an external client` | A guess that reached the journal. Design §7. |
| `## The agent is an external client` | `**Before you propose undoing, retracting or` | 1 † | `SubmitCorrectionsRequest::acknowledge_retraction`; `CorrectImportRequest::source`; `OperationDto::idempotency_key` | — | Pointer to `correcting.md`. |
| `## The agent is an external client` | `**The boundary he draws, and the` | 1 † | `AccountRetirementStateDto`; `ContourDto`; `OperationDto::account`; `ResolveInstrumentRequest::on` | — | Pointer to `the-money-and-the-perimeter.md`. |
| `## What is published is what to convey; the words are yours` | `Everything this system publishes for the` | 3 | — | `## How the conversation goes` | Source material and not a script; the wording is his language. Design §4. |
| `## What is published is what to convey; the words are yours` | `**A relay is one sentence.** Name` | 3 | — | `## How the conversation goes` | Quoting at one end, composing at the other. Design §4. |
| `## What is published is what to convey; the words are yours` | `**One sentence per decision, not per` | 1 | `OpenQuestionDto::alike`; `InterpretationDto::groups`; `RowGroupDto::question` | — | «what is the same decision is published beside the question» is exactly `alike` and `groups`; the sentence for the whole set is `RowGroupDto::question`. |
| `## What is published is what to convey; the words are yours` | `**Before you put a question about` | 1 † | `OpenQuestionDto::printed`; `OpenQuestionDto::alternatives`; `AnswerAlternativeDto::consequence` | — | Pointer to `importing.md` §«A question is a thing». |
| `## What is published is what to convey; the words are yours` | `**And the line the freedom stops` | 3 | — | `## The agent is an external client` | Never answer in his place, never read silence as a value, never narrow the question. Design §7. |
| `## Where an import begins: carry the document, do not read it` | `An import begins with a document` | 3 | — | `## The agent is an external client` | The two acts and the difference between them. Design §7. |
| `## Where an import begins: carry the document, do not read it` | `**You may convey it.** Handing the` | 3 | — | `## The agent is an external client` | Design §7. |
| `## Where an import begins: carry the document, do not read it` | `**You may not interpret it.** Do` | 3 | — | `## The agent is an external client` | A format has one reader; a reading of your own is a second implementation. Design §7. |
| `## Where an import begins: carry the document, do not read it` | `**If you cannot reach the document,` | 3 | — | `## The agent is an external client` | Naming a poorer path as poorer. Design §7. |
| `## Where an import begins: carry the document, do not read it` | `**An empty instance does not know` (a) | 1 | `SourceDocumentDto::unresolved_accounts`; `UnresolvedAccountDto::printed`; `UnresolvedAccountDto::records`; `AccountResolutionDto::unrecognised` | — | The response already summarises the refusals as the distinct printed names with the number of records each accounts for, and says the string is not interpreted. |
| `## Where an import begins: carry the document, do not read it` | `**An empty instance does not know` (b) | **2** | `CreateAccountRequest::provider_account_id` | — | «Give it the printed string as the identifier its source prints for it — not as the title.» The field's description today advises a *derived* value and says nothing about which field the printed string belongs in, which is the mistake being prevented. |
| `## Where an import begins: carry the document, do not read it` | `You do not have to guess` | **2** | `CreateAccountRequest::title` | — | «You do not have to guess a name and you must not.» The description explains what a title is and is not; it does not say that inventing one makes a second account he did not ask for. |
| `## Where an import begins: carry the document, do not read it` | `**Before you send any string that` | 1 † | `OperationDto::account`; `DeclaredSourceDto::account` | — | Pointer to `the-money-and-the-perimeter.md` §«An account is named by an identifier». |
| `## Where an import begins: carry the document, do not read it` | `**But do not ask him name` | 1 | `ProposedAnswerDto`; `ProposedAnswerDto::covers` | — | The two questions one answer fills are the queue's own proposal. |
| `## Where an import begins: carry the document, do not read it` | `**And some of those names are` | 1 | `ActionTargetDto::Options`; `RecordAccountNameDispositionRequest` (struct); `RecordAccountNameDispositionRequest::reason` | — | The item publishes both ways out, and the request's description already says an item closed only by the act he decided against stood as required work against every report. |
| `## Where an import begins: carry the document, do not read it` | `**Never a credential but your own**,` | 3 | — | `## The agent is an external client` | Design §7. |
| `## Where an import begins: carry the document, do not read it` | `**Everything that happens after the document` | 1 † | `ImportSessionDto::assessment` | — | Pointer to `importing.md`. The assessment route is the thing that answers what committing would do. |
| `## The four files beside this one` | `This file is the process. Everything` | 1 † | — | — | The four files cease to exist; so does the sentence describing when to read them. |
| `## The four files beside this one` | `- **\`importing.md\` — read it before` | 1 † | `ImportSessionDto::assessment`; `ImportQuestionDto`; `InterpretationDto` | — | Table of contents for a file being emptied. |
| `## The four files beside this one` | `- **\`the-money-and-the-perimeter.md\` — read it before` | 1 † | `ContourDto`; `AccountRetirementDto`; `CategoryDto`; `ResolveInstrumentRequest` | — | Same. |
| `## The four files beside this one` | `- **\`correcting.md\` — read it before` | 1 † | `CorrectionDto`; `CorrectImportRequest`; `OperationDto::idempotency_key` | — | Same. |
| `## The four files beside this one` | `- **\`reading-the-reports.md\` — read it before` | 1 † | `ConfidenceDto`; `PopulationDto`; `CashFigureDto`; `DataQualityDto` | — | Same. |
| `## What the system does not do` | `It does not compute taxes, does` | 3 | — | `## What the system does not do` | Design §8, including the budget-planning line Task 1 added. |
| `## What the system does not do` | `What the system can do **now**` | 3 | — | `## What the system does not do` | Design §9's «where everything else is», in one sentence. |

## `docs/agent-skill/importing.md`

| Heading | Paragraph | Class | Carrier | Destination | Note |
| --- | --- | --- | --- | --- | --- |
| `# Importing: rows, questions and sessions` | `The process is in \`SKILL.md\`, and` | 1 † | `SKILL.md` `## The agent is an external client` | — | Preamble restating two rules that already stand in `SKILL.md`. |
| `## A row you cannot classify is submitted as such` | `Every operation kind states a conclusion:` | 1 | `OperationKindDto::UnresolvedDirection` (variant description) | — | Carried nearly word for word: «Every other variant here is a conclusion … `deposit` and `withdrawal` assert a direction the source did not give, and `transfer` demands an account the caller does not know.» |
| `## A row you cannot classify is submitted as such` | `There is a shape for the` | 1 | `OperationKindDto::UnresolvedDirection` members `amount`, `direction`, `counterparty`, `far_side`, `source_document`; `OperationDto::source_kind` | — | Every member of the list is a documented member of the variant, including «the amount with the sign the source printed» and «absence is a statement». |
| `## A row you cannot classify is submitted as such` | `**Use it whenever you have not` | 1 | `OperationKindDto::UnresolvedDirection` (variant description) | — | «It is not a weaker version of the others and does not replace them: a caller that **has** concluded is still right to say so, and should.» |
| `## A row you cannot classify is submitted as such` | `What comes back for a row` | 1 | `VerdictDto::verdict` (`VerdictCodeDto` vocabulary); `QuestionSettlementDto::code`; `QuestionSettlementDto::explanation` | — | The published verdict vocabulary says whether the fact reached the journal; the settlement codes name the directory and the rule as what settles a row without asking. |
| `### When the source says whose account the far side is` | `Some statements file a row as` | 1 | `OperationKindDto::UnresolvedDirection` member `far_side` | — | Carried word for word, stronger-than and weaker-than included. |
| `### When the source says whose account the far side is` | `Three things follow, and the third` | 1 | `OperationKindDto::UnresolvedDirection` member `far_side` | — | Scaffolding for the three bullets. |
| `### When the source says whose account the far side is` | `- **Set it only where the` | 1 | `OperationKindDto::UnresolvedDirection` member `far_side` | — | «Send it only where the export says so in words. It is a transcription …» |
| `### When the source says whose account the far side is` | `- **It carries no direction, and` (a) | 1 | `OperationKindDto::UnresolvedDirection` member `far_side` | — | «It carries **no direction**, deliberately … Such a row is recorded as a movement between the owner's own accounts with the far side unnamed, and no question is raised about it.» |
| `### When the source says whose account the far side is` | `- **It carries no direction, and` (b) | **2** | `OperationDto::kind` | — | The residue: **a row that also states a direction posts one leg, and still not as money leaving the perimeter.** The description belongs on the `far_side` member of the `unresolved_direction` variant, which `OperationDto::kind` publishes. |
| `### When the source says whose account the far side is` | `- **It does not decide which` | 1 | `OperationKindDto::UnresolvedDirection` member `far_side`; `CrossSourceMatchingDto::candidates` | — | «a weaker one than naming the account»; which account is settled later by the far side's own statement is the cross-source matching section. |
| `### A row that is settled by producing nothing` | `Two payment instruments over one underlying` | 1 | `AccountDto::aliases`; `SettledRowDto::reason` | — | «Two cards over one underlying account are one account with two aliases»; `one_account_two_instruments`. |
| `### A row that is settled by producing nothing` | `Where the identifier the source printed` | 1 | `CommitDeltaDto::settled_without_fact`; `SettledRowDto::reason`; `SettledRowDto::explanation`; `SettledRowDto::account`; `ImportRowDto::state` | — | Every clause is a published field: the row is `settled`, it is in the plan's list, the verdict carries the determination's code. |
| `### A row that is settled by producing nothing` | `**Do not read that as a` | 1 | `CommitDeltaDto::settled_without_fact`; `ImportRowDto` (struct description) | — | «A third list, because it is a third outcome and not a softer kind of retention … They are also the published explanation for a total that is short of the statement's own turnover with nothing wrong», and `ImportRowDto` says what `quarantined` means and does not. |
| `## A question is a thing, not a sentence` | `The question that comes back is` | 1 | `VerdictDto::question_id`; `VerdictDto::session_id` | — | «what makes the question reachable after this response is gone: the question is a stored row, not a line in a response body». |
| `## A question is a thing, not a sentence` | `Every question publishes the answers it` (a) | 1 | `AnswerAlternativeDto` (struct description) | — | «an answer the question does not admit is a different mistake from an answer that is wrong, and only the first can be refused before anything is written». |
| `## A question is a thing, not a sentence` | `Every question publishes the answers it` (b) | 3 | — | `## The agent is an external client` | «Never answer one yourself» — which way the money went is a fact about his affairs. Design §7. |
| `## A question is a thing, not a sentence` | `**Name the row by the day` | **2** | `ImportQuestionDto::row` | — | The worked example. The description says only «The row in the session the question is about.» The obligation — name the row by the day the source dated it and the amount it printed, with the sign; the number is what you send, the day and the sum are what he reads — has a field and no description on it. |
| `## A question is a thing, not a sentence` | `**Every alternative says what answering it` | 1 | `AnswerAlternativeDto::consequence` | — | The worked example. Documented as «What answering this word does to the owner's money-flow report», always present, one sentence per word. The paragraph is a copy. |
| `## A question is a thing, not a sentence` | `The words and their effects travel` | 1 | `OpenQuestionDto::alternatives` | — | `iaam-ulib`, carried word for word, including that hunting for them elsewhere is reading something stale. |
| `## A question is a thing, not a sentence` | `Where an answer names one of` | 1 | `ImportQuestionDto::accounts`; `InterpretationDto::answer_accounts` | — | Both descriptions say the list travels with the question, that `id` is what the answer takes and `title`/`institution` are what he reads, and that the assessment publishes it once. |
| `## A question is a thing, not a sentence` | `**An answer you relay settles the` | 1 | `QuestionGeneralisationDto` (struct description); `QuestionGeneralisationDto::state` | — | The four words, `available` meaning a rule was possible and the answerer may not generalise, and nothing being refused. |
| `## A question is a thing, not a sentence` | `Read that word. A rule may` | 1 | `QuestionGeneralisationDto::state`; `QuestionGeneralisationDto::proposal` | — | `impossible` versus `available`, and that only the second comes with the rule attached. |
| `## A question is a thing, not a sentence` | `You do not have to remember` | 1 | `ActionDto::goals`; `ActionDto::kind`; `ResolutionOptionDto::required_scope`; `RequestPlanDto::preset` | — | The item exists, says it is a recommendation, names who may send it and carries the rule already filled in. |
| `## A question is a thing, not a sentence` | `The condition a proposal asks about` | **2** | `QuestionGeneralisationDto::proposal` | — | That the proposed condition is **one thing** — the counterparty, failing that the source's word, failing both the description — and that telling him what the condition asks is the part he may want to change, is on no description. `RuleMatcherDto` documents each member and that present members are joined with **and**; it does not say which one a proposal picks or why he should hear it. |
| `### One decision, many lines` | `A statement names one shop on` | 1 | `OpenQuestionDto::alike` | — | «Empty means this decision is asked once» is the field's own last line. |
| `### One decision, many lines` | `Two rows are the same decision` | 1 | `OpenQuestionDto::alike` | — | «The same decision» is the question paired with the direction the source stated — carried word for word, arrival-as-departure example included. |
| `### One decision, many lines` | `An answer can be told to` | 1 | `AnswerImportQuestionRequest::settles`; `ImportQuestionDto::also_settled` | — | «Present and non-empty, it is what the caller must tell the owner: he decided one row and these were decided with it.» |
| `### One decision, many lines` | `**Read what that does and does` | 1 | `AnswerImportQuestionRequest::settles` | — | «The wider word is refused whole rather than in part … the commonest case is an answer naming one of the owner's accounts which another of those rows is itself on.» |
| `### What a first import can settle without asking him about every line` | `A statement he has never imported` | 1 | `OfferedRuleDto` (struct description) | — | «A first import has no rules of the owner's, so every row naming a party becomes a question and the answer is the same for most of them.» |
| `### What a first import can settle without asking him about every line` | `The assessment publishes them in \`offered_rules\`.` | 1 | `InterpretationDto::offered_rules`; `OfferedRuleDto::question`; `OfferedRuleDto::covers`; `OwnerQuestionDto::consequence` | — | The count is the length of `covers`, the rows are `covers`, and the two-part question with the risk in the second part is `OwnerQuestionDto`. |
| `### What a first import can settle without asking him about every line` | `**Adopting it removes the questions it` | 1 | `QuestionSettlementDto::code`; `ImportSessionContentsDto::unanswered`; `InterpretationDto::resolved` | — | `unanswered`'s description tells this exact story under `iaam-m2oi`, rule-settled rows and all. |
| `### What a first import can settle without asking him about every line` | `If a row he expected to` | 1 | `OfferedRuleDto::covers`; `OfferedRuleDto::contains`; `RowShapeDto::direction` | — | `covers` is «exactly the open rows `matcher` matches»; a shape with no direction is the published reason those lines stay put. |
| `### What a first import can settle without asking him about every line` | `What it publishes is the **condition**` | 1 | `OfferedRuleDto` (struct description); `OperationDto::source_category` | — | «It offers a condition and never an outcome» and «Evidence, never a verdict». |
| `### What a first import can settle without asking him about every line` | `A document whose reader transcribes no` | 1 | `InterpretationDto::offered_rules` | — | «Empty where the document printed no category of its own, which is the truthful answer and not a failure.» |
| `### What a first import can settle without asking him about every line` | `**A word whose rows are not` | 1 | `InterpretationDto::withheld_offers`; `WithheldOfferDto::covers`; `WithheldOfferDto::contains`; `WithheldOfferDto::reason` | — | Including that it offers nothing that could be sent as it stands: `WithheldOfferDto` carries no `matcher`. |
| `### What a first import can settle without asking him about every line` | `That is what makes the other` | 1 | `InterpretationDto::offered_rules`; `WithheldOfferDto::contains` | — | «Every entry here is safe to put to the owner … a client that walks this list and relays what it finds cannot relay an offer that would file most of what it matches wrongly.» |
| `### What a first import can settle without asking him about every line` | `**Each open question carries its row` | 1 | `OpenQuestionDto::printed`; `PrintedRowDto` | — | `iaam-pm4w`, carried word for word, including the prohibition on recovering values from the prose. |
| `### What a first import can settle without asking him about every line` | `**And the accounts an answer may` | 1 | `InterpretationDto::answer_accounts`; `PrintedRowDto::account` | — | Carried word for word, «an account is not the other side of itself» included. |
| `## An import can be held open before it is committed` | `Rows can also be accumulated in` | **2** | `ImportSessionDto::state` | — | «`open`, `committed` or `abandoned`» is the whole description. What a session is — opened, fed, questioned, answered, then committed or abandoned; not a database transaction; nothing held open in the machine; the session itself is what is durable — is on nothing. Design §4: «A held session, how it ends». |
| `## An import can be held open before it is committed` | `Everything else follows from two properties:` | **2** | `ImportSessionDto::state` | — | Scaffolding for the two bullets; goes with them. |
| `## An import can be held open before it is committed` | `- **Nothing in a session is` | 1 | `ImportRowDto` (struct description) | — | «A held row will be written, at commit and at no other moment», and that the answer for every held row is «nothing, yet». |
| `## An import can be held open before it is committed` | `- **Abandoning a session leaves the` | **2** | `ImportSessionDto::state` | — | `HeldSessionDto::contribution` says an abandoned session's rows will never become facts, which is the report's half. That abandoning leaves the journal exactly as it was, and how that differs from a retraction afterwards, is on nothing. |
| `## An import can be held open before it is committed` | `A session refuses to commit while` | 1 | `ImportSessionContentsDto::unanswered`; `ImportSessionSummaryDto::unanswered` | — | «Commit refuses while this is not zero», on both. |
| `## An import can be held open before it is committed` | `**«Still waiting on him» is not` | 1 | `ImportQuestionDto::settled_without_answer` | — | Carried word for word, `iaam-m2oi`, including the three ways a question stops waiting and the for-ever-empty `answered_at`. |
| `## An import can be held open before it is committed` | `So do not decide what is` | 1 | `ImportQuestionDto::settled_without_answer`; `QuestionSettlementDto::explanation` | — | «A client showing the owner what is left to do shows the questions with no `answered_at` **and** no `settled_without_answer`.» |
| `## An import can be held open before it is committed` | `**A session you opened and did` | 1 | `ImportSessionSummaryDto` (struct description); `ActionDto::goals`; `ActionTargetDto::Options` | — | The struct's own description is the defect this paragraph describes: «a caller that reads a list and sees nothing outstanding concludes nothing is, and for an import that had never been committed that conclusion is a second import of the same statement». |
| `## An import can be held open before it is committed` | `Read that item rather than concluding` | 1 | `ImportSessionSummaryDto::row_count`; `ImportSessionSummaryDto::unanswered` | — | «The two counts are read in the same store statement as the headers, so the complete answer costs what the incomplete one did.» |
| `## An import can be held open before it is committed` | `**That figure is the never-answered count` | 1 | `ImportSessionSummaryDto::unanswered`; `ImportSessionContentsDto::unanswered` | — | **Stale, and it contradicts the contract.** Since `iaam-m2oi` the figure *is* the waiting count — «Not the number of questions with no answer recorded» — and the queue item's own sentence is built from it. The paragraph tells an agent not to read out a number that is now exactly «how much is left». Delete it; do not carry any of it forward. |
| `## An import can be held open before it is committed` | `A report can still be asked` | 1 | `HeldRowsDto::requested`; `HeldRowsDto::sessions`; `HeldRowsDto::retained_unrecorded`; `HeldSessionDto::revision` | — | Every clause, `none`/`all`/named included. |
| `## An import can be held open before it is committed` | `Read that last count before quoting` | 1 | `HeldRowsDto::retained_unrecorded`; `HeldSessionDto::retained_unrecorded` | — | «An answer that did not publish this count would manufacture confident wrong arithmetic» and «**The figures are short by these.**» |
| `## An import can be held open before it is committed` | `Naming a session that has already` | 1 | `HeldSessionDto::contribution` | — | Carried word for word, both cases. |
| `## How an amount is stated` | `**Amounts are always positive.** The sign` | **2** | `OperationDto::kind` | — | `kind` has no description at all. That the sign is carried by the kind — a contribution and a withdrawal are different kinds, not one sum with two signs — belongs on the request schema it publishes. The «strings, not JSON numbers» half is carried by `AmountDto::amount` and the cash figures; the positive-amount rule is not. Design §4: «how an amount is stated → the request schemas that accept them». |
| `## How an amount is stated` | `**An amount's scale must not exceed` | **2** | `OperationDto::kind` | — | A surplus digit is refused, not rounded. Same carrier, same reason. |
| `## A transfer between the owner's own accounts is one row, not two` | `A transfer operation names the account` | **2** | `OperationDto::kind` | — | The sentence belongs on the `transfer` variant: submitted once, from the sending side, and the system writes both movements from that one row. |
| `## A transfer between the owner's own accounts is one row, not two` | `There is deliberately no way to` | **2** | `OperationDto::kind` | — | Two printed sides become two transfers; import the sending side and drop the receiving row. Same variant. |
| `## A transfer between the owner's own accounts is one row, not two` | `Three properties follow, each got wrong` | **2** | `OperationDto::kind` | — | Scaffolding; goes with the three bullets. |
| `## A transfer between the owner's own accounts is one row, not two` | `- **The amount is positive, like` | **2** | `OperationDto::kind` | — | A negative amount is refused, not read as the outgoing leg. |
| `## A transfer between the owner's own accounts is one row, not two` | `- **The two accounts must differ.**` | **2** | `OperationDto::kind` | — | Refused on the destination field. |
| `## A transfer between the owner's own accounts is one row, not two` | `- **A transfer is not a` | **2** | `OperationDto::kind` | — | `AnswerAlternativeDto::consequence` carries this for the *answer* path only; nothing carries it for the submit path. |
| `## A transfer between the owner's own accounts is one row, not two` | `If you cannot tell whether the` | 1 | `OperationKindDto::UnresolvedDirection` (variant description); its `counterparty` member | — | «this shape exists so the caller does not have to reach one». |
| `## Idempotency keys` | `Always send an idempotency key if` | 1 | `OperationDto::idempotency_key` | — | Carried word for word, «Omitting it is not a lesser version of sending it» included. |
| `## Idempotency keys` | `**A key names a fact, not` | 1 | `OperationDto::idempotency_key` | — | Carried word for word, including that the response is a success and easy to read as «the correction landed». |
| `## Idempotency keys` | `A fact that turned out wrong` | 1 | `OperationDto::idempotency_key` | — | «**A fact that turned out wrong is corrected, never resent** … Re-use is not reversal.» |
| `## Idempotency keys` | `Keys are scoped to the **owner**,` | 1 | `OperationDto::idempotency_key` | — | Carried word for word, «the document and the row within it, rather than the row alone» included. |
| `## What to assert for a reconstructed opening` | `A position-opening operation has an optional` | 1 | `OpeningAssertionsDto` (struct and members) | — | «The fields and their permitted values are in the contract» is a sentence saying to read the contract. |
| `## What to assert for a reconstructed opening` | `An absent block means the owner` | 1 | `OpeningAssertionsDto` (struct description) | — | «default values preserve this lack of knowledge rather than infer confidence from whether other fields are populated». |
| `## What to assert for a reconstructed opening` | `What is asserted here reaches the` | **2** | `OpeningAssertionsDto::acquisition_date` | — | The field has no description. That without it there is no ownership boundary, that such postings land in the material issues as unverifiable instead of being checked, and that «unknown» beats the start of the journal, is on nothing. |

## `docs/agent-skill/the-money-and-the-perimeter.md`

| Heading | Paragraph | Class | Carrier | Destination | Note |
| --- | --- | --- | --- | --- | --- |
| `# The money's shape and the perimeter` | `The process is in \`SKILL.md\`, and` | 1 † | `SKILL.md` `## The agent is an external client` | — | Preamble restating the perimeter boundary that already stands in `SKILL.md`. |
| `## What a contour is` | `A contour is the set of` | **2** | `ContourDto::accounts` | — | «The accounts this version covers» is the whole description. What a contour *is* — the set he considers his portfolio, drawn by him and not by an institution; a move between two of them changes no return; a move in from outside is a contribution — is on nothing. |
| `## What a contour is` | `A contour has a **version**. A` | 1 | `PopulationDto::contour_version`; `PopulationDto::retirement_revision` | — | «two asset snapshots over one contour version are answers to the same question when their retirement revisions match» — the comparability rule, and stronger than the skill's. |
| `## A closed product is retired, never dropped from the contour` | `The owner closes a term deposit` | 1 | `AccountRetirementStateDto` (description) | — | Scene-setting for the case the enum's own description states. |
| `## A closed product is retired, never dropped from the contour` | `**The obvious move is the wrong` | 1 | `AccountRetirementStateDto` (description) | — | «a client that reached for the scope route to record a closure would destroy the answer it was trying to tidy». |
| `## A closed product is retired, never dropped from the contour` | `**What to do instead** is record` | 1 | `AccountRetirementStateDto`; `PopulationAccountDto::retirement` | — | The account stays inside the contour; the snapshot drops the row. |
| `## A closed product is retired, never dropped from the contour` | `A contour says **whose money is` | 1 | `AccountRetirementStateDto` (description) | — | «A scope disposition says whether an account's money belongs in a report; this says whether the product is still there.» |
| `### What the retirement changes, and what it does not` | `From the date the product ceased:` | 1 | `PopulationAccountDto::retirement` | — | Scaffolding for the three bullets. |
| `### What the retirement changes, and what it does not` | `- the asset snapshot stops publishing` | 1 | `PopulationAccountDto::retirement` | — | «from this date on, the asset snapshot drops a retired account's row where all of its figures are zero». |
| `### What the retirement changes, and what it does not` | `- every report's \`population\` goes on` | 1 | `PopulationAccountDto::retirement`; `PopulationAccountDto::standing` | — | «Read the two fields separately, and never report a retirement as an exclusion.» |
| `### What the retirement changes, and what it does not` | `- \`population.retirement_revision\` advances. That field is` | 1 | `PopulationDto::retirement_revision`; `AccountRetirementDto::revision` | — | Carried word for word on both. |
| `### What the retirement changes, and what it does not` | `It changes nothing else. No figure` | **2** | `RecordAccountRetirementRequest::state` | — | What a retirement never changes — no classification ever, a snapshot already taken untouched, the balances answer keeps the row, nothing hidden from the account list or the queue, and how to read «which of his products still exist» — is on nothing. Design §4: «The retirement operation's description». |
| `### A retirement never hides money` | `Where a retired account's figures are` | 1 | `PopulationAccountDto::retirement`; `ConfidenceDto::caveats`; `CaveatDto::detail` | — | «Where a figure is not zero the row stands, and `confidence` carries `retired_account_not_empty` for it — a retirement never hides money.» |
| `### A retirement never hides money` | `The usual cause is that the` | 1 | `CashFigureDto::MovementSinceUnknownStart`; `CaveatDto::detail` | — | The register carries the caveat and its meaning; the cash figure names itself as movement from an unknown start. |
| `### A retirement never hides money` | `**The caveat names its own remedies,` | 1 | `CaveatDto::closed_by`; `CaveatDto::see` | — | Carried word for word, including that one call does not always empty a caveat and that `see` remains the check. |
| `### A retirement never hides money` | `**If instead the queue carries \`retirement_not_assessed\`,` | 1 | `ActionDto::reason`; `ActionTargetDto` | — | The item is the thing that says it could not be computed and names the correction. |
| `### A retirement never hides money` | `One thing the structure cannot warn` | **2** | `CaveatDto::closed_by` | — | The paragraph says in as many words that the structure cannot warn about this. It can: the description can say that a caveat's remedies are that caveat's alone and must not be applied to another caveat on the same account — an owner-balance assertion is checked against the fold rather than added to it. |
| `### A retirement never hides money` | `**Never propose ruling the account outside` | 1 | `AccountRetirementStateDto`; `AccountScopeDispositionDto` | — | Both descriptions draw the same line between the two axes. |
| `### What is refused` | `- a second retirement over one` | 1 | `RecordAccountRetirementRequest` (struct description) | — | «the remedy is two calls — withdraw, then record. Replacing the date in place would silently move the boundary under every snapshot already taken». |
| `### What is refused` | `- withdrawing when nothing stands. Every` | 1 | `AccountRetirementDto::revision` | — | «It advances on **every** accepted call, including a withdrawal, and never on a call that changed nothing.» |
| `### What is refused` | `- a date later than today;` | 1 | `RecordAccountRetirementRequest` (struct description) | — | «A date after today is refused: a product that has not ceased yet has not ceased.» |
| `### What is refused` | `- an account the owner does` | 1 † | the route's own refusal | — | An account he does not hold is not addressable; the refusal says so. Nothing to write. |
| `### What is refused` | `Retiring an account that still holds` | 1 | `PopulationAccountDto::retirement`; `CaveatDto` (`retired_account_not_empty`) | — | «the report says where it disagrees rather than refusing his word» is what the caveat is. |
| `## What a category is, and what it cannot change` | `A contour says which accounts are` | **2** | `CategoryDto::title` | — | «Owner category in the living reference list» is all there is. That a category says what his money was *for*, over the same journal a contour says whose money is *in*, is on nothing. |
| `## What a category is, and what it cannot change` | `A category is the owner's explanation,` | 1 | `OperationDto::source_category`; `CategoryRuleDto::valid_from`; `CategoryRuleDto::valid_to`; `NotDecomposedDto` | — | «Evidence, never a verdict»; the undecomposed block is the report saying so rather than a catch-all. |
| `## What a category is, and what it cannot change` | `Categories reach spending, refunds and kinds` | 1 | `OperationKindDto::Refund` (variant description); `CategoryGroupRequest::is_income` | — | «It reverses spending rather than adding to income: a returned purchase must not appear as money arriving» and «cashback and interest on a balance are the owner's categories exactly as groceries are». |
| `## What a category is, and what it cannot change` | `**Changing only a category assignment cannot` | **2** | `CategoryRuleImpactDto::months` | — | The impact answer says which rows and months a rule moves. That it can move **no** figure of the return — not what was contributed or withdrawn, not the contour's value, not the pre-tax return — is on nothing. |
| `## What a category is, and what it cannot change` | `Say it in those words, because` | **2** | `CategoryRuleImpactDto::months` | — | The other half, and the one that makes the first safe: changing an event's kind, its accounts or the contour's membership *does* move all of them, and that change goes back through the channel the fact arrived by. |
| `## An account is named by an identifier, and every channel reads the same ones` | `Wherever a row you submit names` | 1 | `OperationDto::account`; `DeclaredSourceDto::account` | — | «The same field, the same tiering and the same refusals as `source.account` on the declaration, because it is the same question.» |
| `## An account is named by an identifier, and every channel reads the same ones` | `1. **iaam's own identifier for the` | 1 | `DeclaredSourceDto::account` | — | «an identifier that parses as an account of the owner's *is* that account, before anything else is consulted». |
| `## An account is named by an identifier, and every channel reads the same ones` | `2. **The identifier the account's source` | 1 | `DeclaredSourceDto::account`; `AccountAliasDto::valid_from` | — | «matched against the identity a source prints, then against aliases»; the alias interval is read as of the row's date. |
| `## An account is named by an identifier, and every channel reads the same ones` | `3. **The owner's title** for the` | 1 | `DeclaredSourceDto::account` | — | «then against the title». |
| `## An account is named by an identifier, and every channel reads the same ones` | `Send the first where you have` | **2** | `OperationDto::account` | — | The tiering is published; the **instruction** is not — send the identifier, send the source's where the file prints it, and **do not send the title**, which resolves only so older documents keep parsing. |
| `## An account is named by an identifier, and every channel reads the same ones` | `The order is a rule about` | 1 | `DeclaredSourceDto::account` | — | «stopping at the first tier that matches anything». |
| `## An account is named by an identifier, and every channel reads the same ones` | `A string naming none of his` | 1 | `OperationDto::account`; the refusal itself | — | «two accounts in that tier are refused rather than picked between, and the refusal names both»; «one rejected row and not a rejected request». |
| `## An instrument's external code resolves as of a date` | `An instrument is named by an` (a) | 1 | `AliasNamespaceDto` (published vocabulary); `ResolveInstrumentRequest::namespace`; `ResolveInstrumentRequest::value` | — | The five registers are enumerated in the contract with a sentence each, and «neither half means anything alone» is the field's own words. |
| `## An instrument's external code resolves as of a date` | `An instrument is named by an` (b) | — | — | — | **No home yet.** «The place of custody is named by the owner's own title for it, and nothing else names one.» See the section at the end. |
| `## An instrument's external code resolves as of a date` | `A code resolves **as of the` | 1 | `ResolveInstrumentRequest::on` | — | «Document date. Required: ISIN changes, and there is no «current» answer.» |
| `## An instrument's external code resolves as of a date` | `Two different refusals follow from this,` | **2** | `ResolveInstrumentRequest::on` | — | Scaffolding plus the table below it; goes with it. |
| `## An instrument's external code resolves as of a date` | `\| Refusal \| What happened \|` (table) | **2** | `ResolveInstrumentRequest::on` | — | The two refusals and what each means: an unknown code is the owner's work and the agent may not write to the catalogue; a code known but not on this date means the **document's date** is the more likely error. |
| `## An instrument's external code resolves as of a date` | `The second case is almost always` | **2** | `ResolveInstrumentRequest::on` | — | Same carrier: corrupted data rather than a gap in the catalogue. |
| `## An instrument's external code resolves as of a date` | `The instrument catalogue is **shared across` | 1 | `InstrumentDto` (struct description); `CreateInstrumentRequest` (struct description); `ResolutionOptionDto::required_scope` | — | «the catalogue is global and readable by everyone»; the create route is for an administrator or synchronisation; the scope floor says an owner-only call is not the agent's. |
| `## An instrument's external code resolves as of a date` | `An instrument has three currencies, and` | **2** | `InstrumentDto::denomination_currency` | — | The three currency fields have no descriptions. That they differ, that they diverge on replacement bonds, and that the report currency is not among them — it is a property of the report — is on nothing. |
| `## An instrument's external code resolves as of a date` | `An instrument's kind may be unset.` | 1 | `InstrumentDto::kind` | — | «`null` — no kind is set; such an instrument is assessed as incomplete.» |

## `docs/agent-skill/correcting.md`

| Heading | Paragraph | Class | Carrier | Destination | Note |
| --- | --- | --- | --- | --- | --- |
| `# Correcting what is already recorded` | `The process is in \`SKILL.md\`, and` | 1 † | `SKILL.md` `## The agent is an external client`; `NavCoverageDto::accepted_internal` | — | Preamble and a cross-reference to `reading-the-reports.md`. |
| `## A mistake is retracted, not erased` | `Append-only does not mean irrevocable, and` | 1 | `CorrectionDto::Reversal`; `CorrectionDto::Replacement`; `SubmitCorrectionsRequest::acknowledge_retraction` | — | «Retract the target. It stays in the journal and stops being effective»; «Supersede the target with the operation given here»; «Acknowledge that a retracted fact stops counting in every report». |
| `## A mistake is retracted, not erased` | `Four things follow, and each of` | 1 | — | — | Scaffolding for four paragraphs, all classified below. |
| `## A mistake is retracted, not erased` | `**Correcting the owner's history is his` | 1 | `ResolutionOptionDto::required_scope`; `ClosingOperationDto::required_scope`; `CaveatDto::closed_by` | — | «a register that names an owner-only remedy to an agent has told it to make a call the server will refuse»; preparing the request for him to send is what `closed_by` and `preset` are. |
| `## A mistake is retracted, not erased` | `**Declare a source on everything you` | 1 | `DeclaredSourceDto` (struct); `DeclaredSourceDto::label`; `DeclaredSourceDto::channel`; `SubmitOperationsRequest::source` | — | «Two submissions carrying the same label are one import … Every import worth retracting on its own should be labelled», and the struct says what a submission with no declaration costs. |
| `## A mistake is retracted, not erased` | `**Undoing your own import is yours.**` | 1 | `CorrectImportRequest::source`; `CorrectImportRequest::acknowledge_retraction`; `ImportCorrectionDto::written` | — | «each is a new event referencing the one it retracts» is the retraction being a journal fact he can see. |
| `## A mistake is retracted, not erased` | `The bound is narrow and is` | **2** | `CorrectImportRequest::source` | — | The description says what the label reaches. The **bound** — every row still in force, nothing built on it, no row reversed or replaced by anyone, no balance reconciled against the interval, and that a refusal says which condition failed — is on nothing. |
| `## A mistake is retracted, not erased` | `**Retracting does not free a repeat` | 1 | `OperationDto::idempotency_key`; `SubmitCorrectionsRequest::acknowledge_retraction` | — | «Re-use is not reversal: nothing on the ingest path writes a retraction»; «re-submitting the same rows does not bring it back». |
| `## A mistake is retracted, not erased` | `**Diagnose before proposing.** The journal can` | **2** | `SubmitCorrectionsRequest::corrections` | — | «Applied together or not at all» is the whole description. That the events must be named from the journal read back per row, and that a correction proposed from a report's aggregate is a guess about which rows are wrong, is on nothing. |
| `## A mistake is retracted, not erased` | `**An assertion by the owner is` | **2** | `NavCoverageDto::accepted_internal` | — | The four shares have no descriptions. That a balance he names is what is reconciled *against* and not a second proof — agreement gives `accepted_internal`, which must not be called independently confirmed — is on nothing. |

## `docs/agent-skill/reading-the-reports.md`

| Heading | Paragraph | Class | Carrier | Destination | Note |
| --- | --- | --- | --- | --- | --- |
| `# Reading the reports` | `The process is in \`SKILL.md\`, and` | 1 † | `SKILL.md` `## The overriding rule` | — | Preamble restating the overriding rule. |
| `## Two channels of fact, and what makes a confirmation independent` | `Facts arrive through two independent channels:` | **2** | `DimensionStatusDto::status` | — | Neither field of `DimensionStatusDto` has a description. That only agreement **between** the two channels raises a dimension to `accepted_independent`, and that two reports read the same way are not a second source, is on nothing. |
| `## A cash figure is not a balance until something anchors it` | `The journal begins when it begins.` | 1 | `CashFigureDto::MovementSinceUnknownStart`; `CashFigureDto` (struct description) | — | «Nothing asserts the state this accumulated from, so `movement` is what the recorded interval moved and says nothing about what was there before it.» |
| `## A cash figure is not a balance until something anchors it` | `**The figure names itself, so there` | 1 | `CashFigureDto` (struct description) | — | «So there is no field called `amount` any more … the variant has to be settled before either can be reached.» |
| `## A cash figure is not a balance until something anchors it` | `This is the trap the shape` | 1 | `CashFigureDto` (struct description) | — | «an agent ran a first import, took `accounts[].cash[].amount` for holdings, and reported an impossible negative asset. A caveat beside a number loses to the number.» |
| `## A cash figure is not a balance until something anchors it` | `**A total does not average the` | 1 | `CashClassTotalDto::totals`; `AssetSnapshotDto::total`; `CashFigureDto::Mixed` | — | «Both parts are stated and **their sum is not**»; «A currency whose cash is not entirely balances is absent from this list, halves and all.» |
| `## A cash figure is not a balance until something anchors it` | `**Reconciliation says the same thing in` | 1 | `ObservationBasisDto::start`; `ClaimOutcomeDetailDto::code` | — | «`unasserted` — nothing states it … this is the reason a balance claim answers `not_comparable`.» |
| `## A cash figure is not a balance until something anchors it` | `**A balance can be checked without` (a) | 1 | `ObservationBasisDto::compared`; `ObservationBasisDto::compared_since` | — | «**Read it before reporting a `matched`.** A matched change says the movements recorded since that earlier statement account exactly for the distance to this one; it says nothing about the level.» |
| `## A cash figure is not a balance until something anchors it` | `**A balance can be checked without` (b) | **2** | `ObservationBasisDto::compared` | — | The residue: a `discrepant` change is a discrepancy and **not a correction** — a later statement does not overwrite an earlier one, and correcting a recorded assertion is an explicit act with its own operation. |
| `## A cash figure is not a balance until something anchors it` | `**Never reconstruct what the system compared.**` | 1 | `ClaimOutcomeDto::basis`; `ObservationBasisDto` (struct description); `ObservationBasisDto::folded_from` | — | «a balance folded over one imported month is not the evidence a balance folded over four years is» and «what sent the owner to add up his own account by hand». |
| `## A cash figure is not a balance until something anchors it` | `A negative cash figure is reported` | 1 | `NegativeCashDto::contradicts_expectation`; `NegativeCashDto` (struct description) | — | «A warning about a probable error, not a verdict and not a refusal … the reported case behind this was a missing opening assertion rather than an overdraft.» |
| `## A cash figure is not a balance until something anchors it` | `**Read the classification, not the sign.**` | 1 | `NegativeCashDto::classification` (`NegativeCashClassificationDto` vocabulary); `NegativeCashDto::from`; `NegativeCashDto::resolved` | — | The three classifications are a published vocabulary with a sentence each. |
| `## A cash figure is not a balance until something anchors it` | `The last two carry a consequence` | 1 | `AccountBalanceDto::period_reports`; `NegativeCashClassificationDto` (vocabulary) | — | «the other two refuse them for that account and for no other. None of the three is a reason to hide the figure»; «The refusal is **this account's alone** … It is also not a refusal of this row.» |
| `## A report answers about a population, and names it` | `Every report — balances, flow, returns` | 1 | `PopulationDto` (struct description); `MoneyFlowReportDto::population` | — | |
| `## A report answers about a population, and names it` | `Read it before reading any figure.` | 1 | `PopulationDto` (struct description) | — | «each of them can be clean while the accounts selected for it were the wrong ones … This block is the second statement.» |
| `## A report answers about a population, and names it` | `\`population.known_account_coverage\` is the summary:` | 1 | `PopulationDto::known_account_coverage` | — | Scaffolding for the three bullets, all on the same field. |
| `## A report answers about a population, and names it` | `- \`whole\` — every account the` | 1 | `PopulationDto::known_account_coverage` | — | |
| `## A report answers about a population, and names it` | `- \`bounded\` — accounts are outside` | 1 | `PopulationDto::known_account_coverage` | — | |
| `## A report answers about a population, and names it` | `- \`undecided\` — accounts are outside` | 1 | `PopulationDto::known_account_coverage` | — | |
| `## A report answers about a population, and names it` | `**\`undecided\` is not a milder \`bounded\`.**` | 1 | `PopulationDto::known_account_coverage`; `PopulationAccountDto::title`; `PopulationAccountDto::institution` | — | «`undecided` outranks `bounded`»; the title and institution are there «so that an owner asked to rule on an omission is not asked about a bare identifier». |
| `## A report answers about a population, and names it` | `- \`outside_by_decision\` — he ruled the` | 1 | `PopulationAccountDto::standing` | — | |
| `## A report answers about a population, and names it` | `- \`outside_placed_elsewhere\` — the account sits` | 1 | `PopulationAccountDto::standing` | — | «he said where it belongs, not that it does not belong here». |
| `## A report answers about a population, and names it` | `- \`outside_undecided\` — no scope claims` | 1 | `PopulationAccountDto::standing` | — | «**nobody has ruled on whether it belongs**, which is a different statement from a deliberate omission and must not be read as one». |
| `## A report answers about a population, and names it` | `Each entry also carries \`retirement\` where` | 1 | `PopulationAccountDto::retirement` | — | «**A second axis, not a fifth standing.**» Carried word for word. |
| `## A report answers about a population, and names it` | `A deliberate exclusion never makes the` | 1 | `PopulationDto::known_account_coverage` | — | «an account he ruled outside deliberately is `bounded`, never `whole`: this field says what the figures cover, not how tidy his decisions are». |
| `## A report answers about a population, and names it` | `So a report whose \`population.known_account_coverage\` is` | 1 | `PopulationDto::known_account_coverage` | — | |
| `` ### `whole` is not "everything he has" `` | `**Read the field's name, and report` | 1 | `PopulationDto` (struct description) | — | «An account of the owner's that was never created here appears in neither list, and it is not reported as missing: it is invisible to the fold rather than omitted by it.» |
| `` ### `whole` is not "everything he has" `` | `No field can fix this: the` | 1 | `PopulationDto::known_account_coverage`; `ImportCommitDto::coverage_gaps` | — | «Nothing in this API sees a source document»; the coverage gap is «the rows this server was given and refused, which is a fact it owns». |
| `` ### `whole` is not "everything he has" `` | `So the check belongs to whoever` | 1 | `PopulationDto::known_account_coverage` | — | «The check is not in this field: it is comparing `covered` and `outside` against the accounts the source actually holds, which only the holder of the source can do.» |
| `` ### `whole` is not "everything he has" `` | `- Before reporting coverage, read \`covered\`` | 1 | `PopulationDto::known_account_coverage`; `PopulationDto::covered`; `PopulationDto::outside` | — | |
| `` ### `whole` is not "everything he has" `` | `- An account in the source` | 1 | `UnresolvedAccountDto::printed`; `RecordAccountNameDispositionRequest` | — | «the system holds no account for this one» is what the unresolved-account list already is, and creating one is the item's own resolution. |
| `` ### `whole` is not "everything he has" `` | `- Never report \`known_account_coverage: whole\` as` | 1 | `PopulationDto::known_account_coverage` | — | «`whole` says "every account we know of", never "everything he has".» |
| `## How to read the return report` | `The report returns what was contributed` | 1 | `ReturnsReportDto` (its fields); `ReturnsAnswerDto` | — | A list of the response's own fields. |
| `## How to read the return report` | `**The report's period is the whole` | **2** | `ReturnsReportDto::history_starts` | — | Neither `as_of` nor `history_starts` has a description. That the period is the whole history, and that a return over an arbitrary interval is not computed because it would need the value at the start of it, is on nothing. |
| `## How to read the return report` | `**Call \`xirr_pre_tax\` the pre-tax return.** Not` | 1 | `ReturnsReportDto::xirr_pre_tax` | — | «The field name deliberately includes the qualification … until then this value cannot be called «return» without qualification.» |
| `## What an unconfirmed posting does and does not mean` | `The report distinguishes two things that` | **2** | `DataQualityDto::material_issues` | — | Scaffolding for the two paragraphs below; same carrier. |
| `## What an unconfirmed posting does and does not mean` | `**A payment was not confirmed** —` | **2** | `DataQualityDto::material_issues` | — | The field is an undescribed `Vec<String>`. That an unconfirmed payment is a **defect** — he held the security, the waiting period expired, no crediting fact is in the journal — and what to tell him about it, is on nothing. |
| `## What an unconfirmed posting does and does not mean` | `**There is nothing to reconcile with**` | **2** | `DataQualityDto::material_issues` | — | The other half: no conclusion is possible because the evidence is missing, it is **not** a claim that money went missing, and where the journal simply begins later it is not a defect at all. |
| `## What an unconfirmed posting does and does not mean` | `Several equally unprovable payments for one` | **2** | `DataQualityDto::material_issues` | — | The grouped issue with a count and date bounds exists; that it must not be expanded back into a list of dates for the owner is on nothing. |
| `## What an unconfirmed posting does and does not mean` | `**Never call \`provisional\` an error.** It` | 1 | `DataQualityDto::status` (`DataQualityStatusDto` vocabulary) | — | The published vocabulary already says the status «is replaced neither by a coverage figure of one's own nor by calling the provisional share an error». |
| `## What an unconfirmed posting does and does not mean` | `**And never wait for a reconciliation` | 1 | `VerdictDto::verdict` (`VerdictCodeDto` vocabulary); `VerdictDto::account_id` | — | The vocabulary carries a sentence per code, and `account_id`'s description already says «Those are the two reserved codes, so in practice this field never arrives — see `VerdictCodeDto` for what reports reconciliation instead». |
| `## What an unconfirmed posting does and does not mean` | `Read each of the three where` | 1 | `VerdictCodeDto` (vocabulary) | — | Scaffolding for the three bullets. |
| `## What an unconfirmed posting does and does not mean` | `- **\`accepted\`** — confirmation is in` | 1 | `DataQualityDto::nav_coverage`; `VerdictCodeDto` (vocabulary) | — | |
| `## What an unconfirmed posting does and does not mean` | `- **\`discrepancy\`** — a batch that` | 1 | `ControlReconciliationDto::mismatched_figures`; `ControlCheckDto::delta`; `ActionDto::kind` | — | «named figure by figure, with both numbers and the difference» is `ControlCheckDto`; the queue item is `discrepancy_unresolved` with its own resolution. |
| `## What an unconfirmed posting does and does not mean` | `- **\`needs_reconciliation\`** — nothing is ever` | 1 | `ActionDto::target`; `RequestPlanDto::missing`; `ObservationBasisDto::start` | — | The queue item names the account, the interval and which end the balance is wanted at; `start` is why the opening point is answered before the closing one. |
| `## A fact can be quoted, a derived value cannot` | `The key rate, an FX rate` | 1 | `MarketPriceDto`; `MarketFxDto`; `MarketKeyRateDto` (their fields) | — | Value, date or interval boundaries, source, observation moment and quality are each a field of the row. |
| `## A fact can be quoted, a derived value cannot` | `The **completeness boundary** is carried by` | 1 | `MarketPriceSeriesDto::complete_through`; `MarketFxSeriesDto::complete_through`; `MarketKeyRateSeriesDto::complete_through` | — | Carried word for word on all three, empty-series case included. |
| `## A fact can be quoted, a derived value cannot` | `Adding them up, recomputing them and` | 3 | — | `## The overriding rule` | Arithmetic of the agent's own. Design §6. |
| `## A fact can be quoted, a derived value cannot` | `For prices, distinguish three things: the` (a) | **2** | `MarketPriceDto::quotation_basis` | — | Only `recorded_quotation_basis` has a description. That the effective basis, the recorded basis and the machine status of how well it is proven are three things whose agreement is not a given — a divergence means the source contradicts itself — is on nothing. |
| `## A fact can be quoted, a derived value cannot` | `For prices, distinguish three things: the` (b) | **2** | `MarketKeyRateDto::boundary` | — | The field has no description. That an interval's boundary may be **inferred** — the source gave only trading days and the effective date fell between them — is on nothing. |
| `## A fact can be quoted, a derived value cannot` | `When the API refuses because of` | — | — | — | **No home yet.** See the section at the end. |

## No home yet

Two paragraphs have no carrier this pass could name and no destination in the new
`SKILL.md`. Neither is dropped; each says what a carrier for it would have to be.

### 1. The place of custody is named by the owner's title, and nothing else names one

`the-money-and-the-perimeter.md`, `## An instrument's external code resolves as
of a date`, second half of the first paragraph:

> The place of custody is named by the owner's own title for it, and nothing
> else names one: no source prints an identity for a depository, so there is no
> second vocabulary there to prefer.

It is true, and it is the correct contrast with the account tiering two sections
above. It is also invisible: every custody in a request and a response is a
`Uuid`, and the title resolution happens only while a **document** is being read,
where it lives in a Rust comment on `build_directory`
(`crates/iaam-server/src/routes.rs:5525`) that nothing publishes.

**What a carrier would have to be.** The description of the document-reading
operation — the route that resolves a document's custody column — saying that a
custody cell is matched against the owner's own title and against nothing else,
beside the sentence that already says an account cell goes through the account
tiering. That is a route description rather than a field of `dto.rs`, so no
`Struct::field` names it and this inventory will not invent one. If Task 5 or 6
would rather build a field for it, `SourceDocumentParams` is where a document's
reading is parameterised.

### 2. Lower the frequency rather than repeating immediately

`reading-the-reports.md`, `## A fact can be quoted, a derived value cannot`, last
paragraph:

> When the API refuses because of request frequency, lower the frequency rather
> than repeating immediately.

The failure happens with a call in flight, so it is not class 3. No DTO field
carries it: a rate-limit refusal is a transport-level answer with no schema of its
own in `dto.rs`.

**What a carrier would have to be.** The published description of the rate-limit
refusal on the market routes — the same place the retry interval is stated —
saying that the answer is to lower the frequency and not to repeat. Failing that,
it is one clause and it can ride on the market routes' own description. It is not
worth a class-2 row against a field that does not carry it.

## Counts

| Class | Rows | What Tasks 3–6 do with them |
| --- | --- | --- |
| 1 | 153 | Delete. Nothing is written. 16 of them are marked † — pointers and preambles that die with the files they name. |
| 2 | 41 | Write the description on the named `Struct::field`, then delete the paragraph in the same change. |
| 3 | 34 | Keep, under the named `SKILL.md` heading. |
| No home yet | 2 | Raise, per design §6's first risk. Do not delete until answered. |
| **Total** | **230** | over 43 headings in five files |

Per file: `SKILL.md` 57, `importing.md` 68,
`the-money-and-the-perimeter.md` 44, `correcting.md` 10,
`reading-the-reports.md` 51.

The 230 rows reconcile against the files exactly: 223 blank-line-separated
blocks, plus one row for `SKILL.md`'s own title, plus six paragraphs each split
into an `a` and a `b` row.

### The class-2 carriers, once each

`ActionDto::state` · `CaveatDto::closed_by` · `CategoryDto::title` ·
`CategoryRuleImpactDto::months` · `ContourDto::accounts` ·
`CorrectImportRequest::source` · `CreateAccountRequest::provider_account_id` ·
`CreateAccountRequest::title` · `DataQualityDto::material_issues` ·
`DimensionStatusDto::status` · `ImportQuestionDto::row` ·
`ImportSessionDto::state` · `InstrumentDto::denomination_currency` ·
`MarketKeyRateDto::boundary` · `MarketPriceDto::quotation_basis` ·
`NavCoverageDto::accepted_internal` · `ObservationBasisDto::compared` ·
`OpeningAssertionsDto::acquisition_date` · `OperationDto::account` ·
`OperationDto::kind` · `QuestionGeneralisationDto::proposal` ·
`RecordAccountRetirementRequest::state` · `ResolveInstrumentRequest::on` ·
`ReturnsReportDto::history_starts` · `SubmitCorrectionsRequest::corrections`

Twenty-five distinct fields over thirty-eight rows: `OperationDto::kind` takes
eight (the amount rules and the transfer rules, which are the `unresolved_direction`
and `transfer` variants of the schema it publishes), `ResolveInstrumentRequest::on`
three, and `DataQualityDto::material_issues` four.

## Two things the pass turned up that are not classification

1. **`importing.md` contradicts the contract about the unanswered count.** The
   paragraph beginning «That figure is the never-answered count and not the
   waiting count» describes behaviour `iaam-m2oi` removed. The contract now says
   the opposite in as many words — `ImportSessionContentsDto::unanswered` is
   documented as «Not the number of questions with no answer recorded» — and the
   queue item's own sentence is built from that figure. The skill tells an agent
   never to read out to the owner a number that is now exactly «how much is
   left». It is class 1 and it is deleted; nothing of it is carried forward.

2. **The design's §4 guess held everywhere but one row.** The row it named as
   the one that would not be uniform — what a contour is, what a category cannot
   change, how an account name is read, how an instrument code resolves — is
   indeed the only one that split three ways: the account-naming half is almost
   entirely class 1 (the tiering is published twice over), the contour and
   category meanings are class 2 on `ContourDto::accounts` and `CategoryDto::title`,
   and the custody half has no carrier at all and is in `## No home yet`.
