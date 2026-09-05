# Domain glossary: Russian → English

The codebase is English (see `CLAUDE.md`). This table is what the Russian
domain vocabulary was translated into, and it stays as the reference for
anything written from now on: one term must not travel under three names,
or the code stops being greppable.

If a domain term is missing here, add it before using it. A synonym
invented on the spot is worse than an awkward but shared one.

The Russian column is kept on purpose: the existing spec and plans under
`.internal/` are in Russian and are not being retranslated, so this table
is how you match a term in them to the code.

## Money and value

| Russian | English | Note |
|---|---|---|
| номинал (первоначальный) | face value | `initial_principal` in the schedule is established; do not rename |
| номинал (непогашенный остаток) | principal | the remainder is `remaining_principal` |
| возврат номинала | principal return | |
| доля возврата | returned share | the `ReturnedShare` type |
| доля разнесения | basis allocation | the `BasisAllocation` type |
| налоговая стоимость | cost basis | |
| историческая стоимость приобретения | acquisition basis | |
| освобождённая стоимость | released basis | |
| реализованный результат | realised result | British `-ised`, as already in the code |
| денежный поток | cashflow | |
| НКД | accrued interest | |
| купон | coupon | |
| выплата (плановая) | posting | `ExpectedPosting`, `PostingKind` |
| компенсация | compensation | |
| минимальная единица валюты | minor unit | |
| внесение средств | contribution | `FlowClass::ExternalIn`; money crossing into the contour |
| вывод средств | withdrawal | `FlowClass::ExternalOut` |
| доходность | return | never “yield”; `xirr_pre_tax` is the pre-tax return |
| ключевая ставка | key rate | `KeyRateObservation` |

## Securities and position

| Russian | English | Note |
|---|---|---|
| бумага, инструмент | instrument | in prose, prefer “security” |
| выпуск | issue | `issue_terms` |
| партия, лот | lot | |
| позиция | position | |
| количество | quantity | |
| депозитарий | custody | |
| счёт | account | |
| владелец | owner | |
| контур | contour | |
| периметр | perimeter | |
| прекращение счёта | account retirement | `AccountRetirement`; the owner's statement that one product ceased to exist on a date. **A second axis, not a contour decision**: a retired account normally stays a contour member, so that the interest it paid keeps counting as an earning and the movement that emptied it stays internal (decision 0014). Never «closure» and never «archival» |
| дата прекращения | retirement effective date | `effective_on`; the date in the owner's own history, not the moment he told the system |
| ревизия прекращений | retirement revision | `RetirementRevision`; one monotone coordinate per owner over all of his retirement declarations, stated by every report so that two answers can be compared |
| платёжный инструмент | payment instrument | a card or other means of paying against an account; **not** an account of its own — two of them over one account are one account with two aliases (decision 0004) |
| график выплат | schedule | `BondSchedule` |
| оферта | offer | the right itself is `offer right` |
| погашение | redemption | partial one is a partial redemption |
| амортизация | amortisation | British `-isation`, as already in the code |
| замещение | conversion | `LineageReason::Conversion`; the instrument itself is a replacement bond |
| внешний код | external code | ISIN, ticker, MOEX `SECID`, FIGI, broker code; `AliasNamespace` |
| справочник инструментов | instrument catalogue | `crates/iaam-core/src/instrument.rs`; shared across all owners |
| справочник владельца | directory | `Directory`; the owner's own names for accounts and custody |
| род инструмента | instrument kind | `InstrumentKind`; may be unset, and that is not an error |
| валюта обязательства | denomination currency | |
| валюта расчётов | settlement currency | |
| валюта котировки | quote currency | |
| валюта отчёта | report currency | a property of the report, not of the instrument |

## Journal and projection

| Russian | English | Note |
|---|---|---|
| журнал | journal | the append-only event journal |
| событие | event | |
| факт | fact | |
| приёмка | ingest | |
| нормализация | normalisation | |
| вердикт | verdict | |
| проекция | projection | |
| снимок | snapshot | |
| срез (журнала) | slice | |
| отпечаток | digest / fingerprint | `prefix_digest` and `fingerprint` are different things; do not mix |
| дельта | delta | |
| порядок (внутри дня) | effective order | |
| состояние поручения | order state | `ChannelOrderState`; the broker channel's own state, not ours |
| ключ идемпотентности | idempotency key | `idempotency_key` |
| утверждение (владельца) | assertion | `assertions`; what the owner states — never a second source |
| восстановленное начало | reconstructed opening | a position that existed before the journal began |

## Dates

| Russian | English | Note |
|---|---|---|
| дата сделки | trade date | |
| дата расчётов | settlement date | |
| дата зачисления денег | cash posted date | |
| дата фиксации реестра | record date | |
| дата вступления в силу | effective date | |
| координата знания | knowledge coordinate | |
| на дату | as of | |
| дата приобретения | acquisition date | `acquisition_date` |
| граница владения | ownership boundary | drawn from the acquisition date; without it a posting is unverifiable |
| момент наблюдения | observation moment | `observed_at` |

## Input and import

| Russian | English | Note |
|---|---|---|
| выгрузка (банка) | bank export | the file or page the institution hands the owner |
| организация (где открыт счёт) | institution | the bank, broker or other organisation an account is held at, as the **owner** names it — `CreateAccountRequest.institution`. Free text this system does not interpret and matches nothing against; it is what he reads. Never a synonym for **provider** below, and the question put to him about an account is this one (decision 0030) |
| метка источника | provider | `provider`; the label that scopes the identifier a source prints, so that two sources printing short sequential identifiers cannot collide (decision 0004). Not the institution above and never shown as one: it is derived, and where a source profile read the document the queue mints it from that profile's issuer rather than asking anybody (decision 0030) |
| канал | channel | `SourceChannel`; how a fact reached us, not who stated it |
| предложенный ответ | proposed answer | `ProposedAnswer`; a value this instance worked out for one field, put to the owner **as a question** over every queue item it would fill, with the ground it stands on. Never a **preset**: a preset is the request already answered and is never read out to him, and a proposal exists to be read out (decision 0033). Nothing is recorded until he agrees |
| набор, отвечаемый одним ответом | covered set | the queue items one proposed answer fills, named in full on each of them. Complete: an item that cannot take the answer is in no set rather than left out of one, so an answer carried beyond the set is outside the offer (decision 0033) |
| необязательное поле запроса | optional field | a missing field the route accepts the request without. A fact about the call and not a grade of the question — what leaving it out costs is in the question's consequence, and an optional question is put to the owner with a way past it (decision 0033). Not the same as «the schema does not require it»: a route may refuse a request for a field its schema marks optional |
| решённый пункт | settled item | `ActionState::Settled`; a queue item that wants nothing — the owner decided something, the item says what his decision left standing, and its only call is the withdrawal of that decision. Shown when he asks what has been decided, never raised as work (decision 0036). Not **blocked**, which publishes no call at all, and not **informational**, which is the item's urgency rather than what is wanted of a reader |
| цель отчёта | report goal | `ReportGoal`; one of the four questions this system answers about money — what is held, where money came from and went, what it earned, whether the journal agrees with the sources. A closed vocabulary of four names, published by every queue item, by every report's own statement of what it is silent about, and by the discovery document, and a client joins the three on them |
| положение по отчёту | report standing | `ReportStanding`; one report goal and the outstanding items standing between the owner and it, folded out of the same queue those items are published in. No obstacles named means nothing outstanding stands in the way — never that the report is complete, which is the report's own business to state (decision 0039) |
| пересказ вопроса | relay | putting a published question to the owner: what the published wording fixes — what is asked, what it is for, what the choice changes — said in his language and his register, with none of the machinery in it. Neither a quotation of that wording nor a question invented in place of it (decision 0036) |
| объявленный источник | declared source | `SourceId::declared(owner, account, channel)`; what deduplication is scoped by |
| объявленный импорт | declared import | `ImportId::declared(owner, account, channel, label)`; one submission, and what an import correction is keyed on |
| метка импорта | import label | the caller's name for one import within an account and channel |
| маппинг колонок | column mapping | which export column feeds which field |
| профиль источника | source profile | the JSON file describing one document type of one institution: column mapping plus a translation of that source's own words into iaam's. Data, never code, and it concludes nothing (decision 0019) |
| движок импорта | import engine | the one in-tree reader that takes a document and a profile and produces observations. There is one of it; a profile does not parse, it describes |
| передача документа | conveying | an agent moving a document of the owner's to his own instance, **unread**. Permitted, and the ordinary way an import starts (decision 0022). Never «uploading», which says nothing about who read it |
| истолкование документа | interpreting | producing from a document a claim about what it says: parsing it, summarising its rows, or deciding what a row was. Never an agent's act — the engine reads and the session asks (decision 0022) |
| каталог профилей | profile catalogue | what an instance publishes about the profiles it holds: id, version, digest, origin, and the reason any was refused |
| категория источника | source category | `source_category`; the word the source filed the row under — what the money was **for**. Transcribed verbatim and never mapped (decision 0019 §6); the owner's own rules read it, and two of them do — a category rule files the row under one of his categories, a classification rule says what the row is |
| слово источника об операции | source operation word | `source_kind`; the word the source printed for what the operation **was** — its operation-type cell. A different fact from the source category beside it, and never written through the same slot (decision 0020 §2) |
| карта слов | token map | a profile's mapping from a literal the source prints to one of iaam's own words. Total over the source's vocabulary, with no catch-all: an unmapped word rejects the row |
| слова о своей стороне | own-account words | `own_account_words`; the sentences with which a source asserts, inside a free-text column, that the far side is the owner's. Not a token map and not total: it can only say `own_account`, so a sentence it lacks costs a question that gets asked (decision 0028 §1) |
| статус строки | row status | `RowStatus`; what the source said about whether the movement happened — `completed`, `pending` or `declined`. Transcribed by the profile, acted on by the engine, and carried by no observation: a row that is not `completed` is refused by name (decision 0028 §3) |
| собственная категория владельца | owner category | `owner_category`; the category the **owner himself** filed the row under, at the source, in that institution's own app. His decision, already made, which the export prints back — a different fact from the source category beside it, and never written through the same slot. Transcribed verbatim and never mapped: it is his decision in his bank's vocabulary, and what it is called here is one question per distinct value |
| код категории торговой точки | merchant category code | `source_code`; the code a payment network assigns to a business, printed on a card export. Not one institution's vocabulary, so a rule written on it holds across institutions. Transcribed as text and never as a number — it is an identifier printed with leading zeros — and required of nothing, because a source assigns none to a row that is not a purchase from a merchant (decision 0028 §4) |
| признак учёта у источника | source analytics flag | what a source says about whether **it** counts a row as a real expense. Deliberately not transcribed: it is that institution's reporting perimeter, and the owner has one of his own (decision 0028 §4) |
| ключ строки | row key | the stable identity that makes a re-import idempotent |
| нога (перевода) | leg | one of the two rows an internal transfer produces |
| нога движения | cash leg | `CashLeg`; one side of a cash movement offered to the pairing matcher, which may or may not turn out to be a transfer leg |
| дальняя сторона | far side | the account on the other end of a movement, seen from the account whose statement the row is on; never «destination», which asserts a direction |
| утверждение о дальней стороне | far-side assertion | `FarSide`; what the **source** said about whose account the far side is. `own_account` or `unstated`, and never inferred |
| движение между своими счетами | own-account movement | `EventKind::OwnAccountMovement`; a movement whose far side is the owner's and unnamed. Distinct from a **transfer**, which names both accounts |
| движение без направления | unresolved own-account movement | `EventKind::UnresolvedOwnAccountMovement`; the same movement with no direction stated, so it posts no leg |
| зеркальная строка | mirror row | `iaam_ingest::mirror`; the second row a single document prints for one movement between two of the owner's own accounts — the departure on one account and the arrival on the other. Never a **duplicate**, which is one row submitted twice, and never a **pairing candidate**, which relates two documents |
| вторая нога одного движения | second leg of one movement | `NoFactReason::SecondLegOfOneMovement`; a mirror row that records nothing because another row of the same session already records the movement with a leg on each account (decision 0031) |
| пара вопросов | question pair | `OpenQuestion::pair`; the identifier two open questions share when they are the two legs of one movement. Distinct from **alike rows**, which are the same decision about different money |
| похожие строки | alike rows | `OpenQuestion::alike`; the other open rows of one session raising the same decision — `QuestionSubject`, which is the question paired with the direction the source stated. The same decision about different money, and never the same money printed twice, which is a **question pair** |
| группа строк | row group | `RowGroup`; a set of one session's open rows published as one thing — what its members state alike, how far the ones that differ run, the sentence to put to the owner about the whole of it, and the reach one answer must state to settle it (decision 0034). Never a set of one, and never carrying a **representative row**: a member is a particular, and showing one as the group takes an answer about the rest from evidence about it |
| общее у группы | shared row | `SharedRow`; the values every member of a row group states alike, read off the members and not derived from what made them a group. An absence is «they do not all state the same thing», never «this system could not tell» |
| разброс группы | span | `DaySpan`, `AmountSpan`; the two endpoints the members of a row group run between. A span and not an endpoint — «between these two days» says which page of a statement to open — and the amounts keep the signs the source printed, so a span agrees with the lines the owner is looking at |
| основание факта | fact basis | `FactBasis`; on whose word a row was settled — the caller concluded, the directory recognised, the source asserted, a rule matched, or the owner answered. Published beside the event kind, which says what the fact is and never why it may be written (decision 0031 §7) |
| строка без факта | settled without a fact | `RowResolution::NoFact`; a row read, understood and correctly producing no journal fact. Not a refusal and not a quarantine |
| вопрос ждёт ответа | question waiting on the owner | a question whose row this session's own reading cannot settle, which is what refuses the commit and what a caller may put to him. Never «he has not answered it»: a standing rule of his, his directory, or another row of the session settles a row and leaves `answered_at` empty for ever, because he never spoke about it (decision 0038) |
| снятие вопроса без ответа | question settlement | `QuestionSettlement`; why a question stopped waiting on the owner although he never answered it, said in the two vocabularies that already exist — a **fact basis** where the row became a fact, a **settled without a fact** reason where it did not. Published on the question as `settled_without_answer`, and never a third vocabulary of its own (decision 0038) |
| скилл импорта | import skill | per-institution knowledge held as an agent skill, never as code. Since decision 0019 it is no longer the only place such knowledge may live: a **source profile** holds the format half in the tree, and the skill keeps what the format cannot supply |
| удержанная строка | held row | a row an import session holds and the journal does not. It becomes a fact only at commit, and until then it is outside every figure the system publishes unless the request asks for it (decision 0018) |
| совокупность строк | held-row scope | `HeldScope`; which held rows, **beside** the journal, a report was asked to fold: none, every open session, or the sessions the request named. Never *instead of* the journal — there is no report over held rows alone |

## Refusals and quality

| Russian | English | Note |
|---|---|---|
| отказ | refusal | a refusal to compute; not `error` unless it really is one |
| невычислимо | not computable | the `NotComputable` type |
| неизвестно | unknown | never render as zero |
| разрыв | gap | `BasisGap`, `AllocationGap` |
| разрыв покрытия | coverage gap | `ImportCoverageGap`; refused rows leave named dimensions unconfirmed |
| сверка | reconciliation | |
| измерение (сверки) | dimension | `dimension`; the axis a reconciliation covers — cash, positions, income, tax basis. A verdict names one; it does not reconcile it |
| основание котировки | quotation basis | `QuotationBasis`; effective, recorded and proof status are three different things |
| граница полноты | completeness boundary | `complete_through` |
| выведенная (граница) | inferred | `Boundary::InferredAcrossNonTradingDays`; not “derived” for this |
| расхождение | discrepancy | |
| материальная проблема | material issue | |
| качество данных | data quality | |
| недоказуемо | unverifiable | |
| заслон | guard | the scripts under `scripts/` |
| инвариант | invariant | |
| порог | threshold | |
| карантин | quarantine | the `Quarantined` row: neither a fact nor lost |
| неопределённый поток | indeterminate flow | `FlowClass::Indeterminate`; the cash moved on a contour account and whether it crossed the boundary cannot be decided. An answer, not a missing one |

## Transport and infrastructure

| Russian | English | Note |
|---|---|---|
| эталон | reference implementation | an independent implementation used for parity checks |
| списание лотов | lot disposal | |
| якорь доверия | trust anchor | |
| узел | endpoint | an external HTTP destination |
| шлюз | gateway | |
| среда | environment | prod or sandbox |
| перешифровка | re-encryption | |
| ротация | rotation | |

## What this glossary does not cover

- **Values that come from a source.** `"Оферта"` in `iaam-market` is a
  MOEX ISS value, not our text; translating it breaks parsing. The same
  goes for broker report sheet and column names.
- **Existing Russian documents.** `docs/`, `README.md` and `.internal/`
  are left as they are. New documents are written in English.
