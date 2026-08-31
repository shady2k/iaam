# Необратимое ядро схемы

Изменение любого пункта ниже потребует миграции журнала фактов — то есть
переинтерпретации уже записанных событий. Всё остальное аддитивно (§16.2
спецификации) и добавляется новым типом события или новой проекцией.

| # | Требование §16.1 | Где реализовано |
|---|---|---|
| 1 | Версионированный envelope события | `event::Event`, `event::SCHEMA_VERSION` |
| 2 | Несколько семантических дат | `dates::EventDates` — шесть различных типов |
| 3 | Сохранение сырых значений без потерь | `event::kind::EventKind` — gross, fee, НКД раздельно |
| 4 | Раздельные идентичности | `ids` — owner, account, custody, instrument, source, transfer |
| 5 | Типизированные ноги движения | `event::leg::{Leg, LegKind}` |
| 6 | Проведённые суммы против расчётных | `money::PostedMinor` против `numeric::decimal::Dec` |
| 7 | Append-only с детерминированным разрешением | `event::correction::resolve` |
| 8 | Provenance | `event::provenance::Provenance` — без хеша не конструируется |
| 9 | FIFO не зашит в факт продажи | `rules::lot_disposal` — версионированная стратегия через `RuleRegistry` |
| 10 | `unknown` как значение | `event::Confidence`, `Option<T>` во всех неизвестных полях |

Дополнительно зафиксировано по итогам ревью и исполнения:

| Требование | Где реализовано |
|---|---|
| Перевод несёт **оба** счёта | `EventKind::CashTransfer { transfer_id, from, to, amount }` |
| Классификация относительно контура — от пары принадлежностей | `contour::classify`, `FlowClass::{ExternalIn, ExternalOut, Internal, Irrelevant}` |
| Структура события проверяется по его типу, а не общим балансом ног | `event::Event::validate_structure` |
| Перевод сам на себя отклоняется по существу | `EventValidationError::TransferToSelf` |
| Деньги нельзя сложить в обход валюты | `PostedMinor` с приватным полем, `Money` без `impl Add` |
| Отрицание не паникует на границе типа | `checked_negate`, `checked_sub`, `Exact::neg` через `checked_neg` |
| Три числовых режима | `numeric::{exact, decimal, approx}` |
| Цена — факт с источником и качеством, а не параметр запроса | `EventKind::Valuation`, `valuation::PriceQuality` |
| Нога сверяется с событием по инструменту, счёту и количеству со знаком | `Event::validate_trade`, `Event::validate_opening_position` |
| Количество, цена и сумма сделки обязаны быть положительными | `EventValidationError::NonPositive` |
| Версия схемы события — 3: вариант `ControlAssertion` добавлен после версии 2 | `event::SCHEMA_VERSION` |
| Журнал переживает круг через JSON: проверен каждый вариант события | `crates/iaam-core/tests/serde_roundtrip.rs` |
| Несовместимость типов идентификаторов, дат и режимов чисел — заслон, а не соглашение | `crates/iaam-core/tests/ui/` |
| Журнал append-only на уровне базы, а не кода | триггеры `0001_initial.sql` |
| Данные восстановимы из переносимого архива, повреждённый архив отклоняется | `iaam_store::bundle` |


E2 добавил поля и правила, которые также нельзя переопределять без
переинтерпретации уже записанного журнала:

| Требование | Где реализовано |
|---|---|
| Канал, источник, версия парсера и идентичность документа различаются | `event::provenance::Provenance`, `reconciliation::evidence::SourceChannel` |
| Контрольное утверждение не двигает деньги и относится к одному измерению | `EventKind::ControlAssertion`, `reconciliation::claim::ControlClaim` |
| Сверка имеет ключ `интервал × измерение`, а не один статус счёта | `reconciliation::ReconciliationLedger`, `Dimension` и `DimensionStatus` |
| `accepted_independent` требует доказанной независимости канала | `reconciliation::evidence::{Evidence, Ground}` |
| Расхождение и отсутствие покрытия не смешиваются | `reconciliation::check::ClaimOutcome` |
| Денежный эффект неподдерживаемой операции сохраняется, экономика не достраивается | `perimeter::{PerimeterAssessment, ReconciliationException}` |
| Качество NAV хранит четыре доли: independent, internal, provisional, discrepant | `returns::{DataQuality, NavCoverage, MaterialIssue}` |
| Результат строки импорта не скрывает отказ или неоднозначность | `iaam_ingest::verdict::Verdict` и `iaam-server::dto::VerdictDto` |
| Версия правила классификации и план пересчёта отделены от фактов | `iaam_ingest::classification::ClassificationRule::version`, `recompute_plan` |


Особенно важна независимость: другой отчёт того же брокера, разобранный
тем же парсером, не может задним числом стать независимым подтверждением.
Изменение правил определения канала, документа или версии парсера
изменило бы уже вычисленные основания сверки.

E3.4.8 и E3.4.14 добавили сверку запланированных выплат с журналом.
Её вердикты уже выданы владельцу, поэтому переопределить их без
переинтерпретации журнала нельзя:

| Требование | Где реализовано |
|---|---|
| Право на выплату определяется на дату фиксации, а не на дату платежа | `rules::posting_match::PostingMatchV2`, `rules::cashflow::ScheduledPosting::entitlement` |
| Ширина, односторонность и жадность окна сопоставления версионируются отдельно от хранения фактов | `rules::posting_match::{PostingMatchVersion, PostingMatchV2::version}` |
| Владение выводится из диапазона возможного остатка и имеет третье значение `Unknown` | `projection::ownership::{Ownership, OwnershipHistory}` |
| Датированный факт дохода привязан к счёту, инструменту и виду выплаты | `projection::income::{IncomeLedger, ReceivedPosting}` |
| Недоказуемость отделена от пропуска и несёт причину | `returns::{MaterialIssue::ScheduledPostingUnverifiable, UnverifiableReason}` |
| Утверждения владельца о восстановленном начале хранятся отдельно от количества | `event::kind::OpeningAssertions` |

Окно в 21 календарный день — не настройка. Оно выведено из депозитарной
цепочки «эмитент → НРД → депозитарий брокера → владелец»: до семи рабочих
дней на последнее звено (ст. 8.7 Федерального закона 39-ФЗ) плюс
предыдущие звенья дают около десяти рабочих дней, что через праздничный
период превращается в 21 календарный. До истечения окна отсутствие факта
не является пропуском. Сузить окно задним числом — значит объявить
пропущенными выплаты, которые в момент прошлого отчёта ещё законно шли по
цепочке.

Три значения `Ownership` тоже необратимы. `Unknown` — это ответ, а не
отсутствие ответа: он говорит, что границу владения провести нечем, и
именно он не даёт объявить пропуском выплату по бумаге, проданной внутри
непокрытого журналом интервала. Сведение владения к паре «владел /
не владел» превратило бы каждую такую выплату в ложную тревогу.

## E3.4: a T-Invest execution becomes a fact

The T-Invest channel stopped treating a broker order as a trade. What it
records now, and what it refuses to record, has already reached owners as
facts and as quarantine rows, so none of it can be redefined without
reinterpreting the journal.

| Requirement | Where implemented |
|---|---|
| The channel's own order state is a typed contract, not a string compared in passing | `iaam_broker::tinkoff::ChannelOrderState`, with `Unrecognised(String)` for a state the contract does not name |
| Only an executed order becomes a fact; every other state becomes one quarantine row rather than silence or an aborted batch | the state gate in `adapt_operations`, between `dictionary.kind_of` and `operation_to_submitted` |
| One fill is one fact: quantity, price and moment come from the fill, not from the order that carried it | one `SubmittedOperation` per element of `trades_info.trades` |
| Custody is the position's identifier, never the account's | `positionUid` on the trading row; a row without one is quarantined naming the field |
| A security leg whose custody equals the account is a defect of past imports, not an old but valid shape | the whole history is read through `Date::MAX`, and the account's import is refused before any append (repair: iaam-y3a2) |
| A securities transfer is not cash, and its direction is part of its meaning | `ChannelOperationKind::{SecuritiesTransferIn, SecuritiesTransferOut}`, dictionary codes `securities_transfer_in` and `securities_transfer_out` |
| A reported accrued interest of zero is a value; only a charged fee treats zero as absent | `accrued_interest_money` against `fee_money` in `iaam_ingest::operation` |
| An order's reported payment is checked against the exact total of its fills, and the remainder is allocated by a stated rule rather than by the adapter | `rules::trade_allocation::{check_order_completeness, allocate_minor}` |
| The channel's arithmetic is the core's decision, not the shell's convenience | `money::{sum_quantities, gross_for_fill}` beside their siblings; the adapter calls each once and composes nothing |

Schema version 14 carries this: migration
`0014_securities_transfer_kinds.sql` rewrites the dictionary rows already
recorded for `OPERATION_TYPE_INPUT_SECURITIES` and
`OPERATION_TYPE_OUTPUT_SECURITIES`, which until now meant a movement of
money. The rewrite is the migration this section is about — a dictionary
row is an owner's decision about meaning, and the old meaning was wrong
rather than merely coarse.

The refusal deserves its own note. Re-importing is not repair: an old
fact carries account-derived custody inside its `OperationKind`, so a
re-imported one differs by content fingerprint, arrives as `Fresh`, and
is inserted beside the old one — double-counting the position. Until the
reversal exists, refusing the account's import is the only answer that
does not corrupt the journal, and it is deliberately wider than the
requested interval: an owner re-importing a suspicious month must not be
told the rest of the history is sound.

Cannot change without a migration:

- The state gate's verdict — which channel states produce a fact. Owners
  have already been shown quarantine rows naming the state, and widening
  the gate would silently turn a reported non-fact into a fact.
- One-fact-per-fill and the identity built from the fill. Collapsing
  fills back onto the order would change source-operation identity for
  every trade already recorded.
- Custody as the position's identifier, and the defect predicate that
  detects the account-derived shape. The predicate is the shape itself,
  not the parser version and not the source channel: a fact from this
  branch is `tinkoff-api/3` and still defective, and a persisted
  `SourceId` names an access record that may since have been revoked.
- The meaning and the codes of `securities_transfer_in` and
  `securities_transfer_out` — they are already in `broker_operation_kinds`.
- Accrued interest accepting a reported zero. Restoring the rejection
  would retroactively unrecord trades that reported no coupon.

## E3.4, continued: a per-row refusal is not a response-wide verdict

One row's property was being applied to a whole response, and one import
attempt's shortfall to a whole interval. The verdicts and the facts below have
already reached owners, so none of them can be redefined without
reinterpreting the journal.

| Requirement | Where implemented |
|---|---|
| A channel says what its portfolio answer describes rather than accepting a date it will ignore | `ports::PortfolioAsOf::{Requested, Current}` in `PortfolioSnapshot` |
| A portfolio dated outside the requested interval becomes no assertion, and the refusal is named rather than reported as a zero | `sync::AssertionsWithheld::PortfolioDescribesAnotherDay` |
| A row that cannot become a fact is quarantined and named, and the batch continues | `Verdict::Quarantined { reason }`, code `quarantined` |
| A row whose event fails structural validation is refused and counted into the coverage gap | `scenarios::ingest::structural_rejection` in `sync::sync_broker` |
| A refusal of the row is a different thing from a defect of the adapter, and the difference is observable | `RowRefusal::{Row, Adapter}` against `BrokerError::Adapter` |
| An import attempt that refused rows cannot itself confirm the dimensions those rows would have moved | `EventKind::ImportCoverageGap { period, dimensions, refused }` |
| The gap disqualifies the attempt, not the interval | the subtraction in `reconciliation::confirmed_dimensions` and in each ground, with no change to `raise`, `merge_status` or `with_external_evidence` |
| A gap is correlated by account, period, source and parser version, deliberately without the document | `reconciliation::tainted_dimensions` |
| Which dimensions a refused kind cannot confirm | the classification in `adapters::tinkoff` and `sync::operation_dimensions` |

Schema version 7 carries the new variant.

The correlation deserves its own note, because the obvious version of it does
not work and fails silently. `collect_groups` builds a group's channel with
the event's raw hash as its `document`, and `assertion_event` gives every
claim a synthetic hash derived from an identity string containing the claim
itself. Every API claim is therefore already a singleton group, and a gap
matched to a group by `SourceChannel` equality would match nothing at all —
while every test asserting that a gap was written would still pass.

The choice of mechanism was made against a cheaper one and the cheaper one is
wrong. An interval-wide confidence cap is bypassable, because `raise` is a
monotonic maximum and both `merge_status` and `with_external_evidence` apply
after the status is built; it has no recovery, because the journal is
append-only and a fact meaning "this interval had refused rows" would poison
the interval for ever; and it asserts something false, because a refused row
does not prove the journal is incomplete — the same operation may already be
present from a broker report. Withholding the attempt's evidence avoids the first
and the third: a claim's outcome stays truthful and no status is capped, so
another channel or another interval raises confidence with no special case.

Recovery is narrower than it first appears, and the limit is worth stating.
The correlation is by channel, not by attempt, so a gap withholds that
channel's evidence for that interval permanently. Re-running the same import
cleanly does not lift it — and cannot, because the assertion idempotency key
is fixed by account, interval, source and claim, so a repeat records no new
group to recover into. Confirmation for such an interval comes from a
different channel or a different interval. Making recovery by the same
channel possible needs an attempt identity the journal does not yet carry.

Cannot change without a migration:

- The meaning of `ImportCoverageGap` and its correlation rule. Gaps already
  recorded decide what evidence past intervals have, and a rule that matched
  differently would silently re-decide them.
- The requirement that a gap name at least one dimension. An empty gap would
  be a fact asserting nothing, and `validate_structure` refuses it.
- The classification of a refusal into dimensions. Narrowing it later would
  retroactively raise confidence that was withheld; widening it would withdraw
  confirmation already reported to the owner.
- The code `quarantined` and what it means: the row was read and no fact was
  recorded from it. It is neither `unsupported`, whose monetary effect **is**
  preserved, nor `rejected`, which carries a structured rejection of a row
  that could not be parsed.

## Что менять нельзя без миграции

- Состав и семантику `EventDates` — от них зависит налоговый период.
- Порядок полей `EffectiveOrder` — от него зависит детерминизм сортировки.
- Значения `EventKind::discriminant()` — они попадают в хранилище.
- Семантику `flow_endpoints()` и состав `CashTransfer` — от них зависит
  вся доходность.
- Требование `Provenance` — восстановить происхождение задним числом
  невозможно.
- Семантику `SourceChannel` и правило независимости по версии парсера и
  документу — иначе прежние основания сверки изменят смысл.
- Состав `ControlClaim`, `Dimension` и `ClaimOutcome` — это координаты
  многомерной сверки, а не отображение ответа API.
- Коды и запись `Verdict` — построчный исход уже сообщает владельцу,
  записан ли факт и какое действие требуется.
- Версию `ClassificationRule` и append-only план `recompute_plan` — правка
  классификации должна добавлять исправляющие факты, а не переписывать
  историю.
- Ширину и односторонность окна `PostingMatchV2` без новой
  `PostingMatchVersion` — вердикты «не подтверждена» уже сообщены
  владельцу, и молчаливая правка окна изменит их смысл.
- Семантику `ScheduledPosting::entitlement` — отсутствие даты фиксации
  обязано давать недоказуемость, а не суждение по дате платежа.
- Состав `UnverifiableReason` и его коды — по ним владелец узнаёт, что
  именно дозагрузить, чтобы сверка стала возможной.
- Состав `OpeningAssertions` — пустое поле означает, что владелец ничего
  не утверждал; вывести эту уверенность задним числом из соседних полей
  невозможно.

## Что можно добавлять свободно

Новые варианты `EventKind` и корпоративных действий; новые версии правил
в `RuleRegistry`; новые проекции; налоговые базы и лоты; NAV и TWR;
разложение результата; рыночные данные.

## Чего проверки не гарантируют

Это не оговорки, а границы доверия к зелёной сборке.

- **Мутационный заслон почти слеп на `contour::classify`
  и `EventKind::flow_endpoints`.** Исчерпывающий `match`, возвращающий
  `enum` без `Default`, даёт единственный нежизнеспособный мутант.
  Гарантию даёт табличный тест на все шестнадцать сочетаний.
- **Свойство сохранения стоимости не ловит неверное разнесение.**
  Невыбывшая часть считается от того же значения, которое вернуло
  разнесение. Величину ловит только детерминированный тест
  с посчитанным вручную ожиданием.
- **Тексты диагностик в `tests/ui/*.stderr` привязаны к версии
  тулчейна.** Обновление тулчейна требует перегенерации
  (`TRYBUILD=overwrite`) и **чтения диффов**: исчезнувшая ошибка
  означает исчезнувшую защиту, а не устаревший тест.
- **Мутационный заслон не покрывает чтение снимка и ранний возврат при
  нарушении инварианта.** Оба мутанта эквивалентны по наблюдаемому
  поведению: снимок является кэшем, а полный пересчёт воспроизводит то
  же нарушение. Обоснование — в описании бида iaam-1fk.18 и рядом со
  списком модулей в `scripts/check-mutants.sh`.
- **Конкурентная запись закрыта транзакцией с немедленным захватом
  и уникальным индексом** `(owner, effective_date, sequence)`. Тест на
  два одновременных запроса есть:
  `concurrent_writers_assign_distinct_sequences_or_report_an_error`
  в `crates/iaam-store/tests/journal.rs` поднимает двух писателей на один
  файл базы и проверяет, что ни один номер не выдан дважды, а при
  коллизии видна ошибка, а не тихая перестановка. Тест проверен
  фальсификацией: вынос чтения `MAX(sequence)` из транзакции вместе со
  снятием уникальности индекса роняет его.
- **Один мьютекс на всё хранилище** сериализует чтения и записи. На
  одного пользователя это не проблема; при втором писателе понадобится
  пул и разделение читателей.
- `cargo-mutants` не порождает мутантов для функций с именем `new`,
  для `is_zero()`, для замыканий в `.map(...).sum()` и для тел
  `else`-ветвей.
- `cargo-mutants` с флагом `--package` гоняет тесты **только этого
  пакета**. Модули оболочки проверяются контрактными тестами из
  `iaam-server`, поэтому `scripts/check-mutants.sh` обязан передавать
  `--test-workspace true`. Без него заслон печатает «выживших нет» для
  кода, который никто не тестировал.
