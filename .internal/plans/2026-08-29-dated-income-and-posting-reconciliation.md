# Датированные факты дохода и сверка запланированных выплат — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** Отчёт доказательно отличает «запланированная выплата получена» от «не получена» и от «проверить нечем», вместо сегодняшнего молчания.

**Architecture:** Новый четвёртый читатель журнала `IncomeLedger` в `LedgerState` копит датированные факты дохода по паре (счёт, инструмент). `CashflowPlan.past` расширяется до вида выплаты. Версионированное правило `PostingMatchV1` сопоставляет план с фактами по одностороннему окну в 21 календарный день, one-to-one, в границах владения. Результат — `MaterialIssue::ScheduledPostingNotReceived` либо новый `ScheduledPostingUnverifiable`.

**Tech Stack:** Rust 2024, workspace из 10 крейтов, `serde`/CBOR для снимков, `thiserror`, `time::Date`, `cargo nextest`, `cargo-mutants`.

**Спека:** `.internal/specs/2026-08-29-dated-income-and-posting-reconciliation-design.md`
**Бид:** `iaam-d8b.12.13` · **Brainstorming:** `iaam-oqmp`

## Global Constraints

- **`make check` зелёный на каждом коммите.** Это `fmt lint arch fixtures deps test doc-test`.
- **`f64` в ядре запрещён** — `make arch` это проверяет. Деньги — `Money`/`CalcMoney`, дроби — `Dec`.
- **`async` в ядре запрещён** — `make arch` это проверяет.
- **Ядро детерминировано:** ни случайных идентификаторов, ни системного времени. Повторная проекция того же журнала обязана дать тот же результат (§3.1, §15.3).
- **Ноль вместо неизвестного запрещён (§4.9).** Отсутствующая величина — `None` или отказ с причиной, никогда не подстановка.
- **Новые `allow`/`ignore`/`todo!` ловит `make diff-lint`.** Не добавлять.
- **Правка файлов политики** (в т.ч. `tests/fixtures/`) требует отдельного коммита с `POLICY_CHANGE_APPROVED=1` и меткой `policy-change` (`scripts/check-diff-lint.sh:80`).
- **Порог покрытия добавленных строк — 90%** (`make diff-coverage BASE=...`).
- **Мутационный заслон** — `make mutants`, порог по каждому модулю.
- Комментарии и сообщения об ошибках — по-русски, в стиле окружающего кода: объясняют **почему**, а не пересказывают код.

## File Structure

| Файл | Ответственность | Задача |
|---|---|---|
| `crates/iaam-core/src/rules/cashflow.rs` | `ScheduledPosting`, `past: Vec<ScheduledPosting>` | 1 |
| `crates/iaam-core/src/projection/income.rs` **(новый)** | `IncomeLedger`, `ReceivedPosting`, `IncomeGap` | 2–4 |
| `crates/iaam-core/src/projection/state.rs` | поле `income` в `LedgerState`, аксессоры | 2 |
| `crates/iaam-core/src/projection/mod.rs` | `pub mod income`, вызов в `fold`, `PROJECTION_VERSION` 4 | 2 |
| `crates/iaam-core/src/rules/posting_match.rs` **(новый)** | `PostingMatchV1`, `PostingMatchVersion`, `MatchOutcome` | 5 |
| `crates/iaam-core/src/rules/mod.rs` | реэкспорт правила | 5 |
| `crates/iaam-core/src/returns/mod.rs` | `MaterialIssue`, `is_defect`, сверка, `AppliedRules` | 6 |
| `crates/iaam-server/src/dto.rs` | DTO обеих проблем | 7 |
| `crates/iaam-server/tests/contract.rs` | контракт | 7 |
| `crates/iaam-core/tests/` | регрессии и свойства | 8 |

---

## Task 1: Вид выплаты доезжает до `past`

**Files:**
- Modify: `crates/iaam-core/src/rules/cashflow.rs:74-81` (объявление `CashflowPlan`), `:226-294` (построение `past`)
- Test: `crates/iaam-core/src/rules/cashflow.rs` (модуль `tests` в конце файла)

**Interfaces:**
- Produces: `ScheduledPosting { pub date: Date, pub kind: PostingKind }`; `CashflowPlan.past: Vec<ScheduledPosting>`.

**Acceptance Criteria:**
- `past` несёт вид выплаты для всех трёх видов: купон, возврат номинала, расчёт по оферте.
- Порядок `past` — по возрастанию даты, при равенстве дат — по виду выплаты; сортировка полная и детерминированная.
- Существующий тест `past_scheduled_dates_are_listed_separately_from_future_postings` обновлён и зелёный.

- [ ] **Step 1: Написать падающий тест**

В модуле `tests` файла `crates/iaam-core/src/rules/cashflow.rs`:

```rust
#[test]
fn past_postings_carry_their_kind_so_reconciliation_can_match_them() {
    // Купон и возврат номинала подтверждаются РАЗНЫМИ событиями журнала:
    // купон приходит `Income`, амортизация — `CorporateAction`. Без вида
    // выплаты сверка искала бы купонный факт под возврат номинала и
    // поднимала ложную тревогу на каждой амортизируемой облигации.
    let plan = plan_with_past_coupon_and_past_principal_return();

    assert_eq!(
        plan.past,
        vec![
            ScheduledPosting {
                date: date!(2026 - 03 - 15),
                kind: PostingKind::Coupon,
            },
            ScheduledPosting {
                date: date!(2026 - 06 - 15),
                kind: PostingKind::PrincipalReturn,
            },
        ]
    );
}
```

Вспомогательную `plan_with_past_coupon_and_past_principal_return()` собрать по образцу существующего теста `past_scheduled_dates_are_listed_separately_from_future_postings` (`cashflow.rs:510`): тот же `CashflowInput`, но `as_of` позже и купонной даты, и даты амортизации.

- [ ] **Step 2: Убедиться, что тест падает**

Run: `cargo nextest run -p iaam-core past_postings_carry_their_kind`
Expected: FAIL — `cannot find struct ScheduledPosting`.

- [ ] **Step 3: Ввести тип и заменить поле**

В `crates/iaam-core/src/rules/cashflow.rs`, рядом с `ExpectedPosting`:

```rust
/// Запланированная выплата, срок которой уже наступил.
///
/// Вид обязателен: купон подтверждается `Income`, возврат номинала —
/// `CorporateAction`, расчёт по оферте — `OfferExercise`. Одна дата без
/// вида не позволяет отличить неполученный купон от неполученного
/// возврата номинала, а искать их надо в разных событиях журнала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScheduledPosting {
    pub date: Date,
    pub kind: PostingKind,
}
```

`PostingKind` уже выводит `PartialEq, Eq` (`cashflow.rs:65`); добавить ему `PartialOrd, Ord`, чтобы сортировка `ScheduledPosting` была полной.

Заменить объявление поля (`cashflow.rs:80`):

```rust
    /// Запланированные выплаты, срок которых не позже `as_of`. Правило
    /// не знает и не может знать, пришли ли деньги; сверку с журналом
    /// делает вызывающая сторона правилом `PostingMatchV1`.
    pub past: Vec<ScheduledPosting>,
```

- [ ] **Step 4: Заполнить вид во всех трёх местах**

`cashflow.rs:231`:

```rust
                past.push(ScheduledPosting {
                    date: period.payment_date,
                    kind: PostingKind::Coupon,
                });
```

`cashflow.rs:253`:

```rust
                past.push(ScheduledPosting {
                    date: principal_return.repayment_date,
                    kind: PostingKind::PrincipalReturn,
                });
```

`cashflow.rs:279`:

```rust
                past.push(ScheduledPosting {
                    date: terms.execution_date,
                    kind: PostingKind::OfferSettlement,
                });
```

Сортировку (`:290`) оставить `past.sort();` — производного `Ord` достаточно, и он сортирует сначала по дате, затем по виду.

- [ ] **Step 5: Починить существующий тест**

`past_scheduled_dates_are_listed_separately_from_future_postings` (`cashflow.rs:510`) и утверждение `assert!(plan.past.is_empty())` (`:434`) обновить под новый тип. `is_empty` менять не нужно.

- [ ] **Step 6: Прогнать тесты**

Run: `cargo nextest run -p iaam-core cashflow`
Expected: PASS.

- [ ] **Step 7: Собрать воркспейс**

Run: `cargo build --workspace`
Expected: успех. Если `past` читают тесты `golden_zero_reinvestment.rs` или `prop_zero_reinvestment.rs` — поправить и их.

- [ ] **Step 8: Коммит**

```bash
git add crates/iaam-core/src/rules/cashflow.rs
git commit -m "feat(core): вид выплаты доезжает до past (iaam-d8b.12.13)"
```

---

## Task 2: `IncomeLedger` и купонные факты

**Files:**
- Create: `crates/iaam-core/src/projection/income.rs`
- Modify: `crates/iaam-core/src/projection/mod.rs:10-15` (`pub mod`), `:35` (`PROJECTION_VERSION`), `:326-340` (`fold`); `crates/iaam-core/src/projection/state.rs:114-165`
- Test: `crates/iaam-core/src/projection/income.rs` (модуль `tests`)

**Interfaces:**
- Consumes: `ScheduledPosting`, `PostingKind` из Task 1.
- Produces:
  - `IncomeLedger::default()`, `IncomeLedger::apply(&mut self, event: &Event) -> Result<(), IncomeError>`
  - `IncomeLedger::postings(&self, key: &LotKey) -> &[ReceivedPosting]`
  - `IncomeLedger::gap(&self, key: &LotKey) -> Option<IncomeGap>`
  - `ReceivedPosting { pub event: EventId, pub date: Date, pub amount: Money, pub kind: PostingKind }`
  - `enum IncomeGap { IncomeKindUnknown, PaymentDateUnknown }`
  - `LedgerState::income(&self) -> &IncomeLedger`

**Acceptance Criteria:**
- `Income { instrument: Some, kind: Some(Coupon) }` с `cash_posted` даёт ровно один `ReceivedPosting` вида `Coupon` под ключом (счёт, инструмент).
- Дата берётся из `cash_posted`, иначе `paid`; `effective_date()` не используется.
- `Income` без обеих дат факта не создаёт и ставит `IncomeGap::PaymentDateUnknown`.
- `Income { kind: None }` факта не создаёт и ставит `IncomeGap::IncomeKindUnknown`.
- `Income { instrument: None }` игнорируется молча.
- `Dividend` и `DepositInterest` фактов не создают: в графике облигации их нет.
- `PROJECTION_VERSION == 4`.

- [ ] **Step 1: Написать падающие тесты**

Создать `crates/iaam-core/src/projection/income.rs` c модулем `tests` (реализацию — на следующем шаге):

```rust
#[test]
fn a_coupon_with_a_cash_posted_date_becomes_one_dated_fact() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&coupon(account, instrument, date!(2026 - 03 - 18), 500))
        .expect("купон с датой зачисления принимается");

    let key = LotKey { account, instrument };
    let postings = ledger.postings(&key);
    assert_eq!(postings.len(), 1);
    assert_eq!(postings[0].date, date!(2026 - 03 - 18));
    assert_eq!(postings[0].kind, PostingKind::Coupon);
    assert_eq!(ledger.gap(&key), None);
}

#[test]
fn a_payment_without_a_cash_posted_or_paid_date_cannot_be_dated() {
    // `validate_structure` для `Income` требует лишь одну положительную
    // денежную ногу и дат не требует вовсе (`event/mod.rs:197`).
    // Подставить сюда `settled` или `trade` значило бы молча сдвинуть
    // факт: это не даты получения денег (§4.9).
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&coupon_without_payment_date(account, instrument, 500))
        .expect("событие принимается, но датированным фактом не становится");

    let key = LotKey { account, instrument };
    assert!(ledger.postings(&key).is_empty());
    assert_eq!(ledger.gap(&key), Some(IncomeGap::PaymentDateUnknown));
}

#[test]
fn an_income_of_unknown_kind_blocks_reconciliation_rather_than_being_guessed() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&income_of_unknown_kind(account, instrument, date!(2026 - 03 - 18), 500))
        .expect("событие принимается");

    let key = LotKey { account, instrument };
    assert!(ledger.postings(&key).is_empty());
    assert_eq!(ledger.gap(&key), Some(IncomeGap::IncomeKindUnknown));
}

#[test]
fn a_dividend_is_not_a_scheduled_bond_posting() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&dividend(account, instrument, date!(2026 - 03 - 18), 500))
        .expect("дивиденд принимается");

    let key = LotKey { account, instrument };
    assert!(ledger.postings(&key).is_empty());
    assert_eq!(ledger.gap(&key), None);
}

#[test]
fn income_without_an_instrument_has_nothing_to_reconcile_against() {
    let account = AccountId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&income_without_instrument(account, date!(2026 - 03 - 18), 500))
        .expect("принимается");

    assert!(ledger.is_empty());
}
```

Конструкторы событий (`coupon`, `dividend`, …) написать по образцу `crates/iaam-core/src/projection/balances.rs:140` и далее — там уже собирают `Event` с `SCHEMA_VERSION`, `Confidence`, `Relation`, ногами и `EventDates`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo nextest run -p iaam-core income::tests`
Expected: FAIL — модуль не объявлен / типов нет.

- [ ] **Step 3: Реализовать `IncomeLedger`**

В начало `crates/iaam-core/src/projection/income.rs`:

```rust
//! Датированные факты дохода (§7.2).
//!
//! Четвёртый независимый читатель журнала. Он не берёт ничего у лотов
//! намеренно: `received_to_date` отвечает на другой вопрос — сколько
//! получено пожизненно, — и делится по лотам пропорционально. Сверка
//! спрашивает иное: пришла ли конкретная запланированная выплата.
//! Один ряд на пару (счёт, инструмент) — ровно та гранулярность.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::event::Event;
use crate::event::kind::{EventKind, IncomeKind};
use crate::ids::EventId;
use crate::money::Money;
use crate::projection::lots::LotKey;
use crate::rules::PostingKind;

/// Факт дохода с датой и видом.
///
/// Хранится `EventId`, а не событие целиком: ссылка на журнал плюс
/// необходимые факты, а не копия журнала внутри проекции.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivedPosting {
    pub event: EventId,
    pub date: Date,
    pub amount: Money,
    pub kind: PostingKind,
}

/// Почему сверка по паре (счёт, инструмент) недоказуема.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncomeGap {
    /// Есть выплата, вид которой не установлен: на график её не положить.
    IncomeKindUnknown,
    /// Есть выплата без даты зачисления и без даты выплаты.
    PaymentDateUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IncomeError {
    #[error(transparent)]
    Money(#[from] crate::money::MoneyError),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomeLedger {
    entries: BTreeMap<LotKey, Entry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    postings: Vec<ReceivedPosting>,
    gap: Option<IncomeGap>,
}

impl IncomeLedger {
    #[must_use]
    pub fn postings(&self, key: &LotKey) -> &[ReceivedPosting] {
        self.entries
            .get(key)
            .map_or(&[][..], |entry| entry.postings.as_slice())
    }

    #[must_use]
    pub fn gap(&self, key: &LotKey) -> Option<IncomeGap> {
        self.entries.get(key).and_then(|entry| entry.gap)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Дата получения денег.
    ///
    /// `cash_posted`, иначе `paid`. Цепочка `EventDates::effective_date`
    /// здесь не годится: она начинается с `settled` и падает до `trade`,
    /// а это не даты получения денег — подстановка молча сдвинула бы
    /// факт (§4.9).
    fn payment_date(event: &Event) -> Option<Date> {
        event
            .dates
            .cash_posted
            .map(|d| d.0)
            .or_else(|| event.dates.paid.map(|d| d.0))
    }

    fn record(&mut self, key: LotKey, posting: ReceivedPosting) {
        self.entries.entry(key).or_default().postings.push(posting);
    }

    /// Пометить пару недоказуемой. Первая причина побеждает: она
    /// возникла раньше в журнале, и перезаписывать её более поздней
    /// значило бы менять диагноз от порядка чтения.
    fn mark(&mut self, key: LotKey, gap: IncomeGap) {
        let entry = self.entries.entry(key).or_default();
        if entry.gap.is_none() {
            entry.gap = Some(gap);
        }
    }

    pub fn apply(&mut self, event: &Event) -> Result<(), IncomeError> {
        match &event.kind {
            EventKind::Income {
                instrument: Some(instrument),
                gross,
                kind,
            } => {
                let key = LotKey {
                    account: event.account,
                    instrument: *instrument,
                };
                match kind {
                    Some(IncomeKind::Coupon) => {
                        let Some(date) = Self::payment_date(event) else {
                            self.mark(key, IncomeGap::PaymentDateUnknown);
                            return Ok(());
                        };
                        self.record(
                            key,
                            ReceivedPosting {
                                event: event.id,
                                date,
                                amount: *gross,
                                kind: PostingKind::Coupon,
                            },
                        );
                    }
                    // Дивиденд и процент по вкладу в графике облигации
                    // не значатся: подтверждать ими нечего.
                    Some(IncomeKind::Dividend | IncomeKind::DepositInterest) => {}
                    None => self.mark(key, IncomeGap::IncomeKindUnknown),
                }
                Ok(())
            }
            // Без инструмента сверять не с чем.
            EventKind::Income { instrument: None, .. } => Ok(()),
            _ => Ok(()),
        }
    }
}
```

`_ => Ok(())` здесь временный: задачи 3 и 4 заменяют его явными членами. Если `make lint` требует исчерпывающего `match` — сразу перечислить остальные члены `EventKind` с `Ok(())`, как это сделано в `lots.rs:544-550`.

- [ ] **Step 4: Подключить к состоянию и проекции**

`crates/iaam-core/src/projection/mod.rs`, к списку модулей (`:10`):

```rust
pub mod income;
```

`crates/iaam-core/src/projection/state.rs`, поле в `LedgerState` (`:114`) и аксессоры:

```rust
    income: IncomeLedger,
```

```rust
    #[must_use]
    pub const fn income(&self) -> &IncomeLedger {
        &self.income
    }

    pub(super) const fn income_mut(&mut self) -> &mut IncomeLedger {
        &mut self.income
    }
```

В `LedgerState::new` (`:124`) добавить `income: IncomeLedger::default(),`.

`crates/iaam-core/src/projection/mod.rs`, в `fold` (`:333`), после трёх существующих читателей:

```rust
        state.income_mut().apply(event)?;
```

Добавить в `ProjectionError` член `#[error(transparent)] Income(#[from] IncomeError)`.

- [ ] **Step 5: Поднять версию проекции**

`crates/iaam-core/src/projection/mod.rs:35`:

```rust
pub const PROJECTION_VERSION: u32 = 4;
```

Обратная совместимость снимка **не нужна и вредна**: снимок версии 3 не содержит датированных фактов и выглядел бы как позиция без единой полученной выплаты. `advance` отвергнет его (`:278`), а `recompute_is_worth_it` (`crates/iaam-app/src/scenarios/reports.rs:527`) даст полный пересчёт журнала. `#[serde(default)]` не добавлять.

- [ ] **Step 6: Прогнать тесты**

Run: `cargo nextest run -p iaam-core income`
Expected: PASS, пять тестов.

- [ ] **Step 7: Проверить, что старый снимок пересчитывается, а не падает**

Run: `cargo nextest run -p iaam-app snapshot`
Expected: PASS. Если тест закрепляет `PROJECTION_VERSION == 3` — обновить его и **сохранить** утверждение, что несовпадение версии ведёт к пересчёту, а не к ошибке.

- [ ] **Step 8: Коммит**

```bash
git add crates/iaam-core/src/projection/income.rs \
        crates/iaam-core/src/projection/mod.rs \
        crates/iaam-core/src/projection/state.rs
git commit -m "feat(core): датированные купонные факты в проекции (iaam-d8b.12.13)"
```

---

## Task 3: Возврат номинала как датированный факт

**Files:**
- Modify: `crates/iaam-core/src/projection/income.rs`
- Test: там же

**Interfaces:**
- Consumes: `IncomeLedger::apply` из Task 2.
- Produces: факты вида `PostingKind::PrincipalReturn`.

**Acceptance Criteria:**
- `CorporateAction::PartialRedemption` с датой зачисления даёт `ReceivedPosting { kind: PrincipalReturn, amount: compensation }`.
- `CorporateAction::Redemption` даёт то же.
- Сумма — `compensation` (фактически поступившие деньги), а не `principal_returned_per_unit * quantity`: они расходятся на удержанный налог (`event/corporate_action.rs:37-40`).
- `Conversion` факта не создаёт: замещение денег не приносит.
- Корпоративное действие без даты зачисления ставит `IncomeGap::PaymentDateUnknown`.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn an_amortisation_payment_is_a_dated_principal_return() {
    // Амортизация приходит `CorporateAction`, а не `Income`. Искать её
    // среди купонных фактов значило бы поднимать ложную тревогу на
    // каждой амортизируемой облигации.
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&partial_redemption(
            account,
            instrument,
            date!(2026 - 06 - 18),
            300,
        ))
        .expect("амортизация принимается");

    let key = LotKey { account, instrument };
    let postings = ledger.postings(&key);
    assert_eq!(postings.len(), 1);
    assert_eq!(postings[0].kind, PostingKind::PrincipalReturn);
    assert_eq!(postings[0].date, date!(2026 - 06 - 18));
    assert_eq!(postings[0].amount, rub(300));
}

#[test]
fn the_recorded_amount_is_the_money_received_not_the_principal_declared() {
    // `compensation` может быть меньше возвращённого номинала — на
    // удержанный налог, например (`event/corporate_action.rs:37-40`).
    // Сверка отвечает на вопрос «пришли ли деньги», поэтому берёт
    // деньги, а не объявленный номинал.
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&partial_redemption_with_withheld_tax(
            account,
            instrument,
            date!(2026 - 06 - 18),
            /* principal_per_unit */ 400,
            /* compensation */ 348,
        ))
        .expect("принимается");

    let key = LotKey { account, instrument };
    assert_eq!(ledger.postings(&key)[0].amount, rub(348));
}

#[test]
fn a_full_redemption_is_a_dated_principal_return_too() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&redemption(account, instrument, date!(2026 - 09 - 20), 1000))
        .expect("погашение принимается");

    let key = LotKey { account, instrument };
    assert_eq!(ledger.postings(&key)[0].kind, PostingKind::PrincipalReturn);
}

#[test]
fn a_conversion_brings_no_money_and_therefore_no_fact() {
    let account = AccountId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&conversion(account, date!(2026 - 06 - 18)))
        .expect("замещение принимается");

    assert!(ledger.is_empty());
}

#[test]
fn a_corporate_action_without_a_payment_date_cannot_be_dated() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&partial_redemption_without_payment_date(
            account, instrument, 300,
        ))
        .expect("принимается");

    let key = LotKey { account, instrument };
    assert!(ledger.postings(&key).is_empty());
    assert_eq!(ledger.gap(&key), Some(IncomeGap::PaymentDateUnknown));
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo nextest run -p iaam-core income::tests`
Expected: FAIL — пять новых тестов не проходят, ряд пуст.

- [ ] **Step 3: Реализовать**

В `IncomeLedger::apply` добавить член перед `_ =>`:

```rust
            EventKind::CorporateAction { action } => self.apply_corporate_action(event, action),
```

И метод:

```rust
    /// Возврат номинала приносит деньги двумя способами: амортизацией
    /// (позиция остаётся) и погашением (позиция уходит). Замещение денег
    /// не приносит и факта не создаёт.
    ///
    /// Записывается `compensation` — фактически поступившие деньги, а не
    /// объявленный возвращённый номинал: они расходятся на удержанный
    /// налог, а сверка отвечает на вопрос «пришли ли деньги».
    fn apply_corporate_action(
        &mut self,
        event: &Event,
        action: &CorporateAction,
    ) -> Result<(), IncomeError> {
        let (instrument, compensation) = match action {
            CorporateAction::PartialRedemption {
                instrument,
                compensation,
                ..
            }
            | CorporateAction::Redemption {
                instrument,
                compensation,
                ..
            } => (*instrument, *compensation),
            CorporateAction::Conversion { .. } => return Ok(()),
        };
        let key = LotKey {
            account: event.account,
            instrument,
        };
        let Some(date) = Self::payment_date(event) else {
            self.mark(key, IncomeGap::PaymentDateUnknown);
            return Ok(());
        };
        self.record(
            key,
            ReceivedPosting {
                event: event.id,
                date,
                amount: compensation,
                kind: PostingKind::PrincipalReturn,
            },
        );
        Ok(())
    }
```

Если у `CorporateAction` есть члены помимо трёх — перечислить их явно с `return Ok(())` и комментарием почему; `_ =>` в диспетчере не оставлять (иначе новый член корпоративного действия молча потеряется).

- [ ] **Step 4: Прогнать тесты**

Run: `cargo nextest run -p iaam-core income`
Expected: PASS, десять тестов.

- [ ] **Step 5: Коммит**

```bash
git add crates/iaam-core/src/projection/income.rs
git commit -m "feat(core): возврат номинала как датированный факт (iaam-d8b.12.13)"
```

---

## Task 4: Расчёт по оферте как датированный факт

**Files:**
- Modify: `crates/iaam-core/src/projection/income.rs`
- Test: там же

**Interfaces:**
- Produces: факты вида `PostingKind::OfferSettlement`; исчерпывающий диспетчер `EventKind` в `IncomeLedger::apply`.

**Acceptance Criteria:**
- `OfferExerciseAction::Settled` даёт `ReceivedPosting { kind: OfferSettlement }`.
- Сумма — `gross` за вычетом `fee`, плюс `accrued_interest`, если он есть; ни одна неизвестная величина не подставляется нулём.
- `Submitted` и `Cancelled` фактов не создают: денег они не двигают.
- Диспетчер по `EventKind` исчерпывающий, без `_ =>`.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn an_offer_settlement_is_a_dated_fact() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&offer_settled(
            account,
            instrument,
            date!(2026 - 07 - 10),
            /* gross */ 1_000,
            /* fee */ Some(30),
            /* accrued_interest */ Some(25),
        ))
        .expect("выкуп принимается");

    let key = LotKey { account, instrument };
    let postings = ledger.postings(&key);
    assert_eq!(postings.len(), 1);
    assert_eq!(postings[0].kind, PostingKind::OfferSettlement);
    // 1000 - 30 + 25
    assert_eq!(postings[0].amount, rub(995));
}

#[test]
fn a_submitted_offer_moves_no_money_and_creates_no_fact() {
    let account = AccountId::new_random();
    let mut ledger = IncomeLedger::default();

    ledger
        .apply(&offer_submitted(account, date!(2026 - 07 - 01)))
        .expect("заявка принимается");

    assert!(ledger.is_empty());
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo nextest run -p iaam-core income::tests offer`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

Добавить член и метод, повторив арифметику `lots.rs:746-750`, чтобы сумма расчёта считалась в проекции одинаково:

```rust
            EventKind::OfferExercise { action } => self.apply_offer_exercise(event, action),
```

```rust
    /// Заявка и её отзыв денег не двигают: бумага остаётся у владельца,
    /// пока выкуп не состоялся. Их состояние ведёт `OfferBook`.
    fn apply_offer_exercise(
        &mut self,
        event: &Event,
        action: &OfferExerciseAction,
    ) -> Result<(), IncomeError> {
        let OfferExerciseAction::Settled {
            instrument,
            gross,
            fee,
            accrued_interest,
            ..
        } = action
        else {
            return Ok(());
        };
        let key = LotKey {
            account: event.account,
            instrument: *instrument,
        };
        let Some(date) = Self::payment_date(event) else {
            self.mark(key, IncomeGap::PaymentDateUnknown);
            return Ok(());
        };
        let mut amount = *gross;
        if let Some(f) = fee {
            amount = amount.try_sub(*f)?;
        }
        if let Some(interest) = accrued_interest {
            amount = amount.try_add(*interest)?;
        }
        self.record(
            key,
            ReceivedPosting {
                event: event.id,
                date,
                amount,
                kind: PostingKind::OfferSettlement,
            },
        );
        Ok(())
    }
```

- [ ] **Step 4: Сделать диспетчер исчерпывающим**

Заменить `_ => Ok(())` явным перечислением остальных членов `EventKind`, по образцу `lots.rs:544-550`:

```rust
            // Эти события дохода по инструменту не приносят: денежные
            // движения границы контура ведёт `FlowLog`, остатки —
            // `Balances`, лоты — `LotBook`. Молчаливого `_` здесь быть
            // не должно: новый член `EventKind` обязан заставить
            // компилятор задать вопрос, что с ним делает сверка.
            EventKind::CashIn { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Fee { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::OpeningPosition { .. }
            | EventKind::Trade { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. } => Ok(()),
```

Точный список членов взять из `crates/iaam-core/src/projection/lots.rs:520-563`.

- [ ] **Step 5: Прогнать тесты и линт**

Run: `cargo nextest run -p iaam-core income && make lint`
Expected: PASS.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/src/projection/income.rs
git commit -m "feat(core): расчёт по оферте как датированный факт (iaam-d8b.12.13)"
```

---

## Task 5: Правило сопоставления `PostingMatchV1`

**Files:**
- Create: `crates/iaam-core/src/rules/posting_match.rs`
- Modify: `crates/iaam-core/src/rules/mod.rs` (объявление модуля и реэкспорт)
- Test: `crates/iaam-core/src/rules/posting_match.rs` (модуль `tests`)

**Interfaces:**
- Consumes: `ScheduledPosting` (Task 1), `ReceivedPosting` (Task 2).
- Produces:
  - `PostingMatchVersion(pub u16)`
  - `PostingMatchV1 { window_days: u16 }`, `PostingMatchV1::new() -> Self` с `window_days: 21`
  - `PostingMatchV1::unreceived(&self, scheduled: &[ScheduledPosting], facts: &[ReceivedPosting]) -> Vec<ScheduledPosting>`
  - `posting_match_rule() -> (PostingMatchVersion, PostingMatchV1)` в `returns/mod.rs` (Task 6)

**Acceptance Criteria:**
- Факт закрывает выплату, если совпал вид и `scheduled.date <= fact.date <= scheduled.date + window_days`.
- Окно одностороннее: факт раньше плановой даты не закрывает её.
- Граница включающая: `scheduled + 21` закрывает, `scheduled + 22` — нет.
- One-to-one: один факт закрывает не более одной плановой выплаты.
- Результат не зависит от порядка входных срезов.
- `window_days == 21`.

- [ ] **Step 1: Написать падающие тесты**

```rust
fn coupon(day: u8) -> ScheduledPosting { /* 2026-03-<day>, PostingKind::Coupon */ }
fn fact(day: u8) -> ReceivedPosting { /* 2026-03-<day>, PostingKind::Coupon */ }

#[test]
fn a_payment_inside_the_window_is_received() {
    let rule = PostingMatchV1::new();
    assert!(rule.unreceived(&[coupon(15)], &[fact(18)]).is_empty());
}

#[test]
fn the_window_edge_is_inclusive_and_the_day_after_is_not() {
    // 21 календарный день — это 10 рабочих дней депозитарной цепочки
    // (ст. 8.7 ФЗ 39-ФЗ), растянутые через праздничный период.
    let rule = PostingMatchV1::new();
    assert!(rule.unreceived(&[coupon(1)], &[fact(22)]).is_empty());
    assert_eq!(rule.unreceived(&[coupon(1)], &[fact(23)]), vec![coupon(1)]);
}

#[test]
fn money_never_arrives_before_the_schedule_says_it_should() {
    // Окно одностороннее. Факт раньше плановой даты — это другая
    // выплата, а не ранний приход этой.
    let rule = PostingMatchV1::new();
    assert_eq!(rule.unreceived(&[coupon(15)], &[fact(14)]), vec![coupon(15)]);
}

#[test]
fn a_coupon_fact_does_not_confirm_a_principal_return() {
    let rule = PostingMatchV1::new();
    let principal = ScheduledPosting {
        date: date!(2026 - 03 - 15),
        kind: PostingKind::PrincipalReturn,
    };
    assert_eq!(rule.unreceived(&[principal], &[fact(18)]), vec![principal]);
}

#[test]
fn one_fact_cannot_close_two_scheduled_payments() {
    // Иначе пропуск в плотном графике исчез бы: один пришедший купон
    // закрыл бы и себя, и соседа.
    let rule = PostingMatchV1::new();
    assert_eq!(
        rule.unreceived(&[coupon(1), coupon(10)], &[fact(11)]),
        vec![coupon(10)]
    );
}

#[test]
fn the_verdict_does_not_depend_on_the_order_of_the_inputs() {
    let rule = PostingMatchV1::new();
    let forward = rule.unreceived(&[coupon(1), coupon(10)], &[fact(2), fact(11)]);
    let reversed = rule.unreceived(&[coupon(10), coupon(1)], &[fact(11), fact(2)]);
    assert_eq!(forward, reversed);
    assert!(forward.is_empty());
}
```

Пояснение к `one_fact_cannot_close_two_scheduled_payments`: факт 11-го числа попадает в окно и купона 1-го (1..22), и купона 10-го (10..31). Жадность по возрастанию отдаёт его **раннему** — купону 1-го; купон 10-го остаётся неподтверждённым.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo nextest run -p iaam-core posting_match`
Expected: FAIL — модуля нет.

- [ ] **Step 3: Реализовать правило**

```rust
//! Правило сопоставления запланированной выплаты с фактом (§7.2).

use serde::{Deserialize, Serialize};
use time::Duration;

use crate::projection::income::ReceivedPosting;
use crate::rules::cashflow::ScheduledPosting;

/// Версия правила сопоставления. Хранение датированных фактов
/// версионируется `PROJECTION_VERSION`; здесь версионируется
/// **сопоставление**: окно, односторонность и жадность.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostingMatchVersion(pub u16);

/// Первая версия правила.
///
/// Окно — 21 календарный день. Депозитарная цепочка занимает около
/// десяти рабочих дней: эмитент перечисляет в НРД до двух рабочих дней,
/// НРД депозитарию брокера — на следующий рабочий день, а депозитарий
/// конечному владельцу — не позднее семи рабочих дней после дня
/// получения (ст. 8.7 ФЗ 39-ФЗ, «иные депоненты»). Десять рабочих дней
/// через праздничный период дают двадцать один календарный.
///
/// Окно задано в календарных днях, а не в рабочих, потому что
/// производственного календаря в ядре нет вовсе, а заводить его — вносить
/// внешний ежегодно публикуемый источник. Правило версионировано, чтобы
/// решение можно было пересмотреть.
///
/// Граница применимости: самый плотный реальный график — ежемесячный
/// купон, около тридцати дней. Двадцать один меньше тридцати, поэтому
/// окно до соседней выплаты не дотягивается, но запас всего девять дней.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostingMatchV1 {
    window_days: u16,
}

impl Default for PostingMatchV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl PostingMatchV1 {
    #[must_use]
    pub const fn new() -> Self {
        Self { window_days: 21 }
    }

    #[must_use]
    pub const fn window_days(self) -> u16 {
        self.window_days
    }

    /// Запланированные выплаты, под которые факта не нашлось.
    ///
    /// Сопоставление жадное по возрастанию даты и **one-to-one**: факт
    /// расходуется и второй раз не используется. Иначе один пришедший
    /// купон закрыл бы и себя, и пропущенного соседа.
    ///
    /// Оба среза сортируются внутри, поэтому результат не зависит от
    /// порядка событий в журнале (§15.3).
    #[must_use]
    pub fn unreceived(
        &self,
        scheduled: &[ScheduledPosting],
        facts: &[ReceivedPosting],
    ) -> Vec<ScheduledPosting> {
        let mut plan = scheduled.to_vec();
        plan.sort();

        let mut available: Vec<&ReceivedPosting> = facts.iter().collect();
        available.sort_by_key(|fact| (fact.date, fact.event));

        let mut used = vec![false; available.len()];
        let mut missing = Vec::new();
        let window = Duration::days(i64::from(self.window_days));

        for expected in plan {
            let matched = available.iter().enumerate().position(|(index, fact)| {
                !used[index]
                    && fact.kind == expected.kind
                    && fact.date >= expected.date
                    && fact.date <= expected.date + window
            });
            match matched {
                Some(index) => used[index] = true,
                None => missing.push(expected),
            }
        }
        missing
    }
}
```

Если `time::Duration` в ядре нежелателен — сложение дат сделать через `Date::saturating_add`; выбор зафиксировать комментарием.

- [ ] **Step 4: Объявить модуль**

`crates/iaam-core/src/rules/mod.rs`: добавить `pub mod posting_match;` и реэкспорт `PostingMatchV1, PostingMatchVersion` рядом с существующим реэкспортом `CashflowProjectionV1` (`rules/mod.rs:25`).

- [ ] **Step 5: Прогнать тесты**

Run: `cargo nextest run -p iaam-core posting_match`
Expected: PASS, шесть тестов.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/src/rules/posting_match.rs crates/iaam-core/src/rules/mod.rs
git commit -m "feat(core): правило сопоставления выплат PostingMatchV1 (iaam-d8b.12.13)"
```

---

## Task 6: Сверка в отчёте и обе проблемы

**Files:**
- Modify: `crates/iaam-core/src/returns/mod.rs` — `MaterialIssue` (`:234-273`), `is_defect` (`:294-306`), `AppliedRules` (`:457-475`), `bond_scenario` (`:1378-1450`), сборка отчёта (`:1210-1222`)
- Test: `crates/iaam-core/src/returns/mod.rs` (модуль `tests`)

**Interfaces:**
- Consumes: `PostingMatchV1::unreceived` (Task 5), `IncomeLedger::postings`/`gap` (Task 2), `CashflowPlan.past` (Task 1).
- Produces:
  - `MaterialIssue::ScheduledPostingNotReceived { account, instrument, date, kind }`
  - `MaterialIssue::ScheduledPostingUnverifiable { account, instrument, reason }`
  - `enum UnverifiableReason { AcquisitionDateUnknown, IncomeKindUnknown, PaymentDateUnknown, HistoryStartsAfterSchedule }`
  - `AppliedRules.posting_match: PostingMatchVersion`

**Acceptance Criteria:**
- Пропущенный купон даёт ровно одну `ScheduledPostingNotReceived` с верными счётом, инструментом, датой и видом.
- Проверяются только выплаты не раньше минимальной `acquired` среди текущих лотов пары.
- Выплата, прошедшая границу владения, но с датой раньше начала журнала, даёт `Unverifiable { HistoryStartsAfterSchedule }`, а не `NotReceived`.
- `is_defect`: `HistoryStartsAfterSchedule` → `false`, остальные три причины → `true`, `NotReceived` → `true`.
- `AppliedRules` несёт `posting_match`, и версия попадает в `inputs_hash`.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn a_missing_coupon_is_named_with_its_account_instrument_date_and_kind() {
    let report = report_for_bond_with_one_missing_coupon();
    let issues: Vec<_> = report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| matches!(issue, MaterialIssue::ScheduledPostingNotReceived { .. }))
        .collect();

    assert_eq!(issues.len(), 1);
    assert!(matches!(
        issues[0],
        MaterialIssue::ScheduledPostingNotReceived {
            date,
            kind: PostingKind::Coupon,
            ..
        } if *date == date!(2026 - 06 - 15)
    ));
}

#[test]
fn a_coupon_scheduled_before_the_bond_was_bought_is_not_owed_to_anyone() {
    // `CashflowPlan.past` строится от графика выпуска и истории владения
    // не знает. Границу накладывает сверка.
    let report = report_for_bond_bought_after_two_coupons_had_passed();
    assert!(!has_issue::<_>(&report, |issue| matches!(
        issue,
        MaterialIssue::ScheduledPostingNotReceived { .. }
    )));
}

#[test]
fn a_restored_history_reports_that_it_cannot_verify_rather_than_crying_wolf() {
    // OpeningPosition, записанное сегодня с заявленной датой сделки
    // пятилетней давности: границу владения купоны проходят, но фактов
    // под них в журнале нет и быть не может.
    let report = report_for_position_opened_with_a_five_year_old_trade_date();

    assert!(!has_issue(&report, |issue| matches!(
        issue,
        MaterialIssue::ScheduledPostingNotReceived { .. }
    )));
    assert!(has_issue(&report, |issue| matches!(
        issue,
        MaterialIssue::ScheduledPostingUnverifiable {
            reason: UnverifiableReason::HistoryStartsAfterSchedule,
            ..
        }
    )));
}

#[test]
fn a_purchase_without_a_trade_date_leaves_the_ownership_bound_undrawable() {
    // `Lot.acquired` — `Option<TradeDate>` (`rules/lot_disposal.rs:38`),
    // и схема отсутствие даты допускает (§4.9). Такая покупка НЕ метит
    // счёт как restored, поэтому без нового варианта отчёт остался бы
    // Complete при недоказанной сверке.
    let report = report_for_bond_bought_without_a_trade_date();

    assert!(has_issue(&report, |issue| matches!(
        issue,
        MaterialIssue::ScheduledPostingUnverifiable {
            reason: UnverifiableReason::AcquisitionDateUnknown,
            ..
        }
    )));
    assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
}

#[test]
fn the_history_horizon_is_reported_but_does_not_make_the_answer_incomplete() {
    // Зеркалит HistoryStartsAt: факт о периоде, а не дефект.
    assert!(!MaterialIssue::ScheduledPostingUnverifiable {
        account: AccountId::new_random(),
        instrument: InstrumentId::new_random(),
        reason: UnverifiableReason::HistoryStartsAfterSchedule,
    }
    .is_defect());
}

#[test]
fn the_other_unverifiable_reasons_are_defects_because_loading_facts_fixes_them() {
    for reason in [
        UnverifiableReason::AcquisitionDateUnknown,
        UnverifiableReason::IncomeKindUnknown,
        UnverifiableReason::PaymentDateUnknown,
    ] {
        assert!(
            MaterialIssue::ScheduledPostingUnverifiable {
                account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                reason,
            }
            .is_defect(),
            "{reason:?} чинится дозагрузкой фактов и потому дефект"
        );
    }
}

#[test]
fn two_accounts_holding_the_same_bond_give_two_distinguishable_issues() {
    let report = report_for_the_same_bond_missing_a_coupon_on_two_accounts();
    let accounts: BTreeSet<_> = report
        .data_quality
        .material_issues
        .iter()
        .filter_map(|issue| match issue {
            MaterialIssue::ScheduledPostingNotReceived { account, .. } => Some(*account),
            _ => None,
        })
        .collect();
    assert_eq!(accounts.len(), 2);
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo nextest run -p iaam-core scheduled_posting`
Expected: FAIL — вариантов нет.

- [ ] **Step 3: Расширить `MaterialIssue`**

```rust
    /// Запланированная выплата не подтверждена датированным фактом
    /// дохода.
    ///
    /// Счёт обязателен: одна бумага на двух счетах иначе даёт две
    /// неразличимые проблемы. Вид выплаты обязателен: «не пришёл купон»
    /// и «не пришёл возврат номинала» требуют от владельца разного.
    ScheduledPostingNotReceived {
        account: AccountId,
        instrument: InstrumentId,
        date: Date,
        kind: PostingKind,
    },
    /// Сверку запланированных выплат провести нечем.
    ///
    /// Отдельный член, а не молчание: молчание выглядело бы как
    /// «проверили, всё пришло», и воспроизвело бы ровно тот дефект,
    /// который эта сверка устраняет.
    ScheduledPostingUnverifiable {
        account: AccountId,
        instrument: InstrumentId,
        reason: UnverifiableReason,
    },
```

```rust
/// Почему сверка запланированных выплат недоказуема.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnverifiableReason {
    /// Границу владения провести нечем: у лота нет даты приобретения.
    AcquisitionDateUnknown,
    /// Есть выплата, вид которой не установлен.
    IncomeKindUnknown,
    /// Есть выплата без даты зачисления и без даты выплаты.
    PaymentDateUnknown,
    /// Выплата прошла границу владения, но её дата раньше начала
    /// журнала: фактов под неё нет и быть не может.
    HistoryStartsAfterSchedule,
}

impl UnverifiableReason {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AcquisitionDateUnknown => "acquisition_date_unknown",
            Self::IncomeKindUnknown => "income_kind_unknown",
            Self::PaymentDateUnknown => "payment_date_unknown",
            Self::HistoryStartsAfterSchedule => "history_starts_after_schedule",
        }
    }
}
```

- [ ] **Step 4: Расширить `is_defect`**

```rust
    #[must_use]
    pub const fn is_defect(&self) -> bool {
        match self {
            Self::HistoryStartsAt { .. } | Self::NoIndependentSource { .. } => false,
            // Горизонт журнала — факт о периоде, а не дефект: он зеркалит
            // `HistoryStartsAt`. Остальные три причины чинятся
            // дозагрузкой фактов и потому дефект.
            Self::ScheduledPostingUnverifiable { reason, .. } => !matches!(
                reason,
                UnverifiableReason::HistoryStartsAfterSchedule
            ),
            Self::AccruedInterestMismatch { .. }
            | Self::RestoredWithoutBasis { .. }
            | Self::NegativeCash { .. }
            | Self::Discrepancy { .. }
            | Self::UnsupportedFinancing { .. }
            | Self::OfferWindowUnresolved { .. }
            | Self::ScheduledPostingNotReceived { .. } => true,
        }
    }
```

- [ ] **Step 5: Провести сверку в сценарии облигации**

В `bond_scenario` (`returns/mod.rs:1378`) `plan` уже построен, а `lots: Option<&InstrumentLots>` уже под рукой. Добавить в `BondScenarioInputs` поле `income: &'a IncomeLedger` и `history_starts: Option<Date>` (из `state.coverage().first_event()`), затем на ветке `Ok(plan)` вызвать:

```rust
/// Сверяет запланированные выплаты с датированными фактами.
///
/// Границу владения накладывает сверка, а не правило потока: правило
/// строит поток от графика выпуска и истории владения не знает.
fn reconcile_past_postings(
    key: LotKey,
    plan: &CashflowPlan,
    lots: Option<&InstrumentLots>,
    income: &IncomeLedger,
    history_starts: Option<Date>,
    rule: &PostingMatchV1,
) -> Vec<MaterialIssue> {
    let unverifiable = |reason| {
        vec![MaterialIssue::ScheduledPostingUnverifiable {
            account: key.account,
            instrument: key.instrument,
            reason,
        }]
    };

    if let Some(gap) = income.gap(&key) {
        return unverifiable(match gap {
            IncomeGap::IncomeKindUnknown => UnverifiableReason::IncomeKindUnknown,
            IncomeGap::PaymentDateUnknown => UnverifiableReason::PaymentDateUnknown,
        });
    }

    let Some(acquired) = lots.and_then(InstrumentLots::earliest_acquired) else {
        return unverifiable(UnverifiableReason::AcquisitionDateUnknown);
    };

    let owned: Vec<_> = plan
        .past
        .iter()
        .copied()
        .filter(|posting| posting.date >= acquired.0)
        .collect();

    // Выплата, прошедшая границу владения, но датированная раньше
    // первого события журнала: фактов под неё нет и быть не может.
    // Заявленная дата сделки может быть сколь угодно старше журнала —
    // `OpeningPosition` именно так и записывается.
    if let Some(start) = history_starts {
        if owned.iter().any(|posting| posting.date < start) {
            return unverifiable(UnverifiableReason::HistoryStartsAfterSchedule);
        }
    }

    rule.unreceived(&owned, income.postings(&key))
        .into_iter()
        .map(|posting| MaterialIssue::ScheduledPostingNotReceived {
            account: key.account,
            instrument: key.instrument,
            date: posting.date,
            kind: posting.kind,
        })
        .collect()
}
```

Добавить в `InstrumentLots` (`projection/lots.rs`):

```rust
    /// Самая ранняя дата приобретения среди текущих партий.
    ///
    /// `None`, если хоть у одной партии даты нет или есть количество,
    /// восстановленное без стоимости: границу владения тогда провести
    /// нечем, а провести её приблизительно значило бы выдумать дефект
    /// либо скрыть настоящий.
    #[must_use]
    pub fn earliest_acquired(&self) -> Option<TradeDate> {
        if !self.unpriced.0.is_zero() {
            return None;
        }
        self.lots
            .iter()
            .map(|lot| lot.acquired)
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .min()
    }
```

Собранные проблемы влить в `data_quality.material_issues` рядом с `unresolved_offer_issues` (`returns/mod.rs:1213`). Сверка проводится **один раз на пару (счёт, инструмент)**, а не на каждый сценарий оферты: сценарии различаются будущим потоком, а прошлое у них общее. Иначе проблема задвоится.

- [ ] **Step 6: Завести версию правила в `AppliedRules`**

Рядом с `cashflow_projection_rule` (`returns/mod.rs:899`):

```rust
pub(crate) const fn posting_match_rule() -> (PostingMatchVersion, PostingMatchV1) {
    (PostingMatchVersion(1), PostingMatchV1::new())
}
```

Добавить поле в `AppliedRules` (`:457`):

```rust
    /// Версия правила сверки запланированных выплат с журналом.
    pub posting_match: PostingMatchVersion,
```

Заполнить его в сборке отчёта — там же, где заполняются `quotation_rule` и `accrued_interest_rule`.

- [ ] **Step 7: Прогнать тесты**

Run: `cargo nextest run -p iaam-core`
Expected: PASS. Тесты, закрепляющие форму `AppliedRules` или `inputs_hash`, обновить: изменение входа отчёта обязано менять `inputs_hash` — это признак, а не поломка.

- [ ] **Step 8: Коммит**

```bash
git add crates/iaam-core/src/returns/mod.rs crates/iaam-core/src/projection/lots.rs
git commit -m "feat(core): сверка запланированных выплат с журналом (iaam-d8b.12.13)"
```

---

## Task 7: Транспорт — DTO, OpenAPI, контракт

**Files:**
- Modify: `crates/iaam-server/src/dto.rs:1518-1560`
- Modify: `crates/iaam-server/src/openapi.rs`
- Test: `crates/iaam-server/tests/contract.rs`
- Обновится: `crates/iaam-server/tests/snapshots/contract__the_report_shape_is_frozen_by_a_snapshot.snap` (замороженный эталон формы отчёта — `insta`)

**Interfaces:**
- Consumes: `MaterialIssue::ScheduledPostingNotReceived` (новая форма) и `ScheduledPostingUnverifiable` из Task 6.

**Acceptance Criteria:**
- Обе проблемы сериализуются с машиночитаемым кодом и человеко-читаемым текстом по-русски.
- В тексте `NotReceived` названы счёт, инструмент, дата и вид выплаты.
- В тексте `Unverifiable` названа причина.
- OpenAPI перечисляет оба кода и все четыре значения `UnverifiableReason`.
- Контрактный тест закрепляет форму обеих проблем.

- [ ] **Step 1: Написать падающий контрактный тест**

В `crates/iaam-server/tests/contract.rs`:

```rust
#[test]
fn both_scheduled_posting_issues_are_published_in_a_stable_shape() {
    let dto = material_issue_dto(&MaterialIssue::ScheduledPostingNotReceived {
        account,
        instrument,
        date: date!(2026 - 06 - 15),
        kind: PostingKind::Coupon,
    });
    assert_eq!(dto.code, "scheduled_posting_not_received");
    assert!(dto.message.contains("2026-06-15"));
    assert!(dto.message.contains("купон"));

    let dto = material_issue_dto(&MaterialIssue::ScheduledPostingUnverifiable {
        account,
        instrument,
        reason: UnverifiableReason::HistoryStartsAfterSchedule,
    });
    assert_eq!(dto.code, "scheduled_posting_unverifiable");
    assert!(dto.message.contains("журнал"));
}
```

Точную форму DTO взять из соседних веток `dto.rs:1518-1560` — не изобретать новую.

- [ ] **Step 2: Убедиться, что тест падает**

Run: `cargo nextest run -p iaam-server scheduled_posting`
Expected: FAIL.

- [ ] **Step 3: Расширить DTO**

Обновить ветку `ScheduledPostingNotReceived` (`dto.rs:1546`) под новую форму и добавить ветку `ScheduledPostingUnverifiable`. Тексты — по-русски, в стиле соседних веток: называют величины, а не пересказывают имя варианта.

- [ ] **Step 4: Обновить OpenAPI**

`crates/iaam-server/src/openapi.rs`: добавить оба кода проблем и перечисление всех четырёх значений `UnverifiableReason` рядом с описанием существующих `MaterialIssue`.

- [ ] **Step 5: Прогнать заслоны**

Run: `make test && make fixtures`
Expected: PASS.

Замороженный эталон формы отчёта (`crates/iaam-server/tests/snapshots/contract__the_report_shape_is_frozen_by_a_snapshot.snap`) изменится: в отчёте появились новые коды. Принять новый эталон **отдельным коммитом** с `POLICY_CHANGE_APPROVED=1` и меткой `policy-change` (`scripts/check-diff-lint.sh:80`) — эталон затем и заморожен, чтобы изменение формы ответа было видно ревьюеру, а не проехало вместе с кодом.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-server/src/dto.rs crates/iaam-server/tests/contract.rs
git commit -m "feat(server): обе проблемы сверки выплат в транспорте (iaam-d8b.12.13)"
```

---

## Task 8: Заслоны — регрессия, свойства, мутанты

**Files:**
- Create: `crates/iaam-core/tests/scheduled_posting_reconciliation.rs`
- Modify: `crates/iaam-core/tests/prop_zero_reinvestment.rs` (свойства)

**Interfaces:**
- Consumes: всё, построенное задачами 1–7.

**Acceptance Criteria:**
- Облигация с пятилетней историей полученных купонов, факты со сдвигом 1–7 дней — **ноль тревог**. Это главный критерий приёмки бида.
- Амортизируемая облигация: `PartialRedemption` закрывает `PrincipalReturn`, купоны закрывают `Coupon` — ноль тревог.
- Пропуск в середине ряда даёт ровно одну проблему; соседи не задеты.
- Свойство: результат не зависит от порядка событий в журнале.
- Свойство: повторная проекция того же журнала даёт тот же результат.
- `make mutants` зелёный по порогу для `projection/income.rs` и `rules/posting_match.rs`.

**Откуда брать фикстуры.** Вспомогательные конструкторы ниже
(`quarterly_bond_bought`, `returns_report_for`, `scheduled_posting_issues`,
`amortising_bond_journal`, `bond_journal_strategy`) не изобретать: их прямые
образцы уже есть — сборка журнала и вызов отчёта в
`crates/iaam-core/tests/golden_zero_reinvestment.rs`, фильтрация
`material_issues` в `crates/iaam-core/tests/data_quality.rs`, стратегии
`proptest` в `crates/iaam-core/tests/prop_zero_reinvestment.rs`. Общий
вспомогательный код кладётся в `crates/iaam-core/tests/support/mod.rs`.

- [ ] **Step 1: Написать регрессию — главный критерий приёмки**

```rust
#[test]
fn five_years_of_received_coupons_raise_not_a_single_false_alarm() {
    // Ровно тот дефект, который бид просит устранить: сверка по
    // агрегату дала бы тревогу на каждой здоровой облигации с историей
    // выплат. Сдвиги 1..7 дней — обычная задержка депозитарной цепочки.
    let bond = quarterly_bond_bought(date!(2021 - 03 - 15));
    let mut journal = vec![purchase(&bond)];
    for (index, payment) in bond.quarterly_payments_through(date!(2026 - 03 - 15)) {
        let drift = i64::from(index % 7) + 1;
        journal.push(coupon_received(&bond, payment + Duration::days(drift)));
    }

    let report = returns_report_for(&journal, date!(2026 - 03 - 20));

    assert_eq!(
        scheduled_posting_issues(&report),
        Vec::<MaterialIssue>::new(),
        "здоровая облигация с полной историей выплат не должна давать ни одной тревоги"
    );
}

#[test]
fn an_amortising_bond_is_quiet_because_principal_returns_find_their_facts() {
    // Без вида выплаты в `past` (Task 1) этот тест красный: возвраты
    // номинала искали бы подтверждения среди купонных фактов.
    let report = returns_report_for(&amortising_bond_journal(), date!(2026 - 08 - 01));
    assert!(scheduled_posting_issues(&report).is_empty());
}

#[test]
fn one_missing_coupon_in_the_middle_is_named_and_its_neighbours_are_not() {
    let mut journal = five_years_of_received_coupons();
    journal.retain(|event| coupon_date(event) != Some(date!(2023 - 09 - 15)));

    let report = returns_report_for(&journal, date!(2026 - 03 - 20));
    let missing = scheduled_posting_issues(&report);

    assert_eq!(missing.len(), 1);
    assert!(matches!(
        missing[0],
        MaterialIssue::ScheduledPostingNotReceived { date, .. }
            if date == date!(2023 - 09 - 15)
    ));
}
```

- [ ] **Step 2: Прогнать регрессию**

Run: `cargo nextest run -p iaam-core --test scheduled_posting_reconciliation`
Expected: PASS.

- [ ] **Step 3: Написать свойства**

```rust
proptest! {
    #[test]
    fn the_reconciliation_verdict_is_independent_of_journal_order(
        journal in bond_journal_strategy(),
        seed in any::<u64>(),
    ) {
        let forward = scheduled_posting_issues(&returns_report_for(&journal, AS_OF));
        let shuffled = deterministic_shuffle(&journal, seed);
        let reordered = scheduled_posting_issues(&returns_report_for(&shuffled, AS_OF));
        prop_assert_eq!(forward, reordered);
    }

    #[test]
    fn projecting_the_same_journal_twice_gives_the_same_verdict(
        journal in bond_journal_strategy(),
    ) {
        let first = scheduled_posting_issues(&returns_report_for(&journal, AS_OF));
        let second = scheduled_posting_issues(&returns_report_for(&journal, AS_OF));
        prop_assert_eq!(first, second);
    }
}
```

`deterministic_shuffle` — перестановка по переданному семени, без `rand` из окружения: ядро детерминировано, и тест обязан быть воспроизводим.

- [ ] **Step 4: Прогнать свойства**

Run: `cargo nextest run -p iaam-core prop_`
Expected: PASS.

- [ ] **Step 5: Полные заслоны**

Run: `make check`
Expected: всё зелёное.

- [ ] **Step 6: Покрытие добавленных строк**

Run: `make diff-coverage BASE=main`
Expected: ≥90%. Непокрытые ветки закрыть тестами, а не `allow`.

- [ ] **Step 7: Мутанты**

Run: `make mutants`
Expected: порог по каждому модулю. Особое внимание: границы окна в `posting_match.rs` (`>=` против `>`, `<=` против `<`), `window_days` (21 против 20/22) и флаг `used[index]` в one-to-one — мутанты там переживают охотнее всего.

- [ ] **Step 8: Коммит**

```bash
git add crates/iaam-core/tests/
git commit -m "test(core): заслоны сверки запланированных выплат (iaam-d8b.12.13)"
```

---

## Порядок и зависимости

```
Task 1 (past + kind)
   ├──> Task 5 (PostingMatchV1)
   └──> Task 6 (сверка)
Task 2 (IncomeLedger + купоны)
   ├──> Task 3 (возврат номинала)
   ├──> Task 4 (оферта)
   └──> Task 5
Task 3, Task 4 ──> Task 6
Task 5 ──────────> Task 6
Task 6 ──────────> Task 7 ──> Task 8
```

Задачи 1 и 2 независимы и могут идти параллельно. Задачи 3 и 4 независимы между собой.

## Покрытие спеки

| Раздел спеки | Задача |
|---|---|
| §3.1 отдельный компонент | 2 |
| §3.2 форма `IncomeLedger` | 2 |
| §3.3 источники фактов | 2, 3, 4 |
| §3.4 дата получения | 2 |
| §3.5 миграция снимков | 2 |
| §4 расширение графика | 1 |
| §5.1 окно 21 день | 5 |
| §5.2 условие совпадения | 5 |
| §5.3 one-to-one | 5 |
| §5.4 граница владения | 6 |
| §6 обе проблемы, `is_defect` | 6 |
| §6.3 транспорт | 7 |
| §7 заслоны | 8 |
| §8 отложено | — (вне периметра, зафиксировано в спеке) |
