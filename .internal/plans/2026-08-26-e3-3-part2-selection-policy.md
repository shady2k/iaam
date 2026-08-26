# E3.3 часть 2 — Правило выбора цены

> **For agentic workers:** каждая задача ниже — отдельный бид. Реализация по TDD.
> **Тяжёлые проверки не запускать** — `cargo mutants`, `cargo test --workspace` и
> сетевые тесты гоняет супервайзер в конце эпика.

**Goal:** превратить набор наблюдений в одну цену позиции на дату — детерминированно,
воспроизводимо и с видимым в provenance основанием выбора.

**Architecture:** политика — чистая функция от выборки кандидатов, версионированная
в закрытом реестре `RuleRegistry` рядом с `FifoV1`. Выбор состоит из двух этапов:
окно поиска отсекает слишком старые наблюдения, затем внутри окна применяется
упорядоченный набор критериев. Возраст выбранного наблюдения раскладывается на две
независимые оси — способ выбора и свежесть.

**Tech Stack:** Rust 2024, `rustc 1.98.0`. Тесты — `cargo test -p iaam-core`,
`-p iaam-app`.

## Global Constraints

- Все команды через `nix develop --command <…>`.
- `unsafe_code = "forbid"`, `clippy::all` на уровне `deny`.
- Журнал фактов и `SCHEMA_VERSION = 3` не трогаются.
- **`iaam-core` не зависит ни от одного крейта проекта.** Всё, что знает про
  `iaam-market`, живёт в `iaam-app`.
- Политика — **чистая функция**: ввод-вывод в неё не входит, иначе рушится §3.1.
- Существующие тесты не переписываются: механическая адаптация разрешена,
  изменение утверждения — нет.
- Дизайн: `.internal/specs/2026-08-26-e3-3-valuation-policy-design.md`, разделы 4 и 5.
- Опирается на типы части 1: `.internal/plans/2026-08-26-e3-3-part1-types-and-legacy-boundary.md`.

---

### Task 1: окно поиска и три полосы возраста

Два порога меряют возраст одного наблюдения и должны разделяться этапами, иначе
один гасит другой: при пределе поиска в 10 дней пометка устаревания с порогом 30
недостижима вовсе.

**Files:**
- Create: `crates/iaam-core/src/rules/valuation.rs`
- Modify: `crates/iaam-core/src/rules/mod.rs` — модуль и регистрация версии
- Test: в самом `valuation.rs`

**Interfaces:**
- Consumes: `PriceCandidate`, `PriceQuery`, `SelectedPrice`, `PriceSelection`,
  `PriceFreshness`, `UncoveredReason` из части 1.
- Produces: `ValuationPolicyVersion(pub u32)`, трейт `ValuationRule` с методом
  выбора, `ValuationPolicyV1`, поля порогов `carry_forward_limit: u16` и
  `price_max_age: u16`.

**Acceptance Criteria:**
- Возраст 0 даёт `PriceSelection::AsObserved` и `PriceFreshness::Fresh`.
- Возраст 1..=`carry_forward_limit` даёт `CarriedForward` и `Fresh`.
- Возраст `carry_forward_limit+1`..=`price_max_age` даёт `CarriedForward` и
  `Stale` **одновременно** — тест проверяет оба поля, а не одно.
- Возраст `price_max_age+1` не даёт цены: позиция непокрыта с причиной `TooOld`.
- Границы полос проверены поимённо на значениях 0, 1, 10, 11, 30, 31.
- Пороги — поля политики, доступные для вывода в отчёт, а не константы в теле.

- [ ] **Шаг 1: падающие тесты границ**

```rust
#[test]
fn an_observation_on_the_valuation_date_is_not_carried_forward() {
    let out = policy().select(&query(date!(2026-08-10)), &[candidate(date!(2026-08-10))]);
    let picked = out.selected().expect("цена есть");
    assert_eq!(picked.selection, PriceSelection::AsObserved);
    assert_eq!(picked.freshness, PriceFreshness::Fresh);
}

#[test]
fn a_price_can_be_carried_forward_and_stale_at_the_same_time() {
    let out = policy().select(&query(date!(2026-08-10)), &[candidate(date!(2026-07-11))]);
    let picked = out.selected().expect("30 дней ещё в окне");
    assert_eq!(
        picked.selection,
        PriceSelection::CarriedForward { observed_on: date!(2026-07-11), days: 30 }
    );
    assert_eq!(picked.freshness, PriceFreshness::Stale { days: 30 });
}

#[test]
fn a_price_older_than_the_search_window_is_not_returned_at_all() {
    let out = policy().select(&query(date!(2026-08-10)), &[candidate(date!(2026-07-10))]);
    assert!(out.selected().is_none());
    assert_eq!(out.uncovered_reason(), Some(UncoveredReason::TooOld));
}
```

- [ ] **Шаг 2: убедиться, что тесты падают**

Run: `nix develop --command cargo test -p iaam-core valuation_policy`
Expected: FAIL — типов нет.

- [ ] **Шаг 3: реализация полос**

Возраст считается в календарных днях как `дата оценки − trade_date`. Полосы —
ровно четыре, границы включительные, как в таблице раздела 4.4 дизайна.

- [ ] **Шаг 4: тесты проходят**

Run: `nix develop --command cargo test -p iaam-core valuation_policy`

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/rules
git commit -m "feat(core): окно поиска цены и три полосы возраста"
```

---

### Task 2: порядок критериев внутри окна

Порядок не произволен: свежесть идёт первой, иначе приоритет происхождения даст
старой биржевой цене победить свежий брокерский отчёт, и полезный запасной
вариант умрёт молча.

**Files:**
- Modify: `crates/iaam-core/src/rules/valuation.rs`
- Test: там же

**Interfaces:**
- Produces: `SourcePriorityVersion(pub u32)`; порядок происхождений как данные
  политики, а не как `impl Ord` на `PriceOrigin`.

**Acceptance Criteria:**
- При равных прочих побеждает наименьший возраст.
- При равном возрасте порядок происхождения `Market > ReportParsed > OwnerAsserted`.
- Тест доказывает, что свежий `ReportParsed` побеждает более старый `Market`, —
  именно та ошибка, которую порядок предотвращает.
- Внутри строки вид цены выбирается по списку
  `LegalClose → MarketPrice2 → AdmittedQuote → Close`, первый заполненный.
- `WeightedAverage` и `MarketPrice3` в выбор не входят; тест это фиксирует как
  решение, а не как пропуск.
- Если кандидатов на площадке несколько и справочник не называет предпочтительную
  — отказ `AmbiguousVenue`, а не молчаливый выбор первого.
- Если после всех критериев кандидатов больше одного — отказ `AmbiguousCandidate`.
  Сортировка по случайному полю запрещена.
- Из нескольких версий одного наблюдения берётся максимальный `observed_at`,
  не превышающий `knowledge_as_of`.

- [ ] **Шаг 1: падающий тест на порядок**

```rust
#[test]
fn a_fresh_report_price_beats_a_stale_exchange_price() {
    let out = policy().select(
        &query(date!(2026-08-10)),
        &[
            candidate_from(PriceOrigin::Market { venue: "TQBR".into(), kind: "close".into() },
                           date!(2026-08-01)),
            candidate_from(PriceOrigin::ReportParsed { source: source_id() },
                           date!(2026-08-09)),
        ],
    );
    let picked = out.selected().expect("цена есть");
    assert!(matches!(picked.candidate.origin, PriceOrigin::ReportParsed { .. }));
}

#[test]
fn two_venues_without_a_directory_preference_are_a_refusal_not_a_guess() {
    let out = policy().select(
        &query(date!(2026-08-10)),
        &[candidate_on_venue("TQBR"), candidate_on_venue("SMAL")],
    );
    assert!(out.selected().is_none());
    assert_eq!(out.uncovered_reason(), Some(UncoveredReason::AmbiguousVenue));
}
```

- [ ] **Шаг 2: убедиться, что падают**

Run: `nix develop --command cargo test -p iaam-core valuation_policy`

- [ ] **Шаг 3: реализация порядка**

Критерии применяются в порядке: свежесть, происхождение, площадка и сессия, вид
цены, версия наблюдения. Каждый следующий применяется только к кандидатам,
прошедшим предыдущий.

- [ ] **Шаг 4: тесты проходят**

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/rules
git commit -m "feat(core): порядок критериев выбора цены, отказ вместо угадывания"
```

---

### Task 3: регистрация в `RuleRegistry` и provenance выбора

**Files:**
- Modify: `crates/iaam-core/src/rules/mod.rs` — реестр принимает набор правил оценки
- Modify: `crates/iaam-core/src/rules/valuation.rs` — provenance
- Test: в обоих

**Interfaces:**
- Produces: `RuleRegistry::valuation_rule(ValuationPolicyVersion) -> Option<&dyn ValuationRule>`,
  структура provenance выбора: выбранный вид цены, происхождение, площадка,
  `observed_at`, применённые версии и оба порога.

**Acceptance Criteria:**
- `RuleRegistry::with_defaults()` содержит `ValuationPolicyVersion(1)`.
- Неизвестная версия даёт `None`, а не подстановку версии 1.
- Provenance несёт **оба порога** — §6.6 требует, чтобы цифра, зависящая от
  порога, несла порог рядом с собой; так уже сделано для `perimeter_policy`.
- Provenance называет выбранный вид цены, а не только итоговое число: вопрос
  «сколько было бы по `Close`» отвечается без повторной синхронизации.

- [ ] **Шаг 1: падающий тест**

```rust
#[test]
fn an_unknown_valuation_policy_version_is_not_silently_defaulted() {
    let registry = RuleRegistry::with_defaults();
    assert!(registry.valuation_rule(ValuationPolicyVersion(2)).is_none());
}

#[test]
fn the_provenance_carries_both_thresholds() {
    let out = policy().select(&query(date!(2026-08-10)), &[candidate(date!(2026-08-05))]);
    let p = out.selected().expect("цена есть").provenance;
    assert_eq!(p.carry_forward_limit, 10);
    assert_eq!(p.price_max_age, 30);
}
```

- [ ] **Шаг 2-4: реализация и тесты**

Run: `nix develop --command cargo test -p iaam-core`

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/rules
git commit -m "feat(core): политика оценки в реестре правил, provenance выбора"
```

---

### Task 4: отображение рыночного наблюдения в кандидата

Мост между `iaam-market` и доменом. Живёт в `iaam-app`, потому что `iaam-core` не
знает крейт-источник.

**Files:**
- Create: `crates/iaam-app/src/market_candidate.rs`
- Modify: `crates/iaam-app/src/lib.rs` — объявить модуль
- Test: в самом модуле

**Interfaces:**
- Consumes: `iaam_market::{PriceObservation, PriceKind, Executability}`,
  `iaam_core::valuation::{PriceCandidate, PriceOrigin, SourceExecutability}`.
- Produces: функция преобразования наблюдения в кандидата.

**Acceptance Criteria:**
- `Executability::Executable` → `SourceExecutability::Executable`.
- `Executability::IndicativePreviousClose` → `SourceExecutability::IndicativePreviousClose`.
- Отображение **тотально**: после части 1 у `Executability` ровно два варианта,
  и `match` исчерпывающий без рукава `_`. Появление третьего варианта обязано
  ломать сборку, а не молча падать в умолчание.
- `PriceOrigin::Market` несёт площадку и вид цены строкой; шесть видов
  отображаются в шесть различимых значений, ни один не схлопывается.
- Тест на замороженной фикстуре `tests/fixtures/market/moex-iss-history-sber.json`:
  строка за 2026-08-03 даёт кандидатов с ценами `CLOSE` 281.39,
  `LEGALCLOSEPRICE` 280.15, `WAPRICE` 279.78, `MARKETPRICE2` 280.21,
  `MARKETPRICE3` 280.21, и **не даёт** кандидата на `ADMITTEDQUOTE`, потому что
  там `null`.

- [ ] **Шаг 1: падающий тест на фикстуре**

Использовать уже существующий способ читать фикстуру из тестов `iaam-market`;
свой загрузчик не заводить.

- [ ] **Шаг 2-4: реализация и тесты**

Run: `nix develop --command cargo test -p iaam-app`

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-app/src/market_candidate.rs crates/iaam-app/src/lib.rs
git commit -m "feat(app): рыночное наблюдение становится кандидатом на оценку"
```
