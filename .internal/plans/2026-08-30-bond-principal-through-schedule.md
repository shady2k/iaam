# Номинал облигации через график — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** Номинал облигации доезжает из справочника до расчёта через `BondSchedule`, непогашенный остаток выводится, а налоговая амортизация воспроизводится по журналируемой безразмерной доле — так, что метрики §7.1 и построение потока работают на реальном журнале без подмены CBOR.

**Architecture:** Первоначальный номинал становится полем `BondSchedule` и приходит тем же путём, что цены и графики. Остаток на дату выводится из номинала и ряда возвратов, нигде не хранится. Доля возврата от остатка до события записывается в `CorporateAction::PartialRedemption` вместе с evidence вычисления; вычисляет её слой приложения до построения `Event`. `Lot.principal` и обе функции «единого номинала по лотам» удаляются.

**Tech Stack:** Rust 1.98, `serde`, `ciborium` (снимки), `serde_json` (события), `rust_decimal`, `utoipa` 5 (OpenAPI), `cargo-nextest`, `cargo-mutants`, nix dev shell.

**Спек:** `.internal/specs/2026-08-30-bond-principal-through-schedule-design.md`

## Global Constraints

- Все команды запускаются в nix-шелле: `nix develop -c <cmd>`. Голый `cargo` в PATH отсутствует.
- Тесты: `nix develop -c cargo nextest run --workspace`; doc-тесты отдельно: `nix develop -c cargo test --workspace --doc`.
- Линты: `nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings`. Предупреждение — ошибка.
- Полный заслон: `make check` (fmt, lint, arch, fixtures, deps, test, doc-test).
- `f64` и `async` в `iaam-core` запрещены (`scripts/check-architecture.sh`).
- Неизвестное **никогда** не превращается в ноль (§4.9). Отказ с названной причиной — единственный допустимый ответ.
- Величины на единицу — `PerUnitAmount` (значение + валюта), не голый `Dec`.
- `SCHEMA_VERSION` остаётся **4**. Поднимать запрещено — обоснование в §5 спеки.
- `PROJECTION_VERSION` поднимается **6 → 7** ровно один раз, в Task 9.
- Имена тестов — по существующей конвенции файла: в `iaam-core` английские фразы (`a_percent_quote_becomes_money_through_the_remaining_face`), в приёмочных тестах `crates/iaam-core/tests/` допустимы русские имена хелперов.
- Каждая задача завершается зелёным `nix develop -c cargo nextest run --workspace` и коммитом с идентификатором бида.

---

### Task 1: `ReturnedShare` — доля возврата с инвариантом

**Files:**
- Create: `crates/iaam-core/src/rules/returned_share.rs`
- Modify: `crates/iaam-core/src/rules/mod.rs` (объявление модуля и реэкспорт)

**Interfaces:**
- Produces: `ReturnedShare` с `ReturnedShare::new(Dec) -> Result<Self, ReturnedShareError>`, `ReturnedShare::inner(self) -> Dec`; `ReturnedShareError::{NotPositive, AboveOne}`. `Deserialize` через `TryFrom<Dec>`, `Serialize` как `Dec`.

**Acceptance Criteria:**
- Конструктор отвергает ноль, отрицательное и значение больше единицы; ровно единица принимается.
- Десериализация из JSON и из CBOR не обходит инвариант: невалидное значение даёт ошибку разбора, а не значение.

- [ ] **Step 1: Написать падающий тест**

Создать `crates/iaam-core/src/rules/returned_share.rs` с тестами:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn dec(text: &str) -> Dec {
        Dec::new(text.parse::<Decimal>().expect("десятичное число"))
    }

    #[test]
    fn a_share_of_one_is_accepted_because_a_last_amortisation_returns_everything() {
        assert!(ReturnedShare::new(dec("1")).is_ok());
    }

    #[test]
    fn a_zero_share_is_rejected_because_nothing_was_returned() {
        assert_eq!(
            ReturnedShare::new(dec("0")).unwrap_err(),
            ReturnedShareError::NotPositive
        );
    }

    #[test]
    fn a_negative_share_is_rejected() {
        assert_eq!(
            ReturnedShare::new(dec("-0.1")).unwrap_err(),
            ReturnedShareError::NotPositive
        );
    }

    #[test]
    fn a_share_above_one_is_rejected_because_more_than_the_remainder_cannot_return() {
        assert_eq!(
            ReturnedShare::new(dec("1.0001")).unwrap_err(),
            ReturnedShareError::AboveOne
        );
    }

    #[test]
    fn json_deserialisation_does_not_bypass_the_invariant() {
        let error = serde_json::from_str::<ReturnedShare>("\"1.5\"")
            .expect_err("невалидная доля обязана не разобраться");
        assert!(error.to_string().contains("больше единицы"), "{error}");
    }

    #[test]
    fn cbor_deserialisation_does_not_bypass_the_invariant() {
        let mut body = Vec::new();
        ciborium::into_writer(&dec("2"), &mut body).expect("запись");
        assert!(ciborium::from_reader::<ReturnedShare, _>(body.as_slice()).is_err());
    }
}
```

- [ ] **Step 2: Прогнать тест и убедиться, что он падает**

Run: `nix develop -c cargo nextest run -p iaam-core returned_share`
Expected: FAIL — `cannot find type ReturnedShare in this scope`.

- [ ] **Step 3: Минимальная реализация**

В начало того же файла:

```rust
//! Доля непогашенного номинала, возвращённая одним событием (§6.5).
//!
//! Безразмерная величина, а не сумма: разнесение налоговой стоимости
//! сокращает суммы, и хранение доли делает факт независимым от того,
//! что справочник будет знать о номинале завтра.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::numeric::decimal::Dec;

/// Доля возврата от непогашенного остатка **до** события.
///
/// Единица допустима: последняя амортизация возвращает весь остаток,
/// и юридическое выбытие бумаги — отдельный факт, а не следствие
/// (`event/corporate_action.rs`, `PartialRedemption` против `Redemption`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ReturnedShare(Dec);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReturnedShareError {
    #[error("доля возврата не положительна: событие ничего не вернуло")]
    NotPositive,
    #[error("доля возврата больше единицы: вернуть больше остатка нельзя")]
    AboveOne,
}

impl ReturnedShare {
    /// Конструктор, а не публичное поле: собранное вручную значение
    /// обошло бы обе проверки.
    pub fn new(value: Dec) -> Result<Self, ReturnedShareError> {
        if !value.is_positive() {
            return Err(ReturnedShareError::NotPositive);
        }
        if value > Dec::one() {
            return Err(ReturnedShareError::AboveOne);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn inner(self) -> Dec {
        self.0
    }
}

impl TryFrom<Dec> for ReturnedShare {
    type Error = ReturnedShareError;

    fn try_from(value: Dec) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

// `#[derive(Deserialize)]` собрал бы newtype в обход конструктора,
// поэтому разбор идёт через `TryFrom`.
impl<'de> Deserialize<'de> for ReturnedShare {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Dec::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}
```

`Dec::one()` и `Dec::is_positive()` уже есть
(`crates/iaam-core/src/numeric/decimal.rs:56,66`) — заводить свои
не нужно.

- [ ] **Step 4: Объявить модуль**

В `crates/iaam-core/src/rules/mod.rs` добавить рядом с соседними
объявлениями `pub mod returned_share;` и в реэкспорт —
`pub use returned_share::{ReturnedShare, ReturnedShareError};`.

- [ ] **Step 5: Прогнать тесты**

Run: `nix develop -c cargo nextest run -p iaam-core returned_share`
Expected: PASS, 6 тестов.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/src/rules/returned_share.rs crates/iaam-core/src/rules/mod.rs
git commit -m "feat(core): доля возврата номинала отдельным типом (iaam-d8b.15)"
```

---

### Task 2: `initial_principal` в графике и вывод остатка

**Files:**
- Modify: `crates/iaam-core/src/bond/mod.rs` (поле `BondSchedule.initial_principal`)
- Create: `crates/iaam-core/src/bond/principal.rs` (`remaining_principal`, `RemainingPrincipalError`)
- Modify: `crates/iaam-core/src/bond/mod.rs` (объявление модуля)

**Interfaces:**
- Consumes: `BondSchedule`, `PrincipalReturn` (`bond/mod.rs:52,72`), `ScheduleCompleteness` (`bond/offer.rs:90`).
- Produces: `BondSchedule.initial_principal: Option<PerUnitAmount>`; `remaining_principal(&BondSchedule, Date) -> Result<PerUnitAmount, RemainingPrincipalError>`; `RemainingPrincipalError::{Unknown, ScheduleNotValidated, ShareNotPositive, PrefixAboveHundred, Numeric}`.

**Acceptance Criteria:**
- Остаток на дату равен `initial × (100% − Σ долей с repayment_date <= дата)`.
- Граница включающая: в день возврата остаток уже уменьшен.
- График не `Validated` даёт `ScheduleNotValidated`, а не арифметический ответ.
- Отрицательная доля, доля больше 100% и префикс больше 100% дают названные ошибки, а не остаток.
- Отсутствующий `initial_principal` даёт `Unknown`, а не ноль.

- [ ] **Step 1: Написать падающие тесты**

Создать `crates/iaam-core/src/bond/principal.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bond::{BondSchedule, PrincipalReturn};
    use crate::money::CurrencyCode;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn dec(text: &str) -> Dec {
        Dec::new(text.parse::<Decimal>().expect("десятичное число"))
    }

    fn rub(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(dec(text), CurrencyCode::new("RUB").expect("код валюты"))
    }

    fn schedule(returns: &[(&str, &str)]) -> BondSchedule {
        BondSchedule {
            initial_principal: Some(rub("1000")),
            principal_returns: returns
                .iter()
                .map(|(day, share)| PrincipalReturn {
                    repayment_date: day.parse::<Date>().expect("дата"),
                    share_percent: dec(share),
                })
                .collect(),
            completeness: ScheduleCompleteness::Validated,
            ..Default::default()
        }
    }

    #[test]
    fn the_remainder_is_the_initial_principal_before_any_repayment() {
        let schedule = schedule(&[("2026-06-01", "30")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 05 - 31)).unwrap(),
            rub("1000")
        );
    }

    #[test]
    fn the_repayment_date_itself_already_reduces_the_remainder() {
        let schedule = schedule(&[("2026-06-01", "30")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap(),
            rub("700")
        );
    }

    #[test]
    fn repayments_accumulate() {
        let schedule = schedule(&[("2026-06-01", "30"), ("2026-07-01", "20")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 07 - 01)).unwrap(),
            rub("500")
        );
    }

    #[test]
    fn a_fully_repaid_issue_leaves_a_zero_remainder() {
        let schedule = schedule(&[("2026-06-01", "100")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap(),
            rub("0")
        );
    }

    #[test]
    fn an_untrusted_schedule_gives_no_remainder_even_when_the_arithmetic_works() {
        let mut schedule = schedule(&[("2026-06-01", "30")]);
        schedule.completeness = ScheduleCompleteness::Unknown;
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap_err(),
            RemainingPrincipalError::ScheduleNotValidated
        );
    }

    #[test]
    fn a_missing_initial_principal_is_unknown_and_never_zero() {
        let mut schedule = schedule(&[]);
        schedule.initial_principal = None;
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap_err(),
            RemainingPrincipalError::Unknown
        );
    }

    #[test]
    fn a_negative_share_is_named_and_not_silently_added() {
        let schedule = schedule(&[("2026-06-01", "-10")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap_err(),
            RemainingPrincipalError::ShareNotPositive
        );
    }

    #[test]
    fn a_prefix_above_one_hundred_percent_is_rejected() {
        let schedule = schedule(&[("2026-06-01", "60"), ("2026-07-01", "60")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 07 - 01)).unwrap_err(),
            RemainingPrincipalError::PrefixAboveHundred
        );
    }
}
```

- [ ] **Step 2: Прогнать тест и убедиться, что он падает**

Run: `nix develop -c cargo nextest run -p iaam-core bond::principal`
Expected: FAIL — `cannot find function remaining_principal`, а также
`struct BondSchedule has no field named initial_principal`.

- [ ] **Step 3: Добавить поле в `BondSchedule`**

В `crates/iaam-core/src/bond/mod.rs`, в объявление `BondSchedule`:

```rust
    /// Первоначальный номинал на одну бумагу.
    ///
    /// `None` — источник не сообщил либо бумага не долговая. Ноль
    /// подставлять запрещено (§4.9): «номинал ноль» и «номинал
    /// неизвестен» требуют от владельца разных действий.
    ///
    /// Текущий номинал здесь отсутствует намеренно: остаток выводится
    /// из первоначального и ряда возвратов, и второй источник истины
    /// разошёлся бы с первым молча.
    #[serde(default)]
    pub initial_principal: Option<PerUnitAmount>,
```

Импорт `PerUnitAmount` добавить в шапку файла, если его там нет.

- [ ] **Step 4: Реализовать вывод остатка**

В начало `crates/iaam-core/src/bond/principal.rs`:

```rust
//! Непогашенный остаток номинала на дату (§6.5).
//!
//! Остаток не хранится нигде: он выводится из первоначального номинала
//! и ряда возвратов. Хранить его вторым полем значило бы завести второй
//! источник истины, который разойдётся с первым молча.

use thiserror::Error;
use time::Date;

use crate::bond::BondSchedule;
use crate::bond::offer::ScheduleCompleteness;
use crate::money::PerUnitAmount;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RemainingPrincipalError {
    #[error("первоначальный номинал неизвестен")]
    Unknown,
    #[error("график не проверен: остаток из него брать нельзя")]
    ScheduleNotValidated,
    #[error("доля возврата номинала не положительна")]
    ShareNotPositive,
    #[error("доли возвратов до даты дают больше 100%")]
    PrefixAboveHundred,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Непогашенный номинал на одну бумагу на дату `on`.
///
/// Граница включающая: в день возврата остаток уже уменьшен — право
/// на выплату определяется датой фиксации, а сам возврат происходит
/// в дату платежа.
///
/// Доверие к графику проверяется здесь, а не у вызывающего: раньше
/// котировка брала остаток из лота и от графика не зависела вовсе,
/// теперь зависит. Цена из графика, которому система не доверяет,
/// хуже отсутствия цены.
pub fn remaining_principal(
    schedule: &BondSchedule,
    on: Date,
) -> Result<PerUnitAmount, RemainingPrincipalError> {
    match &schedule.completeness {
        ScheduleCompleteness::Validated => {}
        ScheduleCompleteness::Incomplete { .. } | ScheduleCompleteness::Unknown => {
            return Err(RemainingPrincipalError::ScheduleNotValidated);
        }
    }

    let initial = schedule
        .initial_principal
        .ok_or(RemainingPrincipalError::Unknown)?;

    let mut repaid = Dec::zero();
    for item in &schedule.principal_returns {
        if item.repayment_date > on {
            continue;
        }
        if !item.share_percent.is_positive() {
            return Err(RemainingPrincipalError::ShareNotPositive);
        }
        repaid = repaid.checked_add(item.share_percent)?;
    }

    let hundred = Dec::new(rust_decimal::Decimal::ONE_HUNDRED);
    if repaid > hundred {
        return Err(RemainingPrincipalError::PrefixAboveHundred);
    }

    let remaining_share = hundred.checked_sub(repaid)?;
    let value = initial
        .value()
        .checked_mul(remaining_share)?
        .checked_div(hundred)?;
    Ok(PerUnitAmount::new(value, initial.currency()))
}
```

Все использованные методы `Dec` существуют:
`zero()` (`numeric/decimal.rs:28`), `is_positive()` (`:66`),
`checked_add` (`:104`), `checked_sub` (`:75`), `checked_mul` (`:82`),
`checked_div` (`:116`).

- [ ] **Step 5: Объявить модуль**

В `crates/iaam-core/src/bond/mod.rs` добавить `pub mod principal;` рядом
с `pub mod offer;` и реэкспорт `pub use principal::{remaining_principal, RemainingPrincipalError};`.

- [ ] **Step 6: Прогнать тесты**

Run: `nix develop -c cargo nextest run -p iaam-core bond::principal`
Expected: PASS, 8 тестов.

- [ ] **Step 7: Починить сборку остальных крейтов**

Новое поле со `#[serde(default)]` не ломает разбор, но литералы
`BondSchedule { .. }` без `..Default::default()` перестанут собираться.

Run: `nix develop -c cargo build --workspace --all-targets 2>&1 | grep "missing field"`
Добавить `initial_principal: None` в каждый такой литерал (в рабочем коде —
реальное значение появится в Task 10).

- [ ] **Step 8: Коммит**

```bash
git add crates/iaam-core/src/bond/
git commit -m "feat(core): номинал в графике, остаток выводится из ряда возвратов (iaam-d8b.15)"
```

---

### Task 3: Положительность возврата — инвариант события

**Files:**
- Modify: `crates/iaam-core/src/event/mod.rs:400-412` (`validate_corporate_action`, ветка `PartialRedemption`)

**Interfaces:**
- Consumes: `require_positive` (`event/mod.rs`), `CorporateAction::PartialRedemption`.
- Produces: событие с непозитивным `principal_returned_per_unit` отклоняется `EventValidationError::NonPositive`.

**Acceptance Criteria:**
- `principal_returned_per_unit` равный нулю или отрицательный отклоняется структурной проверкой события.
- Положительный проходит, поведение остальных полей не меняется.

**Почему отдельной задачей:** сегодня положительность возврата косвенно
ловит `ReturnedNotPositive` в правиле амортизации, которое Task 5
удаляет. Без этого шага событие смогло бы утверждать возврат −100 ₽
и пройти ядро. Задача самостоятельна и полезна независимо от остального
плана.

- [ ] **Step 1: Написать падающий тест**

В `#[cfg(test)] mod tests` файла `crates/iaam-core/src/event/mod.rs`, рядом
с соседними тестами валидации:

```rust
#[test]
fn a_partial_redemption_returning_nothing_is_rejected() {
    let event = partial_redemption_event(per_unit("0"));
    assert!(matches!(
        event.validate_structure().unwrap_err(),
        EventValidationError::NonPositive { .. }
    ));
}

#[test]
fn a_partial_redemption_returning_a_negative_principal_is_rejected() {
    let event = partial_redemption_event(per_unit("-100"));
    assert!(matches!(
        event.validate_structure().unwrap_err(),
        EventValidationError::NonPositive { .. }
    ));
}
```

Хелпер `partial_redemption_event` собрать по образцу соседних тестов
модуля: найти существующий литерал `CorporateAction::PartialRedemption`
в тестах (`nix develop -c grep -n "PartialRedemption" crates/iaam-core/src/event/mod.rs`)
и параметризовать его по `principal_returned_per_unit`.

- [ ] **Step 2: Прогнать тест и убедиться, что он падает**

Run: `nix develop -c cargo nextest run -p iaam-core a_partial_redemption_returning`
Expected: FAIL — событие проходит валидацию, `unwrap_err` паникует.

- [ ] **Step 3: Добавить инвариант**

В `crates/iaam-core/src/event/mod.rs`, ветка `PartialRedemption` метода
`validate_corporate_action`: раскрыть `principal_returned_per_unit`
из `..` и добавить проверку рядом с существующими:

```rust
            CorporateAction::PartialRedemption {
                instrument,
                quantity,
                principal_returned_per_unit,
                compensation,
                ..
            } => {
                require_positive(name, "compensation", compensation.amount().raw())?;
                require_positive_quantity(name, "quantity", *quantity)?;
                // Возврат номинала проверяется здесь, а не в правиле
                // разнесения: правило считает по безразмерной доле и
                // сырое денежное утверждение события больше не видит.
                require_positive_dec(
                    name,
                    "principal_returned_per_unit",
                    principal_returned_per_unit.value(),
                )?;
                self.expect_legs(
                    name,
                    &[principal_leg(self.account, *instrument, *compensation)],
                )
            }
```

Если `require_positive_dec` в модуле нет — добавить рядом с
`require_positive`, по его образцу, приняв `Dec` и вернув
`EventValidationError::NonPositive`.

- [ ] **Step 4: Прогнать тесты**

Run: `nix develop -c cargo nextest run -p iaam-core --workspace`
Expected: PASS. Если какой-то существующий тест собирал амортизацию
с нулевым возвратом — исправить сам тест: это ровно тот брак источника,
который теперь отклоняется.

- [ ] **Step 5: Коммит**

```bash
git add crates/iaam-core/src/event/mod.rs
git commit -m "fix(core): возврат номинала обязан быть положительным (iaam-d8b.15)"
```

---

### Task 4: `BasisAllocation` и evidence в событии амортизации

**Files:**
- Create: `crates/iaam-core/src/event/allocation.rs`
- Modify: `crates/iaam-core/src/event/corporate_action.rs` (поле в `PartialRedemption`)
- Modify: `crates/iaam-core/src/event/mod.rs` (объявление модуля)

**Interfaces:**
- Consumes: `ReturnedShare` (Task 1).
- Produces: `BasisAllocation::{Unknown(AllocationGap), Known { share, evidence }}`; `AllocationEvidence { inputs_hash: AllocationInputsHash, knowledge_as_of: OffsetDateTime, algorithm_version: AllocationAlgorithmVersion }`; `AllocationGap` с вариантами из §6.4 спеки; поле `CorporateAction::PartialRedemption.basis_allocation: BasisAllocation` со `#[serde(default)]`.

**Acceptance Criteria:**
- Событие без поля читается и даёт `Unknown` с причиной «поле не заполнялось».
- Событие с полем переживает круг через JSON и через CBOR.
- `Unknown` несёт типизированную причину, а не пустоту.
- `SCHEMA_VERSION` не меняется.

- [ ] **Step 1: Написать падающий тест**

В `crates/iaam-core/src/event/allocation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::decimal::Dec;
    use crate::rules::ReturnedShare;
    use rust_decimal::Decimal;
    use time::OffsetDateTime;

    fn known() -> BasisAllocation {
        BasisAllocation::Known {
            share: ReturnedShare::new(Dec::new(Decimal::new(2, 1))).expect("доля 0.2"),
            evidence: AllocationEvidence {
                inputs_hash: AllocationInputsHash::new("a".repeat(64)).expect("hex"),
                knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
                algorithm_version: AllocationAlgorithmVersion(1),
            },
        }
    }

    #[test]
    fn the_default_allocation_is_unknown_because_the_field_was_never_filled() {
        assert_eq!(
            BasisAllocation::default(),
            BasisAllocation::Unknown(AllocationGap::NotComputed)
        );
    }

    #[test]
    fn a_known_allocation_survives_a_json_round_trip() {
        let text = serde_json::to_string(&known()).expect("запись");
        assert_eq!(
            serde_json::from_str::<BasisAllocation>(&text).expect("чтение"),
            known()
        );
    }

    #[test]
    fn a_known_allocation_survives_a_cbor_round_trip() {
        let mut body = Vec::new();
        ciborium::into_writer(&known(), &mut body).expect("запись");
        assert_eq!(
            ciborium::from_reader::<BasisAllocation, _>(body.as_slice()).expect("чтение"),
            known()
        );
    }

    #[test]
    fn every_gap_names_its_reason() {
        for gap in AllocationGap::ALL {
            assert!(!gap.code().is_empty());
        }
    }
}
```

- [ ] **Step 2: Прогнать тест и убедиться, что он падает**

Run: `nix develop -c cargo nextest run -p iaam-core event::allocation`
Expected: FAIL — `cannot find type BasisAllocation`.

- [ ] **Step 3: Реализовать типы**

```rust
//! Разнесение налоговой стоимости при амортизации как факт события.
//!
//! Доля хранится в самом факте, а не выводится позже: если справочник
//! исправят, вывести её будет неоткуда. Тот же довод, по которому
//! `Conversion` хранит `basis_transfer` — условия живут в решении
//! эмитента, а не в справочнике.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::rules::ReturnedShare;

/// Почему доля разнесения не вычислена.
///
/// Проекции достаточно одного «неизвестно», но владельцу нужно знать,
/// что именно дозагрузить, а аудиту — что именно разошлось.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocationGap {
    /// Событие записано до появления поля либо обогащение не выполнялось.
    NotComputed,
    /// Графика выпуска нет вовсе.
    ScheduleMissing,
    /// График есть, но не проверен.
    ScheduleNotValidated,
    /// В графике нет возврата на дату события.
    NoRepaymentOnDate,
    /// Сумма события не сошлась с плановой долей.
    AmountMismatch,
    /// Валюта возврата не совпала с валютой номинала.
    CurrencyMismatch,
    /// На дату приходится несколько возвратов, которые не удалось
    /// сопоставить событиям.
    AmbiguousSameDateRepayments,
    /// Доли возвратов до даты дают больше 100%.
    InvalidPrefix,
}

impl AllocationGap {
    pub const ALL: [Self; 8] = [
        Self::NotComputed,
        Self::ScheduleMissing,
        Self::ScheduleNotValidated,
        Self::NoRepaymentOnDate,
        Self::AmountMismatch,
        Self::CurrencyMismatch,
        Self::AmbiguousSameDateRepayments,
        Self::InvalidPrefix,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotComputed => "not_computed",
            Self::ScheduleMissing => "schedule_missing",
            Self::ScheduleNotValidated => "schedule_not_validated",
            Self::NoRepaymentOnDate => "no_repayment_on_date",
            Self::AmountMismatch => "amount_mismatch",
            Self::CurrencyMismatch => "currency_mismatch",
            Self::AmbiguousSameDateRepayments => "ambiguous_same_date_repayments",
            Self::InvalidPrefix => "invalid_prefix",
        }
    }
}

/// Версия алгоритма вычисления доли.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AllocationAlgorithmVersion(pub u16);

/// Отпечаток канонической выборки справочных входов вычисления.
///
/// Покрывает первоначальный номинал, валюту номинала, возвраты, вошедшие
/// в остаток до события, возвраты на дату события, идентичность снимка
/// источника и правило группировки одинаковых дат.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AllocationInputsHash(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("отпечаток входов не является 64 шестнадцатеричными знаками")]
pub struct AllocationInputsHashError;

impl AllocationInputsHash {
    pub fn new(value: impl Into<String>) -> Result<Self, AllocationInputsHashError> {
        let value = value.into();
        if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AllocationInputsHashError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Из каких дополнительных входов приложение вывело долю.
///
/// Отдельно от `Provenance`: тот отвечает на вопрос «откуда пришёл сырой
/// факт», а это — «из чего выведено производное поле».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocationEvidence {
    pub inputs_hash: AllocationInputsHash,
    pub knowledge_as_of: OffsetDateTime,
    pub algorithm_version: AllocationAlgorithmVersion,
}

/// Доля разнесения с доказательством её вычисления.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BasisAllocation {
    Unknown(AllocationGap),
    Known {
        share: ReturnedShare,
        evidence: AllocationEvidence,
    },
}

impl Default for BasisAllocation {
    fn default() -> Self {
        Self::Unknown(AllocationGap::NotComputed)
    }
}
```

Сериализацию `OffsetDateTime` привести к той, что уже применяется
в проекте: `nix develop -c grep -rn "OffsetDateTime" crates/iaam-core/src/ | grep serde`.

- [ ] **Step 4: Добавить поле в событие**

В `crates/iaam-core/src/event/corporate_action.rs`, вариант
`PartialRedemption`:

```rust
        /// Доля непогашенного номинала, возвращённая этим событием.
        ///
        /// Умолчание `Unknown` честное: событие, записанное до появления
        /// поля, действительно ничего не утверждало, и приписать ему
        /// долю значило бы объявить вычисленным то, чего никто
        /// не вычислял.
        #[serde(default)]
        basis_allocation: crate::event::allocation::BasisAllocation,
```

Объявить модуль в `crates/iaam-core/src/event/mod.rs`: `pub mod allocation;`.

- [ ] **Step 5: Починить сборку**

Run: `nix develop -c cargo build --workspace --all-targets 2>&1 | grep -c "missing field"`
Добавить `basis_allocation: BasisAllocation::default()` во все литералы.

- [ ] **Step 6: Прогнать тесты, включая круг через JSON**

Run: `nix develop -c cargo nextest run -p iaam-core allocation serde_roundtrip`
Expected: PASS. `every_corporate_action_survives_a_json_round_trip`
(`event/corporate_action.rs:210`) обязан остаться зелёным.

- [ ] **Step 7: Тест обратной совместимости**

В `crates/iaam-core/tests/serde_roundtrip.rs` добавить:

```rust
#[test]
fn a_partial_redemption_written_before_the_allocation_field_reads_as_unknown() {
    // Тело записано до появления `basis_allocation`. Читаться обязано,
    // и доля обязана быть неизвестной, а не нулевой.
    let text = r#"{"type":"partial_redemption","instrument":"..." }"#;
    let action: CorporateAction = serde_json::from_str(text).expect("старое тело читается");
    let CorporateAction::PartialRedemption { basis_allocation, .. } = action else {
        panic!("ожидалась амортизация");
    };
    assert_eq!(
        basis_allocation,
        BasisAllocation::Unknown(AllocationGap::NotComputed)
    );
}
```

Полное тело JSON взять из существующего теста круга: сериализовать
литерал амортизации, скопировать вывод и **удалить** из него ключ
`basis_allocation`.

- [ ] **Step 8: Коммит**

```bash
git add crates/iaam-core/src/event/ crates/iaam-core/tests/serde_roundtrip.rs
git commit -m "feat(core): доля разнесения и её evidence в событии амортизации (iaam-d8b.15)"
```

---

### Task 5: Правило амортизации считает по доле

**Files:**
- Modify: `crates/iaam-core/src/rules/amortisation.rs` (сигнатура `basis_returned`, отказы)
- Modify: `crates/iaam-core/src/projection/lots.rs:959-1015` (`apply_amortisation`)
- Modify: `crates/iaam-core/src/projection/lots.rs` (`BasisGap::PrincipalUnknown` → `AmortisationAllocationUnknown`)

**Interfaces:**
- Consumes: `ReturnedShare` (Task 1), `BasisAllocation` (Task 4), `split_basis` (`rules/lot_disposal.rs:329`).
- Produces: `AmortisationRule::basis_returned(&self, lot: &Lot, share: ReturnedShare) -> Result<Money, AmortisationError>`; `BasisGap::AmortisationAllocationUnknown`.

**Acceptance Criteria:**
- Доля 0.2 от стоимости 100 000 ₽ возвращает 20 000 ₽.
- Доля 1 возвращает всю стоимость; количество бумаг не меняется.
- `BasisAllocation::Unknown` применяет факт и ставит `AmortisationAllocationUnknown` — не ноль и не отказ.
- Отказы `CurrencyMismatch`, `ReturnedAboveRemaining`, `UnknownPrincipal` и `ReturnedNotPositive` из правила удалены.

- [ ] **Step 1: Переписать тесты правила**

В `crates/iaam-core/src/rules/amortisation.rs`, `mod tests`: заменить
хелпер `lot(cost_basis, principal)` на `lot(cost_basis)` без номинала
и переписать тесты на долю. Ключевые:

```rust
    fn share(text: &str) -> ReturnedShare {
        ReturnedShare::new(dec(text)).expect("доля в пределах инварианта")
    }

    #[test]
    fn a_fifth_of_the_remaining_principal_returns_a_fifth_of_the_basis() {
        let lot = lot(rub(100_000));
        assert_eq!(ProRataV1.basis_returned(&lot, share("0.2")).unwrap(), rub(20_000));
    }

    #[test]
    fn the_whole_basis_comes_back_when_the_whole_remainder_does() {
        // Последняя амортизация возвращает весь остаток номинала.
        // Бумага при этом остаётся в позиции: её выбытие — отдельный
        // факт, а не следствие возврата денег.
        let lot = lot(rub(100_000));
        assert_eq!(ProRataV1.basis_returned(&lot, share("1")).unwrap(), rub(100_000));
    }

    #[test]
    fn rounding_follows_the_half_to_even_convention_of_split_basis() {
        let lot = lot(rub(101));
        assert_eq!(ProRataV1.basis_returned(&lot, share("0.5")).unwrap(), rub(50));
    }
```

- [ ] **Step 2: Прогнать тест и убедиться, что он падает**

Run: `nix develop -c cargo nextest run -p iaam-core rules::amortisation`
Expected: FAIL — сигнатура `basis_returned` не совпадает.

- [ ] **Step 3: Переписать правило**

В `crates/iaam-core/src/rules/amortisation.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AmortisationError {
    #[error(transparent)]
    Numeric(#[from] NumericError),
    #[error(transparent)]
    Disposal(#[from] DisposalError),
}

pub trait AmortisationRule: Send + Sync + std::fmt::Debug {
    /// Сколько налоговой стоимости лота возвращается вместе с номиналом.
    ///
    /// Аргумент безразмерный: суммы в формуле сокращаются, и правилу
    /// незачем знать ни первоначальный номинал, ни остаток. Знаменатель
    /// — единица, потому что доля уже посчитана от остатка **до**
    /// события.
    fn basis_returned(&self, lot: &Lot, share: ReturnedShare) -> Result<Money, AmortisationError>;
}

impl AmortisationRule for ProRataV1 {
    fn basis_returned(&self, lot: &Lot, share: ReturnedShare) -> Result<Money, AmortisationError> {
        // Округление и обрезка живут в `split_basis`: она решает ровно
        // задачу «доля от суммы» с конвенцией «половина к чётному»,
        // и своя конвенция внутри одного ядра означала бы два разных
        // ответа на один вопрос.
        Ok(split_basis(
            lot.cost_basis,
            share.inner().inner(),
            rust_decimal::Decimal::ONE,
        )?)
    }
}
```

- [ ] **Step 4: Переписать применение в проекции**

В `crates/iaam-core/src/projection/lots.rs`, `apply_amortisation`:
заменить `facts.returned_per_unit` на долю из события и ветку
`Err(AmortisationError::UnknownPrincipal)` — на разбор `BasisAllocation`:

```rust
        let mut next = entry.clone();
        let mut returned_total = Money::zero(facts.compensation.currency());
        match facts.allocation {
            BasisAllocation::Known { share, .. } => {
                for lot in next.lots_mut() {
                    let returned = rule.basis_returned(lot, share)?;
                    lot.cost_basis = lot.cost_basis.try_sub(returned)?;
                    returned_total = returned_total.try_add(returned)?;
                }
            }
            // Доля неизвестна — факт всё равно применяется, а
            // реализованный результат становится невычислимым (§4.9).
            // Уменьшать нечего: разносить было не по чему.
            BasisAllocation::Unknown(_) => {
                next.mark_basis_gap(BasisGap::AmortisationAllocationUnknown);
            }
        }
        next.add_received(facts.compensation)?;
```

`AmortisationFacts` заменить поле `returned_per_unit: PerUnitAmount`
на `allocation: BasisAllocation`; `apply_corporate_action`
(`projection/lots.rs:845`) прокидывает `basis_allocation` события.

- [ ] **Step 5: Переименовать разрыв**

```bash
nix develop -c grep -rln "PrincipalUnknown" crates/ --include=*.rs
```

В `BasisGap` (`projection/lots.rs:88`) переименовать
`PrincipalUnknown` → `AmortisationAllocationUnknown`, строковый код —
`amortisation_allocation_unknown`. `CashflowError::PrincipalUnknown`
и `QuotationError::PrincipalUnknown` **не трогать**: они про номинал,
а не про долю.

- [ ] **Step 6: Прогнать тесты**

Run: `nix develop -c cargo nextest run -p iaam-core`
Expected: PASS.

- [ ] **Step 7: Коммит**

```bash
git add crates/iaam-core/src/rules/amortisation.rs crates/iaam-core/src/projection/lots.rs
git commit -m "feat(core): разнесение стоимости по безразмерной доле (iaam-d8b.15)"
```

---

### Task 6: Построение потока берёт номинал из графика

**Files:**
- Modify: `crates/iaam-core/src/rules/cashflow.rs:71-78` (`CashflowInput`), `:246-253` (чтение номинала)
- Modify: `crates/iaam-core/src/returns/mod.rs:1489-1530` (`scenario_plan`)

**Interfaces:**
- Consumes: `BondSchedule.initial_principal` (Task 2).
- Produces: `CashflowInput` без поля `principal`; `CashflowError::PrincipalUnknown` означает «справочник не сообщил номинал».

**Acceptance Criteria:**
- Поток строится по номиналу из графика, без обращения к лотам.
- График без `initial_principal` даёт `CashflowError::PrincipalUnknown`.
- Существующий тест `principal_return_uses_original_not_remaining_nominal` (`cashflow.rs:927`) остаётся зелёным по смыслу.

- [ ] **Step 1: Переписать тест**

В `crates/iaam-core/src/rules/cashflow.rs`, `mod tests`: убрать поле
`principal` из литералов `CashflowInput`, задать
`initial_principal: Some(rub("1000"))` в графике. Добавить:

```rust
    #[test]
    fn a_schedule_without_a_face_value_cannot_build_a_flow() {
        let mut schedule = schedule_with_returns();
        schedule.initial_principal = None;
        let input = CashflowInput {
            schedule: &schedule,
            quantity: Quantity(dec("1")),
            choice: &OfferChoice::HoldToMaturity,
            as_of: date!(2026 - 01 - 01),
            report_currency: rub_code(),
        };
        assert_eq!(
            CashflowV1.build(&input).unwrap_err(),
            CashflowError::PrincipalUnknown
        );
    }
```

- [ ] **Step 2: Прогнать тест и убедиться, что он падает**

Run: `nix develop -c cargo nextest run -p iaam-core rules::cashflow`
Expected: FAIL — `struct CashflowInput has field principal`.

- [ ] **Step 3: Убрать поле и переключить источник**

`CashflowInput`: удалить `pub principal: crate::rules::lot_disposal::PrincipalState,`.

`cashflow.rs:246`:

```rust
        let original = input
            .schedule
            .initial_principal
            .ok_or(CashflowError::PrincipalUnknown)?;
```

- [ ] **Step 4: Убрать `common_principal_state` из `scenario_plan`**

В `crates/iaam-core/src/returns/mod.rs:1502` удалить вызов и передачу
`principal` в `CashflowInput`.

- [ ] **Step 5: Прогнать тесты**

Run: `nix develop -c cargo nextest run -p iaam-core`
Expected: PASS.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/src/rules/cashflow.rs crates/iaam-core/src/returns/mod.rs
git commit -m "feat(core): будущий поток берёт номинал из графика (iaam-d8b.15)"
```

---

### Task 7: Котировка берёт остаток из графика

**Files:**
- Modify: `crates/iaam-core/src/returns/mod.rs:2030` (удалить `remaining_face`), места вызова
- Modify: `crates/iaam-core/src/returns/mod.rs` (`NotComputable::RemainingFaceAmbiguous` — удалить)

**Interfaces:**
- Consumes: `remaining_principal` (Task 2).
- Produces: параметр `remaining_face` для `QuotationV1` берётся из графика на `as_of`.

**Acceptance Criteria:**
- Котировка в процентах номинала переводится в деньги по остатку из графика.
- Позиция без графика даёт `QuotationError::PrincipalUnknown`, а не ноль.
- Денежная котировка (`MoneyPerUnit`) работает без графика.
- Позиция из нескольких лотов разных дат получает один и тот же остаток.

- [ ] **Step 1: Написать тест**

В `crates/iaam-core/src/returns/mod.rs`, `mod tests`:

```rust
    #[test]
    fn a_percent_quote_uses_the_issue_remainder_not_the_lots() {
        // Два лота разных дат: остаток принадлежит выпуску, поэтому
        // обоим достаётся один и тот же.
        let report = отчёт_с_двумя_лотами_и_процентной_котировкой();
        let value = report.terminal_value.value().expect("стоимость вычислима");
        assert_eq!(value, ожидаемая_стоимость_по_остатку_700());
    }

    #[test]
    fn a_percent_quote_without_a_schedule_is_not_priced() {
        let report = отчёт_без_графика_с_процентной_котировкой();
        assert!(report.uncovered.iter().any(|item| matches!(
            item.reason,
            UncoveredReason::NotComputable { .. }
        )));
    }
```

Хелперы собрать по образцу соседних тестов модуля.

- [ ] **Step 2: Прогнать тест и убедиться, что он падает**

Run: `nix develop -c cargo nextest run -p iaam-core a_percent_quote_uses_the_issue_remainder`
Expected: FAIL — остаток берётся из лотов и равен `None`.

- [ ] **Step 3: Переключить источник**

Найти место, где `remaining_face(book, key)` подаётся в оценку
(`nix develop -c grep -n "remaining_face" crates/iaam-core/src/returns/mod.rs`),
и заменить на:

```rust
    let remaining_face = request
        .bond_schedules
        .get(&key.instrument)
        .map(|schedule| crate::bond::remaining_principal(schedule, request.as_of))
        .transpose()
        .map_err(|error| NotComputable::from(error))?;
```

Добавить `impl From<RemainingPrincipalError> for NotComputable` рядом
с соседними конверсиями, сохранив причину.

- [ ] **Step 4: Удалить функцию и её отказ**

Удалить `fn remaining_face` (`returns/mod.rs:2030`) и вариант
`NotComputable::RemainingFaceAmbiguous` вместе с его строковым кодом
и тестами.

- [ ] **Step 5: Прогнать тесты**

Run: `nix develop -c cargo nextest run --workspace`
Expected: PASS.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/src/returns/mod.rs
git commit -m "feat(core): котировка берёт остаток номинала из графика (iaam-d8b.15)"
```

---

### Task 8: Удаление `Lot.principal` и `common_principal_state`

**Files:**
- Modify: `crates/iaam-core/src/rules/lot_disposal.rs:53-61` (поле и тип)
- Modify: `crates/iaam-core/src/projection/lots.rs` (места записи `PrincipalState::Unknown`)
- Modify: `crates/iaam-core/src/returns/zero_reinvestment.rs:530` (удалить функцию)
- Modify: `crates/iaam-core/src/returns/mod.rs` (`NotComputable::PrincipalStateAmbiguous` — удалить)

**Interfaces:**
- Produces: `Lot` без поля `principal`; `PrincipalState`, `PrincipalError`, `common_principal_state`, `NotComputable::PrincipalStateAmbiguous` удалены.

**Acceptance Criteria:**
- Ни `PrincipalState`, ни `common_principal_state` не встречаются в дереве.
- Смешанная позиция `priced + unpriced` даёт вычислимый проспективный поток и невычислимые пожизненные метрики.

- [ ] **Step 1: Написать тест на смешанную позицию**

В `crates/iaam-core/src/returns/mod.rs`, `mod tests`:

```rust
    #[test]
    fn a_position_with_unpriced_quantity_still_projects_a_flow_but_has_no_lifetime_metrics() {
        // Номинал принадлежит выпуску, поэтому неизвестность СТОИМОСТИ
        // части позиции не мешает построить поток на всё количество.
        // Пожизненные метрики при этом остаются невычислимыми.
        let report = отчёт_со_смешанной_позицией();
        assert!(поток_построен(&report));
        assert!(report.bond_metrics.iter().any(|metrics| matches!(
            metrics.lifetime,
            Computed::NotComputable(_)
        )));
    }
```

- [ ] **Step 2: Прогнать тест**

Run: `nix develop -c cargo nextest run -p iaam-core a_position_with_unpriced_quantity`
Expected: FAIL — сегодня `PrincipalStateAmbiguous` глушит и поток.

- [ ] **Step 3: Удалить поле и тип**

```bash
nix develop -c grep -rln "PrincipalState\|common_principal_state" crates/ --include=*.rs
```

Удалить поле `principal` из `Lot`, типы `PrincipalState` и
`PrincipalError`, функцию `common_principal_state`, вариант
`NotComputable::PrincipalStateAmbiguous` и все их тесты.

- [ ] **Step 4: Прогнать тесты**

Run: `nix develop -c cargo nextest run --workspace`
Expected: PASS.

- [ ] **Step 5: Убедиться, что тип исчез**

Run: `nix develop -c grep -rn "PrincipalState" crates/ --include=*.rs | wc -l`
Expected: `0`.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/src/
git commit -m "refactor(core): номинал больше не свойство партии (iaam-d8b.15)"
```

---

### Task 9: `prefix_digest` v2 и версия проекции

**Files:**
- Modify: `crates/iaam-core/src/projection/state.rs:241-258` (`prefix_digest`)
- Modify: `crates/iaam-core/src/projection/mod.rs:38` (`PROJECTION_VERSION`)

**Interfaces:**
- Produces: `prefix_digest` хеширует каноническое CBOR-содержимое `Event`; `PROJECTION_VERSION = 7`.

**Acceptance Criteria:**
- Два события, отличающиеся только `basis_allocation`, дают **разные** `prefix_digest`.
- Те же два события дают **одинаковый** `raw_hash` — дедупликация сохранена.
- Снимок версии 6 отвергается и вызывает полный пересчёт.

**Почему в этом плане:** обогащение делает два события с разной долей
неотличимыми по нынешнему digest, который обещает покрывать содержимое
(`state.rs:253`). Обещание не выполняется уже сегодня; здесь оно
становится обязательным.

- [ ] **Step 1: Написать падающий тест**

В `crates/iaam-core/src/projection/state.rs`, `mod tests`:

```rust
    #[test]
    fn two_events_differing_only_in_allocation_get_different_digests() {
        let unknown = amortisation_event(BasisAllocation::default());
        let known = amortisation_event(known_allocation());
        assert_ne!(
            prefix_digest(&[&unknown]),
            prefix_digest(&[&known]),
            "отпечаток обязан покрывать содержимое события"
        );
    }

    #[test]
    fn those_same_events_keep_one_raw_hash_so_deduplication_still_works() {
        let unknown = amortisation_event(BasisAllocation::default());
        let known = amortisation_event(known_allocation());
        assert_eq!(
            unknown.provenance.raw_hash(),
            known.provenance.raw_hash(),
            "повтор того же брокерского факта обязан оставаться дубликатом"
        );
    }
```

- [ ] **Step 2: Прогнать тест и убедиться, что он падает**

Run: `nix develop -c cargo nextest run -p iaam-core two_events_differing_only_in_allocation`
Expected: FAIL — отпечатки равны.

- [ ] **Step 3: Переписать `prefix_digest`**

```rust
/// Отпечаток действующего набора событий.
///
/// Хеширует каноническое содержимое события целиком, а не только
/// `provenance.raw_hash()`: последний покрывает сырой поданный факт
/// и не меняется, когда приложение выводит производное поле. Два
/// события с разной долей разнесения обязаны различаться здесь —
/// и обязаны совпадать по `raw_hash`, иначе повторная отправка того же
/// брокерского факта перестанет быть дубликатом.
pub fn prefix_digest(events: &[&Event]) -> StateHash {
    let mut hasher = Sha256::new();
    hasher.update(b"iaam/journal-prefix/v2");
    hasher.update(
        u64::try_from(events.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for event in events {
        hasher.update(event.id.inner().as_bytes());
        feed_date(&mut hasher, event.order.date());
        hasher.update(event.order.sequence().to_be_bytes());
        let mut body = Vec::new();
        ciborium::into_writer(event, &mut body)
            .expect("событие сериализуемо: обратное — дефект типа, а не данных");
        hasher.update(&body);
    }
    StateHash(hasher.finalize().into())
}
```

- [ ] **Step 4: Поднять версию проекции**

`crates/iaam-core/src/projection/mod.rs:38`:

```rust
/// Версия 7: номинал ушёл из лота, отпечаток префикса покрывает
/// содержимое события (`prefix_digest/v2`). Снимки версии 6 несовместимы
/// и вызывают полный пересчёт.
pub const PROJECTION_VERSION: u32 = 7;
```

- [ ] **Step 5: Прогнать тесты**

Run: `nix develop -c cargo nextest run --workspace`
Expected: PASS. Тест `crates/iaam-server/tests/contract.rs` на
`PROJECTION_VERSION` (`routes.rs:38` использует константу) обновить,
если он сверяет число.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/src/projection/
git commit -m "fix(core): отпечаток префикса покрывает содержимое события (iaam-d8b.15)"
```

---

### Task 10: Номинал доезжает из хранилища в график

**Files:**
- Modify: `crates/iaam-app/src/scenarios/reports.rs:254-280` (сборка `BondSchedule`)
- Modify: `crates/iaam-app/src/market_candidate.rs` (хелпер извлечения номинала)

**Interfaces:**
- Consumes: `IssueTermsRow.initial_face_value: Option<String>`, `face_currency_code: Option<String>` (`iaam-store/src/schedule.rs:94`); `BondSchedule.initial_principal` (Task 2).
- Produces: `initial_principal_from_terms(terms: Option<&IssueTermsRow>) -> Option<PerUnitAmount>`.

**Acceptance Criteria:**
- Облигация с `INITIALFACEVALUE` в хранилище получает `initial_principal` в графике отчёта.
- Строка без номинала или без валюты номинала даёт `None`, а не ноль.
- Неразбираемое число даёт `None`, а не панику.

- [ ] **Step 1: Написать тест**

В `crates/iaam-app/src/market_candidate.rs`, `mod tests`:

```rust
    #[test]
    fn the_initial_face_value_travels_from_the_terms_row() {
        let terms = terms_row(Some("1000"), Some("RUB"));
        assert_eq!(
            initial_principal_from_terms(Some(&terms)),
            Some(PerUnitAmount::new(dec("1000"), rub_code()))
        );
    }

    #[test]
    fn a_face_value_without_a_currency_is_unknown_and_never_zero() {
        let terms = terms_row(Some("1000"), None);
        assert_eq!(initial_principal_from_terms(Some(&terms)), None);
    }

    #[test]
    fn a_malformed_face_value_is_unknown_and_does_not_panic() {
        let terms = terms_row(Some("не число"), Some("RUB"));
        assert_eq!(initial_principal_from_terms(Some(&terms)), None);
    }
```

- [ ] **Step 2: Прогнать тест и убедиться, что он падает**

Run: `nix develop -c cargo nextest run -p iaam-app initial_face_value`
Expected: FAIL — `cannot find function initial_principal_from_terms`.

- [ ] **Step 3: Реализовать хелпер**

```rust
/// Первоначальный номинал из строки условий выпуска.
///
/// Валюта обязательна: номинал без валюты — не число, а догадка.
/// Неразобранное значение даёт `None`, потому что «номинал неизвестен»
/// и «номинал ноль» требуют от владельца разных действий (§4.9).
#[must_use]
pub fn initial_principal_from_terms(terms: Option<&IssueTermsRow>) -> Option<PerUnitAmount> {
    let terms = terms?;
    let value = terms.initial_face_value.as_ref()?.parse::<Decimal>().ok()?;
    let currency = CurrencyCode::new(terms.face_currency_code.as_ref()?).ok()?;
    Some(PerUnitAmount::new(Dec::new(value), currency))
}
```

- [ ] **Step 4: Подключить в сборку графика**

`crates/iaam-app/src/scenarios/reports.rs`, литерал `BondSchedule`
(строка ~264) — `terms` там уже прочитан строкой выше:

```rust
                    initial_principal: crate::market_candidate::initial_principal_from_terms(
                        terms.as_ref(),
                    ),
```

- [ ] **Step 5: Прогнать тесты**

Run: `nix develop -c cargo nextest run --workspace`
Expected: PASS.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-app/src/
git commit -m "feat(app): номинал выпуска доезжает до графика отчёта (iaam-d8b.15)"
```

---

### Task 11: Обогащение — вычисление доли в приложении

**Files:**
- Create: `crates/iaam-app/src/ingest/allocation.rs`
- Modify: `crates/iaam-ingest/src/journal_event.rs:80-100` (сигнатура `normalize_journal_event`)
- Modify: `crates/iaam-server/src/routes.rs:1251-1262` (перенос вызова в сценарий)
- Modify: `crates/iaam-app/src/scenarios/ingest.rs` (сценарий приёмки журнальных фактов)

**Interfaces:**
- Consumes: `BasisAllocation`, `AllocationGap`, `AllocationEvidence` (Task 4); `remaining_principal` (Task 2); `schedule_at_or_before` (`iaam-store`).
- Produces: `resolve_basis_allocation(fact: &JournalFact, schedule: Option<&BondSchedule>, knowledge_as_of: OffsetDateTime) -> BasisAllocation`; `normalize_journal_event(submitted, enrichment, context)`.

**Acceptance Criteria:**
- Возвраты одной даты агрегируются в одну долю: два возврата по 10% дают 20% от остатка на начало дня, а не 19%.
- Сумма события сверяется с плановой: расхождение хотя бы в минимальную единицу валюты даёт `AmountMismatch`.
- Отсутствующий, непроверенный график и отсутствие возврата на дату дают названные причины.
- Валюта возврата, не совпавшая с валютой номинала, даёт `CurrencyMismatch`.
- `iaam-ingest` не получает ни график, ни хранилище — только готовое значение.

- [ ] **Step 1: Написать падающие тесты**

В `crates/iaam-app/src/ingest/allocation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_repayments_on_one_date_are_aggregated_into_one_share() {
        // Остаток 1000, два возврата по 100 в один день. Доля обязана
        // быть 20% от остатка на начало дня, а не 10% + 10% от
        // убывающей базы, что дало бы 19%.
        let schedule = schedule(&[("2026-06-01", "10"), ("2026-06-01", "10")]);
        let allocation = resolve_basis_allocation(
            &amortisation_fact("2026-06-01", "200"),
            Some(&schedule),
            OffsetDateTime::UNIX_EPOCH,
        );
        assert_eq!(share_of(&allocation), dec("0.2"));
    }

    #[test]
    fn the_share_is_taken_from_the_remainder_before_the_event_not_from_the_original() {
        // 30% уже возвращены раньше; возврат 100 из остатка 700.
        let schedule = schedule(&[("2026-01-01", "30"), ("2026-06-01", "10")]);
        let allocation = resolve_basis_allocation(
            &amortisation_fact("2026-06-01", "100"),
            Some(&schedule),
            OffsetDateTime::UNIX_EPOCH,
        );
        assert_eq!(share_of(&allocation), dec("0.142857142857142857142857143"));
    }

    #[test]
    fn an_amount_that_disagrees_with_the_schedule_is_not_trusted() {
        let schedule = schedule(&[("2026-06-01", "10")]);
        let allocation = resolve_basis_allocation(
            &amortisation_fact("2026-06-01", "101"),
            Some(&schedule),
            OffsetDateTime::UNIX_EPOCH,
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::AmountMismatch)
        );
    }

    #[test]
    fn a_missing_schedule_names_its_reason() {
        let allocation = resolve_basis_allocation(
            &amortisation_fact("2026-06-01", "100"),
            None,
            OffsetDateTime::UNIX_EPOCH,
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::ScheduleMissing)
        );
    }

    #[test]
    fn a_date_without_a_scheduled_repayment_names_its_reason() {
        let schedule = schedule(&[("2026-06-01", "10")]);
        let allocation = resolve_basis_allocation(
            &amortisation_fact("2026-07-01", "100"),
            Some(&schedule),
            OffsetDateTime::UNIX_EPOCH,
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::NoRepaymentOnDate)
        );
    }

    #[test]
    fn a_currency_that_disagrees_with_the_face_value_names_its_reason() {
        let schedule = schedule(&[("2026-06-01", "10")]); // номинал в рублях
        let allocation = resolve_basis_allocation(
            &amortisation_fact_in_usd("2026-06-01", "100"),
            Some(&schedule),
            OffsetDateTime::UNIX_EPOCH,
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::CurrencyMismatch)
        );
    }
}
```

- [ ] **Step 2: Прогнать тесты**

Run: `nix develop -c cargo nextest run -p iaam-app allocation`
Expected: FAIL — функции нет.

- [ ] **Step 3: Реализовать вычисление**

```rust
//! Вычисление доли разнесения при приёмке амортизации.
//!
//! Живёт в слое приложения, а не в `iaam-ingest`: приёмка обязана
//! оставаться чистой функцией и не знать ни структуры справочника,
//! ни координаты знания, ни хранилища. В нормализатор уходит уже
//! готовое значение.

/// Доля возврата от остатка **до** события.
///
/// Возвраты одной даты агрегируются: две отдельные амортизации нельзя
/// применить каждую с долей от остатка на начало дня — 10% и 10% от
/// убывающей базы дают 19%, а не 20%. Источник различить их не даёт:
/// `source_entry_id` у MOEX всегда `None`.
pub fn resolve_basis_allocation(
    returned_per_unit: PerUnitAmount,
    on: Date,
    schedule: Option<&BondSchedule>,
    snapshot_id: &str,
    knowledge_as_of: OffsetDateTime,
) -> BasisAllocation {
    let Some(schedule) = schedule else {
        return BasisAllocation::Unknown(AllocationGap::ScheduleMissing);
    };
    if !matches!(schedule.completeness, ScheduleCompleteness::Validated) {
        return BasisAllocation::Unknown(AllocationGap::ScheduleNotValidated);
    }
    let Some(initial) = schedule.initial_principal else {
        return BasisAllocation::Unknown(AllocationGap::ScheduleMissing);
    };
    if initial.currency() != returned_per_unit.currency() {
        return BasisAllocation::Unknown(AllocationGap::CurrencyMismatch);
    }

    let hundred = Dec::new(Decimal::ONE_HUNDRED);
    let mut scheduled_on_date = Dec::zero();
    let mut repaid_before = Dec::zero();
    for item in &schedule.principal_returns {
        if !item.share_percent.is_positive() {
            return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
        }
        let target = if item.repayment_date == on {
            &mut scheduled_on_date
        } else if item.repayment_date < on {
            &mut repaid_before
        } else {
            continue;
        };
        match target.checked_add(item.share_percent) {
            Ok(sum) => *target = sum,
            Err(_) => return BasisAllocation::Unknown(AllocationGap::InvalidPrefix),
        }
    }

    if scheduled_on_date.is_zero() {
        return BasisAllocation::Unknown(AllocationGap::NoRepaymentOnDate);
    }
    let Ok(remaining_before) = hundred.checked_sub(repaid_before) else {
        return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
    };
    if !remaining_before.is_positive() {
        return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
    }

    // Сравниваются суммы, а не доли: доли графика приходят в процентах
    // с точностью источника, а возврат — деньги, округлённые до
    // минимальной единицы валюты. Расхождение хотя бы в одну единицу
    // означает другой возврат или брак источника — обе причины требуют
    // отказа, а не догадки.
    let Ok(planned) = initial
        .value()
        .checked_mul(scheduled_on_date)
        .and_then(|value| value.checked_div(hundred))
    else {
        return BasisAllocation::Unknown(AllocationGap::AmountMismatch);
    };
    if round_to_minor_unit(planned, initial.currency())
        != round_to_minor_unit(returned_per_unit.value(), returned_per_unit.currency())
    {
        return BasisAllocation::Unknown(AllocationGap::AmountMismatch);
    }

    let Ok(share_value) = scheduled_on_date.checked_div(remaining_before) else {
        return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
    };
    let Ok(share) = ReturnedShare::new(share_value) else {
        return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
    };

    BasisAllocation::Known {
        share,
        evidence: AllocationEvidence {
            inputs_hash: allocation_inputs_hash(
                initial,
                &schedule.principal_returns,
                on,
                snapshot_id,
            ),
            knowledge_as_of,
            algorithm_version: ALLOCATION_ALGORITHM_V1,
        },
    }
}

/// Отпечаток канонической выборки входов вычисления.
///
/// Покрывает ровно то, от чего зависит доля: номинал с валютой, все
/// возвраты (их даты и доли), дату события, идентичность снимка
/// источника и версию правила группировки одинаковых дат. Изменение
/// любого из них обязано менять отпечаток — иначе устаревшее evidence
/// будет выглядеть свежим.
fn allocation_inputs_hash(
    initial: PerUnitAmount,
    returns: &[PrincipalReturn],
    on: Date,
    snapshot_id: &str,
) -> AllocationInputsHash {
    let mut hasher = Sha256::new();
    hasher.update(b"iaam/allocation-inputs/v1");
    hasher.update(initial.value().inner().to_string().as_bytes());
    hasher.update(initial.currency().code().as_bytes());
    let mut ordered: Vec<&PrincipalReturn> = returns.iter().collect();
    ordered.sort_by_key(|item| (item.repayment_date, item.share_percent));
    for item in ordered {
        hasher.update(item.repayment_date.to_string().as_bytes());
        hasher.update(item.share_percent.inner().to_string().as_bytes());
    }
    hasher.update(on.to_string().as_bytes());
    hasher.update(snapshot_id.as_bytes());
    AllocationInputsHash::new(format!("{:x}", hasher.finalize()))
        .expect("SHA-256 всегда даёт 64 шестнадцатеричных знака")
}

/// Версия правила: агрегация возвратов одной даты, доля от остатка
/// до события, сверка суммы с точностью до минимальной единицы валюты.
const ALLOCATION_ALGORITHM_V1: AllocationAlgorithmVersion = AllocationAlgorithmVersion(1);
```

`round_to_minor_unit` — округление `Dec` до минимальной единицы валюты
той же конвенцией «половина к чётному», что и `split_basis`. Если такой
функции в проекте нет, найти существующую:
`nix develop -c grep -rn "MidpointNearestEven" crates/iaam-core/src/`
и вынести общий хелпер рядом с ней, а не заводить вторую конвенцию.

- [ ] **Step 4: Изменить сигнатуру нормализатора**

`crates/iaam-ingest/src/journal_event.rs`:

```rust
pub fn normalize_journal_event(
    submitted: &SubmittedJournalEvent,
    enrichment: &JournalEventEnrichment,
    context: NormalizationContext,
) -> Result<Normalized, Rejection>
```

`JournalEventEnrichment { pub basis_allocation: BasisAllocation }` —
в `iaam-ingest`, без зависимости от справочника. `raw_hash` по-прежнему
считается от `submitted` и обогащение в него **не входит**.

- [ ] **Step 5: Перенести вызов в сценарий**

`crates/iaam-server/src/routes.rs:1251` больше не строит `Event`: он
передаёт `SubmittedJournalEvent` в сценарий `iaam-app`, который читает
снимок графика (`schedule_at_or_before`), вызывает
`resolve_basis_allocation` и затем `normalize_journal_event`. Образец
правильной формы — приёмка операций, где приложение уже принимает
submitted-тип и нормализует само.

- [ ] **Step 6: Прогнать тесты**

Run: `nix develop -c cargo nextest run --workspace`
Expected: PASS.

- [ ] **Step 7: Коммит**

```bash
git add crates/iaam-app/src/ crates/iaam-ingest/src/ crates/iaam-server/src/routes.rs
git commit -m "feat(app): доля разнесения вычисляется приложением по графику (iaam-d8b.15)"
```

---

### Task 12: Транспорт — DTO, OpenAPI, контракт

**Files:**
- Modify: `crates/iaam-server/src/dto.rs:3327` (`CorporateActionDto::PartialRedemption`)
- Modify: `crates/iaam-server/src/dto.rs` (текст разрыва `BasisGap`)
- Modify: `crates/iaam-server/src/openapi.rs` (регистрация новых схем)
- Modify: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: `BasisAllocation`, `AllocationGap` (Task 4).
- Produces: входной DTO **без** поля доли; выходные DTO с кодом разрыва `amortisation_allocation_unknown`.

**Acceptance Criteria:**
- Входной DTO не принимает долю: клиент не может прислать налоговую долю в обход справочника.
- Ответ с неизвестной долей несёт код `amortisation_allocation_unknown`.
- OpenAPI-документ содержит новые схемы, все `$ref` разрешаются.

- [ ] **Step 1: Написать контрактный тест**

В `crates/iaam-server/tests/contract.rs`:

```rust
#[tokio::test]
async fn the_ingest_route_ignores_a_client_supplied_allocation() {
    // Доля разнесения — вывод приложения из справочника, а не
    // утверждение клиента. Присланное поле не должно попасть в журнал.
    let harness = harness();
    // Тело собрать из существующего теста приёмки амортизации
    // (`nix develop -c grep -n "partial_redemption" crates/iaam-server/tests/contract.rs`),
    // добавив в объект `action` лишний ключ `basis_allocation`.
    let mut action = валидная_амортизация(&harness);
    action["basis_allocation"] = json!({"state": "known", "share": "0.9"});
    let body = json!({"events": [{
        "account": harness.account,
        "type": "corporate_action",
        "action": action,
    }]});
    let (status, _) = call(&harness.router, post("/v1/ingest/journal-events", body)).await;
    assert_eq!(status, StatusCode::OK);
    let event = harness.last_event().await;
    assert!(matches!(
        allocation_of(&event),
        BasisAllocation::Unknown(_)
    ), "клиентская доля обязана быть проигнорирована");
}
```

- [ ] **Step 2: Прогнать тест**

Run: `nix develop -c cargo nextest run -p iaam-server the_ingest_route_ignores`
Expected: FAIL или ошибка сборки.

- [ ] **Step 3: Обновить DTO**

Поле доли во входной `CorporateActionDto::PartialRedemption` **не
добавлять**. `to_domain` подставляет `BasisAllocation::default()`.
Обновить текст разрыва в функции, печатающей `BasisGap`.

- [ ] **Step 4: Зарегистрировать схемы**

В `crates/iaam-server/src/openapi.rs` добавить в `components(schemas(...))`
типы, попавшие в выходные DTO. Проверить разрешимость ссылок пробой
из бида `iaam-xf5m`, если он к этому моменту закрыт.

- [ ] **Step 5: Прогнать тесты**

Run: `nix develop -c cargo nextest run -p iaam-server`
Expected: PASS.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-server/
git commit -m "feat(server): разрыв разнесения в транспорте, доля не принимается от клиента (iaam-d8b.15)"
```

---

### Task 13: Сквозные тесты без подмены CBOR

**Files:**
- Create: `crates/iaam-core/tests/bond_principal_end_to_end.rs`
- Modify: `crates/iaam-core/src/returns/mod.rs:4039` (удалить `состояние_с_номиналами`)

**Interfaces:**
- Consumes: всё, построенное задачами 1–12.

**Acceptance Criteria:**
- Облигация, заведённая обычным путём через журнал и справочник, даёт вычислимые метрики §7.1 и построенный поток **без подмены CBOR**. Это главный критерий бида.
- Покупка после состоявшихся амортизаций разносит стоимость от остатка на дату события.
- Амортизация без графика применяется, ставит разрыв и не даёт ноль.
- Хелпер `состояние_с_номиналами` удалён.

- [ ] **Step 1: Написать главный тест**

```rust
//! Сквозная проверка: номинал доезжает до расчёта обычным путём.
//!
//! Тест намеренно не подменяет CBOR-поля состояния. Прежний хелпер
//! `состояние_с_номиналами` подставлял номинал мимо рабочего пути,
//! и потому вся линия E3.4 годами проверялась на данных, которых
//! рабочий код никогда не увидит.

#[test]
fn a_bond_entered_the_ordinary_way_yields_computable_metrics() {
    let отчёт = отчёт_по_журналу_и_справочнику();
    assert!(отчёт.bond_metrics.iter().all(|m| m.prospective.value().is_some()));
    assert!(поток_построен(&отчёт));
}

#[test]
fn a_purchase_after_earlier_amortisations_allocates_from_the_remainder() {
    // Выпуск погашен на 30% до покупки. Возврат 10% от ПЕРВОНАЧАЛЬНОГО
    // номинала — это 1/7 остатка, а не 1/10. Отвергнутый вариант
    // «справочник в проекцию» посчитал бы здесь неверно.
    let отчёт = отчёт_с_покупкой_после_амортизаций();
    assert_eq!(освобождённая_стоимость(&отчёт), ожидаемая_седьмая_часть());
}

#[test]
fn an_amortisation_without_a_schedule_is_recorded_and_named_not_zeroed() {
    let отчёт = отчёт_с_амортизацией_без_графика();
    assert!(разрывы(&отчёт).contains(&BasisGap::AmortisationAllocationUnknown));
    assert_ne!(освобождённая_стоимость(&отчёт), ноль());
}
```

- [ ] **Step 2: Прогнать тесты**

Run: `nix develop -c cargo nextest run -p iaam-core --test bond_principal_end_to_end`
Expected: PASS (после задач 1–12).

- [ ] **Step 3: Удалить хелпер подмены**

```bash
nix develop -c grep -rn "состояние_с_номинал" crates/ --include=*.rs
```

Удалить хелпер и переписать использовавшие его тесты на обычный путь.
Если какой-то тест сознательно проверяет неизвестный номинал —
оставить его, но задать неизвестность через отсутствие
`initial_principal` в графике, а не подменой состояния.

- [ ] **Step 4: Полный заслон**

Run: `make check`
Expected: EXIT=0.

- [ ] **Step 5: Коммит**

```bash
git add crates/iaam-core/
git commit -m "test(core): облигация считается обычным путём, без подмены состояния (iaam-d8b.15)"
```

---

### Task 14: Заслоны

**Files:**
- Modify: `scripts/check-mutants.sh` (список модулей)

**Interfaces:**
- Consumes: модули, созданные задачами 1, 2, 11.

**Acceptance Criteria:**
- Новые модули (`rules/returned_share.rs`, `bond/principal.rs`, `ingest/allocation.rs`) внесены в мутационный заслон; порог прежний.
- Выживших мутантов в новых модулях нет.

**Внимание:** `scripts/check-mutants.sh` — файл политики, его правку
стережёт `check-diff-lint.sh`. Обоснование обязано быть в описании бида,
а PR — помечен `policy-change`.

- [ ] **Step 1: Внести модули в список**

Открыть `scripts/check-mutants.sh`, найти список модулей и добавить три
новых рядом с соседями по крейту.

- [ ] **Step 2: Прогнать заслон по новым модулям**

Run: `nix develop -c ./scripts/mutants-in-diff.sh main`
Expected: выживших нет. При выживших — дописать тест, убивающий
мутанта, а не ослаблять порог.

- [ ] **Step 3: Полный заслон**

Run: `make check`
Expected: EXIT=0.

- [ ] **Step 4: Коммит**

```bash
git add scripts/check-mutants.sh
git commit -m "chore(policy): новые модули номинала в мутационном заслоне (iaam-d8b.15)"
```

---

## Порядок и зависимости

```
T1 (ReturnedShare) ─┬─> T4 (BasisAllocation) ─> T5 (правило) ─┐
                    │                                          │
T2 (график+остаток) ─┼─> T6 (поток)  ─────────────────────────┤
                    └─> T7 (котировка) ────────────────────────┼─> T8 (удаление) ─> T9 (digest+версия)
T3 (положительность) ──────────────────────────────────────────┘
T2 ─> T10 (номинал из хранилища) ─> T11 (обогащение) ─> T12 (транспорт) ─> T13 (сквозные) ─> T14 (заслоны)
                                    ↑ T4
```

T1, T2, T3 независимы и могут идти параллельно. T13 требует всех
предыдущих. T14 — последняя.

## Покрытие спеки

| Требование спеки | Задача |
|---|---|
| §4.1 `initial_principal`, `remaining_principal`, доверие графику | T2 |
| §4.2 `CashflowInput` без `principal` | T6 |
| §4.3 котировка через остаток из графика | T7 |
| §4.4 `ReturnedShare`, инвариант, `Deserialize` | T1 |
| §4.4 положительность сырого возврата | T3 |
| §4.4 правило на доле | T5 |
| §4.5 переименование `BasisGap` | T5 |
| §5 `PROJECTION_VERSION` 7, `SCHEMA_VERSION` 4 | T9, T4 |
| §6.1 обогащение в приложении | T11 |
| §6.2 два хеша, `prefix_digest` v2 | T9 |
| §6.3 агрегация дат, сверка суммы | T11 |
| §6.4 типизированные причины | T4, T11 |
| §6.5 evidence | T4, T11 |
| §7 отказы §4.9 | T2, T5, T6, T7 |
| §8 транспорт, доля не от клиента | T12 |
| §9 тесты | во всех задачах, сквозные — T13 |
| §9 заслоны | T14 |
| §2 удаление `Lot.principal` и обеих функций | T8 |
| §10 follow-up биды | заводятся при закрытии эпика |
