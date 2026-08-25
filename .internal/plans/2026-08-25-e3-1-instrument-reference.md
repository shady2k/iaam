# E3.1 — Справочник инструментов и таксономия: план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** Инструмент из отчёта брокера, выгрузки MOEX ISS и CSV владельца разрешается в один `InstrumentId` по внешнему коду на дату документа, а род инструмента и три его валюты хранятся типизированно.

**Architecture:** `iaam-core` получает чистые типы справочника без I/O; `iaam-store` — миграцию 0005 и резолвинг по псевдонимам с интервалом действия; `iaam-app` — объектобезопасный порт; `iaam-ingest` и `iaam-server` — потребители порта.

**Tech Stack:** Rust 1.98.0 (закреплён `rust-toolchain.toml`), `rusqlite` (SQLite STRICT), `rust_decimal`, `time`, `serde`, `proptest`, `axum` + `utoipa`, `thiserror`. Окружение — `nix develop`.

**Спецификация:** `.internal/specs/2026-08-25-e3-1-instrument-reference-design.md`
**Спека проекта:** `.internal/specs/2026-08-22-investment-tracker-design.md`

## Global Constraints

- **Все команды идут через `nix develop -c`.** Снаружи окружения соберётся не тот тулчейн.
- **Воркеры не запускают тяжёлые прогоны.** Разрешено: `cargo check -p <крейт>`, `cargo test -p <крейт> <фильтр>`, `cargo fmt --all`. Запрещено: `cargo mutants`, `cargo llvm-cov`, полный `make check` — они идут один раз в конце эпика у оркестратора.
- **Проза русская, имена тестов английские.** Так написан весь существующий код: `a_new_rule_is_active_from_the_moment_it_is_stored`, doc-комментарии по-русски со ссылками на параграфы спеки.
- **`rustfmt.toml` задаёт `fn_call_width = 60` при `max_width = 100`.** Перед каждым коммитом обязателен `nix develop -c cargo fmt --all`, иначе заслон формата покраснеет на строках, которые выглядят короткими.
- **`iaam-core` — без I/O, без async, без `Mutex`.** Проверяется `scripts/check-architecture.sh`.
- **Исчерпаемые `enum` без `#[non_exhaustive]`.** Образец — `CurrencyCode` в `crates/iaam-core/src/money.rs:22`: атрибут намеренно не применяется, чтобы добавление варианта ломало сборку у всех потребителей (§15.1).
- **`unknown` не является нулём (§4.9).** Отсутствующее значение — `Option<T>`, а не вариант `Unknown` и не пустая строка.
- **Файлы политики не трогать.** `.github/workflows`, `scripts`, `deny.toml`, `clippy.toml`, `Cargo.toml`, `flake.nix`, `rustfmt.toml` — правка требует `POLICY_CHANGE_APPROVED=1` от владельца. Если задача упирается в них — остановка и эскалация, а не обход.
- **Ослабление теста ради прохождения запрещено (§15.7).** Расхождение исправляется в пользу компилятора; если тест приходится ослабить — остановка и эскалация.

---

## Карта файлов

| Файл | Ответственность | Задача |
|---|---|---|
| `crates/iaam-core/src/instrument.rs` | создать: род, пространства имён псевдонимов, роли валют, интервал | T1 |
| `crates/iaam-core/src/lib.rs` | изменить: объявить модуль `instrument` | T1 |
| `crates/iaam-store/migrations/0005_instrument_reference.sql` | создать: пересоздание `instruments`, `instrument_aliases`, `custody_places`, триггер | T2 |
| `crates/iaam-store/src/schema.rs` | изменить: `SCHEMA_VERSION` → 5, пятая строка `MIGRATIONS` | T2 |
| `crates/iaam-store/tests/migration_0005.rs` | создать: перенос данных E1/E2 | T2 |
| `crates/iaam-store/src/reference.rs` | изменить: записи, резолвинг, псевдонимы, места хранения | T2, T3 |
| `crates/iaam-store/src/lib.rs` | изменить: варианты `ResolveError` рядом со `StoreError` | T3 |
| `crates/iaam-store/tests/instrument_directory.rs` | создать: границы интервала, три ошибки резолвинга | T3 |
| `crates/iaam-app/src/ports.rs` | изменить: порт `InstrumentDirectory`, вью-типы | T4 |
| `crates/iaam-app/src/adapters/sqlite.rs` | изменить: реализация порта, отображение `ResolveError` | T4 |
| `crates/iaam-ingest/src/csv_source.rs` | изменить: карта интервалов, разрешение на дату строки | T5 |
| `crates/iaam-server/src/routes.rs` | изменить: `build_directory` (T5), три маршрута (T6) | T5, T6 |
| `crates/iaam-server/src/dto.rs`, `routes.rs` | изменить: DTO и три маршрута | T6 |
| `crates/iaam-server/tests/contract.rs` | изменить: контрактные тесты новых операций | T6 |

**Волны.** T1 ∥ T2 → T3 → T4 → T5 → T6.

T1 и T2 независимы: миграция T2 сохраняет поле `currency: String` внутри
`reference.rs` и не ждёт типов T1; их подменяет T3.

**T5 и T6 идут последовательно, а не параллельно.** Обе правят
`crates/iaam-server/src/routes.rs` — T5 переписывает `build_directory`,
T6 добавляет три маршрута. Два воркера, редактирующие один файл в одном
рабочем дереве, затрут друг друга; параллелить их можно только через
раздельные worktree, и выигрыш этого не стоит.

---

### Task 1: Типы справочника в `iaam-core`

**Files:**
- Create: `crates/iaam-core/src/instrument.rs`
- Modify: `crates/iaam-core/src/lib.rs`

**Interfaces:**
- Consumes: `CurrencyCode` из `crates/iaam-core/src/money.rs`, `InstrumentId` из `crates/iaam-core/src/ids.rs`, `time::Date`.
- Produces: `InstrumentKind`, `AliasNamespace`, `LineageReason`, `CurrencyRoles`, `AliasInterval`, `Lineage`. У каждого enum — `code(&self) -> &'static str` и `from_code(&str) -> Option<Self>`. У `AliasInterval` — `covers(&self, on: Date) -> bool`. Задачи T2–T6 берут имена отсюда.

**Acceptance Criteria:**
- `InstrumentKind` содержит ровно десять вариантов и не содержит `Futures`, `Option` и `Deposit`.
- `code`/`from_code` образуют round-trip для каждого варианта каждого enum.
- `AliasInterval::covers` включает `valid_from` и исключает `valid_to`.
- Открытый интервал (`valid_to = None`) покрывает любую дату не раньше `valid_from`.
- `cargo check -p iaam-core` и `scripts/check-architecture.sh` проходят.

- [ ] **Step 1: Написать падающий тест**

Создать `crates/iaam-core/src/instrument.rs` с одним лишь блоком тестов:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn an_interval_includes_its_first_day() {
        let interval = AliasInterval { valid_from: date!(2023 - 01 - 10), valid_to: None };
        assert!(interval.covers(date!(2023 - 01 - 10)));
    }

    #[test]
    fn an_interval_excludes_the_day_it_ends() {
        let interval = AliasInterval {
            valid_from: date!(2023 - 01 - 10),
            valid_to: Some(date!(2024 - 05 - 20)),
        };
        assert!(interval.covers(date!(2024 - 05 - 19)));
        assert!(!interval.covers(date!(2024 - 05 - 20)));
    }

    #[test]
    fn an_open_interval_covers_every_later_day() {
        let interval = AliasInterval { valid_from: date!(2023 - 01 - 10), valid_to: None };
        assert!(interval.covers(date!(2099 - 12 - 31)));
        assert!(!interval.covers(date!(2023 - 01 - 09)));
    }

    #[test]
    fn every_kind_survives_a_round_trip_through_its_code() {
        for kind in InstrumentKind::ALL {
            assert_eq!(InstrumentKind::from_code(kind.code()), Some(kind));
        }
    }

    #[test]
    fn every_namespace_survives_a_round_trip_through_its_code() {
        for namespace in AliasNamespace::ALL {
            assert_eq!(AliasNamespace::from_code(namespace.code()), Some(namespace));
        }
    }

    #[test]
    fn every_lineage_reason_survives_a_round_trip_through_its_code() {
        for reason in LineageReason::ALL {
            assert_eq!(LineageReason::from_code(reason.code()), Some(reason));
        }
    }

    #[test]
    fn an_unknown_code_is_not_guessed() {
        assert_eq!(InstrumentKind::from_code("derivative"), None);
        assert_eq!(AliasNamespace::from_code("cusip"), None);
    }
}
```

Добавить в `crates/iaam-core/src/lib.rs` рядом с остальными объявлениями модулей:

```rust
pub mod instrument;
```

- [ ] **Step 2: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-core --lib instrument`
Expected: FAIL — `cannot find type AliasInterval in this scope` и такие же ошибки на `InstrumentKind`, `AliasNamespace`, `LineageReason`.

- [ ] **Step 3: Написать типы**

В начало `crates/iaam-core/src/instrument.rs`, перед блоком тестов:

```rust
//! Справочник инструментов: род, псевдонимы, роли валют (§4.5, §5.4, §7.2).
//!
//! Здесь только неизменные свойства инструмента. Строка политики
//! оценки §5.4 зависит ещё и от наличия цены и её возраста на дату,
//! поэтому выводится функцией в E3.3, а не хранится колонкой.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::ids::InstrumentId;
use crate::money::CurrencyCode;

/// Род инструмента. Неизменное свойство: акция не становится облигацией.
///
/// Исчерпаемый `enum` без `#[non_exhaustive]` по образцу [`CurrencyCode`]:
/// добавление рода обязано сломать сборку везде, где его не обработали
/// (§15.1).
///
/// Вариантов `Futures` и `Option` здесь нет намеренно: §11 выводит ПФИ
/// за периметр вместе с шортами, маржой и РЕПО, и ledger обязательств
/// не строится. Вариант `Deposit` отсутствует по другой причине: вклад
/// является счётом, а не инструментом — у него нет ни количества, ни
/// места хранения (§4.5, и doc-комментарий `AccountId` прямо называет
/// вклад денежным счётом).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum InstrumentKind {
    Share,
    DepositaryReceipt,
    Bond,
    /// Биржевой фонд: есть котировка.
    Etf,
    /// Паевой фонд: расчётная стоимость пая, а не котировка.
    MutualFund,
    Currency,
    Crypto,
    RealEstate,
    PrivateShare,
    Loan,
}

impl InstrumentKind {
    /// Все варианты. Существует ради табличных тестов: список,
    /// собранный руками в тесте, разъедется с `enum` молча.
    pub const ALL: [Self; 10] = [
        Self::Share,
        Self::DepositaryReceipt,
        Self::Bond,
        Self::Etf,
        Self::MutualFund,
        Self::Currency,
        Self::Crypto,
        Self::RealEstate,
        Self::PrivateShare,
        Self::Loan,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Share => "share",
            Self::DepositaryReceipt => "depositary_receipt",
            Self::Bond => "bond",
            Self::Etf => "etf",
            Self::MutualFund => "mutual_fund",
            Self::Currency => "currency",
            Self::Crypto => "crypto",
            Self::RealEstate => "real_estate",
            Self::PrivateShare => "private_share",
            Self::Loan => "loan",
        }
    }

    /// Разбор кода. `None`, а не подстановка умолчания: неизвестный род
    /// обязан дойти до вызывающего, а не превратиться в акцию (§4.9).
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// Пространство имён внешнего кода инструмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AliasNamespace {
    Isin,
    MoexSecid,
    Ticker,
    Figi,
    /// Внутренний код брокера: у разных брокеров разный для одной бумаги.
    BrokerCode,
}

impl AliasNamespace {
    pub const ALL: [Self; 5] = [
        Self::Isin,
        Self::MoexSecid,
        Self::Ticker,
        Self::Figi,
        Self::BrokerCode,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Isin => "isin",
            Self::MoexSecid => "moex_secid",
            Self::Ticker => "ticker",
            Self::Figi => "figi",
            Self::BrokerCode => "broker_code",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|namespace| namespace.code() == code)
    }
}

/// Почему у инструмента есть предшественник (§7.2, §4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LineageReason {
    /// Замещающая облигация.
    Replacement,
    Conversion,
    Merger,
    SpinOff,
}

impl LineageReason {
    pub const ALL: [Self; 4] = [Self::Replacement, Self::Conversion, Self::Merger, Self::SpinOff];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Replacement => "replacement",
            Self::Conversion => "conversion",
            Self::Merger => "merger",
            Self::SpinOff => "spin_off",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.code() == code)
    }
}

/// Три роли валюты у одного инструмента (§7.2).
///
/// Структура, а не три позиционных `CurrencyCode`: одинаково
/// типизированные аргументы подряд переставляются местами незаметно
/// для компилятора (§15.1). Валюты отчёта здесь нет — она свойство
/// отчёта и владельца, а не бумаги.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyRoles {
    /// Валюта обязательства.
    pub denomination: CurrencyCode,
    /// Валюта расчётов.
    pub settlement: CurrencyCode,
    /// Валюта котировки.
    pub quote: CurrencyCode,
}

impl CurrencyRoles {
    /// Все три роли совпадают — обычный случай рублёвой бумаги.
    #[must_use]
    pub const fn uniform(currency: CurrencyCode) -> Self {
        Self { denomination: currency, settlement: currency, quote: currency }
    }
}

/// Интервал действия псевдонима.
///
/// Начало включительно, конец исключительно. Полуинтервал выбран,
/// чтобы смежные интервалы одного кода стыковались без зазора и без
/// перекрытия: при включительном конце день смены ISIN принадлежал бы
/// сразу двум записям.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasInterval {
    pub valid_from: Date,
    /// `None` — открытый интервал.
    pub valid_to: Option<Date>,
}

impl AliasInterval {
    #[must_use]
    pub fn covers(&self, on: Date) -> bool {
        on >= self.valid_from && self.valid_to.is_none_or(|end| on < end)
    }
}

/// Происхождение инструмента: замещение, конвертация, слияние (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    pub parent: InstrumentId,
    pub reason: LineageReason,
}
```

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `nix develop -c cargo test -p iaam-core --lib instrument`
Expected: PASS, 7 тестов.

Если `is_none_or` не существует в закреплённом тулчейне — заменить на
`self.valid_to.map_or(true, |end| on < end)` и не поднимать версию Rust:
`rust-toolchain.toml` является файлом политики.

- [ ] **Step 5: Свойство на round-trip**

Добавить в `crates/iaam-core/tests/properties.rs` (файл уже существует,
дописать в конец, сохранив стиль соседних свойств):

```rust
proptest! {
    /// Разбор кода не выдумывает род: любая строка, не совпадающая
    /// с кодом варианта, обязана дать None.
    #[test]
    fn an_arbitrary_string_is_never_mistaken_for_a_kind(text in "\\PC{0,16}") {
        let parsed = iaam_core::instrument::InstrumentKind::from_code(&text);
        let expected = iaam_core::instrument::InstrumentKind::ALL
            .into_iter()
            .find(|kind| kind.code() == text);
        prop_assert_eq!(parsed, expected);
    }
}
```

- [ ] **Step 6: Прогнать свойство и заслон архитектуры**

Run: `nix develop -c cargo test -p iaam-core --test properties`
Expected: PASS.

Run: `nix develop -c ./scripts/check-architecture.sh`
Expected: заслон зелёный — в `instrument.rs` нет ни `f64`, ни `async`, ни I/O.

- [ ] **Step 7: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-core/src/instrument.rs crates/iaam-core/src/lib.rs crates/iaam-core/tests/properties.rs
git commit -m "feat(core): типы справочника инструментов — род, псевдонимы, роли валют (<bead-id>)"
```

---

### Task 2: Миграция 0005 и совместимость хранилища

**Files:**
- Create: `crates/iaam-store/migrations/0005_instrument_reference.sql`
- Create: `crates/iaam-store/tests/migration_0005.rs`
- Modify: `crates/iaam-store/src/schema.rs:12` (`SCHEMA_VERSION`), `crates/iaam-store/src/schema.rs:14-19` (массив `MIGRATIONS`)
- Modify: `crates/iaam-store/src/reference.rs:74-88` (`upsert_instrument`)

**Interfaces:**
- Consumes: ничего из T1 — задача идёт с ней параллельно и намеренно сохраняет `InstrumentRecord.currency: String`.
- Produces: схему версии 5 с таблицами `instruments` (десять колонок), `instrument_aliases`, `custody_places` и триггером `instrument_aliases_do_not_overlap`. T3 заменяет тела методов, не трогая SQL.

**Acceptance Criteria:**
- `SCHEMA_VERSION` равна 5, массив `MIGRATIONS` содержит пять элементов.
- База с инструментами версии 4 проходит миграцию: `kind IS NULL`, а `denomination_currency`, `settlement_currency` и `quote_currency` равны прежней `currency`.
- Две записи `instrument_aliases` с пересекающимися интервалами одного `(namespace, value)` отклоняются базой, а не кодом.
- `custody_places` держит уникальный индекс по `(owner, id)`.
- `cargo check -p iaam-store` проходит: крейта продолжает собираться со старым `InstrumentRecord`.

- [ ] **Step 1: Написать падающий тест миграции**

Создать `crates/iaam-store/tests/migration_0005.rs`:

```rust
//! Перенос данных при миграции 0005.
//!
//! Проверка на непустой базе обязательна: на пустой базе перенос
//! данных верен тривиально, а ломается он ровно на существующих
//! строках.

use rusqlite::Connection;

/// Схема версии 4 в объёме, который затрагивает миграция 0005.
fn database_at_version_four() -> Connection {
    let conn = Connection::open_in_memory().expect("база в памяти");
    conn.execute_batch(
        "CREATE TABLE instruments (
             id       TEXT PRIMARY KEY,
             symbol   TEXT NOT NULL,
             title    TEXT NOT NULL,
             currency TEXT NOT NULL
         ) STRICT;
         INSERT INTO instruments (id, symbol, title, currency)
         VALUES ('11111111-1111-4111-8111-111111111111', 'SBER', 'Сбербанк', 'RUB');
         PRAGMA user_version = 4;",
    )
    .expect("схема версии 4");
    conn
}

fn apply_migration_0005(conn: &Connection) {
    let sql = include_str!("../migrations/0005_instrument_reference.sql");
    conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = 5; COMMIT;"))
        .expect("миграция 0005");
}

#[test]
fn an_existing_instrument_keeps_its_currency_in_all_three_roles() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let (denomination, settlement, quote): (String, String, String) = conn
        .query_row(
            "SELECT denomination_currency, settlement_currency, quote_currency
             FROM instruments WHERE symbol = 'SBER'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("перенесённый инструмент");

    assert_eq!(denomination, "RUB");
    assert_eq!(settlement, "RUB");
    assert_eq!(quote, "RUB");
}

#[test]
fn an_existing_instrument_has_no_kind_guessed_for_it() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let kind: Option<String> = conn
        .query_row("SELECT kind FROM instruments WHERE symbol = 'SBER'", [], |row| row.get(0))
        .expect("перенесённый инструмент");

    assert_eq!(kind, None, "род не известен и не должен подставляться акцией");
}

#[test]
fn overlapping_alias_intervals_are_refused_by_the_database() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);
    conn.execute_batch(
        "INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2020-01-01', '2024-01-01', 'manual', '2026-08-25T00:00:00Z');",
    )
    .expect("первый интервал");

    let overlapping = conn.execute_batch(
        "INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2023-06-01', NULL, 'manual', '2026-08-25T00:00:00Z');",
    );

    assert!(overlapping.is_err(), "пересечение интервалов делает резолвинг неоднозначным");
}

#[test]
fn adjacent_alias_intervals_are_allowed() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let adjacent = conn.execute_batch(
        "INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2020-01-01', '2024-01-01', 'manual', '2026-08-25T00:00:00Z');
         INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2024-01-01', NULL, 'manual', '2026-08-25T00:00:00Z');",
    );

    assert!(
        adjacent.is_ok(),
        "смежные интервалы стыкуются без зазора: конец полуинтервала исключителен"
    );
}
```

Внешнего ключа на `source_documents` в этом тесте нет, потому что тест
поднимает только затронутую часть схемы версии 4. В боевой миграции ключ
присутствует — см. шаг 3.

- [ ] **Step 2: Убедиться, что тест падает**

Run: `nix develop -c cargo test -p iaam-store --test migration_0005`
Expected: FAIL — `couldn't read migrations/0005_instrument_reference.sql: No such file or directory`.

- [ ] **Step 3: Написать миграцию**

Создать `crates/iaam-store/migrations/0005_instrument_reference.sql`:

```sql
-- Справочник инструментов, псевдонимы и места хранения (E3.1).
--
-- Спецификация: .internal/specs/2026-08-25-e3-1-instrument-reference-design.md

-- Пересоздание, а не ALTER: добавить NOT NULL-колонку в существующую
-- STRICT-таблицу с данными SQLite не умеет.
CREATE TABLE instruments_new (
    id                    TEXT PRIMARY KEY,
    -- NULL = род не установлен. Варианта `unknown` нет намеренно:
    -- §4.9 запрещает unknown как нулевое значение, а Option<T>
    -- заставляет обработать отсутствие.
    kind                  TEXT,
    -- Отображаемый символ, а НЕ идентичность: идентичность живёт
    -- в instrument_aliases, потому что ISIN меняется (§4.7).
    symbol                TEXT NOT NULL,
    title                 TEXT NOT NULL,
    denomination_currency TEXT NOT NULL,
    settlement_currency   TEXT NOT NULL,
    quote_currency        TEXT NOT NULL,
    lineage_parent        TEXT REFERENCES instruments_new(id),
    lineage_reason        TEXT,
    created_at            TEXT NOT NULL,
    -- Происхождение без причины и причина без происхождения одинаково
    -- бессмысленны, поэтому пара обязана быть заполнена целиком.
    CHECK ((lineage_parent IS NULL) = (lineage_reason IS NULL))
) STRICT;

-- Перенос: три роли валюты у уже заведённой бумаги совпадают,
-- род не известен и не выдумывается.
INSERT INTO instruments_new
    (id, kind, symbol, title,
     denomination_currency, settlement_currency, quote_currency,
     lineage_parent, lineage_reason, created_at)
SELECT id, NULL, symbol, title,
       currency, currency, currency,
       NULL, NULL, '1970-01-01T00:00:00Z'
FROM instruments;

DROP TABLE instruments;
ALTER TABLE instruments_new RENAME TO instruments;

-- Внешние коды. Каждый со своим интервалом действия: резолвинг идёт
-- на дату документа, потому что ISIN меняется корпоративным
-- действием isin_change (§4.7), а отчёт за прошлый год приходит
-- со старым кодом.
CREATE TABLE instrument_aliases (
    namespace  TEXT NOT NULL,
    value      TEXT NOT NULL,
    instrument TEXT NOT NULL REFERENCES instruments(id),
    valid_from TEXT NOT NULL,
    -- NULL = открытый интервал.
    valid_to   TEXT,
    -- Псевдоним — утверждение о мире, как и цена, и приходит
    -- с provenance (§4.4). Строка «откуда-то узнали» не позволила бы
    -- отозвать псевдонимы испорченного документа.
    source     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (namespace, value, valid_from),
    CHECK (valid_to IS NULL OR valid_to > valid_from)
) STRICT;

CREATE INDEX instrument_aliases_by_instrument
    ON instrument_aliases (instrument);

-- Непересечение интервалов держится базой, а не дисциплиной кода:
-- дисциплина не переживает первый же скрипт починки данных, а
-- пересечение делает резолвинг неоднозначным (§15.2).
-- Полуинтервал [valid_from, valid_to): смежные записи стыкуются,
-- пересекающиеся — нет.
CREATE TRIGGER instrument_aliases_do_not_overlap
BEFORE INSERT ON instrument_aliases
BEGIN
    SELECT RAISE(ABORT, 'интервалы псевдонима пересекаются: резолвинг стал бы неоднозначным')
    WHERE EXISTS (
        SELECT 1 FROM instrument_aliases existing
        WHERE existing.namespace = NEW.namespace
          AND existing.value = NEW.value
          AND (NEW.valid_to IS NULL OR existing.valid_from < NEW.valid_to)
          AND (existing.valid_to IS NULL OR NEW.valid_from < existing.valid_to)
    );
END;

CREATE TRIGGER instrument_aliases_do_not_overlap_on_update
BEFORE UPDATE ON instrument_aliases
BEGIN
    SELECT RAISE(ABORT, 'интервалы псевдонима пересекаются: резолвинг стал бы неоднозначным')
    WHERE EXISTS (
        SELECT 1 FROM instrument_aliases existing
        WHERE existing.namespace = NEW.namespace
          AND existing.value = NEW.value
          AND existing.valid_from <> OLD.valid_from
          AND (NEW.valid_to IS NULL OR existing.valid_from < NEW.valid_to)
          AND (existing.valid_to IS NULL OR NEW.valid_from < existing.valid_to)
    );
END;

-- Место хранения бумаг (§4.5). CustodyId объявлен в
-- crates/iaam-core/src/ids.rs и требуется Leg::security, но таблицы
-- под него до сих пор не было.
CREATE TABLE custody_places (
    id          TEXT PRIMARY KEY,
    owner       TEXT NOT NULL,
    title       TEXT NOT NULL,
    institution TEXT,
    created_at  TEXT NOT NULL
) STRICT;

-- Владелец в уникальном ключе — как у accounts: иначе чужое место
-- хранения подставится в ногу сделки (§14).
CREATE UNIQUE INDEX custody_places_by_owner ON custody_places (owner, id);
```

Внешний ключ `source` на `source_documents(id)` в SQL **не объявляется**:
`source_documents` заведена миграцией 0002 и содержит колонку `owner`, а
`instruments` глобальна. Ссылочная целостность обеспечивается типом
`SourceId` в T3; объявление FK потребовало бы, чтобы каждый псевдоним
происходил из загруженного документа, что неверно для ручного ввода и
для синхронизации с MOEX ISS.

- [ ] **Step 4: Подключить миграцию**

В `crates/iaam-store/src/schema.rs` заменить две строки:

```rust
pub const SCHEMA_VERSION: u32 = 5;

const MIGRATIONS: [(u32, &str); 5] = [
    (1, include_str!("../migrations/0001_initial.sql")),
    (2, include_str!("../migrations/0002_sources_and_rules.sql")),
    (3, include_str!("../migrations/0003_broker_access.sql")),
    (4, include_str!("../migrations/0004_broker_environment.sql")),
    (5, include_str!("../migrations/0005_instrument_reference.sql")),
];
```

- [ ] **Step 5: Починить `upsert_instrument`, не меняя его типа**

В `crates/iaam-store/src/reference.rs` заменить тело `upsert_instrument`
так, чтобы крейта собиралась с новой схемой. `InstrumentRecord` здесь
намеренно **не меняется** — его переписывает T3, которая идёт следом:

```rust
    /// Создание или обновление инструмента.
    ///
    /// Три роли валюты заполняются одним значением: `InstrumentRecord`
    /// ещё не различает их. Различение приходит вместе с
    /// `CurrencyRoles` в следующей задаче.
    pub fn upsert_instrument(&self, instrument: &InstrumentRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO instruments
                 (id, kind, symbol, title,
                  denomination_currency, settlement_currency, quote_currency,
                  lineage_parent, lineage_reason, created_at)
             VALUES (?1, NULL, ?2, ?3, ?4, ?4, ?4, NULL, NULL, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 symbol = excluded.symbol,
                 title = excluded.title,
                 denomination_currency = excluded.denomination_currency,
                 settlement_currency = excluded.settlement_currency,
                 quote_currency = excluded.quote_currency",
            params![
                instrument.id.inner().to_string(),
                instrument.symbol,
                instrument.title,
                instrument.currency,
                now(),
            ],
        )?;
        Ok(())
    }
```

- [ ] **Step 6: Прогнать тесты миграции и существующие тесты хранилища**

Run: `nix develop -c cargo test -p iaam-store --test migration_0005`
Expected: PASS, 4 теста.

Run: `nix develop -c cargo test -p iaam-store --test snapshots_and_reference`
Expected: PASS — существующие тесты справочника переживают смену схемы.

Run: `nix develop -c cargo check -p iaam-store`
Expected: без ошибок.

- [ ] **Step 7: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-store/migrations/0005_instrument_reference.sql \
        crates/iaam-store/src/schema.rs \
        crates/iaam-store/src/reference.rs \
        crates/iaam-store/tests/migration_0005.rs
git commit -m "feat(store): миграция 0005 — справочник инструментов, псевдонимы, места хранения (<bead-id>)"
```

---

### Task 3: Записи и резолвинг в `iaam-store`

**Files:**
- Modify: `crates/iaam-store/src/reference.rs`
- Modify: `crates/iaam-store/src/lib.rs` (добавить `ResolveError` рядом со `StoreError`)
- Create: `crates/iaam-store/tests/instrument_directory.rs`

**Interfaces:**
- Consumes: `InstrumentKind`, `AliasNamespace`, `LineageReason`, `CurrencyRoles`, `AliasInterval`, `Lineage` (T1); схему версии 5 (T2).
- Produces:
  - `InstrumentRecord { id, kind: Option<InstrumentKind>, symbol: String, title: String, currencies: CurrencyRoles, lineage: Option<Lineage> }`
  - `CustodyRecord { id: CustodyId, owner: OwnerId, title: String, institution: Option<String> }`
  - `AliasRecord { namespace: AliasNamespace, value: String, instrument: InstrumentId, interval: AliasInterval, source: SourceId }`
  - `SqliteStore::resolve_instrument(&self, ns: AliasNamespace, value: &str, on: Date) -> Result<InstrumentId, ResolveError>`
  - `SqliteStore::record_alias(&self, alias: &AliasRecord) -> Result<(), StoreError>`
  - `SqliteStore::rename_alias(&mut self, ns, from: &str, to: &str, on: Date, instrument: InstrumentId, source: SourceId) -> Result<(), StoreError>`
  - `SqliteStore::upsert_custody_place(&self, place: &CustodyRecord) -> Result<(), StoreError>`
  - `ResolveError::{Unknown, NotOnDate, Ambiguous, Store}`
  - Тривиальные чтения `instrument(id)`, `list_instruments()`, `list_aliases()` по образцу `list_accounts` — их потребляет T4

**Acceptance Criteria:**
- Резолвинг на дату, равную `valid_from`, находит инструмент; на дату, равную `valid_to`, — нет.
- Код, отсутствующий в таблице, даёт `Unknown`; код, существующий вне интервала, — `NotOnDate` с границами известного интервала.
- `rename_alias` в одной транзакции закрывает старый интервал датой смены и открывает новый с той же даты; после неё документ до смены резолвится в тот же инструмент, что и документ после.
- `upsert_custody_place` не переписывает место хранения другого владельца.
- Все три ошибки резолвинга различимы: слияние их в один `NotFound` — провал приёмки.

- [ ] **Step 1: Написать падающие тесты**

Создать `crates/iaam-store/tests/instrument_directory.rs`:

```rust
//! Резолвинг инструмента по внешнему коду на дату (E3.1).

use iaam_core::ids::{CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{AliasInterval, AliasNamespace, CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_store::reference::{AliasRecord, CustodyRecord, InstrumentRecord};
use iaam_store::{ResolveError, SqliteStore};
use time::macros::date;

fn store_with_one_bond() -> (SqliteStore, InstrumentId) {
    let store = SqliteStore::open_in_memory().expect("база в памяти");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "RU000A0JX0J2".to_owned(),
            title: "ОФЗ 26207".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент заведён");
    (store, instrument)
}

fn alias(instrument: InstrumentId, value: &str, from: time::Date, to: Option<time::Date>) -> AliasRecord {
    AliasRecord {
        namespace: AliasNamespace::Isin,
        value: value.to_owned(),
        instrument,
        interval: AliasInterval { valid_from: from, valid_to: to },
        source: SourceId::new_random(),
    }
}

#[test]
fn a_code_resolves_on_the_first_day_of_its_interval() {
    let (store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(instrument, "RU000A0JX0J2", date!(2020 - 01 - 01), None))
        .expect("псевдоним записан");

    let found = store
        .resolve_instrument(AliasNamespace::Isin, "RU000A0JX0J2", date!(2020 - 01 - 01))
        .expect("резолвинг");

    assert_eq!(found, instrument);
}

#[test]
fn a_code_does_not_resolve_on_the_day_its_interval_ends() {
    let (store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2020 - 01 - 01),
            Some(date!(2024 - 01 - 01)),
        ))
        .expect("псевдоним записан");

    let refused = store.resolve_instrument(
        AliasNamespace::Isin,
        "RU000A0JX0J2",
        date!(2024 - 01 - 01),
    );

    assert!(matches!(refused, Err(ResolveError::NotOnDate { .. })));
}

#[test]
fn an_absent_code_is_told_apart_from_a_code_outside_its_interval() {
    let (store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2020 - 01 - 01),
            Some(date!(2024 - 01 - 01)),
        ))
        .expect("псевдоним записан");

    let absent =
        store.resolve_instrument(AliasNamespace::Isin, "RU000A0ZZZZ9", date!(2021 - 06 - 01));
    let out_of_range =
        store.resolve_instrument(AliasNamespace::Isin, "RU000A0JX0J2", date!(2025 - 06 - 01));

    assert!(
        matches!(absent, Err(ResolveError::Unknown { .. })),
        "новая бумага и испорченная дата — разные ответы разбирающемуся"
    );
    assert!(matches!(out_of_range, Err(ResolveError::NotOnDate { .. })));
}

#[test]
fn a_renamed_code_resolves_from_both_sides_of_the_change() {
    let (mut store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(instrument, "RU000AOLD001", date!(2020 - 01 - 01), None))
        .expect("исходный псевдоним");

    store
        .rename_alias(
            AliasNamespace::Isin,
            "RU000AOLD001",
            "RU000ANEW002",
            date!(2024 - 01 - 01),
            instrument,
            SourceId::new_random(),
        )
        .expect("смена кода");

    let before = store
        .resolve_instrument(AliasNamespace::Isin, "RU000AOLD001", date!(2023 - 06 - 01))
        .expect("документ до смены");
    let after = store
        .resolve_instrument(AliasNamespace::Isin, "RU000ANEW002", date!(2024 - 06 - 01))
        .expect("документ после смены");

    assert_eq!(before, instrument);
    assert_eq!(after, instrument);
}

#[test]
fn the_new_code_does_not_resolve_before_the_change() {
    let (mut store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(instrument, "RU000AOLD001", date!(2020 - 01 - 01), None))
        .expect("исходный псевдоним");
    store
        .rename_alias(
            AliasNamespace::Isin,
            "RU000AOLD001",
            "RU000ANEW002",
            date!(2024 - 01 - 01),
            instrument,
            SourceId::new_random(),
        )
        .expect("смена кода");

    let anachronism =
        store.resolve_instrument(AliasNamespace::Isin, "RU000ANEW002", date!(2023 - 06 - 01));

    assert!(
        matches!(anachronism, Err(ResolveError::NotOnDate { .. })),
        "новый код в документе, датированном до смены, — признак порчи данных"
    );
}

#[test]
fn a_custody_place_of_another_owner_is_not_overwritten() {
    let store = SqliteStore::open_in_memory().expect("база в памяти");
    let place = CustodyId::new_random();
    let mine = OwnerId::new_random();
    let theirs = OwnerId::new_random();

    store
        .upsert_custody_place(&CustodyRecord {
            id: place,
            owner: mine,
            title: "Депозитарий А".to_owned(),
            institution: None,
        })
        .expect("моё место хранения");

    store
        .upsert_custody_place(&CustodyRecord {
            id: place,
            owner: theirs,
            title: "Захвачено".to_owned(),
            institution: None,
        })
        .expect("запрос чужого владельца выполняется, но ничего не меняет");

    let places = store.list_custody_places(mine).expect("список");
    assert_eq!(places[0].title, "Депозитарий А");
}
```

- [ ] **Step 2: Убедиться, что тесты не собираются**

Run: `nix develop -c cargo test -p iaam-store --test instrument_directory`
Expected: FAIL — `no variant or associated item named ... ResolveError`, `no method named resolve_instrument`.

- [ ] **Step 3: Объявить `ResolveError`**

В `crates/iaam-store/src/lib.rs`, рядом с `StoreError`:

```rust
/// Почему инструмент не разрешился по внешнему коду.
///
/// Три случая различаются намеренно. Слить их в один `NotFound`
/// означало бы отдать разбирающемуся сообщение, по которому нельзя
/// отличить новую бумагу от испорченной даты документа.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("код {value} в пространстве {namespace} неизвестен")]
    Unknown { namespace: &'static str, value: String },
    #[error(
        "код {value} в пространстве {namespace} известен, но не на {on}: \
         действует с {known_from} по {known_to}"
    )]
    NotOnDate {
        namespace: &'static str,
        value: String,
        on: String,
        known_from: String,
        known_to: String,
    },
    /// Триггер `instrument_aliases_do_not_overlap` пробит: это дефект
    /// схемы, а не данных, и молчать о нём нельзя.
    #[error("код {value} в пространстве {namespace} на {on} разрешается в {candidates} инструментов")]
    Ambiguous { namespace: &'static str, value: String, on: String, candidates: usize },
    #[error(transparent)]
    Store(#[from] StoreError),
}
```

- [ ] **Step 4: Переписать записи и методы**

В `crates/iaam-store/src/reference.rs` заменить `InstrumentRecord` и
`upsert_instrument`, добавить остальное:

```rust
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{
    AliasInterval, AliasNamespace, CurrencyRoles, InstrumentKind, Lineage, LineageReason,
};
use iaam_core::money::CurrencyCode;
use time::Date;
use time::format_description::well_known::Iso8601;

use crate::{ResolveError, SqliteStore, StoreError, now};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentRecord {
    pub id: InstrumentId,
    /// `None` — род не установлен. Такой инструмент оценивается как
    /// неполный, а не как акция по умолчанию (§4.9, §5.4).
    pub kind: Option<InstrumentKind>,
    pub symbol: String,
    pub title: String,
    pub currencies: CurrencyRoles,
    pub lineage: Option<Lineage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyRecord {
    pub id: CustodyId,
    pub owner: OwnerId,
    pub title: String,
    pub institution: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasRecord {
    pub namespace: AliasNamespace,
    pub value: String,
    pub instrument: InstrumentId,
    pub interval: AliasInterval,
    pub source: SourceId,
}

/// Дата в хранилище — ISO-8601, как и везде в схеме.
fn date_to_text(value: Date) -> String {
    value.format(&Iso8601::DATE).expect("дата форматируется в ISO-8601")
}

fn text_to_date(value: &str) -> Result<Date, StoreError> {
    Date::parse(value, &Iso8601::DATE).map_err(|_| StoreError::NotFound {
        what: "дата псевдонима",
        id: value.to_owned(),
    })
}

impl SqliteStore {
    pub fn upsert_instrument(&self, instrument: &InstrumentRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO instruments
                 (id, kind, symbol, title,
                  denomination_currency, settlement_currency, quote_currency,
                  lineage_parent, lineage_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (id) DO UPDATE SET
                 kind = excluded.kind,
                 symbol = excluded.symbol,
                 title = excluded.title,
                 denomination_currency = excluded.denomination_currency,
                 settlement_currency = excluded.settlement_currency,
                 quote_currency = excluded.quote_currency,
                 lineage_parent = excluded.lineage_parent,
                 lineage_reason = excluded.lineage_reason",
            params![
                instrument.id.inner().to_string(),
                instrument.kind.map(InstrumentKind::code),
                instrument.symbol,
                instrument.title,
                instrument.currencies.denomination.code(),
                instrument.currencies.settlement.code(),
                instrument.currencies.quote.code(),
                instrument.lineage.map(|l| l.parent.inner().to_string()),
                instrument.lineage.map(|l| l.reason.code()),
                now(),
            ],
        )?;
        Ok(())
    }

    /// Создание или обновление места хранения.
    ///
    /// Условие `WHERE custody_places.owner = excluded.owner`
    /// обязательно по той же причине, что и у счетов: без него запрос
    /// с чужим идентификатором переписал бы место хранения другого
    /// владельца (§14).
    pub fn upsert_custody_place(&self, place: &CustodyRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO custody_places (id, owner, title, institution, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 title = excluded.title,
                 institution = excluded.institution
             WHERE custody_places.owner = excluded.owner",
            params![
                place.id.inner().to_string(),
                place.owner.inner().to_string(),
                place.title,
                place.institution,
                now(),
            ],
        )?;
        Ok(())
    }

    pub fn list_custody_places(&self, owner: OwnerId) -> Result<Vec<CustodyRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, title, institution FROM custody_places
             WHERE owner = ?1 ORDER BY title, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut places = Vec::new();
        for row in rows {
            let (id, title, institution) = row?;
            places.push(CustodyRecord {
                id: CustodyId(parse_uuid(&id, "custody")?),
                owner,
                title,
                institution,
            });
        }
        Ok(places)
    }

    pub fn record_alias(&self, alias: &AliasRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO instrument_aliases
                 (namespace, value, instrument, valid_from, valid_to, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                alias.namespace.code(),
                alias.value,
                alias.instrument.inner().to_string(),
                date_to_text(alias.interval.valid_from),
                alias.interval.valid_to.map(date_to_text),
                alias.source.inner().to_string(),
                now(),
            ],
        )?;
        Ok(())
    }

    /// Смена внешнего кода: закрыть старый интервал, открыть новый.
    ///
    /// Одна транзакция обязательна: между двумя операциями старый код
    /// уже закрыт, а новый ещё не заведён, и параллельный резолвинг
    /// документа получил бы `Unknown` вместо инструмента.
    pub fn rename_alias(
        &mut self,
        namespace: AliasNamespace,
        from: &str,
        to: &str,
        on: Date,
        instrument: InstrumentId,
        source: SourceId,
    ) -> Result<(), StoreError> {
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "UPDATE instrument_aliases SET valid_to = ?1
             WHERE namespace = ?2 AND value = ?3 AND valid_to IS NULL",
            params![date_to_text(on), namespace.code(), from],
        )?;
        transaction.execute(
            "INSERT INTO instrument_aliases
                 (namespace, value, instrument, valid_from, valid_to, source, created_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
            params![
                namespace.code(),
                to,
                instrument.inner().to_string(),
                date_to_text(on),
                source.inner().to_string(),
                now(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Инструмент по внешнему коду на дату.
    pub fn resolve_instrument(
        &self,
        namespace: AliasNamespace,
        value: &str,
        on: Date,
    ) -> Result<InstrumentId, ResolveError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT instrument, valid_from, valid_to FROM instrument_aliases
                 WHERE namespace = ?1 AND value = ?2 ORDER BY valid_from",
            )
            .map_err(StoreError::from)?;
        let rows = statement
            .query_map(params![namespace.code(), value], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(StoreError::from)?;

        let mut known: Vec<(String, Date, Option<Date>)> = Vec::new();
        for row in rows {
            let (instrument, from, to) = row.map_err(StoreError::from)?;
            let from = text_to_date(&from)?;
            let to = to.as_deref().map(text_to_date).transpose()?;
            known.push((instrument, from, to));
        }

        if known.is_empty() {
            return Err(ResolveError::Unknown {
                namespace: namespace.code(),
                value: value.to_owned(),
            });
        }

        let matching: Vec<&(String, Date, Option<Date>)> = known
            .iter()
            .filter(|(_, from, to)| {
                AliasInterval { valid_from: *from, valid_to: *to }.covers(on)
            })
            .collect();

        match matching.as_slice() {
            [] => {
                let known_from = known
                    .first()
                    .map(|(_, from, _)| date_to_text(*from))
                    .unwrap_or_default();
                let known_to = known
                    .last()
                    .and_then(|(_, _, to)| *to)
                    .map_or_else(|| "открыт".to_owned(), date_to_text);
                Err(ResolveError::NotOnDate {
                    namespace: namespace.code(),
                    value: value.to_owned(),
                    on: date_to_text(on),
                    known_from,
                    known_to,
                })
            }
            [(instrument, _, _)] => Ok(InstrumentId(
                parse_uuid(instrument, "instrument").map_err(ResolveError::Store)?,
            )),
            many => Err(ResolveError::Ambiguous {
                namespace: namespace.code(),
                value: value.to_owned(),
                on: date_to_text(on),
                candidates: many.len(),
            }),
        }
    }
}
```

Если `CurrencyCode` не разбирается из строки — добавить в
`crates/iaam-core/src/money.rs` метод `from_code(&str) -> Option<Self>`
тем же способом, что `InstrumentKind::from_code` в Task 1, и покрыть его
round-trip-тестом рядом с существующими тестами `money.rs`.

- [ ] **Step 5: Свойство однозначности резолвинга**

Спек §8 требует двух свойств; round-trip кодов написан в Task 1, второе —
здесь. Добавить в конец `crates/iaam-store/tests/instrument_directory.rs`:

```rust
proptest::proptest! {
    /// При непересекающихся интервалах резолвинг на любую дату даёт
    /// не более одного кандидата. Свойство держит триггер схемы;
    /// тест проверяет, что резолвер на него действительно опирается,
    /// а не выбирает первую попавшуюся строку.
    #[test]
    fn a_code_never_resolves_to_two_instruments(
        offset in -3000i64..3000i64,
    ) {
        let (store, instrument) = store_with_one_bond();
        store
            .record_alias(&alias(
                instrument,
                "RU000A0JX0J2",
                date!(2020 - 01 - 01),
                Some(date!(2024 - 01 - 01)),
            ))
            .expect("первый интервал");
        store
            .record_alias(&alias(instrument, "RU000A0JX0J2", date!(2024 - 01 - 01), None))
            .expect("смежный интервал");

        let on = date!(2022 - 01 - 01)
            .checked_add(time::Duration::days(offset))
            .expect("дата в пределах календаря");

        // Ошибка допустима (дата раньше первого интервала), а вот
        // Ambiguous — нет: он означает пробитый триггер.
        let resolved = store.resolve_instrument(AliasNamespace::Isin, "RU000A0JX0J2", on);
        prop_assert!(!matches!(resolved, Err(ResolveError::Ambiguous { .. })));
        if let Ok(found) = resolved {
            prop_assert_eq!(found, instrument);
        }
    }
}
```

- [ ] **Step 6: Прогнать тесты**

Run: `nix develop -c cargo test -p iaam-store --test instrument_directory`
Expected: PASS, 6 тестов и одно свойство.

Run: `nix develop -c cargo test -p iaam-store`
Expected: PASS — соседние тесты хранилища не сломаны сменой `InstrumentRecord`.

- [ ] **Step 7: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-store/src/reference.rs crates/iaam-store/src/lib.rs \
        crates/iaam-store/tests/instrument_directory.rs
git commit -m "feat(store): резолвинг инструмента по коду на дату, места хранения (<bead-id>)"
```

---

### Task 4: Порт `InstrumentDirectory` в `iaam-app`

**Files:**
- Modify: `crates/iaam-app/src/ports.rs`
- Modify: `crates/iaam-app/src/adapters/sqlite.rs`

**Interfaces:**
- Consumes: `resolve_instrument`, `list_custody_places`, `upsert_instrument` (T3).
- Produces: `trait InstrumentDirectory` и три вью-типа `InstrumentView`, `AliasView`, `CustodyView`.

Порт возвращает **вью-типы `iaam-app`, а не записи `iaam-store`** — так устроены
все существующие порты (`ClassificationRuleView`, `BrokerAccessView`): иначе
транспорт узнал бы про SQLite через возвращаемое значение.

**Acceptance Criteria:**
- Трейт объектобезопасен и помечен `#[async_trait]`, как соседние порты.
- Порт объявлен только в `crates/iaam-app/src/ports.rs` — §3.2 запрещает порты в других крейтах.
- Реализация в `SqliteAdapter` делегирует через `self.blocking(...)` и не добавляет логики.
- `ResolveError::Unknown` и `ResolveError::NotOnDate` отображаются в **разные** `AppError`, иначе различие, ради которого они заведены, теряется на границе.

- [ ] **Step 1: Написать падающий тест объектобезопасности**

Дописать в конец `crates/iaam-app/src/ports.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Порт обязан быть объектобезопасным: точка сборки держит
    /// адаптеры за `Arc<dyn ...>`, и выбор адаптера не должен
    /// подниматься в типы на этапе компиляции (§3.2).
    #[test]
    fn the_instrument_directory_port_is_object_safe() {
        fn accepts(_: &dyn InstrumentDirectory) {}
        let _: fn(&dyn InstrumentDirectory) = accepts;
    }
}
```

- [ ] **Step 2: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-app --lib ports`
Expected: FAIL — `cannot find trait InstrumentDirectory in this scope`.

- [ ] **Step 3: Объявить вью-типы и порт**

В `crates/iaam-app/src/ports.rs`, рядом с остальными портами:

```rust
/// Инструмент как его видит транспорт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentView {
    pub id: InstrumentId,
    /// `None` — род не установлен. Оценка такого инструмента неполна,
    /// и подставлять акцию по умолчанию запрещено (§4.9).
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
}

/// Действующий псевдоним инструмента.
///
/// Поля `source` здесь нет намеренно: справочник глобален и читается
/// всеми, а `SourceId` указывает на документ конкретного владельца.
/// Отдать его наружу означало бы раскрыть существование чужой
/// загрузки (§14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasView {
    pub namespace: String,
    pub value: String,
    pub instrument: InstrumentId,
    pub valid_from: Date,
    pub valid_to: Option<Date>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyView {
    pub id: CustodyId,
    pub title: String,
    pub institution: Option<String>,
}

/// Справочник инструментов (§4.5, §4.7).
#[async_trait]
pub trait InstrumentDirectory: Send + Sync {
    /// Инструмент по внешнему коду на дату.
    ///
    /// Дата обязательна и умолчания «сегодня» не имеет: ISIN меняется
    /// корпоративным действием, поэтому «текущего» ответа на вопрос
    /// «какой инструмент стоит за этим кодом» не существует (§4.7).
    async fn resolve(
        &self,
        namespace: &str,
        value: &str,
        on: Date,
    ) -> Result<InstrumentId, AppError>;

    async fn instrument(&self, id: InstrumentId) -> Result<Option<InstrumentView>, AppError>;

    async fn list_instruments(&self) -> Result<Vec<InstrumentView>, AppError>;

    /// Все псевдонимы со своими интервалами.
    ///
    /// Отдаются целиком, одним запросом: разбор документа иначе ходил бы
    /// в базу на каждую строку.
    async fn list_aliases(&self) -> Result<Vec<AliasView>, AppError>;

    async fn list_custody_places(&self, owner: OwnerId) -> Result<Vec<CustodyView>, AppError>;
}
```

Добавить в `use`-блок файла недостающее: `iaam_core::ids::{CustodyId, InstrumentId}`.

- [ ] **Step 4: Реализовать в `SqliteAdapter`**

В `crates/iaam-app/src/adapters/sqlite.rs`, по образцу
`impl ClassificationRuleStore for SqliteAdapter`:

```rust
#[async_trait]
impl InstrumentDirectory for SqliteAdapter {
    async fn resolve(
        &self,
        namespace: &str,
        value: &str,
        on: Date,
    ) -> Result<InstrumentId, AppError> {
        let Some(namespace) = AliasNamespace::from_code(namespace) else {
            return Err(AppError::Invalid {
                field: "namespace",
                expected: "isin, moex_secid, ticker, figi или broker_code".to_owned(),
                actual: namespace.to_owned(),
            });
        };
        let value = value.to_owned();
        self.blocking(move |store| {
            store
                .resolve_instrument(namespace, &value, on)
                .map_err(resolve_error)
        })
        .await
    }

    async fn instrument(&self, id: InstrumentId) -> Result<Option<InstrumentView>, AppError> {
        self.blocking(move |store| {
            store
                .instrument(id)
                .map(|found| found.map(instrument_view))
                .map_err(store_error)
        })
        .await
    }

    async fn list_instruments(&self) -> Result<Vec<InstrumentView>, AppError> {
        self.blocking(|store| {
            store
                .list_instruments()
                .map(|rows| rows.into_iter().map(instrument_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn list_aliases(&self) -> Result<Vec<AliasView>, AppError> {
        self.blocking(|store| {
            store
                .list_aliases()
                .map(|rows| rows.into_iter().map(alias_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn list_custody_places(&self, owner: OwnerId) -> Result<Vec<CustodyView>, AppError> {
        self.blocking(move |store| {
            store
                .list_custody_places(owner)
                .map(|rows| rows.into_iter().map(custody_view).collect())
                .map_err(store_error)
        })
        .await
    }
}

/// Три случая резолвинга обязаны остаться различимыми и по эту сторону
/// порта: слитые в один `NotFound` они перестают отвечать на вопрос
/// «новая это бумага или испорченная дата» (E3.1, §5.1 спека задачи).
fn resolve_error(error: iaam_store::ResolveError) -> AppError {
    match error {
        iaam_store::ResolveError::Unknown { namespace, value } => AppError::NotFound {
            what: "инструмент по коду",
            id: format!("{namespace}:{value}"),
        },
        iaam_store::ResolveError::NotOnDate { namespace, value, on, known_from, known_to } => {
            AppError::Invalid {
                field: "on",
                expected: format!("дата в интервале действия кода {known_from}..{known_to}"),
                actual: format!("{namespace}:{value} на {on}"),
            }
        }
        iaam_store::ResolveError::Ambiguous { namespace, value, on, candidates } => {
            AppError::Internal(format!(
                "код {namespace}:{value} на {on} разрешается в {candidates} инструментов: \
                 триггер instrument_aliases_do_not_overlap пробит"
            ))
        }
        iaam_store::ResolveError::Store(error) => store_error(error),
    }
}

fn instrument_view(record: iaam_store::reference::InstrumentRecord) -> InstrumentView {
    InstrumentView {
        id: record.id,
        kind: record.kind.map(|kind| kind.code().to_owned()),
        symbol: record.symbol,
        title: record.title,
        denomination_currency: record.currencies.denomination.code().to_owned(),
        settlement_currency: record.currencies.settlement.code().to_owned(),
        quote_currency: record.currencies.quote.code().to_owned(),
    }
}

fn alias_view(record: iaam_store::reference::AliasRecord) -> AliasView {
    AliasView {
        namespace: record.namespace.code().to_owned(),
        value: record.value,
        instrument: record.instrument,
        valid_from: record.interval.valid_from,
        valid_to: record.interval.valid_to,
    }
}

fn custody_view(record: iaam_store::reference::CustodyRecord) -> CustodyView {
    CustodyView { id: record.id, title: record.title, institution: record.institution }
}
```

Варианты `AppError::Invalid`, `AppError::NotFound` и `AppError::Internal`
привести к тем, что действительно объявлены в
`crates/iaam-app/src/error.rs` — **прочитать файл**, а не полагаться на
имена выше. Если подходящего варианта для «код известен, но не на эту
дату» нет, добавить его, а не сливать с `NotFound`: различие — предмет
приёмки задачи.

Методы `instrument`, `list_instruments` и `list_aliases` в `iaam-store`
на этот момент могут отсутствовать — добавить их в
`crates/iaam-store/src/reference.rs` тривиальными `SELECT` по образцу
`list_accounts` из того же файла.

- [ ] **Step 5: Прогнать**

Run: `nix develop -c cargo test -p iaam-app`
Expected: PASS.

Run: `nix develop -c cargo check -p iaam-app`
Expected: без ошибок.

- [ ] **Step 6: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-app/src crates/iaam-store/src/reference.rs
git commit -m "feat(app): порт InstrumentDirectory и адаптер поверх SQLite (<bead-id>)"
```

---

### Task 5: CSV принимает код инструмента вместо идентификатора

**Files:**
- Modify: `crates/iaam-ingest/src/csv_source.rs:30-37` (`Directory`), `:191-233` (`build_trade`), рядом с `:234` (`lookup`)
- Modify: `crates/iaam-server/src/routes.rs:1140-1150` (`build_directory`)

**Interfaces:**
- Consumes: `AliasInterval` (T1), `InstrumentDirectory` (T4).
- Produces: `Directory.instruments` типа `BTreeMap<String, Vec<(AliasInterval, InstrumentId)>>` и функцию `resolve_instrument(&InstrumentAliases, &str, Date)`.

**Что здесь на самом деле сломано.** `Directory` в `crates/iaam-ingest/src/csv_source.rs:30`
уже содержит карты `instruments` и `custodies`, но `build_directory`
в `crates/iaam-server/src/routes.rs:1140` заполняет **только** `accounts`.
Обе карты приходят пустыми, `lookup` на них всегда отказывает — это и есть
`iaam-7wo`. Плоская карта «имя → идентификатор» при этом недостаточна:
один ISIN в разные годы принадлежит разным выпускам, а один выпуск за
свою жизнь меняет ISIN (§4.7), поэтому карта становится картой интервалов,
а разрешение идёт на дату строки.

**Acceptance Criteria:**
- Строка CSV, называющая инструмент кодом (тикером или ISIN), разбирается и даёт `InstrumentId`.
- Неизвестный код даёт `Rejection` с полем `instrument`, а не молчаливый пропуск строки.
- Код, известный, но не действующий на дату строки, даёт `Rejection`, отличимый по тексту от неизвестного кода.
- `resolve_custody` не меняется — `build_directory` просто перестаёт отдавать ему пустую карту.
- Дата берётся из разбираемой строки; умолчания «сегодня» нет.

- [ ] **Step 1: Написать падающие тесты**

Дописать в блок `#[cfg(test)] mod tests` файла
`crates/iaam-ingest/src/csv_source.rs`, рядом с существующими
`an_unnamed_custody_falls_back_to_the_default_and_a_named_one_is_looked_up`
(взять оттуда способ сборки `Directory` и `Row`):

```rust
    #[test]
    fn an_instrument_named_by_code_is_resolved_on_the_row_date() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                AliasInterval { valid_from: date!(2020 - 01 - 01), valid_to: None },
                instrument,
            )],
        );

        let found = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01));

        assert_eq!(found.expect("код разрешён"), instrument);
    }

    #[test]
    fn an_unknown_code_is_refused_and_not_skipped() {
        let aliases = BTreeMap::new();

        let refused = resolve_instrument(&aliases, "NOPE", date!(2024 - 03 - 01))
            .expect_err("неизвестный код");

        assert_eq!(refused.field, "instrument");
    }

    #[test]
    fn a_code_outside_its_interval_is_told_apart_from_an_unknown_one() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                AliasInterval {
                    valid_from: date!(2025 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let unknown = resolve_instrument(&BTreeMap::new(), "SBER", date!(2024 - 03 - 01))
            .expect_err("код отсутствует");
        let too_early = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01))
            .expect_err("код ещё не действовал");

        assert_ne!(
            unknown.expected, too_early.expected,
            "новая бумага и испорченная дата обязаны звучать по-разному"
        );
    }
```

- [ ] **Step 2: Убедиться, что тесты не собираются**

Run: `nix develop -c cargo test -p iaam-ingest --lib csv_source`
Expected: FAIL — `cannot find function resolve_instrument in this scope`.

- [ ] **Step 3: Сменить форму карты и добавить разрешение на дату**

В `crates/iaam-ingest/src/csv_source.rs`:

```rust
/// Псевдонимы инструмента со своими интервалами действия.
///
/// Плоская карта «код → идентификатор» здесь неверна: один ISIN
/// в разные годы принадлежит разным выпускам, а один выпуск за свою
/// жизнь меняет ISIN корпоративным действием (§4.7). Разрешение идёт
/// на дату строки, а не на «сегодня».
pub type InstrumentAliases = BTreeMap<String, Vec<(AliasInterval, InstrumentId)>>;

pub struct Directory {
    pub accounts: BTreeMap<String, AccountId>,
    pub custodies: BTreeMap<String, CustodyId>,
    pub instruments: InstrumentAliases,
    pub default_custody: Option<CustodyId>,
}

/// Инструмент по коду на дату строки.
pub fn resolve_instrument(
    aliases: &InstrumentAliases,
    code: &str,
    on: Date,
) -> Result<InstrumentId, Rejection> {
    let Some(candidates) = aliases.get(code) else {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код инструмента из справочника".into(),
            actual: code.to_owned(),
        });
    };
    let matching: Vec<InstrumentId> = candidates
        .iter()
        .filter(|(interval, _)| interval.covers(on))
        .map(|(_, id)| *id)
        .collect();
    match matching.as_slice() {
        [single] => Ok(*single),
        // Код известен, но не на эту дату. Отдельный текст, а не общий
        // отказ: это признак испорченной даты документа, а не новой
        // бумаги, и разбирающийся должен видеть разницу.
        [] => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код, действующий на дату операции".into(),
            actual: code.to_owned(),
        }),
        // Пересечение интервалов ловится триггером схемы; сюда попасть
        // можно только на справочнике, собранном мимо базы.
        _ => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "однозначный код инструмента".into(),
            actual: code.to_owned(),
        }),
    }
}
```

В `build_trade` заменить вызов `lookup(&directory.instruments, ...)` на
`resolve_instrument(&directory.instruments, name, date)`, где `date` —
уже разобранная дата строки. Универсальный `lookup` остаётся: им
пользуются счета и места хранения.

Тип поля `expected` взять тот же, что у соседних `Rejection` в файле;
если это не `String`, а `Cow`, оставить `.into()`, иначе убрать.

- [ ] **Step 4: Заполнить справочник из хранилища**

В `crates/iaam-server/src/routes.rs` заменить `build_directory`:

```rust
async fn build_directory(
    services: &Arc<AppServices>,
    principal: &Principal,
) -> Result<Directory, ApiFailure> {
    let accounts = services.store.list_accounts(principal.owner).await?;
    let places = services.directory.list_custody_places(principal.owner).await?;
    let aliases = services.directory.list_aliases().await?;

    let mut directory = Directory::default();
    for account in accounts {
        directory.accounts.insert(account.title, account.id);
    }
    for place in places {
        directory.custodies.insert(place.title, place.id);
    }
    // Псевдонимы кладутся все: разбор документа иначе ходил бы в базу
    // на каждую строку, а строк в отчёте тысячи.
    for alias in aliases {
        directory
            .instruments
            .entry(alias.value)
            .or_default()
            .push((
                AliasInterval { valid_from: alias.valid_from, valid_to: alias.valid_to },
                alias.instrument,
            ));
    }
    Ok(directory)
}
```

`services.directory` — новое поле `AppServices` типа
`Arc<dyn InstrumentDirectory>`; добавить его туда же, где объявлены
остальные адаптеры, и заполнить в точке сборки тем же `SqliteAdapter`.

- [ ] **Step 5: Прогнать**

Run: `nix develop -c cargo test -p iaam-ingest`
Expected: PASS.

Run: `nix develop -c cargo check -p iaam-server`
Expected: без ошибок.

- [ ] **Step 6: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-ingest/src crates/iaam-server/src crates/iaam-app/src
git commit -m "feat(ingest): CSV принимает код инструмента и имя места хранения (<bead-id>, iaam-7wo)"
```

---

### Task 6: Маршруты справочника в `iaam-server`

**Files:**
- Modify: `crates/iaam-server/src/dto.rs`, `crates/iaam-server/src/routes.rs`
- Modify: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: `InstrumentDirectory`, `InstrumentView`, `AliasView` (T4).
- Produces: `GET /v1/instruments`, `GET /v1/instruments/{id}`, `POST /v1/instruments/resolve`.

Префикс `/v1` обязателен: его несут все существующие маршруты
(`/v1/documents`, `/v1/reconciliation`).

**Acceptance Criteria:**
- `POST /v1/instruments/resolve` принимает `{namespace, value, on}` и возвращает `{instrument}`.
- Неизвестный код даёт `404`; код вне интервала — `422` с указанием поля, ожидаемого и полученного значения (§13); невалидный `namespace` — `422`.
- **DTO не содержит поля `source`.** Справочник глобален и читается всеми, а `SourceId` указывает на документ конкретного владельца.
- Запись в справочник агентским токеном отклоняется `403` — проверяется `Scope::may_administer`.
- Контрактные тесты покрывают все три операции и три негативных сценария.

- [ ] **Step 1: Написать падающие контрактные тесты**

Дописать в `crates/iaam-server/tests/contract.rs`, взяв способ поднятия
сервера и выдачи токена из соседних тестов файла:

```rust
#[tokio::test]
async fn resolving_a_known_code_returns_its_instrument() {
    let (app, token, instrument) = server_with_one_alias().await;

    let response = app
        .oneshot(
            Request::post("/v1/instruments/resolve")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"namespace": "isin", "value": "RU000A0JX0J2", "on": "2024-03-01"})
                        .to_string(),
                ))
                .expect("запрос"),
        )
        .await
        .expect("ответ");

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &response.into_body().collect().await.expect("тело").to_bytes(),
    )
    .expect("JSON");
    assert_eq!(body["instrument"], instrument.inner().to_string());
}

#[tokio::test]
async fn resolving_an_unknown_code_is_a_404() {
    let (app, token, _) = server_with_one_alias().await;

    let response = app
        .oneshot(
            Request::post("/v1/instruments/resolve")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"namespace": "isin", "value": "RU000ANOPE00", "on": "2024-03-01"})
                        .to_string(),
                ))
                .expect("запрос"),
        )
        .await
        .expect("ответ");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resolving_a_code_outside_its_interval_names_the_known_range() {
    let (app, token, _) = server_with_one_alias().await;

    let response = app
        .oneshot(
            Request::post("/v1/instruments/resolve")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"namespace": "isin", "value": "RU000A0JX0J2", "on": "1999-01-01"})
                        .to_string(),
                ))
                .expect("запрос"),
        )
        .await
        .expect("ответ");

    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "известный код вне интервала — не то же самое, что неизвестный код"
    );
}

#[tokio::test]
async fn an_invalid_namespace_is_a_422_naming_the_field() {
    let (app, token, _) = server_with_one_alias().await;

    let response = app
        .oneshot(
            Request::post("/v1/instruments/resolve")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"namespace": "cusip", "value": "037833100", "on": "2024-03-01"})
                        .to_string(),
                ))
                .expect("запрос"),
        )
        .await
        .expect("ответ");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = serde_json::from_slice(
        &response.into_body().collect().await.expect("тело").to_bytes(),
    )
    .expect("JSON");
    assert_eq!(body["field"], "namespace");
}

#[tokio::test]
async fn the_instrument_dto_does_not_leak_the_alias_source() {
    let (app, token, instrument) = server_with_one_alias().await;

    let response = app
        .oneshot(
            Request::get(format!("/v1/instruments/{}", instrument.inner()))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("запрос"),
        )
        .await
        .expect("ответ");

    let body: Value = serde_json::from_slice(
        &response.into_body().collect().await.expect("тело").to_bytes(),
    )
    .expect("JSON");
    assert!(
        !body.to_string().contains("source"),
        "SourceId указывает на документ владельца: наружу он не идёт (§14)"
    );
}

#[tokio::test]
async fn an_agent_token_may_not_write_to_the_directory() {
    let (app, agent_token, _) = server_with_one_alias_and_agent_token().await;

    let response = app
        .oneshot(
            Request::post("/v1/instruments")
                .header("authorization", format!("Bearer {agent_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"symbol": "HACK", "title": "Подменыш", "kind": "share"}).to_string(),
                ))
                .expect("запрос"),
        )
        .await
        .expect("ответ");

    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "справочник глобален: чужая запись портит данные всех владельцев"
    );
}
```

Помощники `server_with_one_alias` и `server_with_one_alias_and_agent_token`
написать по образцу существующей сборки состояния в этом файле: тот же
`SqliteAdapter`, `FixedClock`, `RateLimiter`, `hash_token`, `TokenRecord`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `nix develop -c cargo test -p iaam-server --test contract`
Expected: FAIL — `404` на всех новых маршрутах, включая те, где ожидается `200`.

- [ ] **Step 3: DTO**

В `crates/iaam-server/src/dto.rs`, по образцу соседних типов:

```rust
/// Инструмент справочника.
///
/// Поля `source` псевдонима здесь нет намеренно: справочник глобален
/// и читается всеми, а `SourceId` указывает на документ конкретного
/// владельца (§14).
#[derive(Debug, Serialize, ToSchema)]
pub struct InstrumentDto {
    pub id: String,
    /// `null` — род не установлен; такой инструмент оценивается
    /// как неполный (§4.9, §5.4).
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveInstrumentRequest {
    pub namespace: String,
    pub value: String,
    /// Дата документа. Обязательна: ISIN меняется, и «текущего»
    /// ответа не существует (§4.7).
    pub on: Date,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedInstrumentDto {
    pub instrument: String,
}
```

- [ ] **Step 4: Маршруты**

В `crates/iaam-server/src/routes.rs`, по образцу `upload_document`:

```rust
#[utoipa::path(
    post,
    path = "/v1/instruments/resolve",
    request_body = ResolveInstrumentRequest,
    responses(
        (status = 200, description = "Инструмент по коду на дату", body = ResolvedInstrumentDto),
        (status = 404, description = "Код неизвестен", body = ApiError),
        (status = 422, description = "Код известен, но не на эту дату, либо пространство имён неверно", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn resolve_instrument(
    State(state): State<ServerState>,
    Extension(_principal): Extension<Principal>,
    Json(request): Json<ResolveInstrumentRequest>,
) -> Result<Json<ResolvedInstrumentDto>, ApiFailure> {
    let instrument = state
        .services
        .directory
        .resolve(&request.namespace, &request.value, request.on)
        .await?;
    Ok(Json(ResolvedInstrumentDto { instrument: instrument.inner().to_string() }))
}
```

`GET /v1/instruments` и `GET /v1/instruments/{id}` написать тем же
образом через `list_instruments` и `instrument`. Все три маршрута
зарегистрировать там же, где зарегистрированы существующие, и добавить
в набор операций `utoipa`, иначе контрактный тест не найдёт их в спеке.

Маршрут записи (`POST /v1/instruments`) проверяет `Scope::may_administer`
у `Principal` и отвечает `403` при `Scope::Agent` — вариант уже есть
в `crates/iaam-app/src/ports.rs`.

- [ ] **Step 5: Прогнать**

Run: `nix develop -c cargo test -p iaam-server`
Expected: PASS.

- [ ] **Step 6: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-server
git commit -m "feat(server): маршруты справочника инструментов (<bead-id>)"
```

---

## Приёмка E3.1 (оркестратор, один раз в конце)

Воркеры этот раздел не выполняют.

```bash
nix develop -c cargo fmt --all -- --check
make check
BASE=origin/main make diff-lint
nix develop -c cargo mutants -p iaam-store -p iaam-core --in-diff <diff>
```

- Отчёт со старым ISIN и выгрузка с новым разрешаются в один `InstrumentId`; документ с новым кодом, датированный до смены, даёт `NotOnDate`.
- CSV принимается без идентификаторов инструмента — `iaam-7wo` закрыт.
- База с данными E1/E2 переживает миграцию 0005 без потери валют.
- Попытка записи в справочник агентским токеном отклоняется.
- `make check` зелёный целиком; выживших мутантов в новом коде нет.
- Ревью у codex — один раз, на весь диф E3.1.
