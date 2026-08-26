# E3.4 часть 1 — журнал и проекция амортизации

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** журнал принимает амортизацию, погашение, замещение и исполнение оферты как типизированные факты, а проекция их применяет — так, что ни один принятый факт не остаётся молча проигнорированным.

**Architecture:** типы новых фактов вводятся отдельными модулями **до** включения в `EventKind`, и включение происходит одной задачей, которая закрывает все шесть исчерпывающих диспетчеров разом — иначе воркспейс не собирается посередине плана. Непогашенный номинал живёт в лоте одним типом с инвариантом. Отслеживание заявок по оферте — отдельная проекция, а не поля `LotBook`.

**Tech Stack:** Rust 2024, `rust_decimal`, `serde`, `cargo nextest`, `cargo-mutants`.

**Спека:** `.internal/specs/2026-08-26-e3-4-bonds-design.md`, разделы 3.1–3.11.
**Брейншторм:** `iaam-bx1`. **Родительский эпик:** `iaam-d8b`.
**Ревью плана:** проход `codex` (32 находки) — учтены в этой редакции.

## Global Constraints

- Rust edition 2024, `rust-version = 1.85`; `unsafe_code = "forbid"`, clippy `all = deny`.
- **Ни одного `_` в `match` по новым перечислениям.** Урок `iaam-d8b.1.4`: шесть мутантов выжили ровно на нетипизированном виде цены.
- **`unknown` — полноценное значение (§4.9).** Ни один неизвестный номинал или вид дохода не подменяется умолчанием.
- **Каждое добавляемое сериализуемое поле несёт `#[serde(default)]`.** Это касается и `Lot`: снимки проекций и архивы уже записаны без новых полей.
- **Каждый коммит оставляет `cargo build --workspace` зелёным.** Отсюда порядок: типы раньше вариантов `EventKind`.
- Округление денег — `MidpointNearestEven`, как в существующем `split_basis` (`rules/lot_disposal.rs:205`). Своя конвенция без обоснования — расхождение внутри одного ядра.
- **Файлы политики** (`scripts/`, `Cargo.toml`, `tests/fixtures`, `.cargo/mutants.toml`, `deny.toml`, `clippy.toml`, `.github/workflows`, `flake.*`, `rustfmt.toml`) правятся только с `POLICY_CHANGE_APPROVED=1` и меткой `policy-change` — это решение владельца, а не агента (`scripts/check-diff-lint.sh:80`).
- Комментарии и доккомментарии — по-русски, как во всём ядре.

## Решения, принятые этим планом сверх спеки

1. **Тип величины на единицу — `PerUnitAmount { value: Dec, currency }`, не `Money`.** `Money` хранит проведённые minor units; номинал выпуска — договорная расчётная величина (§3.4), и MOEX отдаёт `FACEVALUE` с точностью до четырёх знаков.
2. **Проекция целится по `(account, instrument)`, custody в событии — факт, а не ключ.** `LotKey` намеренно не различает место хранения: «перевод бумаги между депозитариями не является приобретением и не создаёт новой партии» (`projection/lots.rs:29`).
3. **Количество события сверяется, а не масштабируется.** Несовпадение с позицией — брак источника.
4. **Компенсация дробей при замещении из переносимой стоимости не вычитается.** Как она влияет на базу — налоговое правило, и оно в E5. Часть 1 сохраняет факт, а не решает за E5.
5. **Отслеживание заявок по оферте — отдельная проекция `OfferBook`,** а не новые поля `LotBook`.

## File Structure

| Файл | Ответственность |
|---|---|
| `crates/iaam-core/src/money.rs` | + `PerUnitAmount` |
| `crates/iaam-core/src/numeric/decimal.rs` | + `Dec::checked_div` |
| `crates/iaam-core/src/event/corporate_action.rs` | **новый** — семейство `CorporateAction` |
| `crates/iaam-core/src/event/offer.rs` | **новый** — семейство `OfferExerciseAction` |
| `crates/iaam-core/src/event/legs.rs` | **новый** — помощники структурной проверки ног |
| `crates/iaam-core/src/event/kind.rs` | + `IncomeKind`; + два варианта `EventKind` |
| `crates/iaam-core/src/event/mod.rs` | `SCHEMA_VERSION` 3→4; валидация новых вариантов |
| `crates/iaam-core/src/rules/amortisation.rs` | **новый** — правило распределения возвращённой стоимости |
| `crates/iaam-core/src/rules/lot_disposal.rs` | `Lot.principal`; `split_basis` переиспользуется |
| `crates/iaam-core/src/projection/lots.rs` | применение амортизации, погашения, замещения |
| `crates/iaam-core/src/projection/offers.rs` | **новый** — `OfferBook` |
| `crates/iaam-ingest/src/operation.rs` | `OperationKind::Income` получает вид |
| `crates/iaam-broker/src/tinkoff/parse.rs` | + виды операций амортизации и погашения |

---

### Task 1: `PerUnitAmount` и `Dec::checked_div`

**Files:**
- Modify: `crates/iaam-core/src/money.rs`, `crates/iaam-core/src/numeric/decimal.rs`
- Test: там же + `crates/iaam-core/tests/ui/`

**Interfaces:**
- Produces: `PerUnitAmount::{new, value, currency, checked_mul_quantity}`; `Dec::checked_div(Dec) -> Result<Dec, NumericError>`; `NumericError::DivisionByZero`

**Acceptance Criteria:**
- Величина на единицу и проведённая сумма — разные типы; сложить их нельзя (`tests/ui/`, как уже сделано для остальных несовместимостей).
- Умножение на количество даёт `Dec`, а не `Money`.
- Деление на ноль — ошибка, а не паника.

> Валютного инварианта в `PerUnitAmount` нет намеренно: вторую валюту метод не принимает. Валютная проверка появляется в `PrincipalState` (T4) и в правиле амортизации (T5).

- [ ] **Step 1: Написать падающие тесты**

`money.rs` держит в тестах помощник `rub(minor)`; помощника `dec("…")` там нет — добавить рядом:

```rust
fn dec(text: &str) -> Dec {
    Dec::new(Decimal::from_str_exact(text).unwrap())
}

#[test]
fn per_unit_amount_multiplied_by_quantity_stays_a_calculated_value() {
    let nominal = PerUnitAmount::new(dec("1000.0000"), CurrencyCode::Rub);
    assert_eq!(
        nominal.checked_mul_quantity(Quantity(dec("3"))).unwrap(),
        dec("3000.0000")
    );
}

#[test]
fn per_unit_amount_keeps_precision_finer_than_a_minor_unit() {
    assert_eq!(
        PerUnitAmount::new(dec("333.3333"), CurrencyCode::Rub).value(),
        dec("333.3333")
    );
}

#[test]
fn dividing_by_zero_is_an_error_not_a_panic() {
    assert_eq!(dec("1").checked_div(dec("0")), Err(NumericError::DivisionByZero));
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает**

Run: `cargo nextest run -p iaam-core per_unit_amount dividing_by_zero`
Expected: FAIL — `cannot find type PerUnitAmount`, `no method named checked_div`.

- [ ] **Step 3: Реализация**

```rust
/// Денежная величина **на одну единицу** — расчётная, а не проведённая.
///
/// Номинал выпуска и купон на бумагу договорные, а не списанные со счёта:
/// `Money` хранит minor units, и номинал 333.3333 в нём потерял бы два
/// знака. Отдельный тип не даёт сложить расчётную величину с проведённой
/// суммой — по §3.4 это разные вещи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerUnitAmount {
    value: Dec,
    currency: CurrencyCode,
}

impl PerUnitAmount {
    #[must_use]
    pub const fn new(value: Dec, currency: CurrencyCode) -> Self {
        Self { value, currency }
    }

    #[must_use]
    pub const fn value(&self) -> Dec {
        self.value
    }

    #[must_use]
    pub const fn currency(&self) -> CurrencyCode {
        self.currency
    }

    /// Всего по позиции. Возвращает `Dec`, а не `Money`: результат
    /// остаётся расчётным, пока его не провели по счёту.
    pub fn checked_mul_quantity(&self, quantity: Quantity) -> Result<Dec, NumericError> {
        self.value.checked_mul(quantity.0)
    }
}
```

```rust
// numeric/decimal.rs — по образцу соседних checked_add / checked_sub /
// checked_mul / checked_neg; деления там сегодня нет вовсе.
pub fn checked_div(self, other: Self) -> Result<Self, NumericError> {
    if other.0.is_zero() {
        return Err(NumericError::DivisionByZero);
    }
    self.0.checked_div(other.0).map(Self).ok_or(NumericError::Overflow)
}
```

- [ ] **Step 4: Прогнать.** Expected: PASS (3 теста).

- [ ] **Step 5: Коммит**

```bash
git add crates/iaam-core/src/money.rs crates/iaam-core/src/numeric/decimal.rs crates/iaam-core/tests/ui
git commit -m "feat(core): величина на единицу и деление с отказом на нуле (iaam-d8b)"
```

---

### Task 2: помощники структурной проверки ног

**Files:**
- Create: `crates/iaam-core/src/event/legs.rs`
- Modify: `crates/iaam-core/src/event/mod.rs`

**Interfaces:**
- Produces:
  - `LegExpectation { kind: LegKind, account: AccountId, instrument: Option<InstrumentId>, custody: Option<CustodyId>, money: Option<Money>, quantity: Option<Quantity> }`
  - `Event::expect_legs(name, &[LegExpectation]) -> Result<(), EventValidationError>` — **ровно** эти ноги, ни больше ни меньше
  - `EventValidationError::{UnexpectedLeg, MissingLeg, LegMismatch}`

**Acceptance Criteria:**
- Проверяется не только вид и сумма ноги, но **счёт, инструмент, место хранения и количество со знаком**.
- Лишняя нога отклоняется так же, как недостающая.

> Существующие помощники (`expect_single_cash`, `validate_trade`) сверяют вид, сумму и знак, но не инструмент и не custody. Без этого «одна нога `Principal`» пройдёт с ногой по другой бумаге, и заслон окажется декоративным.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn a_leg_naming_another_instrument_is_refused() {
    let event = with_legs(vec![Leg::principal(account, OTHER_INSTRUMENT, rub(100_000))]);
    assert!(matches!(
        event.expect_legs("x", &[principal_expectation(instrument, rub(100_000))]),
        Err(EventValidationError::LegMismatch { .. })
    ));
}

#[test]
fn an_extra_leg_is_refused_like_a_missing_one() {
    let event = with_legs(vec![
        Leg::principal(account, instrument, rub(100_000)),
        Leg::cash(account, rub(1)),
    ]);
    assert!(event.expect_legs("x", &[principal_expectation(instrument, rub(100_000))]).is_err());
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

Run: `cargo nextest run -p iaam-core expect_legs`

- [ ] **Step 3: Реализация**

```rust
/// Ожидание от одной ноги. Незаполненное поле не проверяется —
/// заполненное обязано совпасть.
#[derive(Debug, Clone, PartialEq)]
pub struct LegExpectation {
    pub kind: LegKind,
    pub account: AccountId,
    pub instrument: Option<InstrumentId>,
    pub custody: Option<CustodyId>,
    pub money: Option<Money>,
    pub quantity: Option<Quantity>,
}

impl Event {
    /// **Ровно** перечисленные ноги. Лишняя нога — такая же ошибка, как
    /// недостающая: событие с посторонним движением не является тем
    /// событием, которым назвалось.
    pub fn expect_legs(
        &self,
        name: &'static str,
        expected: &[LegExpectation],
    ) -> Result<(), EventValidationError> {
        if self.legs.len() != expected.len() {
            return Err(EventValidationError::UnexpectedLeg { event: name });
        }
        let mut unmatched: Vec<&Leg> = self.legs.iter().collect();
        for want in expected {
            let position = unmatched
                .iter()
                .position(|leg| matches_expectation(leg, want))
                .ok_or(EventValidationError::MissingLeg { event: name })?;
            unmatched.swap_remove(position);
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Прогнать.** Expected: PASS (2 теста).

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): проверка ног сверяет счёт, бумагу и место хранения (iaam-d8b)"
```

---

### Task 3: типы корпоративных действий — без включения в `EventKind`

**Files:**
- Create: `crates/iaam-core/src/event/corporate_action.rs`
- Modify: `crates/iaam-core/src/event/mod.rs` (только `pub mod corporate_action;`)

**Interfaces:**
- Consumes: T1
- Produces: `CorporateAction::{PartialRedemption, Redemption, Conversion}`; `FractionalTreatment`; `BasisTransferRule::{CarryOver, Restart}`; `CorporateAction::discriminant()`

**Acceptance Criteria:**
- Тип компилируется и переживает круг через JSON **до** появления варианта `EventKind` — сборка воркспейса остаётся зелёной.
- У каждого члена свои поля; общего мешка `Option`-полей нет.
- `effective_date` обязательна: это идентичность факта. `record_date` и `grounds` необязательны.

- [ ] **Step 1: Написать падающий тест круга**

```rust
#[test]
fn every_corporate_action_survives_a_json_round_trip() {
    for action in [sample_partial_redemption(), sample_redemption(), sample_conversion()] {
        let text = serde_json::to_string(&action).unwrap();
        assert_eq!(serde_json::from_str::<CorporateAction>(&text).unwrap(), action);
    }
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

```rust
//! Корпоративные действия — типизированное семейство (§4.7).
//!
//! Один универсальный `corporate_action` превратился бы в невалидируемый
//! JSON без инвариантов. Здесь у каждого члена свои поля, и `match` по
//! семейству исчерпывающий: новый член обязан сломать сборку.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CorporateAction {
    /// Амортизация: непогашенный номинал уменьшается, деньги приходят,
    /// **количество бумаг не меняется** (§6.5).
    PartialRedemption {
        instrument: InstrumentId,
        /// Место хранения — факт о выплате, а **не** ключ выборки лотов:
        /// `LotKey` намеренно не различает депозитарии (lots.rs:29).
        custody: CustodyId,
        /// Количество, которого касается выплата. Проекция его сверяет,
        /// а не масштабирует по нему номинал.
        quantity: Quantity,
        principal_returned_per_unit: PerUnitAmount,
        /// Денежная компенсация `A`, фактически поступившая владельцу.
        compensation: Money,
        effective_date: Date,
        record_date: Option<Date>,
        grounds: Option<String>,
    },
    /// Окончательное погашение: номинал возвращён целиком и бумага
    /// выбывает из позиции. Обнулить остаток и оставить количество —
    /// позиция из погашенных бумаг, которой не существует.
    Redemption {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Quantity,
        principal_returned_per_unit: PerUnitAmount,
        compensation: Money,
        effective_date: Date,
        record_date: Option<Date>,
        grounds: Option<String>,
    },
    /// Замещение: бумага предшественника меняется на бумагу преемника.
    ///
    /// Поля подобраны так, чтобы E5 посчитал перенос налоговой стоимости
    /// и срока владения, ничего не угадывая (§16.1). Правило переноса
    /// хранится в самом факте: вывести его позже будет нечем.
    Conversion {
        predecessor: InstrumentId,
        successor: InstrumentId,
        custody: CustodyId,
        /// Сколько бумаг преемника на одну бумагу предшественника.
        ratio: Dec,
        quantity_in: Quantity,
        quantity_out: Quantity,
        fractional: FractionalTreatment,
        /// Компенсация дробей. Как она влияет на налоговую базу —
        /// правило E5; часть 1 её только сохраняет.
        compensation: Option<Money>,
        effective_date: Date,
        record_date: Option<Date>,
        grounds: Option<String>,
        basis_transfer: BasisTransferRule,
    },
}

/// Что сделали с дробной частью при замещении.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FractionalTreatment {
    /// Дробь выкуплена деньгами.
    CashCompensated,
    /// Дробь отброшена вниз без компенсации.
    RoundedDown,
    /// Дроби не возникло.
    NotApplicable,
}

/// Правило переноса налоговой стоимости и срока владения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisTransferRule {
    /// Стоимость и срок владения переходят на преемника целиком.
    CarryOver,
    /// Замещение приравнено к продаже и покупке: срок начинается заново.
    Restart,
}
```

- [ ] **Step 4: Прогнать.** Expected: PASS. `cargo build --workspace` зелёный — вариантов `EventKind` пока нет.

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): типы корпоративных действий (iaam-d8b)"
```

---

### Task 4: `PrincipalState` в лоте

**Files:**
- Modify: `crates/iaam-core/src/rules/lot_disposal.rs`

**Interfaces:**
- Consumes: T1
- Produces: `PrincipalState::{Unknown, Known { original_per_unit, remaining_per_unit }}`, `PrincipalState::{known, reduced_by}`, `PrincipalError::{RemainingAboveOriginal, CurrencyMismatch, Negative}`, `impl Default for PrincipalState`; поле `Lot::principal`

**Acceptance Criteria:**
- Конструктор отклоняет разные валюты, отрицательный остаток и остаток больше первоначального.
- Размерность — **на единицу**.
- **`#[serde(default)]` на поле и `Default = Unknown`:** уже записанные снимки проекций и архивы этого поля не содержат и обязаны читаться. Без этого старый архив перестанет открываться — прямое нарушение Global Constraints.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn a_remaining_principal_above_the_original_is_refused() {
    assert_eq!(
        PrincipalState::known(per_unit("1000"), per_unit("1200")).unwrap_err(),
        PrincipalError::RemainingAboveOriginal
    );
}

#[test]
fn principal_in_two_currencies_is_refused() {
    assert!(PrincipalState::known(rub_per_unit("1000"), usd_per_unit("500")).is_err());
}

#[test]
fn a_lot_written_before_principal_existed_reads_as_unknown() {
    // Снимок проекции записан до E3.4 — поля нет.
    let value = lot_json_without_principal();
    assert_eq!(
        serde_json::from_value::<Lot>(value).unwrap().principal,
        PrincipalState::Unknown
    );
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

```rust
/// Состояние непогашенного номинала лота (§6.5).
///
/// Величины — **на одну бумагу**: непогашенный номинал лота равен
/// `quantity × remaining_per_unit`, и частичное списание лота ничего не
/// пересчитывает. При размерности «на лот» каждое списание требовало бы
/// пересчёта.
///
/// Один тип вместо двух `Option`: два независимых поля допускали бы
/// «номинал неизвестен, остаток известен», разные валюты и остаток
/// больше первоначального — состояния, которых не бывает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PrincipalState {
    /// Номинал неизвестен: бумага заведена до того, как справочник его
    /// узнал. Подставлять ноль запрещено (§4.9).
    #[default]
    Unknown,
    Known {
        original_per_unit: PerUnitAmount,
        remaining_per_unit: PerUnitAmount,
    },
}

// в Lot:
    /// `#[serde(default)]` обязателен: снимки проекций и архивы записаны
    /// до E3.4 и этого поля не содержат.
    #[serde(default)]
    pub principal: PrincipalState,
```

- [ ] **Step 4: Прогнать.** Expected: PASS (3 теста).

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): непогашенный номинал лота одним типом с инвариантом (iaam-d8b)"
```

---

### Task 5: правило распределения возвращённой стоимости

**Files:**
- Create: `crates/iaam-core/src/rules/amortisation.rs`
- Modify: `crates/iaam-core/src/rules/mod.rs`, `crates/iaam-core/src/rules/lot_disposal.rs`

**Interfaces:**
- Consumes: T1, T4
- Produces: `AmortisationRuleVersion(u32)`, трейт `AmortisationRule`, `ProRataV1`, `AmortisationError::{UnknownPrincipal, CurrencyMismatch, Disposal}`; `RuleRegistry::amortisation_rule(version)`; `split_basis` становится `pub(crate)`

**Acceptance Criteria:**
- Отдельная карта правил и отдельная версия — **не** расширение `LotDisposalRule`.
- Доля считается от номинала **до события**.
- Округление — тем же `split_basis`, что и списание лотов: он уже решает задачу «доля от суммы» с конвенцией `MidpointNearestEven`. Своя конвенция внутри одного ядра недопустима.
- `PrincipalState::Unknown` даёт `UnknownPrincipal`, валютное расхождение — `CurrencyMismatch`; ни то, ни другое не подставляет ноль.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn basis_returned_is_proportional_to_the_principal_before_the_event() {
    // Номинал 1000, возвращено 200 -> пятая часть стоимости 100 000.
    assert_eq!(ProRataV1.basis_returned(&lot_1000, per_unit("200")).unwrap(), rub(20_000));
}

#[test]
fn a_lot_bought_between_amortisations_uses_its_own_remaining_principal() {
    // Лот куплен уже амортизированным: доля от 800, а не от 1000.
    assert_eq!(ProRataV1.basis_returned(&lot_800, per_unit("200")).unwrap(), rub(25_000));
}

#[test]
fn an_unknown_principal_refuses_instead_of_guessing() {
    assert_eq!(
        ProRataV1.basis_returned(&lot_unknown, per_unit("200")).unwrap_err(),
        AmortisationError::UnknownPrincipal
    );
}

#[test]
fn a_nominal_currency_other_than_the_basis_currency_refuses() {
    assert_eq!(
        ProRataV1.basis_returned(&lot_usd_nominal, rub_per_unit("200")).unwrap_err(),
        AmortisationError::CurrencyMismatch
    );
}

#[test]
fn rounding_follows_the_same_convention_as_lot_disposal() {
    // Половина копейки уходит к чётному — как в split_basis.
    assert_eq!(ProRataV1.basis_returned(&lot_odd, per_unit("1")).unwrap(), rub(2));
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

```rust
/// Правило распределения возвращённой стоимости при амортизации (§6.5).
///
/// Отдельный трейт и отдельная версия, а не расширение
/// `LotDisposalRule`: списание лотов — выбор владельца, амортизация —
/// событие выпуска, и общий номер связал бы два независимых решения.
pub trait AmortisationRule: Send + Sync + std::fmt::Debug {
    fn basis_returned(
        &self,
        lot: &Lot,
        returned_per_unit: PerUnitAmount,
    ) -> Result<Money, AmortisationError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AmortisationRuleVersion(pub u32);

/// Доля стоимости, пропорциональная доле возвращённого номинала.
#[derive(Debug, Default)]
pub struct ProRataV1;

impl AmortisationRule for ProRataV1 {
    fn basis_returned(
        &self,
        lot: &Lot,
        returned_per_unit: PerUnitAmount,
    ) -> Result<Money, AmortisationError> {
        let PrincipalState::Known { remaining_per_unit, .. } = lot.principal else {
            // Ноль означал бы «амортизация ничего не вернула» — неправда.
            return Err(AmortisationError::UnknownPrincipal);
        };
        if remaining_per_unit.currency() != returned_per_unit.currency()
            || remaining_per_unit.currency() != lot.cost_basis.currency()
        {
            // Пересчёт по случайному курсу хуже отказа.
            return Err(AmortisationError::CurrencyMismatch);
        }
        // Знаменатель — номинал ДО события. Округление и обрезка живут в
        // `split_basis`: она уже решает ровно эту задачу «доля от суммы».
        Ok(split_basis(
            lot.cost_basis,
            returned_per_unit.value().into_inner(),
            remaining_per_unit.value().into_inner(),
        )?)
    }
}
```

- [ ] **Step 4: Прогнать.** Expected: PASS (5 тестов).

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): версионированное правило амортизационной базы (iaam-d8b)"
```

---

### Task 6: семейство оферты и проекция заявок

**Files:**
- Create: `crates/iaam-core/src/event/offer.rs`, `crates/iaam-core/src/projection/offers.rs`

**Interfaces:**
- Produces: `OfferSubmissionId(Uuid)`, `OfferWindowId(Uuid)`, `OfferExerciseAction::{Submitted, Cancelled, Settled}`, `OfferBook::{apply, outstanding}`, `OfferError::OverSettled`

**Acceptance Criteria:**
- Три члена, а не два: §3.5 спеки называет отмену наряду с частичным исполнением.
- Сумма исполненных количеств по одной заявке не превосходит заявленного с учётом отмены — инвариант **цепочки**, поэтому живёт в проекции, а не в `validate_structure`.
- `OfferBook` — отдельная проекция; `LotBook` новых полей не получает, и его снимок не меняется.
- `OfferWindowId` здесь — непрозрачная идентичность. Реестра окон в части 1 нет; проверка «окно существует» откладывается в E3.4.6 **явно**, а не забывается.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn settlements_cannot_exceed_the_submitted_quantity() {
    let mut book = OfferBook::default();
    book.apply(&submitted(qty(10))).unwrap();
    book.apply(&settled(qty(6))).unwrap();
    assert!(matches!(book.apply(&settled(qty(5))), Err(OfferError::OverSettled { .. })));
}

#[test]
fn a_partial_settlement_leaves_the_rest_outstanding() {
    let mut book = OfferBook::default();
    book.apply(&submitted(qty(10))).unwrap();
    book.apply(&settled(qty(6))).unwrap();
    assert_eq!(book.outstanding(submission), qty(4));
}

#[test]
fn a_cancellation_frees_the_outstanding_quantity() {
    let mut book = OfferBook::default();
    book.apply(&submitted(qty(10))).unwrap();
    book.apply(&cancelled(qty(10))).unwrap();
    assert_eq!(book.outstanding(submission), qty(0));
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

```rust
//! Исполнение оферты (§4.7).
//!
//! Оферта **не** корпоративное действие: это право владельца, а не
//! решение эмитента. Свести выкуп к `Redemption` значило бы потерять и
//! происхождение выбытия, и сценарность выбора.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OfferExerciseAction {
    /// Поданная заявка. Ног не имеет: ни денег, ни бумаг не двигает —
    /// как `ControlAssertion`.
    Submitted {
        submission: OfferSubmissionId,
        window: OfferWindowId,
        instrument: InstrumentId,
        quantity: Quantity,
    },
    /// Отзыв заявки целиком или частично.
    Cancelled {
        submission: OfferSubmissionId,
        quantity: Quantity,
    },
    /// Совершённый выкуп. Ноги — `Cash` и отрицательная
    /// `SecurityQuantity`; ноги `Principal` нет: бумага выбывает, а не
    /// возвращает номинал. Расчётов по одной заявке бывает несколько.
    Settled {
        submission: OfferSubmissionId,
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Quantity,
        gross: Money,
        fee: Option<Money>,
        accrued_interest: Option<Money>,
    },
}
```

- [ ] **Step 4: Прогнать.** Expected: PASS (3 теста).

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): оферта как право владельца с отдельной проекцией заявок (iaam-d8b)"
```

---

### Task 7: точка включения — варианты `EventKind` и все шесть диспетчеров разом

**Files:**
- Modify: `crates/iaam-core/src/event/kind.rs`, `crates/iaam-core/src/event/mod.rs`, `crates/iaam-app/src/jobs.rs`, `crates/iaam-app/src/scenarios/classification.rs`, `crates/iaam-ingest/src/classification.rs`, `crates/iaam-server/src/dto.rs`

**Interfaces:**
- Consumes: T2, T3, T6
- Produces: `EventKind::{CorporateAction { action }, OfferExercise { action }}`; `IncomeKind::{Coupon, Dividend, DepositInterest}`; `EventKind::Income { …, kind: Option<IncomeKind> }`; `SCHEMA_VERSION = 4`

**Acceptance Criteria:**
- Воркспейс собирается только после закрытия **всех шести** диспетчеров; ни один не закрыт через `_`.
- `SCHEMA_VERSION` равна 4.
- Уже записанный `Income` без поля читается как `kind: None` — «не утверждалось», а не «дивиденд».
- Варианта `IncomeKind::Other` нет: мешок, по которому нельзя принять решение, не отличается от незнания, а его выражает `None`.

**Шесть диспетчеров и решения за них:**

| Место | Решение |
|---|---|
| `event/kind.rs::discriminant` (`:211`) | `"corporate_action"`, `"offer_exercise"` |
| `event/mod.rs::validate_structure` (`:161`) | ветви T8 |
| `event/kind.rs::flow_endpoints` (`:233`) | **`WithinAccount`** для всех новых вариантов. Деньги не приходят в контур извне: бумага уже внутри. Так же там классифицирован купон. `InboundFromOutside` завысил бы внесённое в контур и испортил XIRR |
| `iaam-app/src/jobs.rs:151` | амортизация — **нулевая** дельта количества; погашение — отрицательная; замещение — пара дельт по двум инструментам |
| `iaam-app/src/scenarios/classification.rs:163` | `(Counterparty::Unknown, Movement::In)`, как у `Income` |
| `iaam-ingest/src/classification.rs:264` | ⚠️ **`Classification::Income` неверна.** Амортизация — возврат собственного капитала (§6.5); отнести её к доходу значит завысить доход на весь возвращённый номинал. Нужен отдельный вариант либо `None` |

- [ ] **Step 1: Написать падающие тесты**

Форму JSON **не сочинять.** У `EventKind` нет `rename_all`, поэтому вариант сериализуется как `Income`, а не `income`; `CurrencyCode` — как `Rub`, а не `"RUB"`; `Money` содержит `amount`, а не `minor`. Совместимую строку надо получить из текущего кода:

```rust
#[test]
fn an_income_written_before_the_kind_existed_reads_as_not_asserted() {
    // Значение снимается с сегодняшнего Income и лишается поля kind —
    // ровно то, что лежит в уже записанном журнале.
    let mut value = serde_json::to_value(&income_with_kind(IncomeKind::Coupon)).unwrap();
    strip_field(&mut value, "kind");
    let restored: EventKind = serde_json::from_value(value).unwrap();
    assert!(matches!(restored, EventKind::Income { kind: None, .. }));
}

#[test]
fn amortisation_is_not_classified_as_income() {
    // Ошибка правдоподобна и молчалива: она завысила бы доход на всю
    // сумму возвращённого номинала.
    assert_ne!(classify(&amortisation_event()), Some(Classification::Income));
}

#[test]
fn amortisation_stays_within_the_account() {
    assert_eq!(amortisation_kind().flow_endpoints(), FlowEndpoints::WithinAccount);
}

#[test]
fn amortisation_does_not_change_the_position_count_job() {
    assert_eq!(quantity_delta(&amortisation_event()), Some(Quantity::zero()));
}

#[test]
fn schema_version_moved_to_four() {
    assert_eq!(SCHEMA_VERSION, 4);
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация** вариантов, `IncomeKind` и всех шести ветвей.

```rust
/// Вид выплаченного дохода.
///
/// Варианта `Other` нет намеренно: мешок, по которому нельзя принять
/// решение, не отличается от незнания, а §4.9 требует именно различимого
/// незнания — его выражает `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncomeKind {
    /// Купон по облигации.
    Coupon,
    /// Дивиденд по долевой бумаге.
    Dividend,
    /// Выплаченные проценты по вкладу (на него обопрётся E3.5).
    DepositInterest,
}
```

- [ ] **Step 4: Прогнать весь воркспейс.** Run: `make test`
Expected: зелено. Механическая адаптация конструкторов `Income` разрешена; **утверждения существующих тестов не менять**.

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): новые факты включены в EventKind, SCHEMA_VERSION 4 (iaam-d8b)"
```

---

### Task 8: структурная валидация новых фактов

**Files:**
- Modify: `crates/iaam-core/src/event/mod.rs`

**Interfaces:**
- Consumes: T2, T7

**Acceptance Criteria:**
- Амортизация: **одна** нога `Principal` на сумму компенсации, того же счёта и той же бумаги. Ног `Cash` и `SecurityQuantity` нет — «количество бумаг не уменьшается» становится инвариантом.
- Погашение: `Principal` **и** нога количества ровно `−quantity`, та же бумага, тот же счёт.
- Замещение: отрицательная нога по предшественнику, положительная по преемнику, обе в одном счёте; `Cash` только при `compensation = Some`; `ratio` сверяется с парой количеств.
- Заявка и отзыв по оферте ног не имеют; выкуп несёт `Cash` и отрицательную `SecurityQuantity`, но **не** `Principal`.

**Почему у амортизации одна нога.** `LegKind::Principal` уже входит в `cash_effect()` (`event/leg.rs:112`), и тест `every_money_bearing_kind_counts_as_cash_effect:199` это закрепляет. Пара «Cash + Principal» дала бы двойной денежный эффект.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn amortisation_carries_one_principal_leg_and_nothing_else() {
    assert!(amortisation_event(vec![Leg::principal(account, instrument, rub(100_000))])
        .validate_structure()
        .is_ok());
}

#[test]
fn amortisation_with_a_security_quantity_leg_is_rejected() {
    // §6.5: амортизация выплачивает деньги, но количество не меняет.
    assert!(amortisation_event(vec![
        Leg::principal(account, instrument, rub(100_000)),
        Leg::security(account, custody, instrument, qty(-10)),
    ]).validate_structure().is_err());
}

#[test]
fn amortisation_with_a_cash_leg_is_rejected() {
    // Principal уже денежная нога: пара дала бы двойной эффект.
    assert!(amortisation_event(vec![
        Leg::principal(account, instrument, rub(100_000)),
        Leg::cash(account, rub(100_000)),
    ]).validate_structure().is_err());
}

#[test]
fn a_principal_leg_for_another_bond_is_rejected() {
    assert!(amortisation_event(vec![Leg::principal(account, OTHER, rub(100_000))])
        .validate_structure()
        .is_err());
}

#[test]
fn final_redemption_without_a_security_leg_is_rejected() {
    // Обнулить номинал и оставить количество — позиция из погашенных
    // бумаг, которой не существует.
    assert!(redemption_event(vec![Leg::principal(account, instrument, rub(1_000_000))])
        .validate_structure()
        .is_err());
}

#[test]
fn a_settled_offer_has_no_principal_leg() {
    assert!(offer_settled_event(vec![
        Leg::cash(account, rub(1_000_000)),
        Leg::security(account, custody, instrument, qty(-10)),
        Leg::principal(account, instrument, rub(1)),
    ]).validate_structure().is_err());
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация** через `expect_legs` из T2.

```rust
CorporateAction::PartialRedemption {
    instrument, quantity: _, compensation, ..
} => self.expect_legs(
    name,
    &[LegExpectation {
        kind: LegKind::Principal,
        account: self.account,
        instrument: Some(*instrument),
        custody: None,
        money: Some(*compensation),
        quantity: None,
    }],
),
```

- [ ] **Step 4: Прогнать.** Expected: PASS (6 тестов).

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): ноги новых фактов проверяются по счёту, бумаге и знаку (iaam-d8b)"
```

---

### Task 9: проекция лотов применяет новые факты

**Files:**
- Modify: `crates/iaam-core/src/projection/lots.rs`

**Interfaces:**
- Consumes: T4, T5, T7
- Produces: ветви `LotBook::apply`; `BasisGap::PrincipalUnknown`; `LotError::{QuantityMismatch, UnknownAmortisationRule { version: AmortisationRuleVersion }}`; поле `LotBook::amortisation_version`; `InstrumentLots::lots_mut`

**Acceptance Criteria:**
- Амортизация уменьшает `remaining_per_unit` лотов **этого счёта и этой бумаги** и не меняет `quantity`.
- **Количество события сверяется с позицией**; несовпадение — `QuantityMismatch`, а не молчаливое масштабирование.
- **Неизвестный номинал не роняет проекцию:** факт применяется, а реализованный результат становится `BasisGap::PrincipalUnknown` — тем же механизмом, каким уже выражена нехватка стоимости у восстановленной позиции (`BasisGap::RestoredWithoutBasis`).
- **Применение атомарно:** ошибка на втором лоте не оставляет первый изменённым.
- `Realised_amort = compensation − Σ BasisReturned`; при равенстве нулевой.
- Ни одного `_` в диспетчере.

> `LotError::UnknownRule` типизирован под `LotRuleVersion` и для версии амортизационного правила не подходит — нужен отдельный вариант. Метод списания называется `dispose`, а не `apply_disposal`. Поле `InstrumentLots::lots` приватное: доступ внутри модуля есть, но метод-аксессор честнее.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn amortisation_reduces_principal_and_leaves_the_quantity_alone() {
    let mut book = book_with_bond(qty(10), per_unit("1000"), rub(1_000_000));
    book.apply(&amortisation(qty(10), per_unit("200"), rub(200_000)), &rules).unwrap();
    let lots = book.entry(&key).unwrap();
    assert_eq!(lots.quantity().unwrap(), qty(10));
    assert_eq!(remaining(lots), per_unit("800"));
}

#[test]
fn an_amortisation_for_a_different_quantity_is_an_error_not_a_scaling() {
    // Амортизация касается всех бумаг на счёте. Несовпадение — брак
    // источника, а не повод уменьшить номинал пропорционально.
    let mut book = book_with_bond(qty(10), per_unit("1000"), rub(1_000_000));
    assert!(matches!(
        book.apply(&amortisation(qty(4), per_unit("200"), rub(80_000)), &rules),
        Err(LotError::QuantityMismatch { .. })
    ));
}

#[test]
fn an_amortisation_returning_exactly_the_basis_realises_nothing() {
    // §6.5: возврат собственного капитала доходом не является.
    assert_eq!(apply_and_take_realised(rub(200_000), rub(200_000)), Dec::zero());
}

#[test]
fn an_unknown_principal_records_a_basis_gap_instead_of_failing() {
    let mut book = book_with_bond_of_unknown_principal(qty(10), rub(1_000_000));
    book.apply(&amortisation(qty(10), per_unit("200"), rub(200_000)), &rules).unwrap();
    assert_eq!(book.entry(&key).unwrap().basis_gap(), Some(BasisGap::PrincipalUnknown));
}

#[test]
fn a_failure_on_the_second_lot_leaves_the_first_untouched() {
    let mut book = book_with_two_lots_second_in_another_currency();
    let before = book.clone();
    assert!(book.apply(&amortisation(qty(20), per_unit("200"), rub(400_000)), &rules).is_err());
    assert_eq!(book, before);
}

#[test]
fn an_amortisation_on_another_account_leaves_this_book_alone() {
    let mut book = book_with_bond(qty(10), per_unit("1000"), rub(1_000_000));
    book.apply(&amortisation_on_other_account(), &rules).unwrap();
    assert_eq!(remaining(book.entry(&key).unwrap()), per_unit("1000"));
}

#[test]
fn the_projection_uses_the_actual_payment_not_the_announced_schedule() {
    // Фактическая выплата меньше объявленной: берётся факт.
    let mut book = book_with_bond(qty(10), per_unit("1000"), rub(1_000_000));
    book.apply(&amortisation(qty(10), per_unit("150"), rub(150_000)), &rules).unwrap();
    assert_eq!(remaining(book.entry(&key).unwrap()), per_unit("850"));
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

```rust
/// Амортизация: остаток номинала уменьшается, количество — нет (§6.5).
///
/// Целимся по `(account, instrument)`: `LotKey` намеренно не различает
/// место хранения, и custody из события — факт о выплате, а не ключ.
fn apply_amortisation(
    &mut self,
    event: &Event,
    facts: AmortisationFacts,
    rules: &RuleRegistry,
) -> Result<(), LotError> {
    let key = LotKey { account: event.account, instrument: facts.instrument };
    let Some(lots) = self.entries.get(&key) else {
        return Err(LotError::SaleWithoutPosition {
            event: event.id,
            instrument: facts.instrument,
        });
    };
    // Событие касается всех бумаг на счёте: расхождение — брак источника.
    let held = lots.quantity()?;
    if held != facts.quantity {
        return Err(LotError::QuantityMismatch { held, declared: facts.quantity });
    }
    let rule = rules
        .amortisation_rule(self.amortisation_version)
        .ok_or(LotError::UnknownAmortisationRule { version: self.amortisation_version })?;

    // Считаем на копии и подменяем целиком: иначе отказ на втором лоте
    // оставит первый уже изменённым.
    let mut next = lots.clone();
    let mut returned_total = Money::zero(facts.compensation.currency());
    for lot in next.lots_mut() {
        match rule.basis_returned(lot, facts.returned_per_unit) {
            Ok(returned) => {
                lot.cost_basis = lot.cost_basis.try_sub(returned)?;
                returned_total = returned_total.try_add(returned)?;
            }
            // Номинал неизвестен — факт всё равно применяется, а
            // реализованный результат становится невычислимым (§4.9).
            Err(AmortisationError::UnknownPrincipal) => {
                next.mark_basis_gap(BasisGap::PrincipalUnknown);
            }
            Err(other) => return Err(other.into()),
        }
        lot.principal = lot.principal.reduced_by(facts.returned_per_unit)?;
    }
    next.add_realised(facts.compensation.try_sub(returned_total)?)?;
    self.entries.insert(key, next);
    Ok(())
}
```

- [ ] **Step 4: Прогнать весь воркспейс.** Run: `make test`

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): книга лотов применяет амортизацию, погашение и замещение (iaam-d8b)"
```

---

### Task 10: достаточность факта замещения

**Files:**
- Create: `crates/iaam-core/tests/conversion_fact_is_sufficient.rs`
- Modify: `crates/iaam-core/src/projection/lots.rs`

**Interfaces:**
- Consumes: T3, T9
- Produces: применение `Conversion` к книге лотов

**Acceptance Criteria:**
- Тест стартует **с лотов предшественника и события** и получает лоты преемника — иначе он доказывал бы достаточность правила, а не факта.
- Срок владения при `CarryOver` не обнуляется; при `Restart` начинается с `effective_date`.
- Компенсация дробей из переносимой стоимости **не вычитается**: как она влияет на базу — правило E5, и решать за него часть 1 не вправе.

> Это единственная задача, чей провал означает необратимую потерю: если тест не пишется, факт недостаточен, а дописать поля в уже записанные события нельзя (§16.1).

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn successor_lots_are_derived_from_predecessor_lots_and_the_event_alone() {
    let mut book = book_with_lots(predecessor, qty(10), rub(1_000_000), acquired(2024, 3, 1));
    book.apply(&conversion_event(ratio("1"), BasisTransferRule::CarryOver), &rules).unwrap();
    let successor_lots = book.entry(&successor_key).unwrap();
    assert_eq!(successor_lots.remaining_basis().unwrap(), Some(rub(1_000_000)));
    assert_eq!(acquired_of(successor_lots), Some(acquired(2024, 3, 1)));
    assert!(book.entry(&predecessor_key).unwrap().quantity().unwrap().0.is_zero());
}

#[test]
fn a_restart_rule_starts_the_holding_period_at_the_effective_date() {
    let mut book = book_with_lots(predecessor, qty(10), rub(1_000_000), acquired(2024, 3, 1));
    book.apply(&conversion_event(ratio("1"), BasisTransferRule::Restart), &rules).unwrap();
    assert_eq!(acquired_of(book.entry(&successor_key).unwrap()), Some(effective_date()));
}

#[test]
fn a_cash_compensated_fraction_does_not_silently_reduce_the_basis() {
    // Как компенсация влияет на базу — правило E5. Часть 1 её хранит.
    let mut book = book_with_lots(predecessor, qty(10), rub(1_000_000), acquired(2024, 3, 1));
    book.apply(&conversion_with_compensation(rub(500)), &rules).unwrap();
    assert_eq!(
        book.entry(&successor_key).unwrap().remaining_basis().unwrap(),
        Some(rub(1_000_000))
    );
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация.**

- [ ] **Step 4: Прогнать.** Expected: PASS (3 теста).

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): замещение сохраняет достаточный факт для переноса базы (iaam-d8b)"
```

---

### Task 11: вид дохода доходит из канала до журнала и до API

**Files:**
- Modify: `crates/iaam-ingest/src/operation.rs`, `crates/iaam-server/src/dto.rs`, `crates/iaam-app/src/adapters/tinkoff.rs`

**Interfaces:**
- Consumes: T7
- Produces: `OperationKind::Income { …, kind: Option<IncomeKind> }`; `IncomeKindDto`

**Acceptance Criteria:**
- `iaam-ingest` использует **core-овский** `IncomeKind`, а не собственное перечисление: крейта уже зависит от `iaam-core`, а второй тип пришлось бы отображать в первый и терять на этом.
- Схлопывание `Dividend | Coupon` в `adapters/tinkoff.rs:114` снято.
- `OperationKindDto::Income` тоже несёт вид: без этого API продолжит его терять при уже умеющем хранить журнале.
- Незнакомый вид операции по-прежнему отклоняется, а не превращается в приход денег.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn a_coupon_reaches_the_journal_as_a_coupon() {
    let submitted = operation_to_submitted(account, coupon_operation()).unwrap();
    assert!(matches!(
        submitted.kind,
        OperationKind::Income { kind: Some(IncomeKind::Coupon), .. }
    ));
}

#[test]
fn a_dividend_does_not_become_a_coupon() {
    let submitted = operation_to_submitted(account, dividend_operation()).unwrap();
    assert!(matches!(
        submitted.kind,
        OperationKind::Income { kind: Some(IncomeKind::Dividend), .. }
    ));
}

#[test]
fn the_api_does_not_drop_the_income_kind() {
    assert!(matches!(
        income_dto_with(IncomeKindDto::Coupon).to_domain().unwrap(),
        OperationKind::Income { kind: Some(IncomeKind::Coupon), .. }
    ));
}

#[test]
fn an_unknown_operation_kind_is_still_refused() {
    // Молчаливое превращение неизвестного в приход денег хуже отказа.
    assert!(operation_to_submitted(account, unknown_operation()).is_err());
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

```rust
// crates/iaam-app/src/adapters/tinkoff.rs — схлопывание снято:
        ChannelOperationKind::Dividend | ChannelOperationKind::Coupon => {
            let (gross_minor, currency) = required_money(operation.payment, "payment")?;
            let income_kind = match operation.kind {
                ChannelOperationKind::Coupon => IncomeKind::Coupon,
                ChannelOperationKind::Dividend => IncomeKind::Dividend,
                // Внешний match уже сузил варианты. Ветвь недостижима и
                // обязана быть шумной, а не подставлять дивиденд.
                ref other => {
                    return Err(unparsable(format!("вид дохода разъехался: {other:?}")));
                }
            };
            OperationKind::Income {
                instrument: optional_instrument(&operation)?,
                gross_minor,
                currency,
                kind: Some(income_kind),
            }
        }
```

- [ ] **Step 4: Прогнать.** Run: `make test`

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(ingest): вид дохода доходит из канала до журнала и до API (iaam-d8b)"
```

---

### Task 12: импорт амортизации с брокерского канала

**Files:**
- Modify: `crates/iaam-broker/src/tinkoff/parse.rs`, `crates/iaam-app/src/adapters/tinkoff.rs`

**Interfaces:**
- Consumes: T3, T11
- Produces: `ChannelOperationKind::{Amortisation, Redemption}`

**Acceptance Criteria:**
- Виды операций амортизации и погашения перестают падать в `Other(_)` и отклоняться как «неподдержанный вид операции».
- **Недостающих для факта данных адаптер не выдумывает.** `ChannelOperation` несёт `payment` и `quantity`, но **не** несёт возвращённого номинала на единицу и custody. Пока их неоткуда взять, операция отклоняется с внятной причиной — это отказ, а не заглушка.

> ⚠️ Строковых констант T-Invest для амортизации в фикстуре `tests/fixtures/api/tinkoff-operations.json` нет (там только `OPERATION_TYPE_BUY`, `OPERATION_TYPE_INPUT`, `OPERATION_TYPE_BROKER_FEE`). Их **обязательно** подтвердить живым ответом до реализации: несуществующая константа не сломает сборку — она просто никогда не совпадёт.
>
> Расширение фикстуры — правка файла политики (`tests/fixtures` в списке `scripts/check-diff-lint.sh:80`): отдельный коммит с `POLICY_CHANGE_APPROVED=1` и меткой `policy-change`.

- [ ] **Step 1: Подтвердить константы живым ответом, снять фикстуру.**

Run: `make sandbox` (требует `IAAM_DATABASE` и `IAAM_BROKER_KEY_FILE`).

- [ ] **Step 2: Написать падающие тесты**

```rust
#[test]
fn an_amortisation_operation_is_no_longer_an_unsupported_kind() {
    assert_eq!(operation_kind(AMORTISATION_TYPE), ChannelOperationKind::Amortisation);
}

#[test]
fn an_amortisation_without_the_returned_principal_is_refused_not_invented() {
    // Возвращённый номинал на единицу канал не сообщает. Придумать его
    // значит записать в неизменяемый журнал выдуманное число.
    let err = operation_to_submitted(account, amortisation_operation()).unwrap_err();
    assert!(format!("{err}").contains("возвращённый номинал"));
}
```

- [ ] **Step 3: Реализация отображения и отказа.**

- [ ] **Step 4: Прогнать.** Run: `make test`

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(broker): амортизация и погашение импортируются или внятно отклоняются (iaam-d8b)"
```

---

### Task 13: сверка, архив и транспорт

**Files:**
- Modify: `crates/iaam-core/src/reconciliation/observed.rs`, `crates/iaam-store/src/bundle.rs`, `crates/iaam-server/src/dto.rs`

**Interfaces:**
- Consumes: T7–T12

**Acceptance Criteria:**
- Амортизация даёт денежный оборот и **не** даёт изменения количества; погашение даёт оба.
- Архив переживает круг со всеми новыми вариантами.
- DTO отдают новые виды событий.

> Наблюдаемые величины живут в `ObservedTotals` (`reconciliation/observed.rs`) — `turnover`, `position_at` и соседи. `DimensionStatus` — про достоверность, а не про величины; смешивать их в тесте нельзя.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn amortisation_moves_cash_but_not_the_position_count() {
    let totals = observed_totals(&[amortisation(qty(10), per_unit("200"), rub(200_000))]);
    assert_eq!(totals.turnover(CurrencyCode::Rub), rub(200_000));
    assert_eq!(totals.position_at(instrument, as_of), qty(10));
}

#[test]
fn a_bundle_round_trip_keeps_the_new_facts() {
    let journal = journal_with_every_new_variant();
    assert_eq!(import_bundle(&export_bundle(&journal)), journal);
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация.**

Перед правкой прочитать `reconciliation/observed.rs` целиком: величины считаются по ногам, а `Principal` уже входит в `cash_effect()`, поэтому денежный оборот может получиться сам. Проверить, а не предположить: если получается сам — тест это докажет, и правки не нужно.

- [ ] **Step 4: Прогнать.** Run: `make test`

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(store): сверка и архив знают новые факты (iaam-d8b)"
```

---

### Task 14: заслоны и след

**Files:**
- Modify: `crates/iaam-core/tests/serde_roundtrip.rs`
- Modify (**правка политики, отдельный коммит**): `scripts/check-mutants.sh`

**Acceptance Criteria:**
- Круг через JSON проверен для **каждого** нового варианта события.
- `make check` зелёный; `make diff-coverage` не ниже 90%.
- Новые модули в списке мутационного заслона, выживших нет.

- [ ] **Step 1: Дописать round-trip по каждому новому варианту события.**

- [ ] **Step 2: Прогнать полный заслон.** Run: `make check`

- [ ] **Step 3: Остановиться и запросить разрешение владельца.**

Каталог `scripts` — файлы политики (`scripts/check-diff-lint.sh:80`), и агент их не правит. Обоснование записать в описание бида, PR пометить `policy-change`.

- [ ] **Step 4: После разрешения** добавить `event/corporate_action.rs`, `event/offer.rs`, `event/legs.rs`, `rules/amortisation.rs`, `projection/offers.rs` в список и прогнать.

Run: `make mutants`
Expected: выживших нет. Выживший мутант — недостающий тест, а не шум.

- [ ] **Step 5: Два коммита — тесты и правка политики раздельно**

```bash
git commit -am "test(core): круг через JSON для каждого нового факта (iaam-d8b)"
git commit -am "chore(policy): новые модули E3.4 в мутационном заслоне (iaam-d8b)"
```

---

## Порядок и зависимости

```
T1 (PerUnitAmount, checked_div) ─┬─> T3 (типы CorporateAction) ──────────────┐
                                 └─> T4 (PrincipalState) ─> T5 (правило) ────┤
T2 (помощники ног) ──────────────────────────────────────────────────────────┤
T6 (оферта + OfferBook) ─────────────────────────────────────────────────────┤
                                                                             v
                                            T7 (точка включения в EventKind)
                                                                             │
                                            T8 (валидация) ──────────────────┤
                                                                             v
                        T9 (проекция) ─> T10 ─> T11 ─> T12 ─> T13 ─> T14
```

**T7 — единственное место, где ломается сборка воркспейса,** и она чинит все
шесть диспетчеров одним коммитом. До неё новые типы существуют, но в `EventKind`
не включены, поэтому T1–T6 оставляют дерево зелёным и идут тремя независимыми
ветками: `T1→T4→T5`, `T2`, `T3`, `T6`.

## Что этот план намеренно не делает

- **Не трогает `cash_effect()` и семантику `LegKind::Principal`** — она уже описывает амортизацию верно; правка задела бы необратимое ядро.
- **Не дробит лоты по местам хранения** — `LotKey` не различает custody сознательно, и отменять это решение E3.4 не вправе.
- **Не решает, как компенсация дробей влияет на налоговую базу** — это E5.
- **Не добавляет основание котировки** — бид `iaam-a75`, отдельный план; он опирается на `PrincipalState` из T4 и идёт после этой части.
- **Не строит график выплат** — часть 2, и она ждёт живой проверки MOEX ISS.
- **Не проверяет существование окна оферты** — реестра окон в части 1 нет, проверка в E3.4.6.
