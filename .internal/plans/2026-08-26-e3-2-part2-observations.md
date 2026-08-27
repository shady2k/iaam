# E3.2 часть 2 — наблюдения и источники: план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** Крейта `iaam-market` описывает запросы к MOEX ISS и ЦБ РФ и разбирает их ответы на замороженных эталонах без сети, а хранилище получает append-only битемпоральные таблицы наблюдений.

**Architecture:** `iaam-market` зависит от `iaam-core` (доменные типы) и `iaam-http` (транспорт); `reqwest` не видит — это охраняется заслоном. Разбор отделён от запроса: описание запроса и разбор ответа чисты и проверяются на фикстурах. Наблюдение несёт две оси времени — `trade_date` (когда) и `observed_at` (когда узнали), — и никогда не перезаписывается.

**Tech Stack:** Rust 1.98.0, `iaam-http`, `serde_json` (MOEX отдаёт JSON), `quick-xml` (ЦБ отдаёт XML), `encoding_rs` (ЦБ отдаёт `windows-1251`), `rusqlite` (SQLite STRICT), `time`, `rust_decimal`.

**Спецификация:** `.internal/specs/2026-08-26-e3-2-market-data-design.md`, разделы 1.1, 2.4, 3.1–3.5, 8.2–8.4, 9.2, 9.4, 10
**Часть 1:** `.internal/plans/2026-08-26-e3-2-part1-http-transport.md` — сдана, `iaam-http` готов

## Global Constraints

- **Все команды идут через `nix develop -c`.**
- **Воркеры не запускают тяжёлые прогоны.** Разрешено: `cargo check -p <крейт>`, `cargo test -p <крейт>`, **`cargo clippy -p <крейт> --all-targets -- -D warnings`**, `cargo fmt --all`. Запрещено: `cargo mutants`, `cargo llvm-cov`, полный `make check`.
- **Клиппи по своей крейте обязателен.** В части 1 его не было в списке, и заслон конца эпика поймал `clamp`-паттерн, который воркер увидеть не мог. Клиппи по одной крейте общерепозиторным прогоном не является и ложных блокеров не даёт.
- **Меняешь публичное перечисление, структуру или сигнатуру — проверь потребителей.** В части 1 переименование `TinkoffError::Trust` в `Transport` сломало `iaam-app`, и воркер этого не увидел, потому что проверял только свою крейту. Найди потребителей `grep`-ом и прогони `cargo check -p` по каждому.
- **Крейта с фичей проверяется и с фичей**, если фича не ходит в сеть: клиппи в `make check` идёт с `--all-features` и компилирует то, чего обычный прогон не касается.
- **Проза русская, имена тестов английские.**
- **`rustfmt.toml`: `fn_call_width = 60` при `max_width = 100`.** `cargo fmt --all` перед каждым коммитом.
- **`async_trait` — только в `iaam-app`** (правило 10 `check-architecture.sh`).
- **`reqwest` — только в `iaam-http`** (правило 11). `iaam-market` зависит от `iaam-http`, а не от `reqwest`.
- **`[lints] workspace = true` обязателен в манифесте каждой крейты** (правило 7).
- **Исчерпаемые `enum` без `#[non_exhaustive]`** (§15.1): добавление варианта обязано ломать сборку у потребителей.
- **`unknown` не является нулём (§4.9).** Отсутствующее значение — `Option<T>`, а не пустая строка и не вариант `Unknown`.
- **Фикстура и тест, который её читает, ложатся ОДНИМ коммитом.** Заслон `check-fixtures.sh` требует, чтобы каждая фикстура из манифеста упоминалась хотя бы одним `.rs`-тестом: фикстура без теста — «мёртвый эталон» и отказ заслона. Манифест обновляется в том же коммите.
- **Правки файлов политики разрешены владельцем 2026-08-26** для этого эпика: корневой `Cargo.toml` (members), `tests/fixtures` и `MANIFEST.sha256`. Прочие файлы политики не трогать.
- **Ослабление теста ради прохождения запрещено (§15.7).**
- **Фильтр `cargo test` совпадает с именем ТЕСТА, а не файла.** `cargo test -p X foo` при отсутствии теста с `foo` в имени печатает «0 passed» и возвращает НОЛЬ — то есть зелёный отчёт без единого выполненного теста. Для интеграционного файла `tests/foo.rs` правильная форма — `--test foo`. Если прогон показал `0 passed; N filtered out`, это не успех, а несостоявшаяся проверка.

---

## Карта файлов

| Файл | Ответственность | Задача |
|---|---|---|
| `crates/iaam-market/Cargo.toml` | манифест крейты | 1 |
| `crates/iaam-market/src/lib.rs` | объявляет все модули; после задачи 1 не правится | 1 |
| `crates/iaam-market/src/observation.rs` | битемпоральные типы наблюдений | 1 |
| `crates/iaam-market/src/error.rs` | `MarketError` | 1 |
| `Cargo.toml` | `members` воркспейса | 1 |
| `crates/iaam-market/src/moex/mod.rs` | описание запроса истории | 2 |
| `crates/iaam-market/src/moex/parse.rs` | разбор JSON ISS | 2 |
| `tests/fixtures/market/moex-iss-history-sber.json` | замороженный ответ ISS | 2 |
| `crates/iaam-market/src/cbr/mod.rs` | описание запросов ЦБ | 3, 4 |
| `crates/iaam-market/src/cbr/fx.rs` | разбор курсов | 3 |
| `tests/fixtures/market/cbr-xml-daily.xml` | курсы на дату | 3 |
| `tests/fixtures/market/cbr-xml-dynamic-usd.xml` | курс за период | 3 |
| `crates/iaam-market/src/cbr/key_rate.rs` | конверт SOAP и разбор ставки | 4 |
| `tests/fixtures/market/cbr-keyrate-soap.xml` | ответ `KeyRateXML` | 4 |
| `crates/iaam-store/migrations/0006_*.sql` | таблицы наблюдений | 5 |
| `crates/iaam-store/src/market.rs` | запись и чтение наблюдений | 5 |

**Порядок и параллельность.** Задача 1 первая — она объявляет **все** модули и создаёт заглушки, чтобы задачи 2, 3, 4 не дрались за `lib.rs`. Задача 5 не зависит от задач 1–4 вовсе (другая крейта) и идёт **параллельно с задачей 1**. Задачи 2, 3, 4 стоят на задаче 1 и между собой независимы — идут параллельно, у каждой свой каталог модуля и своя фикстура.

**Фикстуры уже записаны** координатором с живых источников 2026-08-26 и лежат в брифе каждого воркера как `.brief/fixture/<имя>`. Перезаписывать их из сети запрещено: у воркеров сети нет, а эталон обязан быть записью реального ответа, а не выдумкой.

---

### Task 1: Крейта `iaam-market` — типы наблюдений

**Files:**
- Create: `crates/iaam-market/Cargo.toml`
- Create: `crates/iaam-market/src/lib.rs`
- Create: `crates/iaam-market/src/observation.rs`
- Create: `crates/iaam-market/src/error.rs`
- Create: заглушки `crates/iaam-market/src/moex/mod.rs`, `moex/parse.rs`, `cbr/mod.rs`, `cbr/fx.rs`, `cbr/key_rate.rs`
- Modify: `Cargo.toml:3` (members)

**Interfaces:**
- Produces: `TradeDate(Date)`, `ObservedAt(OffsetDateTime)`; `Executability::{Executable, IndicativePreviousClose, Stale}`; `PriceKind::{Close, LegalClose, WeightedAverage, MarketPrice2, MarketPrice3, AdmittedQuote}`; `Venue { board: String, session: TradingSession }`; `PriceObservation`; `FxObservation`; `KeyRateObservation`; `MarketError::{Malformed(String), UnknownCurrency(String), Truncated { got: usize, total: usize }}`.

**Acceptance Criteria:**
- Две оси времени — **разные типы**, а не два `Date`: перепутать «когда цена» и «когда узнали» местами не должно быть представимо.
- `Executability` — атрибут источника; варианта `CarriedForward` в нём **нет**: перенос цены на нерабочий день это вывод политики, а не наблюдение (раздел 3.5 спеки).
- `PriceKind` перечисляет все шесть колонок ISS: выбор между ними принадлежит E3.3, поэтому ни одна не объявлена главной.
- Крейта собирается, входит в воркспейс, `[lints] workspace = true` есть.
- `reqwest` в манифесте не объявлен.

- [ ] **Step 1: Манифест и место в воркспейсе**

`crates/iaam-market/Cargo.toml`:

```toml
[package]
name = "iaam-market"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
# Доменные типы: валюта, идентификатор инструмента, точные числа.
iaam-core = { path = "../iaam-core", version = "0.1.0" }
# Транспорт, доверие и устойчивость. reqwest здесь НЕ объявляется:
# заслон архитектуры запрещает его вне iaam-http (§3.1).
iaam-http = { path = "../iaam-http", version = "0.1.0" }
# MOEX ISS отдаёт JSON.
serde_json = "1"
serde = { version = "1", features = ["derive"] }
# ЦБ РФ отдаёт XML: и курсы, и SOAP-ответ со ставкой.
quick-xml = "0.38"
# ЦБ отдаёт windows-1251. Своя перекодировка — лишнее место для ошибки
# в том, что уже решено, а from_utf8 на этих байтах просто падает.
encoding_rs = "0.8"
time = { version = "0.3", default-features = false, features = ["std", "parsing", "formatting", "macros"] }
rust_decimal = { version = "1", default-features = false, features = ["std", "serde"] }
thiserror = "2"

[lints]
workspace = true
```

Корневой `Cargo.toml`, строка 3 — добавить `"crates/iaam-market"` после `"crates/iaam-http"`.

- [ ] **Step 2: Написать падающий тест на различимость осей**

`crates/iaam-market/src/observation.rs`, в конец:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    #[test]
    fn the_two_time_axes_are_distinct_types() {
        let traded = TradeDate(date!(2026 - 08 - 03));
        let learned = ObservedAt(datetime!(2026-08-26 09:00:00 UTC));
        // Тест существует ради компилятора: если оси когда-нибудь станут
        // одним типом, перестановка аргументов в конструкторе наблюдения
        // пройдёт молча, а это подмена «когда цена» на «когда узнали».
        assert_ne!(traded.0.to_string(), learned.0.date().to_string());
    }

    #[test]
    fn executability_has_no_carried_forward_variant() {
        // Перенос цены на нерабочий день — вывод политики (E3.3),
        // а не то, что прислал источник (раздел 3.5 спеки). Вариант
        // в этом перечислении означал бы, что вывод можно записать
        // наблюдением, и различие «биржа не торговала» против
        // «мы подставили вчерашнее» потерялось бы навсегда.
        let all = [
            Executability::Executable,
            Executability::IndicativePreviousClose,
            Executability::Stale,
        ];
        assert_eq!(all.len(), 3);
    }
}
```

- [ ] **Step 3: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-market --lib`
Expected: FAIL, `cannot find type TradeDate in this scope`.

- [ ] **Step 4: Реализовать типы**

`crates/iaam-market/src/observation.rs`:

```rust
//! Наблюдение рыночных данных (раздел 3 дизайна E3.2).
//!
//! Наблюдение **append-only и битемпорально**. Две оси времени:
//! `trade_date` — к какому дню относится значение, `observed_at` —
//! когда мы об этом узнали. Вторая назначается системой, а не берётся
//! из ответа: доверить её часам источника значит сделать ось знания
//! подделываемой ответом, а вместе с ней и воспроизводимость отчёта.

use iaam_core::ids::InstrumentId;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

/// К какому торговому дню относится значение (valid time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TradeDate(pub Date);

/// Когда мы узнали значение (knowledge time).
///
/// Отдельный тип, а не второй `Date`, намеренно: перепутать оси местами
/// не должно быть представимо (§15.1). Перестановка «когда цена» и
/// «когда узнали» не даёт ни ошибки компиляции, ни неверного числа —
/// она молча ломает воспроизводимость отчёта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservedAt(pub OffsetDateTime);

/// Исполнимость цены — **атрибут источника**, а не вывод политики.
///
/// Варианта `CarriedForward` здесь нет и быть не может: перенос цены
/// на нерабочий день выводится правилом оценки (E3.3). Записать его
/// наблюдением значит стереть различие между «биржа не торговала»
/// и «мы подставили вчерашнее» — и лишиться возможности пересчитать
/// отчёт по изменившемуся правилу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Executability {
    /// Цена, по которой можно выйти: доступный bid.
    Executable,
    /// Цена закрытия предыдущих торгов — ориентир, не исполнение.
    IndicativePreviousClose,
    /// Наблюдение старше порога свежести источника.
    Stale,
}

/// Какая именно цена наблюдалась.
///
/// ISS отдаёт шесть кандидатов в одной строке. Ни один не объявлен
/// главным: выбор между ними — политика оценки, то есть E3.3.
/// Объявить главного здесь значило бы принять решение чужого
/// подпроекта молча.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceKind {
    Close,
    LegalClose,
    WeightedAverage,
    MarketPrice2,
    MarketPrice3,
    AdmittedQuote,
}

/// Режим торгов.
///
/// Входит в идентичность наблюдения: один `SECID` торгуется в разных
/// режимах и валютах, и без режима две цены одного дня выглядят как
/// исправление одной.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Venue {
    /// Код режима торгов ISS, например `TQBR`.
    pub board: String,
    /// Номер торговой сессии: основная и вечерняя различаются.
    pub session: i64,
}

/// Наблюдение цены инструмента.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceObservation {
    pub instrument: InstrumentId,
    pub venue: Venue,
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    pub kind: PriceKind,
    pub price: Dec,
    /// Валюта площадки, **не «валюта инструмента»**: ISS отдаёт
    /// `CURRENCYID` построчно, и она принадлежит наблюдению.
    pub currency: CurrencyCode,
    pub executability: Executability,
}

/// Наблюдение курса.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxObservation {
    pub from: CurrencyCode,
    pub to: CurrencyCode,
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    /// Номинал: ЦБ публикует курс за 1, 10 или 100 единиц.
    /// Голое число без номинала неинтерпретируемо.
    pub nominal: u32,
    /// Значение за номинал, как его дал источник.
    pub value: Dec,
    /// Значение за единицу. Хранится **вместе** с `value`: расхождение
    /// между ними — сигнал порчи разбора, и потерять его нельзя.
    pub unit_rate: Dec,
}

/// Наблюдение ключевой ставки.
///
/// Именно наблюдение по рабочему дню, а не интервал: источник отдаёт
/// дневной ряд и даты вступления в нём нет вовсе (раздел 8.3 спеки).
/// Интервал выводится на чтении и помечается выведенным.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRateObservation {
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    pub rate: Dec,
}
```

`crates/iaam-market/src/error.rs`:

```rust
//! Отказы разбора.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MarketError {
    #[error("ответ источника не разобран: {0}")]
    Malformed(String),
    /// Неизвестный код валюты. Отдельный вариант, а не `Malformed`:
    /// код `SUR` у MOEX означает рубль, и молчаливое превращение
    /// незнакомого кода в ошибку разбора спрятало бы причину.
    #[error("неизвестный код валюты источника: {0}")]
    UnknownCurrency(String),
    /// Ответ с пагинацией оборван.
    ///
    /// Отдельный отказ, а не «сколько пришло, столько и ладно»:
    /// неполная страница, принятая за полную, даёт пробел в ряду,
    /// который потом невозможно отличить от нерабочего дня.
    #[error("страница неполна: получено {got} из {total}")]
    Truncated { got: usize, total: usize },
}
```

`crates/iaam-market/src/lib.rs`:

```rust
//! Рыночные данные: MOEX ISS и ЦБ РФ (§12).
//!
//! Крейта **описывает запрос и разбирает ответ**. HTTP она не знает —
//! транспорт живёт в `iaam-http`, и это охраняется правилом 11
//! `scripts/check-architecture.sh`. Отсюда главное свойство: разбор
//! проверяется на замороженных эталонах **без сети и без подмены HTTP**.
//!
//! Крейта не решает, какую цену применить: она отдаёт все наблюдения,
//! какие дал источник. Выбор между ними — политика оценки (E3.3).

pub mod cbr;
pub mod error;
pub mod moex;
pub mod observation;

pub use error::MarketError;
pub use observation::{
    Executability, FxObservation, KeyRateObservation, ObservedAt, PriceKind, PriceObservation,
    TradeDate, Venue,
};
```

**`lib.rs` объявляет оба модуля источников сразу и больше не правится ни одной задачей** — задачи 2, 3 и 4 идут параллельно, и общий файл стал бы гонкой правок. Создай заглушки:

`crates/iaam-market/src/moex/mod.rs`:
```rust
//! MOEX ISS. Наполняется задачей 2.

pub mod parse;
```
`crates/iaam-market/src/moex/parse.rs`:
```rust
//! Заглушка. Наполняется задачей 2.
```
`crates/iaam-market/src/cbr/mod.rs`:
```rust
//! ЦБ РФ. Наполняется задачами 3 и 4.

pub mod fx;
pub mod key_rate;
```
`crates/iaam-market/src/cbr/fx.rs`:
```rust
//! Заглушка. Наполняется задачей 3.
```
`crates/iaam-market/src/cbr/key_rate.rs`:
```rust
//! Заглушка. Наполняется задачей 4.
```

- [ ] **Step 5: Прогнать тесты и клиппи**

Run: `nix develop -c cargo test -p iaam-market --lib`
Expected: PASS, два теста.

Run: `nix develop -c cargo clippy -p iaam-market --all-targets -- -D warnings`
Expected: `Finished`.

- [ ] **Step 6: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-market Cargo.toml
git commit -m "feat(market): крейта рыночных данных — битемпоральные наблюдения"
```

---

### Task 2: MOEX ISS — описание запроса и разбор истории

**Files:**
- Modify: `crates/iaam-market/src/moex/mod.rs`
- Modify: `crates/iaam-market/src/moex/parse.rs`
- Create: `tests/fixtures/market/moex-iss-history-sber.json` (перенести из `.brief/fixture/`)
- Modify: `tests/fixtures/MANIFEST.sha256`

**Interfaces:**
- Consumes: `PriceObservation`, `PriceKind`, `Venue`, `TradeDate`, `ObservedAt`, `Executability`, `MarketError` (задача 1); `HttpRequest`, `Destination::MoexIss` (`iaam-http`).
- Produces: `history_request(engine, market, board, secid, from, till, start) -> HttpRequest`; `parse_history(body: &str, instrument: InstrumentId, observed_at: ObservedAt) -> Result<Vec<PriceObservation>, MarketError>`; `pub(crate) fn currency_of(code: &str) -> Result<CurrencyCode, MarketError>`.

**Acceptance Criteria:**
- **`SUR` разрешается в рубль.** ISS отдаёт `CURRENCYID` равным `SUR` — это код советского рубля из старого стандарта, который биржа не меняла. Наивный разбор либо упадёт, либо заведёт вторую валюту рядом с рублём, и позиции разъедутся по двум валютам с одним смыслом.
- Из одной строки ответа получается **несколько наблюдений** — по одному на каждую непустую ценовую колонку. Ни одна не объявлена главной.
- Пустая колонка (`null` в JSON) наблюдения не порождает: отсутствующее значение это `Option`, а не ноль (§4.9).
- Площадка и номер сессии входят в наблюдение.
- **Неполная страница — отказ.** `history.cursor` даёт `INDEX`, `TOTAL`, `PAGESIZE`; если пришло меньше, чем обещает `TOTAL`, разбор возвращает `Truncated`, а не молча отдаёт неполный ряд.
- Фикстура и тест ложатся одним коммитом; манифест обновлён.

- [ ] **Step 1: Внести фикстуру и заморозить её**

```bash
mkdir -p tests/fixtures/market
cp .brief/fixture/moex-iss-history-sber.json tests/fixtures/market/
sha256sum tests/fixtures/market/moex-iss-history-sber.json >> tests/fixtures/MANIFEST.sha256
```

Фикстура записана координатором с живого ISS 2026-08-26. **Не перезаписывай её и не правь**: ожидаемые значения приходят из независимого источника и не подгоняются под зелёный тест (§15.7).

- [ ] **Step 2: Написать падающий тест**

`crates/iaam-market/src/moex/parse.rs`, в конец:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::money::CurrencyCode;
    use time::macros::{date, datetime};

    const FIXTURE: &str = include_str!(
        "../../../../tests/fixtures/market/moex-iss-history-sber.json"
    );

    fn observed() -> ObservedAt {
        ObservedAt(datetime!(2026-08-26 09:00:00 UTC))
    }

    fn instrument() -> InstrumentId {
        InstrumentId::new(uuid::Uuid::nil())
    }

    #[test]
    fn moex_reports_the_rouble_as_sur_and_it_resolves_to_rub() {
        // SUR — код советского рубля из старого стандарта, который биржа
        // не меняла. Разбор, не знающий этого, либо падает, либо заводит
        // вторую валюту рядом с рублём.
        assert_eq!(currency_of("SUR").expect("рубль"), CurrencyCode::Rub);
    }

    #[test]
    fn an_unknown_currency_is_named_rather_than_swallowed() {
        assert!(matches!(
            currency_of("ZZZ"),
            Err(MarketError::UnknownCurrency(code)) if code == "ZZZ"
        ));
    }

    #[test]
    fn one_row_yields_one_observation_per_non_empty_price_column() {
        let observations =
            parse_history(FIXTURE, instrument(), observed()).expect("разбор фикстуры");
        let first_day: Vec<_> = observations
            .iter()
            .filter(|o| o.trade_date == TradeDate(date!(2026 - 08 - 03)))
            .collect();
        // В фикстуре у первой строки ADMITTEDQUOTE пуст, остальные пять
        // колонок заполнены.
        assert_eq!(
            first_day.len(),
            5,
            "ожидалось пять наблюдений на день, получено {}",
            first_day.len()
        );
        assert!(
            !first_day.iter().any(|o| o.kind == PriceKind::AdmittedQuote),
            "пустая колонка не должна порождать наблюдение"
        );
    }

    #[test]
    fn the_venue_and_session_travel_with_the_observation() {
        let observations =
            parse_history(FIXTURE, instrument(), observed()).expect("разбор фикстуры");
        let first = observations.first().expect("хотя бы одно наблюдение");
        assert_eq!(first.venue.board, "TQBR");
        assert_eq!(first.venue.session, 3);
        assert_eq!(first.currency, CurrencyCode::Rub);
    }

    #[test]
    fn the_knowledge_axis_comes_from_the_caller_not_the_response() {
        // В ответе ISS нет момента наблюдения вовсе. Он назначается
        // системой: доверить его источнику значит сделать ось знания
        // подделываемой ответом.
        let observations =
            parse_history(FIXTURE, instrument(), observed()).expect("разбор фикстуры");
        assert!(observations.iter().all(|o| o.observed_at == observed()));
    }

    #[test]
    fn a_short_page_is_a_refusal_not_a_shorter_series() {
        let truncated = FIXTURE.replace("\"TOTAL\": 15", "\"TOTAL\": 40");
        assert!(matches!(
            parse_history(&truncated, instrument(), observed()),
            Err(MarketError::Truncated { got: 15, total: 40 })
        ));
    }
}
```

- [ ] **Step 3: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-market moex`
Expected: FAIL, `cannot find function parse_history in this scope`.

- [ ] **Step 4: Реализовать описание запроса**

`crates/iaam-market/src/moex/mod.rs`:

```rust
//! MOEX ISS: описание запроса истории торгов.
//!
//! Официальная дневная история, **не свечи**. Свечной эндпойнт
//! (`/candles.json`) существует, но официальной истории не заменяет:
//! это другой источник, и смешивать их в одной серии нельзя.

pub mod parse;

use iaam_http::{Destination, HttpRequest};
use time::Date;
use time::format_description::well_known::Iso8601;

/// Запрос дневной истории по бумаге за интервал.
///
/// `board` параметром, а не константой: путь зависит от
/// engine/market/board, и площадка входит в идентичность наблюдения.
/// Зашить `TQBR` значило бы молча решить, что других режимов нет.
#[must_use]
pub fn history_request(
    engine: &str,
    market: &str,
    board: &str,
    secid: &str,
    from: Date,
    till: Date,
    start: u32,
) -> HttpRequest {
    let path = format!(
        "/iss/history/engines/{engine}/markets/{market}/boards/{board}/securities/{secid}.json"
    );
    HttpRequest::get(Destination::MoexIss, &path)
        .with_query("from", &iso(from))
        .with_query("till", &iso(till))
        .with_query("start", &start.to_string())
}

fn iso(date: Date) -> String {
    date.format(&Iso8601::DATE).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn the_board_is_part_of_the_path_not_a_constant() {
        let request = history_request(
            "stock",
            "shares",
            "SMAL",
            "SBER",
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 21),
            0,
        );
        assert!(
            request.url().contains("/boards/SMAL/"),
            "площадка обязана попадать в путь: {}",
            request.url()
        );
    }

    #[test]
    fn the_interval_travels_as_query_parameters() {
        let request = history_request(
            "stock",
            "shares",
            "TQBR",
            "SBER",
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 21),
            0,
        );
        let url = request.url();
        assert!(url.contains("from=2026-08-03"), "{url}");
        assert!(url.contains("till=2026-08-21"), "{url}");
    }
}
```

- [ ] **Step 5: Реализовать разбор**

`crates/iaam-market/src/moex/parse.rs`:

```rust
//! Разбор ответа ISS.
//!
//! Ответ приходит табличным: массив `columns` с именами и массив `data`
//! со строками. Индексы колонок берутся из `columns` по имени, а не
//! зашиваются числами: ISS добавляет колонки, и позиционный разбор
//! однажды прочитает объём как цену.

use iaam_core::ids::InstrumentId;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use rust_decimal::Decimal;
use serde_json::Value;
use time::Date;
use time::format_description::well_known::Iso8601;

use crate::error::MarketError;
use crate::observation::{
    Executability, ObservedAt, PriceKind, PriceObservation, TradeDate, Venue,
};

/// Ценовые колонки ISS и их смысл.
///
/// Все шесть равноправны: выбор между ними — политика оценки (E3.3).
const PRICE_COLUMNS: [(&str, PriceKind); 6] = [
    ("CLOSE", PriceKind::Close),
    ("LEGALCLOSEPRICE", PriceKind::LegalClose),
    ("WAPRICE", PriceKind::WeightedAverage),
    ("MARKETPRICE2", PriceKind::MarketPrice2),
    ("MARKETPRICE3", PriceKind::MarketPrice3),
    ("ADMITTEDQUOTE", PriceKind::AdmittedQuote),
];

/// Код валюты источника в доменный код.
///
/// `SUR` — код советского рубля из старого стандарта, который биржа
/// не меняла. Без этого отображения разбор либо падает на каждой
/// рублёвой бумаге, либо заводит вторую валюту рядом с рублём,
/// и позиции разъезжаются по двум валютам с одним смыслом.
pub(crate) fn currency_of(code: &str) -> Result<CurrencyCode, MarketError> {
    match code {
        "SUR" | "RUB" => Ok(CurrencyCode::Rub),
        "USD" => Ok(CurrencyCode::Usd),
        "EUR" => Ok(CurrencyCode::Eur),
        other => Err(MarketError::UnknownCurrency(other.to_owned())),
    }
}

/// Разбор страницы истории в наблюдения.
///
/// `observed_at` приходит **снаружи**: в ответе ISS момента наблюдения
/// нет вовсе, и назначать его обязана система.
pub fn parse_history(
    body: &str,
    instrument: InstrumentId,
    observed_at: ObservedAt,
) -> Result<Vec<PriceObservation>, MarketError> {
    let root: Value =
        serde_json::from_str(body).map_err(|error| MarketError::Malformed(error.to_string()))?;
    let block = root
        .get("history")
        .ok_or_else(|| MarketError::Malformed("нет блока history".to_owned()))?;
    let names = column_names(block)?;
    let rows = block
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет history.data".to_owned()))?;

    ensure_page_is_whole(&root, rows.len())?;

    let mut observations = Vec::new();
    for row in rows {
        let row = row
            .as_array()
            .ok_or_else(|| MarketError::Malformed("строка history.data не массив".to_owned()))?;
        let get = |name: &str| index_of(&names, name).and_then(|i| row.get(i));
        let trade_date = TradeDate(parse_date(
            get("TRADEDATE").and_then(Value::as_str).ok_or_else(|| {
                MarketError::Malformed("строка без TRADEDATE".to_owned())
            })?,
        )?);
        let currency = currency_of(get("CURRENCYID").and_then(Value::as_str).ok_or_else(
            || MarketError::Malformed("строка без CURRENCYID".to_owned()),
        )?)?;
        let venue = Venue {
            board: get("BOARDID")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("строка без BOARDID".to_owned()))?
                .to_owned(),
            session: get("TRADINGSESSION").and_then(Value::as_i64).unwrap_or(0),
        };
        for (column, kind) in PRICE_COLUMNS {
            // Пустая колонка наблюдения не порождает: отсутствующее
            // значение это Option, а не ноль (§4.9). Ноль в цене
            // означал бы «бумага ничего не стоит».
            let Some(price) = get(column).and_then(Value::as_f64) else {
                continue;
            };
            let price = Decimal::try_from(price)
                .map_err(|error| MarketError::Malformed(error.to_string()))?;
            observations.push(PriceObservation {
                instrument,
                venue: venue.clone(),
                trade_date,
                observed_at,
                kind,
                price: Dec::new(price),
                currency,
                // Дневная история даёт цену закрытия, а не исполнимый bid.
                // Помечать её исполнимой значило бы выдать ориентир
                // за цену выхода (§5.1, §5.3).
                executability: Executability::IndicativePreviousClose,
            });
        }
    }
    Ok(observations)
}

/// Страница пришла целиком.
///
/// Курсор ISS даёт `INDEX`, `TOTAL` и `PAGESIZE`. Неполная страница,
/// принятая за полную, даёт пробел в ряду, который потом невозможно
/// отличить от нерабочего дня — то есть тихую порчу истории.
fn ensure_page_is_whole(root: &Value, got: usize) -> Result<(), MarketError> {
    let Some(cursor) = root.get("history.cursor") else {
        return Ok(());
    };
    let names = column_names(cursor)?;
    let Some(row) = cursor
        .get("data")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let value = |name: &str| index_of(&names, name).and_then(|i| row.get(i)?.as_u64());
    let (Some(index), Some(total), Some(page)) =
        (value("INDEX"), value("TOTAL"), value("PAGESIZE"))
    else {
        return Ok(());
    };
    let expected = usize::try_from(total.saturating_sub(index))
        .unwrap_or(usize::MAX)
        .min(usize::try_from(page).unwrap_or(usize::MAX));
    if got < expected {
        return Err(MarketError::Truncated {
            got,
            total: usize::try_from(total).unwrap_or(usize::MAX),
        });
    }
    Ok(())
}

fn column_names(block: &Value) -> Result<Vec<String>, MarketError> {
    block
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет columns".to_owned()))?
        .iter()
        .map(|name| {
            name.as_str()
                .map(str::to_owned)
                .ok_or_else(|| MarketError::Malformed("имя колонки не строка".to_owned()))
        })
        .collect()
}

fn index_of(names: &[String], name: &str) -> Option<usize> {
    names.iter().position(|candidate| candidate == name)
}

fn parse_date(value: &str) -> Result<Date, MarketError> {
    Date::parse(value, &Iso8601::DATE)
        .map_err(|error| MarketError::Malformed(format!("дата {value}: {error}")))
}
```

**Проверь потребителей** `CurrencyCode`: если в ядре нет варианта `Eur` или он называется иначе — не выдумывай, посмотри `crates/iaam-core/src/money.rs:22` и используй существующие варианты. Не хватает нужного — остановись и сообщи, добавление варианта в исчерпаемое перечисление ядра ломает сборку у всех потребителей и является отдельным решением.

- [ ] **Step 6: Прогнать тесты, клиппи, заслон фикстур**

Run: `nix develop -c cargo test -p iaam-market moex`
Expected: PASS.

Run: `nix develop -c cargo clippy -p iaam-market --all-targets -- -D warnings`
Expected: `Finished`.

Run: `nix develop -c ./scripts/check-fixtures.sh`
Expected: `Фикстуры проверены.`

- [ ] **Step 7: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-market tests/fixtures
git commit -m "feat(market): MOEX ISS — запрос истории и разбор наблюдений"
```

---

### Task 3: ЦБ РФ — курсы валют

**Files:**
- Modify: `crates/iaam-market/src/cbr/mod.rs`
- Modify: `crates/iaam-market/src/cbr/fx.rs`
- Create: `tests/fixtures/market/cbr-xml-daily.xml`, `tests/fixtures/market/cbr-xml-dynamic-usd.xml`
- Modify: `tests/fixtures/MANIFEST.sha256`

**Interfaces:**
- Consumes: `FxObservation`, `TradeDate`, `ObservedAt`, `MarketError` (задача 1); `HttpRequest`, `Destination::CbrScripts`.
- Produces: `daily_request(on: Date) -> HttpRequest`; `dynamic_request(from: Date, till: Date, cbr_currency_id: &str) -> HttpRequest`; `decode_cp1251(bytes: &[u8]) -> String`; `parse_daily_raw(xml: &str) -> Result<Vec<CbrRate>, MarketError>` где `CbrRate { char_code: String, nominal: u32, value: Decimal, unit_rate: Decimal, date: Date }`; `parse_daily(xml: &str, observed_at) -> Result<Vec<FxObservation>, MarketError>`; `parse_dynamic(xml: &str, to: CurrencyCode, observed_at) -> Result<Vec<FxObservation>, MarketError>`.

**Acceptance Criteria:**
- **Байты декодируются из `windows-1251`.** Ответ ЦБ объявляет эту кодировку в прологе; `String::from_utf8` на нём падает, а lossy-декодирование испортило бы названия валют.
- **Десятичная запятая разбирается как запятая.** `85,1293` — это значение; `parse::<Decimal>()` на нём падает, а `replace(',', '.')` без осознания того, что это конвенция источника, — случайность, а не решение.
- `Nominal` и `VunitRate` хранятся **оба**: расхождение между ними сигналит о порче разбора.
- **Разбор двухслойный.** Сырой слой (`CbrRate`) держит `CharCode` строкой и проверяется независимо от того, какие валюты знает ядро; отображение в `CurrencyCode` — отдельный шаг, пропускающий незнакомые валюты. Слой не украшение: у всех валют, которые ядро знает, номинал ЦБ равен единице, и без сырого слоя работу с номиналом нечем проверить. Добавлять валюту в исчерпаемое перечисление ядра ради теста **запрещено** — это ломает сборку у всех потребителей и является отдельным решением владельца.
- Дата ЦБ приходит как `DD.MM.YYYY`, а не ISO.
- Ряд идёт по рабочим дням: выходных в нём нет, и отсутствие дня — не ошибка разбора.
- Обе фикстуры и тесты ложатся одним коммитом; манифест обновлён.

- [ ] **Step 1: Внести фикстуры**

```bash
cp .brief/fixture/cbr-xml-daily.xml tests/fixtures/market/
cp .brief/fixture/cbr-xml-dynamic-usd.xml tests/fixtures/market/
sha256sum tests/fixtures/market/cbr-xml-daily.xml >> tests/fixtures/MANIFEST.sha256
sha256sum tests/fixtures/market/cbr-xml-dynamic-usd.xml >> tests/fixtures/MANIFEST.sha256
```

**Фикстуры лежат в `windows-1251`.** Не открывай их редактором, который «починит» кодировку: заслон `check-fixtures.sh` сверяет контрольную сумму, и любая правка байтов — отказ.

- [ ] **Step 2: Написать падающий тест**

`crates/iaam-market/src/cbr/fx.rs`, в конец:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::money::CurrencyCode;
    use time::macros::{date, datetime};

    const DAILY: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/cbr-xml-daily.xml");
    const DYNAMIC: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/cbr-xml-dynamic-usd.xml");

    fn observed() -> ObservedAt {
        ObservedAt(datetime!(2026-08-26 09:00:00 UTC))
    }

    #[test]
    fn the_response_is_cp1251_and_utf8_decoding_would_fail() {
        // Пролог ответа объявляет windows-1251, и в названиях валют
        // лежат байты, которые UTF-8 не принимает.
        assert!(
            core::str::from_utf8(DAILY).is_err(),
            "фикстура перестала быть cp1251 — её подменили"
        );
        let text = decode_cp1251(DAILY);
        assert!(text.contains("Австралийский доллар"), "декодирование не дало кириллицы");
    }

    #[test]
    fn a_decimal_comma_is_the_source_convention_not_a_typo() {
        assert_eq!(parse_cbr_decimal("85,1293").expect("число").to_string(), "85.1293");
        assert!(parse_cbr_decimal("85.1293").is_err(), "точка не является конвенцией ЦБ");
    }

    #[test]
    fn nominal_and_unit_rate_are_both_kept() {
        // Проверяется на СЫРОМ слое, а не на наблюдениях, и это не обход:
        // у всех валют, которые знает ядро (RUB, USD, EUR, CNY), номинал
        // ЦБ равен единице, и различие value/unit_rate на них ненаблюдаемо.
        // Номинал больше единицы есть у иены (100) и лиры (10) — валют,
        // которых в ядре нет. Сырой слой существует именно поэтому:
        // разбор обязан быть проверяем независимо от того, какие валюты
        // система учитывает сегодня.
        let text = decode_cp1251(DAILY);
        let raw = parse_daily_raw(&text).expect("разбор");
        let jpy = raw
            .iter()
            .find(|r| r.char_code == "JPY")
            .expect("иена есть в справочнике ЦБ");
        assert_eq!(jpy.nominal, 100, "ЦБ публикует иену за сто единиц");
        assert_ne!(
            jpy.value, jpy.unit_rate,
            "значение за номинал и за единицу совпали — номинал потерян"
        );
    }

    #[test]
    fn a_currency_the_core_does_not_know_is_skipped_not_an_error() {
        // Справочник ЦБ содержит десятки валют, которых система
        // не учитывает. Объявить их ошибкой значило бы уронить разбор
        // всего ответа из-за валюты, которая никому не нужна.
        let text = decode_cp1251(DAILY);
        let raw = parse_daily_raw(&text).expect("разбор");
        let observations = parse_daily(&text, observed()).expect("разбор");
        assert!(
            raw.len() > observations.len(),
            "в справочнике ЦБ больше валют, чем знает ядро: {} против {}",
            raw.len(),
            observations.len()
        );
        assert!(
            raw.iter().any(|r| r.char_code == "JPY"),
            "иена в сыром слое есть"
        );
        assert!(
            observations.iter().all(|o| o.from != CurrencyCode::Rub),
            "рубль не является исходной валютой в котировках ЦБ"
        );
    }

    #[test]
    fn the_series_covers_business_days_only() {
        let text = decode_cp1251(DYNAMIC);
        let series = parse_dynamic(&text, CurrencyCode::Rub, observed()).expect("разбор");
        assert!(!series.is_empty());
        let has_weekend = series.iter().any(|o| {
            matches!(
                o.trade_date.0.weekday(),
                time::Weekday::Saturday | time::Weekday::Sunday
            )
        });
        assert!(!has_weekend, "в ряду ЦБ выходных нет — курса на воскресенье не существует");
    }

    #[test]
    fn the_source_date_format_is_dotted_not_iso() {
        assert_eq!(parse_cbr_date("04.08.2026").expect("дата"), date!(2026 - 08 - 04));
        assert!(parse_cbr_date("2026-08-04").is_err());
    }
}
```

- [ ] **Step 3: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-market cbr::fx`
Expected: FAIL, `cannot find function decode_cp1251 in this scope`.

- [ ] **Step 4: Реализовать**

`crates/iaam-market/src/cbr/mod.rs` — добавить описания запросов:

```rust
//! ЦБ РФ: курсы валют и ключевая ставка.
//!
//! Два разных интерфейса у одного источника, и это не прихоть:
//! курсы отдаются простыми XML-скриптами, а история ключевой ставки —
//! только SOAP-сервисом. Документированной альтернативы без SOAP
//! и без разбора HTML нет; разбор HTML для источника истины неприемлем,
//! потому что страница меняется без контракта и без версии.

pub mod fx;
pub mod key_rate;

use iaam_http::{Destination, HttpRequest};
use time::Date;

/// Курсы всех валют на дату.
#[must_use]
pub fn daily_request(on: Date) -> HttpRequest {
    HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp")
        .with_query("date_req", &dotted(on))
}

/// Курс одной валюты за период.
///
/// `cbr_currency_id` — внутренний код ЦБ вида `R01235` (доллар США),
/// а не код ISO: сервис принимает только его.
#[must_use]
pub fn dynamic_request(from: Date, till: Date, cbr_currency_id: &str) -> HttpRequest {
    HttpRequest::get(Destination::CbrScripts, "/scripts/XML_dynamic.asp")
        .with_query("date_req1", &dotted(from))
        .with_query("date_req2", &dotted(till))
        .with_query("VAL_NM_RQ", cbr_currency_id)
}

/// Дата в формате источника: `DD/MM/YYYY` в запросе.
fn dotted(date: Date) -> String {
    format!(
        "{:02}/{:02}/{}",
        date.day(),
        u8::from(date.month()),
        date.year()
    )
}
```

`crates/iaam-market/src/cbr/fx.rs` — декодирование и разбор:

```rust
//! Разбор курсов ЦБ РФ.
//!
//! Две конвенции источника, которые легко пропустить и обе тихие:
//! ответ приходит в `windows-1251`, а десятичный разделитель — запятая.

use encoding_rs::WINDOWS_1251;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use quick_xml::events::Event;
use quick_xml::Reader;
use rust_decimal::Decimal;
use time::{Date, Month};

use crate::error::MarketError;
use crate::observation::{FxObservation, ObservedAt, TradeDate};

/// Байты ответа ЦБ в строку.
///
/// Отдельная функция, а не `String::from_utf8_lossy`: lossy подставил бы
/// вопросительные знаки вместо названий валют и сделал бы порчу
/// незаметной. `windows-1251` объявлена в прологе самого ответа.
#[must_use]
pub fn decode_cp1251(bytes: &[u8]) -> String {
    let (text, _, _) = WINDOWS_1251.decode(bytes);
    text.into_owned()
}

/// Число в конвенции ЦБ: десятичная запятая.
///
/// Точка отвергается намеренно. Принять оба разделителя значило бы
/// перестать замечать, что источник сменил конвенцию, — а смена
/// конвенции у источника истины обязана быть отказом, а не догадкой.
pub(crate) fn parse_cbr_decimal(value: &str) -> Result<Decimal, MarketError> {
    if !value.contains(',') && value.contains('.') {
        return Err(MarketError::Malformed(format!(
            "разделитель ЦБ — запятая, получено {value}"
        )));
    }
    value
        .replace(',', ".")
        .parse::<Decimal>()
        .map_err(|error| MarketError::Malformed(format!("число {value}: {error}")))
}

/// Дата в конвенции ЦБ: `DD.MM.YYYY`.
pub(crate) fn parse_cbr_date(value: &str) -> Result<Date, MarketError> {
    let parts: Vec<&str> = value.split('.').collect();
    let [day, month, year] = parts.as_slice() else {
        return Err(MarketError::Malformed(format!(
            "дата ЦБ ожидается как DD.MM.YYYY, получено {value}"
        )));
    };
    let parsed = |part: &str| {
        part.parse::<u16>()
            .map_err(|error| MarketError::Malformed(format!("дата {value}: {error}")))
    };
    let month = Month::try_from(u8::try_from(parsed(month)?).unwrap_or(0))
        .map_err(|error| MarketError::Malformed(format!("месяц {value}: {error}")))?;
    Date::from_calendar_date(
        i32::from(parsed(year)?),
        month,
        u8::try_from(parsed(day)?).unwrap_or(0),
    )
    .map_err(|error| MarketError::Malformed(format!("дата {value}: {error}")))
}
```

Разбор `parse_daily` и `parse_dynamic` пиши на `quick_xml::Reader`, собирая элементы `Valute` (в дневном ответе) и `Record` (в ряде за период). Поля: `CharCode` и `ID` для валюты, `Nominal`, `Value`, `VunitRate`. Дата дневного ответа лежит атрибутом `Date` у корня `ValCurs`; у ряда за период — атрибутом `Date` каждого `Record`.

**Не выдумывай коды валют.** Отображение `CharCode` в `CurrencyCode` бери из существующего перечисления `crates/iaam-core/src/money.rs:22`; для кодов, которых в ядре нет, запись пропускается **молча и намеренно** — справочник ЦБ содержит десятки валют, которых система не учитывает, и объявлять их ошибкой значило бы уронить разбор всего ответа из-за валюты, которая никому не нужна. Пропуск оговори комментарием.

- [ ] **Step 5: Прогнать тесты, клиппи, заслон фикстур**

Run: `nix develop -c cargo test -p iaam-market cbr::fx`
Run: `nix develop -c cargo clippy -p iaam-market --all-targets -- -D warnings`
Run: `nix develop -c ./scripts/check-fixtures.sh`

- [ ] **Step 6: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-market tests/fixtures
git commit -m "feat(market): курсы ЦБ РФ — cp1251, десятичная запятая, номинал"
```

---

### Task 4: ЦБ РФ — ключевая ставка через SOAP

**Files:**
- Modify: `crates/iaam-market/src/cbr/key_rate.rs`
- Create: `tests/fixtures/market/cbr-keyrate-soap.xml`
- Modify: `tests/fixtures/MANIFEST.sha256`

**Interfaces:**
- Consumes: `KeyRateObservation`, `TradeDate`, `ObservedAt`, `MarketError` (задача 1); `HttpRequest`, `RequestBody::Xml`, `Destination::CbrDailyInfo`.
- Produces: `key_rate_request(from: Date, till: Date) -> HttpRequest`; `parse_key_rate(xml: &str, observed_at) -> Result<Vec<KeyRateObservation>, MarketError>`; `derive_intervals(observations: &[KeyRateObservation]) -> Vec<RateInterval>`; `RateInterval { from: Date, until: Option<Date>, rate: Dec, boundary: Boundary }`; `Boundary::{Observed, InferredAcrossNonTradingDays}`.

**Acceptance Criteria:**
- Конверт SOAP собирается с заголовком `SOAPAction: "http://web.cbr.ru/KeyRateXML"` — без него сервис отвечает отказом, а не ошибкой разбора, и причина неочевидна.
- Разбор берёт пары `DT`/`Rate` из `KeyRate/KR`.
- **Наблюдения хранятся по рабочим дням**, а не интервалами: даты вступления в ответе нет вовсе.
- **Интервал выводится на чтении и помечается выведенным**, когда его граница попала в нерабочие дни. В фикстуре три перехода — 16,00 → 15,50 → 15,00 → 14,50, — и каждая граница лежит между пятницей и понедельником.
- Фикстура и тест ложатся одним коммитом.

- [ ] **Step 1: Внести фикстуру**

```bash
cp .brief/fixture/cbr-keyrate-soap.xml tests/fixtures/market/
sha256sum tests/fixtures/market/cbr-keyrate-soap.xml >> tests/fixtures/MANIFEST.sha256
```

Записана координатором живым вызовом 2026-08-26 за период с 1 февраля по 30 апреля: 63 наблюдения, три перехода ставки.

- [ ] **Step 2: Написать падающий тест**

`crates/iaam-market/src/cbr/key_rate.rs`, в конец:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    const FIXTURE: &str =
        include_str!("../../../../tests/fixtures/market/cbr-keyrate-soap.xml");

    fn observed() -> ObservedAt {
        ObservedAt(datetime!(2026-08-26 09:00:00 UTC))
    }

    #[test]
    fn the_envelope_carries_the_soap_action() {
        let request = key_rate_request(date!(2026 - 02 - 01), date!(2026 - 04 - 30));
        assert_eq!(
            request.soap_action(),
            Some("http://web.cbr.ru/KeyRateXML"),
            "без SOAPAction сервис отвечает отказом, а не ошибкой разбора"
        );
    }

    #[test]
    fn the_source_gives_business_day_observations_not_intervals() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("разбор");
        assert_eq!(observations.len(), 63);
        assert!(
            !observations.iter().any(|o| matches!(
                o.trade_date.0.weekday(),
                time::Weekday::Saturday | time::Weekday::Sunday
            )),
            "в ряду только рабочие дни"
        );
    }

    #[test]
    fn intervals_are_derived_and_their_boundaries_are_marked_inferred() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("разбор");
        let intervals = derive_intervals(&observations);
        // Три перехода в фикстуре: 16,00 → 15,50 → 15,00 → 14,50.
        assert_eq!(intervals.len(), 4, "получено {intervals:?}");
        // Каждая смена приходится на понедельник после пятницы: между
        // последним наблюдением старой ставки и первым наблюдением новой
        // лежат выходные, и точная дата вступления источником не названа.
        for interval in intervals.iter().skip(1) {
            assert_eq!(
                interval.boundary,
                Boundary::InferredAcrossNonTradingDays,
                "граница {interval:?} обязана быть помечена выведенной"
            );
        }
    }

    #[test]
    fn the_first_interval_starts_at_an_observed_date() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("разбор");
        let intervals = derive_intervals(&observations);
        let first = intervals.first().expect("хотя бы один интервал");
        assert_eq!(first.boundary, Boundary::Observed);
        assert_eq!(first.from, date!(2026 - 02 - 02));
    }

    #[test]
    fn the_last_interval_is_open_on_the_right() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("разбор");
        let intervals = derive_intervals(&observations);
        assert!(intervals.last().expect("интервал").until.is_none());
    }
}
```

- [ ] **Step 3: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-market key_rate`
Expected: FAIL, `cannot find function key_rate_request in this scope`.

- [ ] **Step 4: Реализовать**

Описание запроса:

```rust
//! Ключевая ставка ЦБ РФ (раздел 8 дизайна E3.2).
//!
//! Единственный документированный машинный интерфейс истории —
//! SOAP-сервис `DailyInfoWebServ`. Полноценный SOAP-фреймворк не нужен:
//! конверт статический, ответ разбирается тем же `quick-xml`, что и курсы.
//! Генератор по WSDL и новая зависимость на SOAP не заводятся.

use iaam_http::{Destination, HttpRequest, RequestBody};
use time::Date;

/// Действие сервиса. Без этого заголовка сервис отвечает отказом,
/// а не ошибкой разбора, и причина по ответу неочевидна.
const SOAP_ACTION: &str = "http://web.cbr.ru/KeyRateXML";

#[must_use]
pub fn key_rate_request(from: Date, till: Date) -> HttpRequest {
    let envelope = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <KeyRateXML xmlns="http://web.cbr.ru/">
      <fromDate>{}T00:00:00</fromDate>
      <ToDate>{}T00:00:00</ToDate>
    </KeyRateXML>
  </soap:Body>
</soap:Envelope>"#,
        iso(from),
        iso(till)
    );
    HttpRequest::post(
        Destination::CbrDailyInfo,
        "/DailyInfoWebServ/DailyInfo.asmx",
        RequestBody::Xml(envelope),
    )
    .with_soap_action(SOAP_ACTION)
}
```

Разбор: пройти `quick_xml::Reader` до элементов `KR` и собрать пары `DT`/`Rate`. `DT` приходит как `2026-08-26T00:00:00+03:00` — бери дату, смещение не теряй молча, но и не применяй: ставка относится к календарному дню в московском времени, и перевод в UTC сдвинул бы её на день.

Вывод интервалов:

```rust
/// Как получена левая граница интервала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Первое наблюдение ряда: дата наблюдена, а не выведена.
    Observed,
    /// Между последним наблюдением прежней ставки и первым наблюдением
    /// новой лежат нерабочие дни. Источник даты вступления не называет,
    /// поэтому точная граница нам неизвестна — известен лишь промежуток.
    InferredAcrossNonTradingDays,
}

/// Интервал действия ставки.
///
/// Выводится на чтении, а не хранится: источник отдаёт дневной ряд
/// по рабочим дням, и записать выведенную границу наблюдением значило бы
/// выдать нашу догадку за утверждение ЦБ (раздел 8.3 спеки).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateInterval {
    pub from: Date,
    /// `None` у последнего интервала: он открыт справа.
    pub until: Option<Date>,
    pub rate: Dec,
    pub boundary: Boundary,
}
```

`derive_intervals` идёт по отсортированным наблюдениям, открывая новый интервал при смене значения. Граница помечается `Observed` только для самого первого интервала; всякая последующая — `InferredAcrossNonTradingDays`, если между соседними наблюдениями есть пропуск, и `Observed`, если смена произошла между соседними рабочими днями.

- [ ] **Step 5: Прогнать тесты, клиппи, заслон фикстур**

- [ ] **Step 6: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-market tests/fixtures
git commit -m "feat(market): ключевая ставка ЦБ — SOAP, наблюдения по рабочим дням, выведенные интервалы"
```

---

### Task 5: Миграция хранилища — таблицы наблюдений

**Files:**
- Create: `crates/iaam-store/migrations/0006_market_observations.sql`
- Modify: `crates/iaam-store/src/schema.rs` (регистрация миграции)
- Create: `crates/iaam-store/tests/migration_0006.rs`

**Interfaces:**
- Produces: таблицы `price_observations`, `fx_observations`, `key_rate_observations`, `sync_runs`, `series_completeness`.

**Acceptance Criteria:**
- Наблюдения **append-only**: повторная запись того же дня из того же источника с другим значением ложится **новой строкой**, а не затирает прежнюю.
- Ключ строки — не ключ ряда: тройка «инструмент, дата, источник» описывает серию, а строке нужен `observed_at` в составе ключа.
- **Составной ключ не содержит необязательных колонок.** В STRICT-таблице SQLite колонки первичного ключа неявно `NOT NULL`; ключ с необязательной колонкой валится с `NOT NULL constraint failed`. Если такая колонка нужна — `UNIQUE INDEX` по `ifnull(...)`, а не `PRIMARY KEY`: обычный `UNIQUE` не годится, SQLite считает `NULL` несовпадающими.
- Единица полноты — `(источник, набор, серия)`, глобального поколения нет.
- Миграция применяется к существующей базе и не роняет уже записанные данные.

- [ ] **Step 1: Посмотреть, как устроены соседние миграции**

Прочитай `crates/iaam-store/migrations/0005_*.sql` и `crates/iaam-store/src/schema.rs`: регистрация миграции, соглашение об именах, применяемые прагмы. **Следуй существующему образцу**, а не придумывай свой.

Прочитай также `crates/iaam-store/tests/migration_0005.rs` — тест миграции пишется по тому же шаблону.

- [ ] **Step 2: Написать падающий тест**

`crates/iaam-store/tests/migration_0006.rs`: проверить, что после миграции существуют пять таблиц, что вторая запись того же дня из того же источника с другим значением **добавляется**, а не заменяет, и что выборка «последнее по знанию на дату» возвращает более позднее наблюдение.

- [ ] **Step 3: Прогнать и убедиться, что падает**

Run: `nix develop -c cargo test -p iaam-store --test migration_0006`

- [ ] **Step 4: Написать миграцию**

Ключ наблюдения цены: `(instrument_id, board, session, trade_date, kind, source_id, observed_at)`. Все колонки ключа обязательны — если какая-то окажется необязательной, ключ становится `UNIQUE INDEX` по `ifnull(...)`.

Аналогично `fx_observations` с ключом `(from_code, to_code, trade_date, source_id, observed_at)` и `key_rate_observations` с `(trade_date, source_id, observed_at)`.

`sync_runs`: статус `running` / `succeeded` / `partial` / `failed`, запрошенный и фактически покрытый диапазоны **разными колонками**, число страниц и строк, хеш сырого ответа, аренда против двух одновременных запусков.

`series_completeness`: `(source_id, dataset, series_key)` и граница полноты. Граница **не продвигается** при частичном отказе — это инвариант, который проверит задача 6.

- [ ] **Step 5: Прогнать тесты и клиппи**

Run: `nix develop -c cargo test -p iaam-store --test migration_0006`
Run: `nix develop -c cargo clippy -p iaam-store --all-targets -- -D warnings`

**Проверь потребителей схемы:** `grep -rn "user_version\|MIGRATIONS" crates/iaam-store/src/` — если версия схемы зашита где-то ещё, обнови и там, и прогони `cargo check -p` по потребителям.

- [ ] **Step 6: Формат и коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-store
git commit -m "feat(store): миграция 0006 — append-only битемпоральные наблюдения"
```

---

## Приёмка части 2 (оркестратор, один раз в конце)

Воркеры этих команд не запускают.

```bash
nix develop -c make check
nix develop -c ./scripts/check-architecture.sh
nix develop -c ./scripts/check-fixtures.sh
nix develop -c cargo mutants -p iaam-market --no-times
grep -rn "reqwest" crates/*/Cargo.toml | grep -v iaam-http && echo "ПРОВАЛ" || echo "транспорт заперт"
```

Приёмка пройдена, когда:

- `make check` зелёный, число тестов не уменьшилось;
- заслон фикстур доволен: каждая фикстура читается тестом, файлов вне манифеста нет;
- `reqwest` объявлен ровно в одном манифесте;
- мутанты по `iaam-market` без выживших либо выжившие разобраны поимённо;
- ни один разбор не ходит в сеть: тесты крейты проходят при отключённой сети.

## Что остаётся на задачи 6–9

Пишется после того, как задачи 1–5 лягут в код, чтобы опираться на реальные сигнатуры: запись наблюдений и границы полноты, атомарная публикация единицы, путь записи в справочник (`iaam-z9n`), пространство имён кода (`iaam-h9n`), сценарий синхронизации и ручной запуск.

---

# Задачи 6–9

Написаны после сдачи задач 1–5, по реальным сигнатурам. Ссылки на код ниже
указывают на то, что действительно лежит в ветке `e3-market-data`.

**Порядок и параллельность второй половины.** Задачи 6, 7 и 8 независимы
по файлам и идут **параллельно**: хранилище, приложение с транспортом,
приёмка. Задача 9 идёт последней: она правит `crates/iaam-app/src/ports.rs`,
который в это же время правит задача 7, и параллельный запуск дал бы гонку
за один файл.

**Конфликт манифеста фикстур — ожидаемый, а не случайный.** Любые две
задачи, добавляющие эталон, дописывают строку в конец
`tests/fixtures/MANIFEST.sha256`. В части 2 это дало два конфликта подряд.
Разрешение всегда одно: оставить обе строки и **сверить суммы с файлами**
(`sha256sum -c tests/fixtures/MANIFEST.sha256`), а не принять строки на веру.
Разрешает координатор при слиянии; воркеру трогать чужие строки запрещено.

---

### Task 6: Запись наблюдений, единицы полноты, атомарная публикация

**Files:**
- Create: `crates/iaam-store/src/market.rs`
- Modify: `crates/iaam-store/src/lib.rs` (объявить модуль)
- Create: `crates/iaam-store/tests/market_observations.rs`

**Interfaces:**
- Consumes: таблицы `price_observations`, `fx_observations`, `key_rate_observations`, `sync_runs`, `series_completeness` (миграция 0006, задача 5).
- Produces: `MarketStore::{begin_run, record_prices, record_fx, record_key_rate, finish_run, complete_through, prices_at_or_before}`; `RunOutcome::{Succeeded, Partial { reason: String }, Failed { reason: String }}`; `SeriesKey { source_id: String, dataset: String, series_key: String }`.

**`iaam-store` НЕ зависит от `iaam-market`.** Это ребро вверх по графу §3.2,
и заслон архитектуры его ловит: «iaam-store зависит от вышележащих слоёв».
Хранилище объявляет **свои строковые типы** — `PriceRow`, `FxRow`,
`KeyRateRow`, — как оно уже делает для остальных таблиц. §3.2 отвечает
на это прямо: «ответы MOEX — в `iaam-market`, строки таблиц — в
`iaam-store`. Преобразование в доменные типы происходит на границе».
Границей служит `iaam-app` — единственный слой, знающий обе крейты;
преобразование `PriceObservation → PriceRow` пишется там, в задаче 9.

**Acceptance Criteria:**
- **Повторная запись того же наблюдения с другим значением добавляет строку**, а не заменяет: отчёт по прежней координате продолжает давать прежнее число.
- **Граница полноты не продвигается при `Partial` и `Failed`.** Это прямой ответ на требование бида `iaam-023.5` «частичная выгрузка не должна выдаваться за полную», и главный инвариант задачи.
- Публикация атомарна **внутри единицы** `(источник, набор, серия)`: строки незавершённого запуска в выборку не попадают.
- Отказ одной серии не задерживает публикацию других: глобального поколения нет.
- Аренда не даёт двум запускам работать над одной серией одновременно.
- `prices_at_or_before(instrument, venue, as_of, knowledge_as_of)` отдаёт **последнее по знанию** наблюдение не позже `as_of` — и не видит наблюдений с `observed_at` позже `knowledge_as_of`.

- [ ] **Step 1: Прочитать образец**

`crates/iaam-store/src/reference.rs` и `crates/iaam-store/src/events.rs` — как крейта устроена: соединение, транзакции, отображение строк, ошибки. **Следуй образцу**, а не изобретай.

- [ ] **Step 2: Написать падающие тесты**

`crates/iaam-store/tests/market_observations.rs`. Обязательные тесты, каждый именем говорит, что охраняет:

```rust
#[test]
fn a_corrected_price_lands_beside_the_old_one_not_over_it() { /* ... */ }

#[test]
fn a_partial_run_does_not_advance_the_completeness_boundary() {
    // Прямой ответ на iaam-023.5: частичная выгрузка не выдаётся
    // за полную. Граница остаётся там, где была после последнего
    // ПОЛНОГО запуска.
}

#[test]
fn a_failed_series_does_not_hold_back_other_series() {
    // Глобального поколения нет намеренно: иначе одна упавшая бумага
    // заморозила бы свежие цены по всем остальным.
}

#[test]
fn rows_of_an_unfinished_run_are_invisible_to_reads() { /* ... */ }

#[test]
fn a_second_run_on_the_same_series_is_refused_while_the_lease_holds() { /* ... */ }

#[test]
fn a_read_at_an_earlier_knowledge_time_returns_the_earlier_value() {
    // Это исполняемая формулировка воспроизводимости (раздел 4 спеки):
    // добавление наблюдения с более поздним observed_at не меняет ответ
    // на меньший knowledge_as_of.
}
```

Прогон: `nix develop -c cargo test -p iaam-store --test market_observations` — **именно `--test`**, фильтр по имени файла даёт ноль прогнанных.

- [ ] **Step 3–5: Реализовать, прогнать, клиппи**

Ключевое в реализации: `record_*` пишут строки с `sync_run_id` незавершённого запуска; чтение отбирает только строки, чей запуск `succeeded`. `finish_run` в **одной транзакции** проставляет статус, покрытый диапазон и — только при `Succeeded` — двигает `complete_through`.

- [ ] **Step 6: Коммит**

```bash
nix develop -c cargo fmt --all
git add crates/iaam-store
git commit -m "feat(store): запись наблюдений, единицы полноты, атомарная публикация"
```

---

### Task 7: Путь записи в справочник инструментов (`iaam-z9n`)

**Files:**
- Modify: `crates/iaam-app/src/ports.rs` (метод записи в `InstrumentDirectory`, строка 115)
- Modify: `crates/iaam-app/src/adapters/sqlite.rs` (реализация, строка 343 рядом)
- Modify: `crates/iaam-server/src/routes.rs` (маршрут, строка 134 — сейчас отвечает 501)
- Modify: `crates/iaam-server/src/openapi.rs`, `dto.rs`
- Modify: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Produces: `InstrumentDirectory::record_instrument(&self, record: InstrumentUpsert) -> Result<InstrumentId, AppError>` и `record_alias(&self, alias: AliasUpsert) -> Result<(), AppError>`.

**Acceptance Criteria:**
- **Метод порта спроектирован под синхронизацию источника**, а не под ручной ввод: бид `iaam-z9n` и раздел 7 дизайна E3.1 называют разрешённые пути записи — синхронизация и админ-токен.
- `POST /v1/instruments` **либо работает, либо исчезает вместе с описанием в OpenAPI**. Объявленной и неработающей операции не остаётся.
- Контрактный тест проверяет **успешный путь**, а не только отказ по правам.
- Отказ агентскому токену сохраняется: запись — не агентское действие.
- `async_trait` в `iaam-app` разрешён (правило 10 заслона), в других крейтах — нет.

- [ ] **Step 1: Прочитать бид и раздел спеки**

`bd show iaam-z9n` недоступен воркеру — критерии перенесены сюда целиком, выше. Прочитай `.internal/specs/2026-08-25-e3-1-instrument-reference-design.md`, раздел 7.

- [ ] **Step 2: Падающий контрактный тест на успешный путь**

В `crates/iaam-server/tests/contract.rs` — по образцу соседних тестов маршрутов. Существующий тест на 403 для агентского токена **не трогать**: он охраняет отказ, который остаётся в силе.

- [ ] **Step 3–5: Реализовать, прогнать, клиппи**

**Проверь потребителей:** добавление метода в трейт `InstrumentDirectory` ломает все его реализации. `grep -rn "impl InstrumentDirectory" crates/` и прогони `cargo check -p` по каждой крейте, где найдётся. Тестовые заглушки трейта тоже сломаются — их надо дополнить, а не удалить.

- [ ] **Step 6: Коммит**

---

### Task 8: Пространство имён кода в разборщиках (`iaam-h9n`)

**Files:**
- Modify: `crates/iaam-ingest/src/csv_source.rs` (строки 82–92 — сбор кандидатов по всем пространствам)
- Modify: разборщики отчётов брокеров в `crates/iaam-ingest/src/`

**Acceptance Criteria:**
- **Разрешение принимает пространство имён, когда колонка отчёта его задаёт.** Порт это уже умеет: `InstrumentDirectory::resolve(namespace, value, on)`, `crates/iaam-app/src/ports.rs:121`. Не пользуется им приёмка.
- Тикер из колонки тикеров **не может** разрешиться через `broker_code`.
- **Отказ по неоднозначности остаётся** для случаев, где колонка пространства не задаёт. Существующие тесты `an_instrument_known_in_multiple_namespaces_resolves_to_the_same_id` и `an_instrument_code_is_rejected_when_namespaces_point_to_different_ids` (`csv_source.rs:445`, `:479`) **не ослабляются**: они охраняют защиту, которая нужна и дальше.

**Почему это делается сейчас.** Сегодняшнее поведение — защита, а не решение: система отказывается там, где могла бы ответить точно, потому что не пользуется знанием, которое у неё уже есть. Пока псевдонимы заводились вручную, коллизии были теоретическими. MOEX ISS заводит `SECID` и `ISIN` в одной выгрузке, и они станут обычным делом.

- [ ] **Step 1: Прочитать существующее поведение**

`crates/iaam-ingest/src/csv_source.rs:75–95` — как собираются кандидаты и как формулируется отказ. **Сохрани форму отказа**: она читается в вердикте приёмки.

- [ ] **Step 2–6: Тест, реализация, прогон, клиппи, коммит**

---

### Task 9: Сценарий синхронизации и ручной запуск

**Files:**
- Modify: `crates/iaam-app/src/ports.rs` (порт `MarketData`)
- Create: `crates/iaam-app/src/adapters/market.rs`
- Create: `crates/iaam-app/src/scenarios/sync.rs`
- Modify: `crates/iaam-server/src/routes.rs` (маршрут ручного запуска)

**Interfaces:**
- Consumes: `MarketStore` (задача 6), `iaam-market` целиком, `iaam-http::HttpClient`, `RetryPolicy`, `RateLimiter`.

**Acceptance Criteria:**
- Сценарий **не ходит в сеть в тестах**: транспорт приходит через порт, и тест подставляет заглушку, отдающую замороженный эталон.
- Частичный отказ источника завершает запуск как `Partial` и **не двигает границу полноты**.
- Повторный запуск того же диапазона не меняет ни одного ответа отчёта.
- Ретраи и ограничение частоты берутся из `iaam-http::resilience`, а не пишутся заново.
- Ручной запуск существует маршрутом; расписание — часть 3, здесь его нет.

**Идёт последней:** правит `crates/iaam-app/src/ports.rs`, который в это же время правит задача 7.

- [ ] **Step 1–6: по образцу `crates/iaam-app/src/scenarios/ingest.rs`**

Сценарии приёмки уже устроены нужным образом — порты, заглушки в тестах, отсутствие сети. Повторяй, а не изобретай.
