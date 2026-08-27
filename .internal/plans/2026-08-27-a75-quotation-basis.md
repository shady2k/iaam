# Основание котировки облигации — план исправления `iaam-a75`

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** наблюдение цены несёт доказанное основание котировки, а стоимость позиции считается по нему через версионированное правило — так, что облигация по 98.5 при номинале 1000 ₽ оценивается в 985 ₽, а при неизвестном номинале отказывает вместо догадки.

**Architecture:** основание — атрибут наблюдения **от источника**, а не вывод политики. Оно рождается при разборе из пары `(engine, market)` запроса ISS, вместе с признаком, по которому выведено, и проходит всю цепочку до расчёта и до аудита. Умножение количества на цену перестаёт быть арифметикой в двух местах и становится одним версионированным правилом.

**Tech Stack:** Rust 2024, `rust_decimal`, `serde`, `rusqlite` (STRICT), `cargo nextest`, `cargo-mutants`.

**Спека:** `.internal/specs/2026-08-26-e3-4-bonds-design.md`, раздел 10.
**Бид:** `iaam-a75`. **Опирается на:** `PrincipalState` из E3.4.1.T4 (закрыт).
**Решение:** ADR-0002 (полнота оценки и исполнимость — две оси) — основание котировки третья ось и выводу политикой не подлежит.

## Global Constraints

- Rust edition 2024, `rust-version = 1.85`; `unsafe_code = "forbid"`, clippy `all = deny`.
- **Ни одного `_` в `match` по `QuotationBasis`.** Урок `iaam-d8b.1.4`: шесть мутантов выжили ровно на нетипизированном виде цены.
- **`Unknown` — полноценное значение (§4.9).** Наблюдение с недоказанным происхождением получает именно его и отказ при оценке; молча присвоить `MoneyPerUnit` нельзя.
- **Основание доказывается, а не угадывается по роду инструмента.** Строка вида `if kind == Bond { Percent }` — недокументированная эвристика и запрещена спекой (§10.2).
- **Каждое добавляемое сериализуемое поле несёт `#[serde(default)]`.** Снимки проекций и архивы уже записаны без них.
- **Каждый коммит оставляет `cargo build --workspace` зелёным.**
- **Файлы политики** (`scripts/`, `Cargo.toml`, `tests/fixtures`, `.cargo/mutants.toml`, `deny.toml`, `clippy.toml`, `.github/workflows`, `flake.*`, `rustfmt.toml`) правятся только с `POLICY_CHANGE_APPROVED=1` и меткой `policy-change` (`scripts/check-diff-lint.sh:80`). **Задачи 2 и 10 упираются в это — см. их тексты.**
- Комментарии и доккомментарии — по-русски, как во всём ядре.
- Заслоны прогоняются командой `nix develop -c make check`; вне `nix develop` `cargo` в PATH отсутствует.

## Решения, принятые этим планом сверх спеки

1. **Признак основания — пара `(engine, market)` из пути запроса ISS.** Спека требует «назвать в реализации конкретный признак» и запрещает эвристику по роду инструмента. Пара уже известна адаптеру: `MarketSource::Moex { engine, market, board, secid, instrument }` (`iaam-app/src/scenarios/sync.rs:294`) строит путь `/iss/history/engines/{engine}/markets/{market}/boards/{board}/…` (`iaam-market/src/moex/mod.rs:30`). Живой проверки ISS это решение не требует: путь формирует сам адаптер.
2. **Признак хранится строкой рядом с основанием** (`basis_evidence`), а не восстанавливается по основанию. Без него запись «percent_of_remaining_face» недоказуема при разборе аудита, а спека требует положить признак в provenance.
3. **`Venue` не расширяется.** `engine` и `market` не входят в идентичность наблюдения: она уже задана `(instrument, board, session, trade_date, kind, source, observed_at)`, и добавление полей в первичный ключ переписало бы таблицу ради величины, которая от строки к строке не меняется.
4. **Остаточный номинал берётся из лотов, а не из справочника.** Реестра параметров выпуска в E3.4 части 1 нет; `Lot::principal` есть. Расхождение номинала между лотами одной пары «счёт и бумага» — отказ, а не усреднение: одна бумага одного выпуска на одну дату имеет один непогашенный номинал, и расхождение означает брак данных.
5. **`xirr::account_values` правится, хотя сегодня не сломан.** `PriceBoard` заполняется только из `EventKind::Valuation` (`projection/mod.rs:339`), а те по §10.3 деньги за единицу по определению. Но спека прямо требует общего правила: два независимых пересчёта в двух местах разъедутся, как только рыночная цена дойдёт до XIRR.

## File Structure

| Файл | Ответственность |
|---|---|
| `crates/iaam-core/src/valuation/candidate.rs` | + `QuotationBasis`; + поля `basis`/`basis_evidence` в `PriceCandidate` и `PriceProvenance` |
| `crates/iaam-market/src/observation.rs` | + поля `basis`/`basis_evidence` в `PriceObservation` |
| `crates/iaam-market/src/moex/parse.rs` | вывод основания из `(engine, market)` |
| `crates/iaam-market/src/moex/mod.rs` | без изменений — путь уже несёт engine и market |
| `crates/iaam-store/migrations/0008_quotation_basis.sql` | **новая** — две колонки в `price_observations` |
| `crates/iaam-store/src/schema.rs` | `SCHEMA_VERSION` 7→8 |
| `crates/iaam-store/src/market.rs` | `PriceRow` + запись + три пути чтения |
| `crates/iaam-app/src/market_candidate.rs` | перенос основания из наблюдения в кандидата |
| `crates/iaam-core/src/rules/quotation.rs` | **новый** — правило пересчёта котировки в деньги |
| `crates/iaam-core/src/rules/mod.rs` | карта правил котировки в `RuleRegistry` |
| `crates/iaam-core/src/returns/mod.rs` | `position_value` через правило; `AppliedRules.quotation_rule`; `inputs_hash` |
| `crates/iaam-core/src/returns/xirr.rs` | `account_values` через то же правило |
| `crates/iaam-server/src/dto.rs` | `QuotationBasisDto` в `PriceProvenanceDto` и `MarketPriceDto` |

---

### Task 1: `QuotationBasis` в ядре и в наблюдении

**Files:**
- Modify: `crates/iaam-core/src/valuation/candidate.rs`, `crates/iaam-market/src/observation.rs`

**Interfaces:**
- Produces: `QuotationBasis::{MoneyPerUnit, PercentOfRemainingFace, Unknown}`, `QuotationBasis::code() -> &'static str`, `impl Default for QuotationBasis`; поля `PriceObservation::{basis, basis_evidence}`, `PriceCandidate::{basis, basis_evidence}`

**Acceptance Criteria:**
- Тип живёт в `iaam-core` и используется обоими крейтами: `iaam-market` уже зависит от `iaam-core` (`crates/iaam-market/Cargo.toml:10`), и второй тип пришлось бы отображать в первый и терять на этом.
- `Default` = `Unknown`, поле несёт `#[serde(default)]`: наблюдения записаны до этой правки, и подставить им `MoneyPerUnit` значит объявить доказанным то, чего никто не доказывал.
- Признак, по которому основание выведено, хранится рядом строкой и переживает круг через JSON.

- [ ] **Step 1: Написать падающие тесты**

В `crates/iaam-core/src/valuation/candidate.rs`, в существующий `mod tests`:

```rust
#[test]
fn an_undecided_quotation_basis_is_unknown_not_money_per_unit() {
    // Строка, записанная до появления основания, недоказуема.
    // `MoneyPerUnit` по умолчанию объявил бы её доказанной (§4.9).
    assert_eq!(QuotationBasis::default(), QuotationBasis::Unknown);
}

#[test]
fn every_quotation_basis_names_itself() {
    assert_eq!(QuotationBasis::MoneyPerUnit.code(), "money_per_unit");
    assert_eq!(
        QuotationBasis::PercentOfRemainingFace.code(),
        "percent_of_remaining_face"
    );
    assert_eq!(QuotationBasis::Unknown.code(), "unknown");
}

#[test]
fn a_quotation_basis_survives_a_round_trip_through_its_code() {
    for basis in [
        QuotationBasis::MoneyPerUnit,
        QuotationBasis::PercentOfRemainingFace,
        QuotationBasis::Unknown,
    ] {
        assert_eq!(QuotationBasis::from_code(basis.code()), Some(basis));
    }
}

#[test]
fn an_unrecognised_code_does_not_fall_back_to_a_basis() {
    // Неизвестный код из базы — это порча, а не `Unknown`: `Unknown`
    // означает «источник не доказал», а не «строку не прочитали».
    assert_eq!(QuotationBasis::from_code("percent"), None);
}
```

В `crates/iaam-market/src/observation.rs`, в существующий `mod tests` (`observation.rs:117`). `serde_json` объявлен обычной зависимостью крейты (`iaam-market/Cargo.toml`), поэтому в тестах доступен без правки манифеста:

```rust
#[test]
fn an_observation_written_before_the_basis_existed_reads_as_unknown() {
    let value = serde_json::json!({
        "instrument": InstrumentId::new_random(),
        "venue": {"board": "TQBR", "session": 3},
        "trade_date": TradeDate(date!(2026 - 08 - 03)),
        "observed_at": ObservedAt(datetime!(2026-08-03 19:00:00 UTC)),
        "kind": PriceKind::Close,
        "price": Dec::new(Decimal::from(100)),
        "currency": CurrencyCode::Rub,
        "executability": Executability::IndicativePreviousClose,
    });
    let observation: PriceObservation = serde_json::from_value(value).unwrap();
    assert_eq!(observation.basis, QuotationBasis::Unknown);
    assert_eq!(observation.basis_evidence, "");
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает**

Run: `nix develop -c cargo nextest run -p iaam-core -p iaam-market -E 'test(quotation_basis) + test(basis)'`
Expected: FAIL — `cannot find type QuotationBasis`, `no field basis`.

- [ ] **Step 3: Реализация**

В `crates/iaam-core/src/valuation/candidate.rs`, рядом с `SourceExecutability`:

```rust
/// Единица, в которой источник назвал цену (§10.2).
///
/// Третья ось наряду с полнотой и исполнимостью (ADR-0002), и, как они,
/// **атрибут наблюдения от источника**, а не вывод политики: основание
/// задаётся рынком и режимом торгов, из которого адаптер брал строку.
/// Вывести его правилом задним числом — то же смешение осей, которое
/// решение 0002 запрещает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QuotationBasis {
    /// Деньги за одну бумагу. Валюта числа — валюта наблюдения.
    MoneyPerUnit,
    /// Проценты непогашенного номинала. Само число **безразмерно**:
    /// денежная валюта приходит из валюты номинала, а не отсюда.
    PercentOfRemainingFace,
    /// Источник основания не доказал. Отказ при оценке, а не догадка.
    #[default]
    Unknown,
}

impl QuotationBasis {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MoneyPerUnit => "money_per_unit",
            Self::PercentOfRemainingFace => "percent_of_remaining_face",
            Self::Unknown => "unknown",
        }
    }

    /// Разбор кода из хранилища. `None`, а не `Unknown`: неизвестный код —
    /// порча строки, и выдать её за недоказанное наблюдение значит
    /// спрятать порчу.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::MoneyPerUnit,
            Self::PercentOfRemainingFace,
            Self::Unknown,
        ]
        .into_iter()
        .find(|basis| basis.code() == code)
    }
}
```

В той же файле, в `PriceCandidate`, после `currency`:

```rust
    /// Единица цены. `#[serde(default)]` не нужен: `PriceCandidate`
    /// не сериализуется, он строится на каждом расчёте.
    pub basis: QuotationBasis,
    /// Признак, по которому основание выведено. Хранится рядом, а не
    /// восстанавливается по основанию: без него запись недоказуема
    /// при разборе аудита (§10.2).
    pub basis_evidence: String,
```

В `crates/iaam-market/src/observation.rs`, в `PriceObservation`, после `currency`:

```rust
    /// Единица цены, доказанная при разборе (§10.2).
    ///
    /// `#[serde(default)]` обязателен: наблюдения записаны до появления
    /// поля, и подставить им `MoneyPerUnit` значит объявить доказанным
    /// то, чего никто не доказывал.
    #[serde(default)]
    pub basis: QuotationBasis,
    /// Признак, из которого основание выведено.
    #[serde(default)]
    pub basis_evidence: String,
```

- [ ] **Step 4: Прогнать.** Expected: PASS (5 тестов). Механическая правка литералов `PriceObservation` и `PriceCandidate` в тестах разрешена; **утверждения существующих тестов не менять**.

Run: `nix develop -c cargo nextest run --workspace`

- [ ] **Step 5: Коммит**

```bash
git add crates/iaam-core/src/valuation/candidate.rs crates/iaam-market/src/observation.rs
git commit -m "feat(core): основание котировки как атрибут наблюдения (iaam-a75)"
```

---

### Task 2: разбор MOEX доказывает основание из пути запроса

**Files:**
- Modify: `crates/iaam-market/src/moex/parse.rs`, `crates/iaam-app/src/scenarios/sync.rs`, `crates/iaam-app/src/market_candidate.rs` (вызовы `parse_history` в тестах)

**Interfaces:**
- Consumes: T1
- Produces: `parse_history(body, instrument, observed_at, segment: MarketSegment)`; `MarketSegment { engine: &str, market: &str }`; `MarketSegment::quotation_basis() -> (QuotationBasis, String)`

**Acceptance Criteria:**
- Основание выводится из пары `(engine, market)` — того же, из чего построен путь запроса, — и **не** из рода инструмента.
- Признак попадает в `basis_evidence` строкой вида `iss:engines/stock/markets/bonds`.
- Незнакомая пара даёт `Unknown`, а не `MoneyPerUnit`: неизвестный рынок котирует неизвестно как.
- Ни одного `_` в `match` по паре: новый рынок обязан быть решением, а не умолчанием.

> ⚠️ **Фикстура `tests/fixtures/market/moex-iss-history-sber.json` — файл политики** (`tests/fixtures` в списке `scripts/check-diff-lint.sh:80`). Эта задача её **не трогает**: существующая фикстура акций проверяет ветку `MoneyPerUnit`, а ветка `PercentOfRemainingFace` проверяется на том же теле с другим сегментом — основание из тела ответа не читается вовсе. Облигационная фикстура понадобится только части 2 E3.4.

- [ ] **Step 1: Написать падающие тесты**

В `crates/iaam-market/src/moex/parse.rs`, в существующий `mod tests`:

```rust
const BONDS: MarketSegment<'static> = MarketSegment {
    engine: "stock",
    market: "bonds",
};
const SHARES: MarketSegment<'static> = MarketSegment {
    engine: "stock",
    market: "shares",
};

#[test]
fn the_bond_market_quotes_in_percent_of_remaining_face() {
    let (basis, evidence) = BONDS.quotation_basis();
    assert_eq!(basis, QuotationBasis::PercentOfRemainingFace);
    assert_eq!(evidence, "iss:engines/stock/markets/bonds");
}

#[test]
fn the_share_market_quotes_in_money_per_unit() {
    assert_eq!(SHARES.quotation_basis().0, QuotationBasis::MoneyPerUnit);
}

#[test]
fn an_unfamiliar_market_does_not_default_to_money_per_unit() {
    // Неизвестный рынок котирует неизвестно как. `MoneyPerUnit` здесь —
    // догадка, которая занизила бы облигацию молча.
    let segment = MarketSegment {
        engine: "currency",
        market: "selt",
    };
    assert_eq!(segment.quotation_basis().0, QuotationBasis::Unknown);
}

#[test]
fn the_basis_comes_from_the_segment_not_from_the_response_body() {
    // То же тело ответа, разобранное как облигационный сегмент, даёт
    // процент. Основание задаётся рынком, а не содержимым строки, —
    // иначе оно было бы эвристикой по роду инструмента (§10.2).
    let instrument = InstrumentId::new_random();
    let observed_at = ObservedAt(datetime!(2026-08-21 19:00:00 UTC));
    let as_shares = parse_history(FIXTURE, instrument, observed_at, SHARES).unwrap();
    let as_bonds = parse_history(FIXTURE, instrument, observed_at, BONDS).unwrap();

    assert_eq!(as_shares[0].basis, QuotationBasis::MoneyPerUnit);
    assert_eq!(as_bonds[0].basis, QuotationBasis::PercentOfRemainingFace);
    assert_eq!(as_shares[0].price, as_bonds[0].price, "цена не меняется");
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

Run: `nix develop -c cargo nextest run -p iaam-market -E 'test(quotes_in) + test(unfamiliar_market) + test(basis_comes_from_the_segment)'`

- [ ] **Step 3: Реализация**

В `crates/iaam-market/src/moex/parse.rs`, перед `parse_history`:

```rust
/// Сегмент ISS, из которого взята строка котировки.
///
/// Это **тот же** engine и market, из которых собран путь запроса
/// (`super::history_request`), поэтому признак не восстанавливается
/// и не угадывается: адаптер его знал, когда шёл за данными.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketSegment<'a> {
    pub engine: &'a str,
    pub market: &'a str,
}

impl MarketSegment<'_> {
    /// Основание котировки и признак, по которому оно выведено.
    ///
    /// Таблица, а не эвристика по роду инструмента: MOEX котирует
    /// долговой рынок в процентах непогашенного номинала, долевой —
    /// в деньгах за бумагу. Незнакомая пара даёт `Unknown`: рынок,
    /// про который правило не написано, котирует неизвестно как,
    /// и молчаливый `MoneyPerUnit` занизил бы облигацию в номинал/100 раз.
    #[must_use]
    pub fn quotation_basis(self) -> (QuotationBasis, String) {
        let basis = match (self.engine, self.market) {
            ("stock", "bonds") => QuotationBasis::PercentOfRemainingFace,
            ("stock", "shares") => QuotationBasis::MoneyPerUnit,
            _ => QuotationBasis::Unknown,
        };
        (basis, self.evidence())
    }

    fn evidence(self) -> String {
        format!("iss:engines/{}/markets/{}", self.engine, self.market)
    }
}
```

> **Ветка `_` здесь разрешена и обоснована:** образец идёт по паре **строк**, а не по перечислению, поэтому исчерпывающего `match` не существует. Запрет из Global Constraints относится к `match` по `QuotationBasis`, и он соблюдён: все три члена перечислены в `code()`.

`parse_history` получает четвёртый параметр и заполняет поля:

```rust
pub fn parse_history(
    body: &str,
    instrument: InstrumentId,
    observed_at: ObservedAt,
    segment: MarketSegment<'_>,
) -> Result<Vec<PriceObservation>, MarketError> {
    let (basis, basis_evidence) = segment.quotation_basis();
    // …существующее тело без изменений до построения наблюдения…
            observations.push(PriceObservation {
                instrument,
                venue: venue.clone(),
                trade_date,
                observed_at,
                kind,
                price: Dec::new(price),
                currency,
                basis,
                basis_evidence: basis_evidence.clone(),
                executability: Executability::IndicativePreviousClose,
            });
```

В `crates/iaam-app/src/scenarios/sync.rs:462` вызов получает сегмент из того же источника, из которого построен путь:

```rust
        MarketSource::Moex {
            instrument,
            engine,
            market,
            ..
        } => {
            let body = core::str::from_utf8(body)
                .map_err(|error| AppError::Store(format!("ответ MOEX не UTF-8: {error}")))?;
            iaam_market::moex::parse::parse_history(
                body,
                *instrument,
                observed_at,
                iaam_market::moex::parse::MarketSegment { engine, market },
            )
            .map(ParsedObservations::Prices)
            .map_err(|error| AppError::Store(error.to_string()))
        }
```

- [ ] **Step 4: Прогнать.** Expected: PASS (4 теста). Механическая правка вызовов `parse_history` в `crates/iaam-app/src/market_candidate.rs:143,184` разрешена.

Run: `nix develop -c cargo nextest run --workspace`

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(market): основание котировки доказывается сегментом ISS (iaam-a75)"
```

---

### Task 3: миграция 0008 — основание в хранилище

**Files:**
- Create: `crates/iaam-store/migrations/0008_quotation_basis.sql`
- Modify: `crates/iaam-store/src/schema.rs`
- Test: `crates/iaam-store/tests/migration_0008.rs` (**новый**, по образцу `migration_0007.rs`)

**Interfaces:**
- Consumes: T1
- Produces: колонки `price_observations.quotation_basis` и `price_observations.basis_evidence`; `SCHEMA_VERSION = 8`

**Acceptance Criteria:**
- Существующие строки получают `'unknown'`, а **не** `'money_per_unit'`: доказательства, что облигационных строк в них нет, у миграции нет, и неинтерпретируемая строка честнее подставленной (§10.4).
- `CHECK` ограничивает колонку тремя кодами: неизвестный код в базу не попадает.
- База версии 7 открывается и мигрирует; база версии 8 остаётся читаемой.
- Триггеры неизменяемости и запрета удаления сохраняются: правка таблицы не имеет права снять заслон append-only.

- [ ] **Step 1: Написать падающий тест**

`crates/iaam-store/tests/migration_0008.rs`:

```rust
//! Миграция 0008: основание котировки в наблюдении цены.

use iaam_store::SqliteStore;

#[test]
fn an_existing_observation_migrates_to_an_undecided_basis() {
    // Подставить старой строке `money_per_unit` значило бы объявить
    // доказанным то, чего никто не доказывал: облигационных строк
    // в ней могло и не быть, а могло и быть (§10.4).
    let store = SqliteStore::open_in_memory().unwrap();
    let basis: String = store
        .connection()
        .query_row(
            "SELECT quotation_basis FROM price_observations LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "unknown".to_owned());
    assert_eq!(basis, "unknown");
}

#[test]
fn an_unknown_basis_code_is_refused_by_the_table() {
    let store = SqliteStore::open_in_memory().unwrap();
    let refused = store.connection().execute(
        "INSERT INTO price_observations (
             instrument_id, board, session, trade_date, kind, source_id,
             observed_at, price, currency, quotation_basis, basis_evidence,
             executability, raw_hash, sync_run_id
         ) VALUES ('i','TQBR',3,'2026-08-03','close','s','2026-08-03T19:00:00Z',
                   '100','RUB','percent','x','executable','h','r')",
        [],
    );
    assert!(refused.is_err(), "неизвестный код основания обязан быть отвергнут");
}

#[test]
fn the_append_only_triggers_survive_the_migration() {
    // Правка таблицы не имеет права снять заслон: пересоздание через
    // `_new` уносит триггеры вместе со старой таблицей.
    let store = SqliteStore::open_in_memory().unwrap();
    let count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'trigger' AND tbl_name = 'price_observations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2, "оба триггера append-only обязаны существовать");
}
```

> `SqliteStore::connection() -> &Connection` существует (`crates/iaam-store/src/lib.rs:173`, `const fn`), поэтому тест читает схему напрямую. Проверено при написании плана.

- [ ] **Step 2: Прогнать, убедиться, что падает.**

Run: `nix develop -c cargo nextest run -p iaam-store --test migration_0008`

- [ ] **Step 3: Реализация**

`crates/iaam-store/migrations/0008_quotation_basis.sql`:

```sql
-- 0008: основание котировки — атрибут наблюдения от источника (§10.2).
-- Дефект iaam-a75: без основания облигация по 98.5 при номинале 1000 ₽
-- оценивалась в 98.5 ₽ вместо 985 ₽.
--
-- Существующие строки получают 'unknown', а НЕ 'money_per_unit':
-- доказательства, что облигационных строк среди них нет, у миграции
-- не существует, а неинтерпретируемая строка честнее подставленной.
DROP TRIGGER price_observations_are_immutable;
DROP TRIGGER price_observations_are_not_deletable;
DROP INDEX price_observations_by_series;

CREATE TABLE price_observations_new (
    instrument_id   TEXT NOT NULL REFERENCES instruments(id),
    board           TEXT NOT NULL,
    session         INTEGER NOT NULL,
    trade_date      TEXT NOT NULL,
    kind            TEXT NOT NULL,
    source_id       TEXT NOT NULL,
    observed_at     TEXT NOT NULL,
    price           TEXT NOT NULL,
    currency        TEXT NOT NULL,
    quotation_basis TEXT NOT NULL,
    basis_evidence  TEXT NOT NULL,
    executability   TEXT NOT NULL,
    raw_hash        TEXT NOT NULL,
    sync_run_id     TEXT NOT NULL REFERENCES sync_runs(id),
    PRIMARY KEY (
        instrument_id, board, session, trade_date, kind, source_id, observed_at
    ),
    CHECK (executability IN ('executable', 'indicative_previous_close')),
    CHECK (quotation_basis IN ('money_per_unit', 'percent_of_remaining_face', 'unknown'))
) STRICT;

INSERT INTO price_observations_new (
    instrument_id, board, session, trade_date, kind, source_id, observed_at,
    price, currency, quotation_basis, basis_evidence, executability,
    raw_hash, sync_run_id
)
SELECT
    instrument_id, board, session, trade_date, kind, source_id, observed_at,
    price, currency, 'unknown', '', executability, raw_hash, sync_run_id
FROM price_observations;

DROP TABLE price_observations;
ALTER TABLE price_observations_new RENAME TO price_observations;

CREATE INDEX price_observations_by_series
    ON price_observations (
        instrument_id, board, session, trade_date, source_id, observed_at
    );

CREATE TRIGGER price_observations_are_immutable
BEFORE UPDATE ON price_observations
BEGIN
    SELECT RAISE(ABORT, 'наблюдение цены append-only: исправление — новая строка');
END;

CREATE TRIGGER price_observations_are_not_deletable
BEFORE DELETE ON price_observations
BEGIN
    SELECT RAISE(ABORT, 'наблюдение цены append-only: удаление запрещено');
END;
```

В `crates/iaam-store/src/schema.rs`:

```rust
pub const SCHEMA_VERSION: u32 = 8;

const MIGRATIONS: [(u32, &str); 8] = [
    // …семь существующих строк без изменений…
    (8, include_str!("../migrations/0008_quotation_basis.sql")),
];
```

- [ ] **Step 4: Прогнать.** Expected: PASS (3 теста), весь `iaam-store` зелёный.

Run: `nix develop -c cargo nextest run -p iaam-store`

- [ ] **Step 5: Коммит**

```bash
git add crates/iaam-store/migrations/0008_quotation_basis.sql crates/iaam-store/src/schema.rs crates/iaam-store/tests/migration_0008.rs
git commit -m "feat(store): миграция 0008 — основание котировки (iaam-a75)"
```

---

### Task 4: хранилище проносит основание в обе стороны

**Files:**
- Modify: `crates/iaam-store/src/market.rs`

**Interfaces:**
- Consumes: T3
- Produces: поля `PriceRow::{quotation_basis, basis_evidence}`

**Acceptance Criteria:**
- Записанное основание читается обратно всеми **тремя** путями чтения: `market.rs:448` (одна строка), `:494` (`prices_for_instrument_between`), `:549` (третья выборка). Пропущенный путь теряет основание молча.
- Строка с неизвестным кодом отвергается на чтении отказом, а не превращается в `Unknown`.

- [ ] **Step 1: Написать падающий тест**

В `crates/iaam-store/tests/market_observations.rs`:

```rust
#[test]
fn the_quotation_basis_survives_a_round_trip_through_every_read_path() {
    // Основание, потерянное на одном из путей чтения, обнаружится
    // не отказом, а заниженной в номинал/100 раз стоимостью позиции.
    let (mut store, instrument) = store_with_instrument();
    let run = store
        .begin_run(
            series_with_dataset("moex", "SBER"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("запуск цен");
    let rows = [bond_price(
        instrument,
        "2026-08-03T19:00:00Z",
        "98.5",
        "percent_of_remaining_face",
        "iss:engines/stock/markets/bonds",
    )];
    store.record_prices(&run, "raw-basis", &rows).unwrap();
    store.finish_run(&run).unwrap();

    let read = store
        .prices_for_instrument_between(
            instrument,
            "moex",
            MarketWindow {
                from: "2026-08-03",
                to: "2026-08-03",
                knowledge_as_of: "2026-08-04T00:00:00Z",
            },
        )
        .unwrap();
    assert_eq!(read[0].quotation_basis, "percent_of_remaining_face");
    assert_eq!(read[0].basis_evidence, "iss:engines/stock/markets/bonds");
}
```

> Помощники `store_with_instrument` (`market_observations.rs:14`), `series_with_dataset` (`:34`) и `lease` (`:76`) существуют. Помощника с основанием нет: существующий `price(instrument, observed_at, value)` (`:42`) заполняет `PriceRow` без новых полей. `bond_price` дописывается рядом с ним — расширять `price` пятью аргументами нельзя, порог `too-many-arguments-threshold = 6` действует и в тестах.

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

В `crates/iaam-store/src/market.rs`, в `PriceRow`, после `currency`:

```rust
    /// Код основания котировки. Строкой, как и остальные значения
    /// источника: хранилище не зависит от крейты формата.
    pub quotation_basis: String,
    /// Признак, по которому основание выведено.
    pub basis_evidence: String,
```

`record_prices` (`market.rs:195`) — две колонки в списке и два плейсхолдера:

```rust
        "INSERT INTO price_observations (
             instrument_id, board, session, trade_date, kind, source_id,
             observed_at, price, currency, quotation_basis, basis_evidence,
             executability, raw_hash, sync_run_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
```

и в `params![…]` — `row.quotation_basis.as_str(), row.basis_evidence.as_str()`
**в том же порядке**, что и колонки: перепутанные местами строки не дадут
ни ошибки типа, ни отказа `CHECK`, потому что обе текстовые.

Все три выборки (`:448`, `:494`, `:549`) добавляют колонки в `SELECT`
и в замыкание сборки:

```rust
                    Ok(PriceRow {
                        // …существующие поля без изменений…
                        currency: row.get(8)?,
                        quotation_basis: row.get(9)?,
                        basis_evidence: row.get(10)?,
                        executability: row.get(11)?,
                    })
```

> Индексы столбцов сдвигаются во **всех трёх** замыканиях. Сдвиг,
> сделанный в двух из трёх, читается как валюта в поле основания —
> и `CHECK` его не поймает, потому что проверяет запись, а не чтение.
> Тест из шага 1 идёт через `prices_for_instrument_between`; для двух
> других путей дописываются такие же.

- [ ] **Step 4: Прогнать.** Expected: PASS, весь `iaam-store` зелёный.

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(store): основание котировки проходит запись и все три чтения (iaam-a75)"
```

---

### Task 5: кандидат и происхождение несут основание

**Files:**
- Modify: `crates/iaam-app/src/market_candidate.rs`, `crates/iaam-core/src/valuation/candidate.rs`, `crates/iaam-core/src/rules/valuation.rs`, `crates/iaam-core/src/returns/mod.rs`

**Interfaces:**
- Consumes: T1, T2, T4
- Produces: `PriceProvenance::{quotation_basis, basis_evidence}`

**Acceptance Criteria:**
- `candidate_from_market_observation` переносит основание и признак без изменений.
- Кандидат из журнального `Valuation` получает `MoneyPerUnit` с признаком `journal:valuation`: по §10.3 цена владельца — деньги за единицу **по определению**, и это не догадка, а контракт события.
- Основание попадает в `PriceProvenance` выбранной цены: без него след аудита не объясняет, откуда взялась денежная стоимость.

- [ ] **Step 1: Написать падающие тесты**

В `crates/iaam-app/src/market_candidate.rs`, в `mod tests`:

```rust
#[test]
fn the_candidate_keeps_the_basis_the_observation_proved() {
    let mut observation = observation(PriceKind::Close, Executability::Executable);
    observation.basis = QuotationBasis::PercentOfRemainingFace;
    observation.basis_evidence = "iss:engines/stock/markets/bonds".to_owned();

    let candidate = candidate_from_market_observation(observation);

    assert_eq!(candidate.basis, QuotationBasis::PercentOfRemainingFace);
    assert_eq!(candidate.basis_evidence, "iss:engines/stock/markets/bonds");
}
```

В `crates/iaam-core/src/returns/mod.rs`, в `mod tests`:

```rust
#[test]
fn an_owner_valuation_is_money_per_unit_by_contract_not_by_guess() {
    // §10.3: цена в журнальном `Valuation` — оценка владельца
    // для неликвида, и она деньги за единицу по определению.
    // Ввод процента номинала через это событие запрещён.
    //
    // Кандидат из `InstrumentPrice` собирается прямо в
    // `position_assessments` (`returns/mod.rs:782`), отдельной функции
    // для него нет: проверяем через собранную оценку позиции.
    let assessments = position_assessments(&state_with_owner_valuation(), &request);
    let PositionAssessmentKind::Selected(selected) = &assessments[0].kind else {
        panic!("владельческая оценка обязана быть выбрана");
    };
    assert_eq!(selected.candidate.basis, QuotationBasis::MoneyPerUnit);
    assert_eq!(selected.candidate.basis_evidence, "journal:valuation");
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

В `crates/iaam-app/src/market_candidate.rs` — два поля в собираемого кандидата:

```rust
    PriceCandidate {
        instrument: observation.instrument,
        price: observation.price,
        currency: observation.currency,
        basis: observation.basis,
        basis_evidence: observation.basis_evidence,
        trade_date: observation.trade_date.0,
        // …остальное без изменений…
    }
```

В `crates/iaam-core/src/valuation/candidate.rs`, в `PriceProvenance`,
после `venue`:

```rust
    /// Единица, в которой источник назвал цену. Без неё след аудита
    /// не объясняет, откуда взялась денежная стоимость позиции.
    pub quotation_basis: QuotationBasis,
    /// Признак, по которому основание выведено.
    pub basis_evidence: String,
```

В `crates/iaam-core/src/rules/valuation.rs`, там где `ValuationPolicyV1`
собирает `PriceProvenance` выбранного кандидата, оба поля берутся
**из кандидата**, а не выводятся заново:

```rust
            quotation_basis: candidate.basis,
            basis_evidence: candidate.basis_evidence.clone(),
```

В `crates/iaam-core/src/returns/mod.rs:782` кандидат из `InstrumentPrice`:

```rust
                let candidate = PriceCandidate {
                    instrument: price.instrument,
                    price: price.price,
                    currency: price.currency,
                    // §10.3: цена владельца — деньги за единицу
                    // по определению, а не по догадке. Ввод процента
                    // номинала через `EventKind::Valuation` запрещён.
                    basis: QuotationBasis::MoneyPerUnit,
                    basis_evidence: "journal:valuation".to_owned(),
                    trade_date: price.as_of,
                    observed_at: request.coordinate.knowledge_as_of,
                    origin: crate::valuation::PriceOrigin::ReportParsed { source },
                    executability: SourceExecutability::Unknown,
                };
```

- [ ] **Step 4: Прогнать.** Run: `nix develop -c cargo nextest run --workspace`

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): основание доходит до кандидата и до следа аудита (iaam-a75)"
```

---

### Task 6: версионированное правило пересчёта котировки в деньги

**Files:**
- Create: `crates/iaam-core/src/rules/quotation.rs`
- Modify: `crates/iaam-core/src/rules/mod.rs`

**Interfaces:**
- Consumes: T1
- Produces: `QuotationRuleVersion(u32)`, трейт `QuotationRule`, `QuotationV1`, `QuotationError::{BasisUnknown, PrincipalUnknown, Numeric}`; `RuleRegistry::{quotation_rule, latest_quotation_version}`

**Acceptance Criteria:**
- Одно правило на оба места умножения: два независимых пересчёта разъедутся (§10.4).
- `MoneyPerUnit` возвращает цену и валюту наблюдения без изменений.
- `PercentOfRemainingFace` даёт `price / 100 × remaining_per_unit`, а валюту берёт **из номинала**: число 98.5 безразмерно, и `PriceObservation.currency` валютой этого числа не является (§10.2).
- `PercentOfRemainingFace` при неизвестном номинале — `PrincipalUnknown`, при `QuotationBasis::Unknown` — `BasisUnknown`. Ни то, ни другое не подставляет цену как деньги.
- **Варианта `CurrencyMismatch` в `QuotationError` нет намеренно:** правило сравнивать нечего — валюта результата приходит из номинала, валюта площадки к безразмерному проценту не относится, а перевод в валюту отчёта делает `FxTable` уже после. Недостижимый вариант — выживший мутант и ложное обещание проверки.
- Деление на 100 — через `Dec::checked_div` (добавлен в E3.4.1.T1), а не через литерал с плавающей точкой.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn money_per_unit_passes_the_price_and_its_currency_through() {
    assert_eq!(
        QuotationV1
            .money_per_unit(QuotationBasis::MoneyPerUnit, dec("270.13"), CurrencyCode::Rub, None)
            .unwrap(),
        (dec("270.13"), CurrencyCode::Rub)
    );
}

#[test]
fn a_percent_quote_becomes_money_through_the_remaining_face() {
    // 98.5% от непогашенного номинала 1000 ₽ — это 985 ₽,
    // а не 98.5 ₽. Ровно дефект iaam-a75.
    assert_eq!(
        QuotationV1
            .money_per_unit(
                QuotationBasis::PercentOfRemainingFace,
                dec("98.5"),
                CurrencyCode::Rub,
                Some(per_unit("1000")),
            )
            .unwrap(),
        (dec("985.000"), CurrencyCode::Rub)
    );
}

#[test]
fn a_percent_quote_takes_its_currency_from_the_face_not_from_the_venue() {
    // Число 98.5 безразмерно: валюта площадки валютой этого числа
    // не является (§10.2).
    let (_, currency) = QuotationV1
        .money_per_unit(
            QuotationBasis::PercentOfRemainingFace,
            dec("98.5"),
            CurrencyCode::Rub,
            Some(usd_per_unit("1000")),
        )
        .unwrap();
    assert_eq!(currency, CurrencyCode::Usd);
}

#[test]
fn a_percent_quote_without_a_known_face_refuses_instead_of_guessing() {
    assert_eq!(
        QuotationV1
            .money_per_unit(
                QuotationBasis::PercentOfRemainingFace,
                dec("98.5"),
                CurrencyCode::Rub,
                None,
            )
            .unwrap_err(),
        QuotationError::PrincipalUnknown
    );
}

#[test]
fn an_undecided_basis_refuses_rather_than_assuming_money() {
    // Занизить облигацию в номинал/100 раз молча — худшее,
    // что может сделать оценка.
    assert_eq!(
        QuotationV1
            .money_per_unit(QuotationBasis::Unknown, dec("98.5"), CurrencyCode::Rub, None)
            .unwrap_err(),
        QuotationError::BasisUnknown
    );
}

#[test]
fn the_registry_resolves_the_default_quotation_rule() {
    let registry = RuleRegistry::with_defaults();
    assert_eq!(registry.latest_quotation_version(), Some(QuotationRuleVersion(1)));
    assert!(registry.quotation_rule(QuotationRuleVersion(1)).is_some());
    assert!(registry.quotation_rule(QuotationRuleVersion(999)).is_none());
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

```rust
//! Пересчёт котировки в деньги за бумагу (§10.2).
//!
//! Отдельное версионированное правило, а не арифметика на месте:
//! умножение количества на цену живёт в двух местах —
//! `returns::position_value` и `returns::xirr::account_values`, — и два
//! независимых пересчёта неизбежно разъедутся (§10.4).

pub trait QuotationRule: Send + Sync + std::fmt::Debug {
    /// Деньги за одну бумагу и валюта этих денег.
    fn money_per_unit(
        &self,
        basis: QuotationBasis,
        price: Dec,
        venue_currency: CurrencyCode,
        remaining_face: Option<PerUnitAmount>,
    ) -> Result<(Dec, CurrencyCode), QuotationError>;
}

#[derive(Debug, Default)]
pub struct QuotationV1;

impl QuotationRule for QuotationV1 {
    fn money_per_unit(
        &self,
        basis: QuotationBasis,
        price: Dec,
        venue_currency: CurrencyCode,
        remaining_face: Option<PerUnitAmount>,
    ) -> Result<(Dec, CurrencyCode), QuotationError> {
        match basis {
            // Валюта наблюдения остаётся валютой числа, как и было.
            QuotationBasis::MoneyPerUnit => Ok((price, venue_currency)),
            QuotationBasis::PercentOfRemainingFace => {
                let face = remaining_face.ok_or(QuotationError::PrincipalUnknown)?;
                let fraction = price
                    .checked_div(Dec::new(Decimal::ONE_HUNDRED))
                    .map_err(QuotationError::Numeric)?;
                let money = fraction
                    .checked_mul(face.value())
                    .map_err(QuotationError::Numeric)?;
                // Денежная валюта приходит из номинала: само число
                // безразмерно, и валюта площадки к нему не относится.
                Ok((money, face.currency()))
            }
            // Наблюдение, происхождение которого не доказано, оценке
            // не подлежит: догадка занизила бы облигацию молча.
            QuotationBasis::Unknown => Err(QuotationError::BasisUnknown),
        }
    }
}
```

`RuleRegistry` получает третью карту — по образцу `amortisation_rules`, добавленной в E3.4.1.T5 (`crates/iaam-core/src/rules/mod.rs`).

- [ ] **Step 4: Прогнать.** Expected: PASS (6 тестов).

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(core): версионированное правило пересчёта котировки (iaam-a75)"
```

---

### Task 7: `position_value` считает через правило и остаточный номинал

**Files:**
- Modify: `crates/iaam-core/src/returns/mod.rs`

**Interfaces:**
- Consumes: T5, T6
- Produces: `NotComputable::{QuotationBasisUnknown, RemainingFaceUnknown, RemainingFaceAmbiguous}`; `AppliedRules::quotation_rule`

**Acceptance Criteria:**
- Стоимость позиции по проценту номинала считается через остаточный номинал лотов пары «счёт и бумага».
- Расхождение остаточного номинала между лотами одной пары — `RemainingFaceAmbiguous`, а не усреднение: одна бумага одного выпуска на одну дату имеет один непогашенный номинал.
- Неизвестный номинал и недоказанное основание дают `not_computable` с **разными** причинами: они чинятся по-разному.
- Версия правила входит в `applied_rules` полем `quotation_rule: QuotationRuleVersion`: цифра, зависящая от правила, обязана нести правило рядом с собой. `AppliedRules` (`returns/mod.rs:299`) — не `Serialize`-часть слепка, поэтому `#[serde(default)]` ей не нужен; DTO отчёта правится вместе с ней в T9.
- Валютный порядок — номинал → курс до валюты отчёта, а не наоборот.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn a_bond_quoted_in_percent_is_valued_through_its_remaining_face() {
    // Ровно дефект iaam-a75: 10 бумаг по 98.5% номинала 1000 ₽ — это
    // 9850 ₽, а не 985 ₽.
    let report = report_with_bond(qty(10), per_unit("1000"), percent_candidate("98.5"));
    assert_eq!(terminal_value_of(&report), Some(dec("9850")));
}

#[test]
fn a_bond_without_a_known_face_is_not_computable_with_its_own_reason() {
    // Неизвестный номинал и недоказанное основание чинятся по-разному,
    // поэтому и причины разные.
    let report = report_with_bond(qty(10), PrincipalState::Unknown, percent_candidate("98.5"));
    assert!(matches!(
        uncovered_reason_of(&report),
        Some(NotComputable::RemainingFaceUnknown { .. })
    ));
}

#[test]
fn an_undecided_basis_is_not_computable_rather_than_valued_as_money() {
    // Строка, мигрировавшая с `unknown`, обязана отказать: догадка
    // занизила бы стоимость в номинал/100 раз молча.
    let report = report_with_bond(qty(10), per_unit("1000"), unknown_basis_candidate("98.5"));
    assert!(matches!(
        uncovered_reason_of(&report),
        Some(NotComputable::QuotationBasisUnknown { .. })
    ));
}

#[test]
fn lots_that_disagree_about_the_remaining_face_refuse_instead_of_averaging() {
    // Одна бумага одного выпуска на одну дату имеет один непогашенный
    // номинал. Расхождение — брак данных, и среднее его спрячет.
    let report = report_with_two_faces(per_unit("1000"), per_unit("800"));
    assert!(matches!(
        uncovered_reason_of(&report),
        Some(NotComputable::RemainingFaceAmbiguous { .. })
    ));
}

#[test]
fn a_share_quoted_in_money_is_valued_exactly_as_before() {
    // Заслон против регрессии: правка облигаций не имеет права
    // изменить ни одной существующей цифры по акциям.
    let report = report_with_share(qty(10), money_candidate("270.13"));
    assert_eq!(terminal_value_of(&report), Some(dec("2701.30")));
}

#[test]
fn the_report_names_the_quotation_rule_it_applied() {
    let report = report_with_share(qty(10), money_candidate("270.13"));
    assert_eq!(report.applied_rules.quotation_rule, QuotationRuleVersion(1));
}
```

> Помощники (`report_with_bond`, `terminal_value_of`, …) в файле отсутствуют и пишутся в этой задаче по образцу существующих тестов `returns/mod.rs`. **Перед написанием прочитать существующий `mod tests` целиком**: конверт отчёта там уже собирается, и второй, написанный вручную, разойдётся с настоящим.

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

`position_assessments` дополнительно достаёт остаточный номинал:

```rust
/// Непогашенный номинал бумаги на счёте по лотам.
///
/// Источник — `Lot::principal` (E3.4.1.T4): реестра параметров выпуска
/// в части 1 нет, а лот номинал уже несёт. Расхождение между лотами —
/// отказ: одна бумага одного выпуска на одну дату имеет один
/// непогашенный номинал, и усреднение спрятало бы брак данных.
fn remaining_face(book: &LotBook, key: LotKey) -> Result<Option<PerUnitAmount>, NotComputable> {
    let Some(entry) = book.entry(&key) else {
        return Ok(None);
    };
    let mut found: Option<PerUnitAmount> = None;
    for lot in entry.lots() {
        let Some(remaining) = lot.principal.remaining_per_unit() else {
            continue;
        };
        match found {
            None => found = Some(remaining),
            Some(previous) if previous == remaining => {}
            Some(_) => {
                return Err(NotComputable::RemainingFaceAmbiguous {
                    instrument: key.instrument,
                });
            }
        }
    }
    Ok(found)
}
```

`position_value` получает основание и номинал и вызывает правило **до** умножения на количество и **до** курса:

```rust
fn position_value(
    assessment: &PositionAssessment,
    price: Dec,
    basis: QuotationBasis,
    venue_currency: CurrencyCode,
    remaining_face: Option<PerUnitAmount>,
    rule: &dyn QuotationRule,
    request: &ReturnsRequest<'_>,
) -> Result<Dec, NotComputable> {
    let (money_per_unit, currency) = rule
        .money_per_unit(basis, price, venue_currency, remaining_face)
        .map_err(NotComputable::from)?;
    let local = assessment
        .quantity
        .0
        .checked_mul(money_per_unit)
        .map_err(|_| NotComputable::Numeric { code: "numeric" })?;
    let rate = request
        .fx
        .rate(currency, request.report_currency, request.as_of)
        .ok_or(NotComputable::MissingFxRate {
            from: currency,
            to: request.report_currency,
            date: request.as_of,
        })?;
    local
        .checked_mul(rate)
        .map_err(|_| NotComputable::Numeric { code: "numeric" })
}
```

> Порог `too-many-arguments-threshold = 6` действует (`clippy.toml`), а подавлять линт запрещено: аргументы сворачиваются в структуру по образцу `TradeDeclaration` (`event/mod.rs:763`).

`NotComputable` (`returns/mod.rs:68`) получает три варианта и три кода
в `NotComputable::code` (`:94`) — диспетчер исчерпывающий, ветки `_`
в нём нет и быть не должно. Отображение отказа правила в отказ отчёта
пишется явным `impl`, а не `?`:

```rust
impl From<QuotationError> for NotComputable {
    fn from(error: QuotationError) -> Self {
        match error {
            QuotationError::BasisUnknown => Self::QuotationBasisUnknown,
            QuotationError::PrincipalUnknown => Self::RemainingFaceUnknown,
            QuotationError::Numeric(_) => Self::Numeric { code: "numeric" },
        }
    }
}
```

Варианты `QuotationBasisUnknown` и `RemainingFaceUnknown` несут
`instrument: InstrumentId` — без него отчёт называет отказ, но не
называет бумагу, которую чинить.

- [ ] **Step 4: Прогнать весь воркспейс.** Run: `nix develop -c make test`

- [ ] **Step 5: Коммит**

```bash
git commit -am "fix(core): облигация оценивается через непогашенный номинал (iaam-a75)"
```

---

### Task 8: XIRR считает тем же правилом

**Files:**
- Modify: `crates/iaam-core/src/returns/xirr.rs`

**Interfaces:**
- Consumes: T6, T7

**Acceptance Criteria:**
- `account_values` умножает количество на цену **через то же правило**, а не собственной арифметикой.
- Цена из `PriceBoard` подаётся как `MoneyPerUnit`: `PriceBoard` заполняется только из `EventKind::Valuation` (`projection/mod.rs:339`), а те по §10.3 деньги за единицу по определению. Это контракт, и он записан тестом, а не комментарием.
- Терминальная стоимость и стоимость позиции в отчёте сходятся на одном и том же наборе входов.

- [ ] **Step 1: Написать падающий тест**

```rust
#[test]
fn the_terminal_value_and_the_position_value_agree_on_the_same_inputs() {
    // Два независимых пересчёта разъедутся (§10.4). Тест закрепляет,
    // что пересчёт один.
    let state = state_with_owner_valuation(qty(10), dec("270.13"));
    assert_eq!(
        terminal_value(&state, &request).unwrap(),
        position_total_of_report(&state, &request)
    );
}

#[test]
fn an_owner_valuation_reaches_xirr_as_money_per_unit() {
    // §10.3: журнальный `Valuation` — оценка владельца для неликвида,
    // деньги за единицу по определению. Ввод процента номинала через
    // это событие запрещён, поэтому иного основания здесь быть не может.
    let state = state_with_owner_valuation(qty(10), dec("100"));
    assert_eq!(terminal_value(&state, &request).unwrap(), dec("1000"));
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

В `crates/iaam-core/src/returns/xirr.rs`, в `account_values`, ветка позиций
(`xirr.rs:127-137`) перестаёт умножать сама:

```rust
        let price = state
            .prices()
            .price_at_or_before(key.instrument, request.as_of)
            .ok_or(NotComputable::MissingPrice {
                instrument: key.instrument,
            })?;
        // `PriceBoard` заполняется только из `EventKind::Valuation`
        // (`projection/mod.rs:339`), а та по §10.3 деньги за единицу
        // по определению. Номинал здесь не нужен и не спрашивается.
        let (money_per_unit, currency) = rule.money_per_unit(
            QuotationBasis::MoneyPerUnit,
            price.price,
            price.currency,
            None,
        )?;
        let local = mul(quantity.0, money_per_unit)?;
        let converted = in_report_currency(local, currency, request)?;
```

Правило берётся из **одного** места на весь модуль отчёта. `ReturnsRequest`
(`returns/mod.rs:335`) реестра правил не несёт — проверено, поля там:
`contour`, `as_of`, `report_currency`, `fx`, `solver_policy`, `coordinate`,
`ledger`, `perimeter`, `market_prices`, — а `ValuationPolicyV1` уже
создаётся прямо в `position_assessments` (`returns/mod.rs:761`). Тащить
реестр через сигнатуру ради одного правила значило бы менять контракт
запроса; вместо этого в `returns/mod.rs` появляется общий помощник:

```rust
/// Правило пересчёта котировки, применяемое отчётом.
///
/// Существует, чтобы `position_value` и `xirr::account_values`
/// считали **одной** реализацией. Спека предупреждает не о числе
/// экземпляров, а о двух независимых пересчётах: они разъедутся,
/// один — нет (§10.4).
pub(crate) const fn quotation_rule() -> (QuotationRuleVersion, QuotationV1) {
    (QuotationRuleVersion(1), QuotationV1)
}
```

`xirr::account_values` зовёт его же, а не создаёт свой экземпляр:
версия из этой пары попадает в `applied_rules` и потому обязана
описывать то, чем считали.

- [ ] **Step 4: Прогнать.** Run: `nix develop -c make test`

- [ ] **Step 5: Коммит**

```bash
git commit -am "fix(core): XIRR и отчёт считают стоимость позиции одним правилом (iaam-a75)"
```

---

### Task 9: основание доходит до `inputs_hash`, DTO и OpenAPI

**Files:**
- Modify: `crates/iaam-core/src/returns/mod.rs`, `crates/iaam-server/src/dto.rs`, `crates/iaam-server/src/openapi.rs`

**Interfaces:**
- Consumes: T5, T7
- Produces: `QuotationBasisDto`; поля `PriceProvenanceDto::{quotation_basis, basis_evidence}`, `MarketPriceDto::{quotation_basis, basis_evidence}`

**Acceptance Criteria:**
- Основание входит в `inputs_hash`: два отчёта, различающиеся только основанием, обязаны иметь **разные** хеши, иначе координата знания не восстанавливает набор входов.
- `PriceProvenanceDto` отдаёт основание и признак: без них внешний агент не может объяснить денежную стоимость.
- `MarketPriceDto` отдаёт то же по справочной поверхности рынка.
- `QuotationBasisDto` зарегистрирован в компонентах OpenAPI.

- [ ] **Step 1: Написать падающий тест**

```rust
#[test]
fn two_reports_differing_only_in_the_quotation_basis_hash_differently() {
    // Совпадающий хеш означал бы, что координата знания не отличает
    // 985 ₽ от 98.5 ₽ — то есть не восстанавливает набор входов.
    let as_money = report_with_basis(QuotationBasis::MoneyPerUnit);
    let as_percent = report_with_basis(QuotationBasis::PercentOfRemainingFace);
    assert_ne!(as_money.inputs_hash, as_percent.inputs_hash);
}

#[test]
fn the_wire_explains_where_the_money_came_from() {
    let dto = provenance_dto_of(selected_percent_price());
    assert_eq!(dto.quotation_basis, QuotationBasisDto::PercentOfRemainingFace);
    assert_eq!(
        dto.basis_evidence.as_deref(),
        Some("iss:engines/stock/markets/bonds")
    );
}
```

- [ ] **Step 2: Прогнать, убедиться, что падает.**

- [ ] **Step 3: Реализация**

`SelectedObservation` (`returns/mod.rs:411`) — `#[derive(Serialize)]`
и входит в канонический слепок, из которого считается `inputs_hash`,
поэтому два поля в ней и есть вход в хеш:

```rust
#[derive(Serialize)]
struct SelectedObservation {
    instrument: InstrumentId,
    price: Dec,
    currency: CurrencyCode,
    quotation_basis: &'static str,
    basis_evidence: String,
    trade_date: Date,
    // …остальное без изменений…
}
```

> `&'static str` от `QuotationBasis::code()`, а не сам `enum`: слепок
> обязан быть стабильным при переименовании варианта в коде. Тот же
> приём уже применён к `executability` в этой же структуре.

В `crates/iaam-server/src/dto.rs` — по образцу `IncomeKindDto`
(добавлен в E3.4.1.T11):

```rust
/// Единица, в которой источник назвал цену.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotationBasisDto {
    MoneyPerUnit,
    PercentOfRemainingFace,
    /// Источник основания не доказал: цена этой строки в деньги
    /// не пересчитывается.
    Unknown,
}

impl QuotationBasisDto {
    #[must_use]
    pub const fn from_domain(basis: QuotationBasis) -> Self {
        match basis {
            QuotationBasis::MoneyPerUnit => Self::MoneyPerUnit,
            QuotationBasis::PercentOfRemainingFace => Self::PercentOfRemainingFace,
            QuotationBasis::Unknown => Self::Unknown,
        }
    }
}
```

`PriceProvenanceDto` (`dto.rs:665`) и `MarketPriceDto` (`dto.rs:1375`)
получают по два поля; `QuotationBasisDto` добавляется в список импортов
и в `components(schemas(…))` в `crates/iaam-server/src/openapi.rs`
(строки 18 и 85 — оба места, иначе тип отсутствует в спеке при живом
поле в ответе).

- [ ] **Step 4: Прогнать.** Run: `nix develop -c make check`

- [ ] **Step 5: Коммит**

```bash
git commit -am "feat(server): основание котировки доходит до следа и до API (iaam-a75)"
```

---

### Task 10: свойство линейности и заслоны

**Files:**
- Modify: `crates/iaam-core/tests/properties.rs`
- Modify (**правка политики, отдельный коммит**): `scripts/check-mutants.sh`

**Interfaces:**
- Consumes: T6–T9

**Acceptance Criteria:**
- Свойство: при **зафиксированных** котировке, количестве и курсе стоимость позиции по `PercentOfRemainingFace` линейна по остаточному номиналу. Оговорка обязательна: между датами меняется и рыночная цена, и амортизация не обязана уменьшить наблюдаемую стоимость в той же пропорции (§10.4).
- `make check` зелёный; `make diff-coverage` не ниже 90%.
- `crates/iaam-core/src/rules/quotation.rs` в списке мутационного заслона, выживших нет.

- [ ] **Step 1: Написать свойство**

```rust
proptest! {
    #[test]
    fn value_is_linear_in_the_remaining_face_at_a_fixed_quote(
        quote in 1i64..20_000,
        face in 1i64..1_000_000,
        multiplier in 2i64..10,
    ) {
        // Котировка, количество и курс зафиксированы; меняется только
        // непогашенный номинал. Без этой оговорки свойство неверно.
        // `Dec` не реализует `From<i64>` намеренно: расчётная величина
        // строится из `Decimal`, как во всём файле (`properties.rs:41`).
        let single = value_at(quote, face);
        let scaled = value_at(quote, face * multiplier);
        let factor = Dec::new(Decimal::from(multiplier));
        prop_assert_eq!(scaled, single.checked_mul(factor).unwrap());
    }
}
```

- [ ] **Step 2: Прогнать полный заслон.** Run: `nix develop -c make check && nix develop -c make diff-coverage`

- [ ] **Step 3: Прогнать мутантов по новому модулю без правки политики.**

```bash
nix develop -c cargo mutants --package iaam-core --file crates/iaam-core/src/rules/quotation.rs
```

Выживший мутант — недостающий тест, а не шум. Убить, прогнать снова.

- [ ] **Step 4: Остановиться и запросить разрешение владельца.**

Каталог `scripts` — файлы политики (`scripts/check-diff-lint.sh:80`), и агент их не правит. Обоснование записать в описание бида, PR пометить `policy-change`.

Обоснование для списка: `quotation.rs` решает, **во что** превращается котировка. Мутант, подменивший ветку `PercentOfRemainingFace` на `MoneyPerUnit`, не ломает ни одной суммы — он занижает стоимость облигационной позиции в `номинал/100` раз, и по самим цифрам это неотличимо от честного расчёта. Тот же класс, что `instrument.rs` и `rules/valuation.rs`.

- [ ] **Step 5: Два коммита — тесты и правка политики раздельно**

```bash
git commit -am "test(core): стоимость по проценту номинала линейна по номиналу (iaam-a75)"
git commit -am "chore(policy): правило котировки в мутационном заслоне (iaam-a75)"
```

---

## Порядок и зависимости

```
T1 (QuotationBasis) ─┬─> T2 (разбор MOEX) ──────────────┐
                     ├─> T3 (миграция) ─> T4 (хранилище)┤
                     └─> T6 (правило) ──────────────────┤
                                                        v
                                          T5 (кандидат и провенанс)
                                                        │
                                          T7 (position_value)
                                                        │
                                          T8 (XIRR) ─> T9 (хеш и DTO) ─> T10
```

**T7 — единственная задача, меняющая цифру отчёта.** До неё основание проходит цепочку, но на расчёт не влияет, поэтому T1–T6 оставляют все существующие цифры нетронутыми. Тест `a_share_quoted_in_money_is_valued_exactly_as_before` в T7 — заслон против регрессии по акциям.

## Что этот план намеренно не делает

- **Не строит реестр параметров выпуска.** Остаточный номинал берётся из лота; реестр — часть 2 E3.4, и она ждёт живой проверки MOEX ISS.
- **Не добавляет основание в `EventKind::Valuation`.** По §10.3 цена владельца — деньги за единицу по определению; правка тронула бы `SCHEMA_VERSION` ради величины, которая у владельческой оценки не определена.
- **Не заполняет основание задним числом для уже записанных наблюдений.** Доказательства, что облигационных строк среди них нет, не существует, и `unknown` честнее подстановки (§10.4).
- **Не трогает валютную формулу denomination → settlement (§2.7).** Она часть 2; здесь валюта номинала используется напрямую.
- **Не заводит облигационную фикстуру MOEX.** Основание из тела ответа не читается, поэтому обе ветки проверяются на существующей фикстуре с разным сегментом.
