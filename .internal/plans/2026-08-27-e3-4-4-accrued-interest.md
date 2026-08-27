# E3.4.4 — НКД, ближайшая выплата и окончательность: план реализации

> **Для агентов:** ОБЯЗАТЕЛЬНАЯ ПОДСКИЛЛА — `beads-superpowers:subagent-driven-development`
> (рекомендуется) либо `beads-superpowers:executing-plans`. Каждая Задача становится
> бидом (`bd create -t task --parent <epic-id>`). Шаги внутри задач — чекбоксы для
> человека.

**Цель:** отчёт отдаёт по каждой облигационной позиции три величины §5.1 —
начисленный НКД, реализуемую при выходе сумму и дату ближайшей выплаты, — а
возврат номинала знает, окончателен ли он.

**Архитектура:** наблюдение `ACCINT` разбирается из уже читаемого ответа
`history` MOEX и хранится наблюдением; расчёт НКД — версионированное правило
ядра поверх снимка графика E3.4.2; `iaam-app` переводит снимок в лёгкие
доменные типы **структурно**, без единого правила. Заслон §3.2 (`iaam-core` не
зависит от крейт воркспейса) держится тем же приёмом, что и на ценах:
`market_candidate.rs`.

**Стек:** Rust, `rust_decimal` через `Dec`, `time::Date`, `rusqlite`
(STRICT-таблицы), `serde_json` для разбора ISS.

**Спека:** `.internal/specs/2026-08-27-e3-4-4-accrued-interest-design.md`
**Родительский эпик:** `iaam-d8b` (E3). Закрывает также `iaam-d8b.4.3`.

## Глобальные ограничения

- `iaam-core` не зависит ни от одной крейты воркспейса (`scripts/check-architecture.sh`, §3.2).
- `reqwest` объявляется только в `iaam-http` (§3.1).
- В ядре нет `async` и нет `f64`.
- Правило, влияющее на цифру, обязано быть версионированным и попадать в
  `AppliedRules` и в `inputs_hash`.
- Вывод (расчёт, перенос, признак окончательности) в хранилище наблюдений
  не пишется — ADR-0002.
- Отсутствие знания — `Computed::NotComputable { reason }`, **никогда не ноль**.
- `cargo build --workspace` зелёный на каждом коммите.
- Правка `tests/fixtures/**` и `scripts/check-mutants.sh` — файлы политики
  (`scripts/check-diff-lint.sh:80`): отдельный коммит с `POLICY_CHANGE_APPROVED=1`
  и меткой PR `policy-change`.
- Порог покрытия на добавленных строках — 90 % (`make diff-coverage BASE=...`).

## Карта файлов

| файл | ответственность |
|---|---|
| `crates/iaam-core/src/numeric/decimal.rs` | + `Dec::checked_round_to_scale` |
| `crates/iaam-core/src/bond/mod.rs` (создать) | `AccrualPeriod`, `PrincipalReturn`, `PrincipalReturnFinality` |
| `crates/iaam-core/src/bond/finality.rs` (создать) | вывод окончательности возврата номинала |
| `crates/iaam-core/src/bond/posting.rs` (создать) | `next_posting_date` |
| `crates/iaam-core/src/rules/accrued_interest.rs` (создать) | `AccruedInterestRule`, `AccruedInterestV1`, `AccruedInterestRuleVersion` |
| `crates/iaam-core/src/rules/mod.rs` | регистрация правила в `RuleRegistry` |
| `crates/iaam-core/src/returns/mod.rs` | `BondPositionAttributes`, поле отчёта, `AppliedRules.accrued_interest_rule`, новые `NotComputable`, новый `MaterialIssue` |
| `crates/iaam-market/src/observation.rs` | `AccruedInterestObservation` |
| `crates/iaam-market/src/moex/parse.rs` | `parse_accrued_interest` |
| `crates/iaam-store/migrations/0011_accrued_interest.sql` (создать) | таблица наблюдений НКД |
| `crates/iaam-store/src/market.rs` | `AccruedInterestRow`, `record_accrued_interest`, `accrued_interest_at_or_before` |
| `crates/iaam-app/src/market_candidate.rs` | переводы `StoredSnapshot` → `AccrualPeriod`/`PrincipalReturn`, наблюдение → ядро |
| `crates/iaam-app/src/scenarios/sync.rs` | запись наблюдений НКД вместе с ценами |
| `crates/iaam-app/src/scenarios/reports.rs` | чтение графика и наблюдений, прокладка в отчёт |
| `crates/iaam-server/src/dto.rs` | DTO трёх величин |

---

### Задача 1: округление до знака в `Dec`

**Файлы:**
- Изменить: `crates/iaam-core/src/numeric/decimal.rs`

**Интерфейсы:**
- Отдаёт: `Dec::checked_round_to_scale(self, scale: u32) -> Result<Dec, NumericError>`

**Приёмка:**
- Округление до копейки даёт 0.71 из 0.70571 и 18.00 из 17.99571.
- Запрошенный знак больше `Dec::max_scale()` — отказ, а не молчаливое усечение.

- [ ] **Шаг 1: написать падающий тест**

В конец блока `mod tests` файла `crates/iaam-core/src/numeric/decimal.rs`:

```rust
    #[test]
    fn rounding_to_a_scale_matches_the_kopeck_of_the_source() {
        // Числа взяты из живой сверки с MOEX: линейный расчёт даёт
        // 0.70571 и 17.99571, источник печатает 0.71 и 18.00.
        let value = Dec::new(Decimal::from_str_exact("0.70571").unwrap());
        assert_eq!(
            value.checked_round_to_scale(2).unwrap(),
            Dec::new(Decimal::from_str_exact("0.71").unwrap())
        );
        let value = Dec::new(Decimal::from_str_exact("17.99571").unwrap());
        assert_eq!(
            value.checked_round_to_scale(2).unwrap(),
            Dec::new(Decimal::from_str_exact("18.00").unwrap())
        );
    }

    #[test]
    fn a_scale_beyond_the_limit_is_refused_not_truncated() {
        // Молчаливое усечение до max_scale дало бы число, о котором
        // вызывающий думает, что оно точнее, чем есть.
        let value = Dec::new(Decimal::from_str_exact("1.5").unwrap());
        assert!(value.checked_round_to_scale(Dec::max_scale() + 1).is_err());
    }
```

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-core --lib numeric::decimal 2>&1 | tail -20`
Expected: FAIL — `no method named checked_round_to_scale`.

- [ ] **Шаг 3: реализовать**

В `impl Dec` файла `crates/iaam-core/src/numeric/decimal.rs`:

```rust
    /// Округление до знака после запятой, половина — от нуля.
    ///
    /// Отдельный метод, а не `Decimal::round_dp` на месте: правило НКД
    /// округляет до минорной единицы валюты, и стратегия округления —
    /// часть версионированного правила, а не вкус вызывающего.
    pub fn checked_round_to_scale(self, scale: u32) -> Result<Self, NumericError> {
        if scale > Self::max_scale() {
            return Err(NumericError::ScaleTooLarge { scale });
        }
        Ok(Self::new(self.0.round_dp_with_strategy(
            scale,
            rust_decimal::RoundingStrategy::MidpointAwayFromZero,
        )))
    }
```

Если варианта `NumericError::ScaleTooLarge` нет — добавить в
`crates/iaam-core/src/numeric/mod.rs`:

```rust
    #[error("запрошен знак {scale}, больше предельного")]
    ScaleTooLarge { scale: u32 },
```

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-core --lib numeric::decimal`
Expected: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/numeric/decimal.rs crates/iaam-core/src/numeric/mod.rs
git commit -m "feat(core): округление Dec до знака как отдельная операция (iaam-d8b)"
```

---

### Задача 2: наблюдение НКД в `iaam-market`

**Файлы:**
- Изменить: `crates/iaam-market/src/observation.rs`
- Изменить: `crates/iaam-market/src/lib.rs` (реэкспорт)

**Интерфейсы:**
- Отдаёт: `AccruedInterestObservation { instrument, venue, trade_date, observed_at, per_unit: PerUnitAmount }`

**Приёмка:**
- Величина хранится в `PerUnitAmount`, то есть перепутать её с суммой сделки нельзя.
- Валюта живёт внутри `PerUnitAmount` и приходит из наблюдения, а не из номинала.

- [ ] **Шаг 1: написать падающий тест**

В `mod tests` файла `crates/iaam-market/src/observation.rs`:

```rust
    #[test]
    fn accrued_interest_is_measured_per_bond_not_per_trade() {
        // Trade.accrued_interest — сумма ВСЕЙ сделки (event/mod.rs,
        // trade_settlement складывает её с gross целиком). Наблюдение —
        // величина на одну бумагу. Тип обязан делать подмену
        // непредставимой: голый Dec её не остановит.
        let observation = AccruedInterestObservation {
            instrument: InstrumentId::new_random(),
            venue: Venue { board: "TQOB".to_owned(), session: 3 },
            trade_date: TradeDate(date!(2026 - 08 - 20)),
            observed_at: ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
            per_unit: PerUnitAmount::new(
                Dec::new(Decimal::from_str_exact("15.17").unwrap()),
                CurrencyCode::Rub,
            ),
        };
        assert_eq!(observation.per_unit.currency(), CurrencyCode::Rub);
    }
```

Импорты блока тестов дополнить: `use iaam_core::money::PerUnitAmount;`,
`use iaam_core::ids::InstrumentId;`, `use rust_decimal::Decimal;`.

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-market --lib observation 2>&1 | tail -20`
Expected: FAIL — `cannot find struct AccruedInterestObservation`.

- [ ] **Шаг 3: реализовать**

В `crates/iaam-market/src/observation.rs` после `PriceObservation`:

```rust
/// Наблюдение накопленного купонного дохода.
///
/// Отдельный тип, а не поле в [`PriceObservation`], по трём причинам.
/// Во-первых, после `iaam-a75` котировка облигации — процент номинала,
/// а НКД — деньги: одна структура на две размерности возвращает ровно
/// ту ошибку, которую `iaam-a75` чинил. Во-вторых, исполнимости у НКД
/// нет: это не цена, по которой кто-то торгует. В-третьих, у акции такое
/// поле было бы вечно пустым.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccruedInterestObservation {
    pub instrument: InstrumentId,
    pub venue: Venue,
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    /// На ОДНУ бумагу, вместе с валютой из `FACEUNIT`.
    pub per_unit: PerUnitAmount,
}
```

Дополнить импорты файла: `use iaam_core::money::PerUnitAmount;`.
В `crates/iaam-market/src/lib.rs` добавить `AccruedInterestObservation`
в список реэкспорта рядом с `PriceObservation`.

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-market --lib observation`
Expected: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-market/src/observation.rs crates/iaam-market/src/lib.rs
git commit -m "feat(market): наблюдение НКД отдельным типом от наблюдения цены (iaam-d8b)"
```

---

### Задача 3: разбор `ACCINT` из ответа истории

**Файлы:**
- Изменить: `crates/iaam-market/src/moex/parse.rs`

**Интерфейсы:**
- Потребляет: `AccruedInterestObservation` (Задача 2), `currency_of` (уже есть)
- Отдаёт: `parse_accrued_interest(body: &str, instrument: InstrumentId, observed_at: ObservedAt) -> Result<Vec<AccruedInterestObservation>, MarketError>`

**Приёмка:**
- Валюта берётся из `FACEUNIT`, а не из `CURRENCYID`: в одной строке они
  различаются (`RUB` против `SUR`).
- Строка без `ACCINT` или с `null` наблюдения не порождает — не ноль.
- Ответ без колонки `ACCINT` (акции) даёт пустой список, а не отказ.

- [ ] **Шаг 1: написать падающий тест**

В `mod tests` файла `crates/iaam-market/src/moex/parse.rs`:

```rust
    const BOND_HISTORY: &str = r#"{"history":{
        "columns":["BOARDID","TRADEDATE","SECID","CLOSE","ACCINT","CURRENCYID","FACEUNIT","TRADINGSESSION"],
        "data":[
            ["TQOB","2026-08-20","SU26238RMFS4",53.198,15.17,"SUR","RUB",3],
            ["TQOB","2026-08-21","SU26238RMFS4",53.355,null,"SUR","RUB",3]
        ]}}"#;

    #[test]
    fn accrued_interest_takes_its_currency_from_face_unit_not_from_currency_id() {
        // В одной строке источник называет валюту дважды и по-разному:
        // CURRENCYID=SUR и FACEUNIT=RUB. НКД выражен в валюте номинала.
        let observations = parse_accrued_interest(
            BOND_HISTORY,
            InstrumentId::new_random(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .unwrap();
        assert_eq!(observations.len(), 1, "строка с null наблюдения не даёт");
        assert_eq!(observations[0].per_unit.currency(), CurrencyCode::Rub);
        assert_eq!(
            observations[0].per_unit.value(),
            Dec::new(Decimal::from_str_exact("15.17").unwrap())
        );
    }

    #[test]
    fn a_response_without_the_column_yields_nothing_rather_than_failing() {
        // Ответ по акции колонки ACCINT не содержит вовсе. Отказ здесь
        // сломал бы синхронизацию всех необлигаций.
        let body = r#"{"history":{"columns":["BOARDID","TRADEDATE","CLOSE","CURRENCYID","TRADINGSESSION"],
            "data":[["TQBR","2026-08-20",300.5,"SUR",3]]}}"#;
        let observations = parse_accrued_interest(
            body,
            InstrumentId::new_random(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .unwrap();
        assert!(observations.is_empty());
    }
```

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-market --lib moex::parse 2>&1 | tail -20`
Expected: FAIL — `cannot find function parse_accrued_interest`.

- [ ] **Шаг 3: реализовать**

В `crates/iaam-market/src/moex/parse.rs`:

```rust
/// Разбор наблюдений НКД из той же страницы истории.
///
/// Отдельная функция, а не ветка внутри `parse_history`: величины разной
/// размерности (процент номинала против денег) и разной судьбы —
/// смешивать их в одном цикле значит однажды записать одну вместо другой.
pub fn parse_accrued_interest(
    body: &str,
    instrument: InstrumentId,
    observed_at: ObservedAt,
) -> Result<Vec<AccruedInterestObservation>, MarketError> {
    let root: Value =
        serde_json::from_str(body).map_err(|error| MarketError::Malformed(error.to_string()))?;
    let block = root
        .get("history")
        .ok_or_else(|| MarketError::Malformed("нет блока history".to_owned()))?;
    let names = column_names(block)?;
    // Колонки нет вовсе — это не облигационный сегмент, а не поломка.
    if index_of(&names, "ACCINT").is_none() {
        return Ok(Vec::new());
    }
    let rows = block
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет history.data".to_owned()))?;

    let mut observations = Vec::new();
    for row in rows {
        let row = row
            .as_array()
            .ok_or_else(|| MarketError::Malformed("строка history.data не массив".to_owned()))?;
        let get = |name: &str| index_of(&names, name).and_then(|i| row.get(i));
        // Пустое значение наблюдения не порождает: ноль НКД означал бы
        // начало купонного периода, а не отсутствие торгов.
        let Some(value) = get("ACCINT").and_then(Value::as_number) else {
            continue;
        };
        let amount = value
            .to_string()
            .parse::<Decimal>()
            .map_err(|error| MarketError::Malformed(error.to_string()))?;
        // Валюта НКД — валюта номинала (FACEUNIT), а не валюта расчётов
        // площадки (CURRENCYID). В одной строке они различаются.
        let currency = currency_of(
            get("FACEUNIT")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("строка с ACCINT без FACEUNIT".to_owned()))?,
        )?;
        let trade_date = TradeDate(parse_date(
            get("TRADEDATE")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("строка без TRADEDATE".to_owned()))?,
        )?);
        observations.push(AccruedInterestObservation {
            instrument,
            venue: Venue {
                board: get("BOARDID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| MarketError::Malformed("строка без BOARDID".to_owned()))?
                    .to_owned(),
                session: get("TRADINGSESSION").and_then(Value::as_i64).unwrap_or(0),
            },
            trade_date,
            observed_at,
            per_unit: PerUnitAmount::new(Dec::new(amount), currency),
        });
    }
    Ok(observations)
}
```

Дополнить импорты файла: `use iaam_core::money::PerUnitAmount;` и
`AccruedInterestObservation` в списке из `crate::observation`.

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-market --lib moex::parse`
Expected: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-market/src/moex/parse.rs
git commit -m "feat(market): разбор ACCINT из страницы истории, валюта из FACEUNIT (iaam-d8b)"
```

---

### Задача 4: хранение наблюдений НКД

**Файлы:**
- Создать: `crates/iaam-store/migrations/0011_accrued_interest.sql`
- Изменить: `crates/iaam-store/src/schema.rs`
- Изменить: `crates/iaam-store/src/market.rs`

**Интерфейсы:**
- Отдаёт: `AccruedInterestRow`, `SqliteStore::record_accrued_interest(&mut self, run: &RunHandle, raw_hash: &str, rows: &[AccruedInterestRow]) -> Result<usize, StoreError>`
- Отдаёт: `SqliteStore::accrued_interest_at_or_before(&self, instrument_id: &str, venue: &PriceVenue, as_of: &str, knowledge_as_of: &str) -> Result<Option<AccruedInterestRow>, StoreError>`

**Приёмка:**
- Чтение на координату знания не видит наблюдения, записанные позже координаты.
- Чтение не видит строк незавершённого запуска.

- [ ] **Шаг 1: написать падающий тест**

В `crates/iaam-store/tests/market_observations.rs` (файл существует; если
имя иное — положить рядом с существующими тестами наблюдений):

```rust
#[test]
fn accrued_interest_is_invisible_before_its_knowledge_coordinate() {
    // Наблюдение, записанное позже координаты, обязано быть невидимым:
    // иначе отчёт «на вчера» пересчитается от завтрашнего знания.
    let mut store = fresh_store();
    let run = begin_price_run(&mut store);
    store
        .record_accrued_interest(
            &run,
            "hash",
            &[AccruedInterestRow {
                instrument_id: INSTRUMENT.to_owned(),
                board: "TQOB".to_owned(),
                session: 3,
                trade_date: "2026-08-20".to_owned(),
                observed_at: "2026-08-27T12:00:00Z".to_owned(),
                per_unit: "15.17".to_owned(),
                currency: "RUB".to_owned(),
            }],
        )
        .unwrap();
    finish_ok(&mut store, &run);

    let venue = PriceVenue { board: "TQOB".to_owned(), session: 3 };
    assert!(
        store
            .accrued_interest_at_or_before(INSTRUMENT, &venue, "2026-08-20", "2026-08-26T00:00:00Z")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .accrued_interest_at_or_before(INSTRUMENT, &venue, "2026-08-20", "2026-08-27T12:00:00Z")
            .unwrap()
            .map(|row| row.per_unit),
        Some("15.17".to_owned())
    );
}
```

Вспомогательные `fresh_store`, `begin_price_run`, `finish_ok`, `INSTRUMENT`
переиспользовать из уже существующих тестов файла; если их нет — написать
по образцу соседнего теста цен в том же файле.

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-store --test market_observations 2>&1 | tail -20`
Expected: FAIL — `no method named record_accrued_interest`.

- [ ] **Шаг 3: реализовать**

`crates/iaam-store/migrations/0011_accrued_interest.sql`:

```sql
-- Наблюдения накопленного купонного дохода.
--
-- Отдельная таблица от price_observations: величины разной размерности
-- (процент номинала против денег), и общая таблица заставила бы каждую
-- выборку цен фильтровать по виду строки.
--
-- Расчётный НКД сюда не пишется НИКОГДА: он вывод, а не наблюдение
-- (ADR-0002).
CREATE TABLE accrued_interest_observations (
    id             INTEGER PRIMARY KEY,
    instrument_id  TEXT NOT NULL,
    board          TEXT NOT NULL,
    session        INTEGER NOT NULL,
    trade_date     TEXT NOT NULL,
    source_id      TEXT NOT NULL,
    observed_at    TEXT NOT NULL,
    per_unit       TEXT NOT NULL,
    currency       TEXT NOT NULL,
    raw_hash       TEXT NOT NULL,
    sync_run_id    TEXT NOT NULL REFERENCES sync_runs(id)
) STRICT;

CREATE INDEX accrued_interest_observations_lookup
    ON accrued_interest_observations (instrument_id, board, session, trade_date, observed_at);
```

В `crates/iaam-store/src/schema.rs` добавить в `MIGRATIONS`:

```rust
    (
        11,
        include_str!("../migrations/0011_accrued_interest.sql"),
    ),
```

и поднять `SCHEMA_VERSION` до `11`.

В `crates/iaam-store/src/market.rs` рядом с `PriceRow`:

```rust
/// Строка наблюдения НКД. Значения строками, как и везде в хранилище.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccruedInterestRow {
    pub instrument_id: String,
    pub board: String,
    pub session: i64,
    pub trade_date: String,
    pub observed_at: String,
    /// На одну бумагу.
    pub per_unit: String,
    pub currency: String,
}
```

и два метода в `impl SqliteStore` по образцу `record_prices` /
`prices_at_or_before`: `record_accrued_interest` вставляет строки в
транзакции с `ensure_run` и увеличивает счётчики запуска;
`accrued_interest_at_or_before` выбирает последнюю строку с
`trade_date <= ?`, `observed_at <= ?` и `r.status = 'succeeded'`,
сортировка `ORDER BY observed_at DESC, trade_date DESC LIMIT 1`.

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-store`
Expected: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-store/migrations/0011_accrued_interest.sql \
        crates/iaam-store/src/schema.rs crates/iaam-store/src/market.rs \
        crates/iaam-store/tests/market_observations.rs
git commit -m "feat(store): наблюдения НКД, миграция 0011 (iaam-d8b)"
```

---

### Задача 5: синхронизация пишет НКД вместе с ценой

**Файлы:**
- Изменить: `crates/iaam-app/src/scenarios/sync.rs`

**Интерфейсы:**
- Потребляет: `parse_accrued_interest` (Задача 3), `record_accrued_interest` (Задача 4)
- Отдаёт: `ParsedObservations::Prices { prices: Vec<PriceObservation>, accrued: Vec<AccruedInterestObservation> }`

**Приёмка:**
- Один ответ MOEX по облигации даёт и цены, и наблюдения НКД, второго запроса нет.
- Ответ по акции даёт цены и пустой список НКД, запуск успешен.

- [ ] **Шаг 1: написать падающий тест**

В `mod tests` файла `crates/iaam-app/src/scenarios/sync.rs`:

```rust
    #[test]
    fn one_bond_response_yields_both_prices_and_accrued_interest() {
        // ACCINT приходит в той же строке, что и CLOSE. Второй запрос
        // за ним был бы лишним обращением к источнику и второй
        // координатой знания на одну и ту же строку.
        let body = std::fs::read(
            "../../tests/fixtures/market/moex-iss-history-ofz.json",
        )
        .unwrap();
        let parsed = parse_response(
            &MarketSource::Moex {
                instrument: iaam_core::ids::InstrumentId::new_random(),
                engine: "stock".to_owned(),
                market: "bonds".to_owned(),
                board: "TQOB".to_owned(),
            },
            &body,
            iaam_market::ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .unwrap();
        let ParsedObservations::Prices { prices, accrued } = parsed else {
            panic!("облигационный ответ обязан дать оба вида наблюдений");
        };
        assert!(!prices.is_empty());
        assert!(!accrued.is_empty());
    }
```

Поля варианта `MarketSource::Moex` сверить с текущим определением в этом же
файле и подставить фактические.

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-app --lib scenarios::sync 2>&1 | tail -20`
Expected: FAIL — вариант `Prices` не имеет поля `accrued`; фикстуры нет.

Фикстура появляется в Задаче 13; до неё тест держать помеченным
`#[ignore = "фикстура появляется в задаче 13"]` и снять метку в Задаче 13.

- [ ] **Шаг 3: реализовать**

В `crates/iaam-app/src/scenarios/sync.rs` заменить вариант:

```rust
    Prices {
        prices: Vec<PriceObservation>,
        accrued: Vec<AccruedInterestObservation>,
    },
```

В `parse_response` для `MarketSource::Moex`:

```rust
            let prices = iaam_market::moex::parse::parse_history(
                body,
                *instrument,
                observed_at,
                iaam_market::moex::parse::MarketSegment { engine, market },
            )
            .map_err(|error| AppError::Store(error.to_string()))?;
            // Тот же ответ, та же координата знания: НКД лежит в той же
            // строке, что и цена, и второго обращения не требует.
            let accrued =
                iaam_market::moex::parse::parse_accrued_interest(body, *instrument, observed_at)
                    .map_err(|error| AppError::Store(error.to_string()))?;
            Ok(ParsedObservations::Prices { prices, accrued })
```

В ветке записи:

```rust
        ParsedObservations::Prices { prices, accrued } => {
            let rows = prices.iter().map(price_row).collect::<Vec<_>>();
            let written = match store.record_prices(&handle, &response.raw_hash, &rows) {
                Ok(count) => count,
                Err(error) => return Err(fail_run(store, &handle, error)),
            };
            let accrued_rows = accrued.iter().map(accrued_interest_row).collect::<Vec<_>>();
            match store.record_accrued_interest(&handle, &response.raw_hash, &accrued_rows) {
                Ok(count) => written + count,
                Err(error) => return Err(fail_run(store, &handle, error)),
            }
        }
```

и функция перевода рядом с `price_row`:

```rust
fn accrued_interest_row(observation: &AccruedInterestObservation) -> AccruedInterestRow {
    AccruedInterestRow {
        instrument_id: observation.instrument.inner().to_string(),
        board: observation.venue.board.clone(),
        session: observation.venue.session,
        trade_date: observation.trade_date.0.to_string(),
        observed_at: observation
            .observed_at
            .0
            .format(&Rfc3339)
            .unwrap_or_default(),
        per_unit: observation.per_unit.value().inner().to_string(),
        currency: observation.per_unit.currency().code().to_owned(),
    }
}
```

Способ получения кода валюты сверить с `price_row` в этом же файле и
повторить его — второго способа заводить не нужно.

- [ ] **Шаг 4: сборка зелёная**

Run: `cargo build --workspace && cargo test -p iaam-app --lib`
Expected: PASS (тест задачи 5 пока `#[ignore]`).

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-app/src/scenarios/sync.rs
git commit -m "feat(app): синхронизация пишет наблюдения НКД вместе с ценами (iaam-d8b)"
```

---

### Задача 6: доменные типы графика в ядре

**Файлы:**
- Создать: `crates/iaam-core/src/bond/mod.rs`
- Изменить: `crates/iaam-core/src/lib.rs` (`pub mod bond;`)

**Интерфейсы:**
- Отдаёт: `AccrualPeriod { period_start: Date, accrual_end: Date, payment_date: Date, coupon_per_unit: Option<PerUnitAmount> }`
- Отдаёт: `PrincipalReturn { repayment_date: Date, share_percent: Dec }`

**Приёмка:**
- Тип периода несёт `accrual_end` и `payment_date` раздельно.
- Неопределённая сумма купона представлена `None`, а не нулём.

- [ ] **Шаг 1: написать падающий тест**

`crates/iaam-core/src/bond/mod.rs`, блок `mod tests`:

```rust
    #[test]
    fn an_accrual_period_keeps_accrual_end_and_payment_date_apart() {
        // НКД считается по accrual_end, ближайшая выплата — по
        // payment_date. Перенос с выходного двигает второе, но не первое.
        let period = AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 03),
            coupon_per_unit: None,
        };
        assert_ne!(period.accrual_end, period.payment_date);
    }

    #[test]
    fn an_undetermined_coupon_is_absent_not_zero() {
        // Ноль купона означал бы бумагу, которая ничего не платит,
        // и занизил бы и НКД, и все метрики §7.1.
        let period = AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 02),
            coupon_per_unit: None,
        };
        assert!(period.coupon_per_unit.is_none());
    }
```

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-core --lib bond 2>&1 | tail -20`
Expected: FAIL — модуля `bond` нет.

- [ ] **Шаг 3: реализовать**

`crates/iaam-core/src/bond/mod.rs`:

```rust
//! Доменные типы графика выплат, нужные расчёту (§7 плана E3.4.4).
//!
//! Это НЕ зеркало `iaam_market::schedule`. Ядро не зависит от крейт
//! воркспейса (§3.2), а правило НКД — политика и обязано жить здесь,
//! рядом с `ValuationPolicyV1`. Перевод снимка источника в эти типы
//! делает `iaam-app` и делает **структурно**: любое условие в нём —
//! признак, что правило утекло из ядра.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::money::PerUnitAmount;
use crate::numeric::decimal::Dec;

/// Купонный период: начисление и платёж — разные даты.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccrualPeriod {
    pub period_start: Date,
    /// Конец начисления. По нему считается НКД.
    pub accrual_end: Date,
    /// Дата платежа. Двигается переносом с выходного.
    pub payment_date: Date,
    /// Сумма купона за период на одну бумагу.
    ///
    /// `None` — сумма не определена (флоатер, будущий период). Ноль
    /// означал бы бумагу, которая ничего не платит.
    pub coupon_per_unit: Option<PerUnitAmount>,
}

/// Возврат части номинала.
///
/// Доля, а не сумма: сумма зависит от остатка, а остаток выводится
/// из первоначального номинала и ряда возвратов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalReturn {
    pub repayment_date: Date,
    /// Доля ПЕРВОНАЧАЛЬНОГО номинала, в процентах.
    pub share_percent: Dec,
}
```

В `crates/iaam-core/src/lib.rs` добавить `pub mod bond;`.

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-core --lib bond && ./scripts/check-architecture.sh`
Expected: PASS, заслон молчит.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/bond/mod.rs crates/iaam-core/src/lib.rs
git commit -m "feat(core): доменные типы купонного периода и возврата номинала (iaam-d8b)"
```

---

### Задача 7: правило НКД `AccruedInterestV1`

**Файлы:**
- Создать: `crates/iaam-core/src/rules/accrued_interest.rs`
- Изменить: `crates/iaam-core/src/rules/mod.rs`

**Интерфейсы:**
- Потребляет: `AccrualPeriod` (Задача 6), `Dec::checked_round_to_scale` (Задача 1)
- Отдаёт: `AccruedInterestRuleVersion(pub u32)`
- Отдаёт: `trait AccruedInterestRule { fn accrued_per_unit(&self, periods: &[AccrualPeriod], as_of: Date) -> Result<PerUnitAmount, AccruedInterestError>; }`
- Отдаёт: `AccruedInterestV1`
- Отдаёт: `enum AccruedInterestError { OutsideCoverage, CouponUndetermined, Numeric(NumericError) }`
- Отдаёт: `RuleRegistry::accrued_interest_rule(&self, version: AccruedInterestRuleVersion) -> Option<&dyn AccruedInterestRule>`

**Приёмка:**
- На подтверждённых живьём точках правило воспроизводит `ACCINT` MOEX до копейки.
- На `accrual_end` НКД равен нулю следующего периода, а не целому купону.
- `as_of` вне покрытия графика — отказ, а не ноль.
- Неопределённая сумма купона — отказ, а не ноль.

- [ ] **Шаг 1: написать падающий тест**

`crates/iaam-core/src/rules/accrued_interest.rs`, блок `mod tests`:

```rust
    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    /// Купонный период ОФЗ SU26238RMFS4, проверенный живьём 2026-08-27:
    /// 2026-06-03 → 2026-12-02, купон 35.40 ₽ на бумагу.
    fn ofz_periods() -> Vec<AccrualPeriod> {
        vec![
            AccrualPeriod {
                period_start: date!(2026 - 06 - 03),
                accrual_end: date!(2026 - 12 - 02),
                payment_date: date!(2026 - 12 - 02),
                coupon_per_unit: Some(PerUnitAmount::new(dec("35.40"), CurrencyCode::Rub)),
            },
            AccrualPeriod {
                period_start: date!(2026 - 12 - 02),
                accrual_end: date!(2027 - 06 - 02),
                payment_date: date!(2027 - 06 - 02),
                coupon_per_unit: Some(PerUnitAmount::new(dec("35.40"), CurrencyCode::Rub)),
            },
        ]
    }

    #[test]
    fn the_rule_reproduces_the_kopeck_the_exchange_published() {
        // Три точки сняты живым вызовом ISS: 15.17, 15.37 и 15.95.
        // Это эталон против конкретного источника, а не абстрактное
        // свойство: если правило разъедется с биржей, разъедется тут.
        let rule = AccruedInterestV1;
        let periods = ofz_periods();
        for (day, expected) in [
            (date!(2026 - 08 - 20), "15.17"),
            (date!(2026 - 08 - 21), "15.37"),
            (date!(2026 - 08 - 24), "15.95"),
        ] {
            assert_eq!(
                rule.accrued_per_unit(&periods, day).unwrap().value(),
                dec(expected),
                "расхождение на {day}"
            );
        }
    }

    #[test]
    fn on_the_accrual_end_the_next_period_starts_at_zero() {
        // Главная ловушка полуоткрытой границы: на accrual_end купон
        // уже начислен целиком и относится к ПРОШЕДШЕМУ периоду.
        // Включительная граница показала бы целый купон вместо нуля.
        let rule = AccruedInterestV1;
        assert_eq!(
            rule.accrued_per_unit(&ofz_periods(), date!(2026 - 12 - 02))
                .unwrap()
                .value(),
            Dec::zero()
        );
    }

    #[test]
    fn a_date_outside_the_schedule_is_refused_not_zeroed() {
        // Ноль здесь неотличим от незнания и молча занизил бы NAV.
        let rule = AccruedInterestV1;
        assert!(matches!(
            rule.accrued_per_unit(&ofz_periods(), date!(2026 - 01 - 01)),
            Err(AccruedInterestError::OutsideCoverage)
        ));
    }

    #[test]
    fn an_undetermined_coupon_is_refused_not_zeroed() {
        // Флоатер с неназванной суммой: правильный ответ — «не знаем».
        let rule = AccruedInterestV1;
        let periods = vec![AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 02),
            coupon_per_unit: None,
        }];
        assert!(matches!(
            rule.accrued_per_unit(&periods, date!(2026 - 08 - 20)),
            Err(AccruedInterestError::CouponUndetermined)
        ));
    }
```

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-core --lib rules::accrued_interest 2>&1 | tail -20`
Expected: FAIL — модуля нет.

- [ ] **Шаг 3: реализовать**

`crates/iaam-core/src/rules/accrued_interest.rs`:

```rust
//! Накопленный купонный доход (§3.2 спеки E3.4.4).
//!
//! Версионированное правило, а не арифметика на месте: включительность
//! границы периода и стратегия округления меняют сумму при одинаковом
//! `inputs_hash` (§2.7 основной спеки E3.4).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::bond::AccrualPeriod;
use crate::money::PerUnitAmount;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Версия правила НКД.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AccruedInterestRuleVersion(pub u32);

/// Причина, по которой НКД не считается.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AccruedInterestError {
    #[error("дата вне покрытия графика")]
    OutsideCoverage,
    #[error("сумма купона периода не определена")]
    CouponUndetermined,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Начисленный на дату купонный доход на одну бумагу.
pub trait AccruedInterestRule: Send + Sync + std::fmt::Debug {
    fn accrued_per_unit(
        &self,
        periods: &[AccrualPeriod],
        as_of: Date,
    ) -> Result<PerUnitAmount, AccruedInterestError>;
}

/// Линейное начисление внутри периода.
///
/// Базы начисления дней правило НЕ требует: доля периода
/// самонормируется. Это существенно — MOEX базы не даёт вовсе (§2.11
/// основной спеки), а подставленная база даёт правдоподобно неверный НКД.
///
/// Эквивалентность ACT/365 проверена живьём на 6814 наблюдениях по пяти
/// бумагам, включая нерегулярный период в 175 дней: ноль расхождений.
#[derive(Debug, Default)]
pub struct AccruedInterestV1;

impl AccruedInterestRule for AccruedInterestV1 {
    fn accrued_per_unit(
        &self,
        periods: &[AccrualPeriod],
        as_of: Date,
    ) -> Result<PerUnitAmount, AccruedInterestError> {
        // Граница полуоткрыта: [period_start, accrual_end). На accrual_end
        // купон начислен целиком и принадлежит прошедшему периоду, а
        // следующий период стартует с нуля — инвариант замкнутой цепи
        // (completeness.rs) это гарантирует.
        let period = periods
            .iter()
            .find(|period| period.period_start <= as_of && as_of < period.accrual_end)
            .ok_or(AccruedInterestError::OutsideCoverage)?;
        let coupon = period
            .coupon_per_unit
            .as_ref()
            .ok_or(AccruedInterestError::CouponUndetermined)?;

        let elapsed = (as_of - period.period_start).whole_days();
        let whole = (period.accrual_end - period.period_start).whole_days();
        // Период нулевой длины разделить нельзя; график с таким периодом
        // структурно неверен, и молчаливый ноль его бы спрятал.
        if whole <= 0 {
            return Err(AccruedInterestError::OutsideCoverage);
        }
        let fraction =
            Dec::new(Decimal::from(elapsed)).checked_div(Dec::new(Decimal::from(whole)))?;
        let accrued = coupon.value().checked_mul(fraction)?;
        let rounded = accrued.checked_round_to_scale(coupon.currency().minor_units())?;
        Ok(PerUnitAmount::new(rounded, coupon.currency()))
    }
}
```

В `crates/iaam-core/src/rules/mod.rs`: `mod accrued_interest;`, реэкспорт
`pub use accrued_interest::{AccruedInterestError, AccruedInterestRule, AccruedInterestRuleVersion, AccruedInterestV1};`,
поле `accrued_interest_rules: BTreeMap<AccruedInterestRuleVersion, Box<dyn AccruedInterestRule>>`
с засевом `AccruedInterestRuleVersion(1) -> AccruedInterestV1` и геттер
`accrued_interest_rule` — всё по образцу `quotation_rules` в этом же файле.

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-core --lib rules::accrued_interest`
Expected: PASS, четыре теста.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/rules/accrued_interest.rs crates/iaam-core/src/rules/mod.rs
git commit -m "feat(core): правило НКД — линейно по периоду, без базы начисления дней (iaam-d8b)"
```

---

### Задача 8: признак окончательности возврата номинала

**Файлы:**
- Создать: `crates/iaam-core/src/bond/finality.rs`
- Изменить: `crates/iaam-core/src/bond/mod.rs` (`pub mod finality;`)

**Закрывает бид:** `iaam-d8b.4.3`

**Интерфейсы:**
- Потребляет: `PrincipalReturn` (Задача 6)
- Отдаёт: `enum PrincipalReturnFinality { Final, Partial, Unknown }`
- Отдаёт: `fn finality_of(returns: &[PrincipalReturn]) -> Result<Vec<(PrincipalReturn, PrincipalReturnFinality)>, NumericError>`

**Приёмка:**
- Признак выводится из накопленной суммы долей, а не из кода источника.
- Бумага с шестью амортизациями без кода `maturity` получает окончательный
  последний возврат.
- Доли, не дающие 100 %, не делают окончательным ни один возврат.

- [ ] **Шаг 1: написать падающий тест**

`crates/iaam-core/src/bond/finality.rs`, блок `mod tests`:

```rust
    fn ret(day: Date, share: &str) -> PrincipalReturn {
        PrincipalReturn {
            repayment_date: day,
            share_percent: Dec::new(Decimal::from_str_exact(share).unwrap()),
        }
    }

    #[test]
    fn six_amortisations_without_a_maturity_code_still_end_finally() {
        // У шести бумаг из пятидесяти проверенных последний возврат
        // приходит обычной строкой амортизации, без кода погашения.
        // Читать код источника значит потерять окончательность у них.
        let returns = vec![
            ret(date!(2027 - 01 - 15), "10"),
            ret(date!(2028 - 01 - 15), "10"),
            ret(date!(2029 - 01 - 15), "10"),
            ret(date!(2030 - 01 - 15), "20"),
            ret(date!(2031 - 01 - 15), "20"),
            ret(date!(2032 - 01 - 15), "30"),
        ];
        let marked = finality_of(&returns).unwrap();
        assert_eq!(marked[5].1, PrincipalReturnFinality::Final);
        assert_eq!(marked[4].1, PrincipalReturnFinality::Partial);
    }

    #[test]
    fn shares_short_of_a_hundred_make_nobody_final() {
        // Усечённая страница даёт правдоподобный, но неполный ряд.
        // Объявить последнюю строку окончательной значит закрыть
        // бумагу на десять лет раньше срока.
        let returns = vec![ret(date!(2027 - 01 - 15), "40"), ret(date!(2028 - 01 - 15), "35")];
        let marked = finality_of(&returns).unwrap();
        assert!(
            marked.iter().all(|(_, finality)| *finality == PrincipalReturnFinality::Unknown)
        );
    }

    #[test]
    fn returns_are_walked_in_date_order_not_in_source_order() {
        // Источник порядок строк не гарантирует, а накопление доли
        // от порядка зависит целиком.
        let returns = vec![ret(date!(2028 - 01 - 15), "60"), ret(date!(2027 - 01 - 15), "40")];
        let marked = finality_of(&returns).unwrap();
        let final_one = marked
            .iter()
            .find(|(_, finality)| *finality == PrincipalReturnFinality::Final)
            .unwrap();
        assert_eq!(final_one.0.repayment_date, date!(2028 - 01 - 15));
    }
```

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-core --lib bond::finality 2>&1 | tail -20`
Expected: FAIL — модуля нет.

- [ ] **Шаг 3: реализовать**

`crates/iaam-core/src/bond/finality.rs`:

```rust
//! Окончательность возврата номинала (§6 спеки E3.4.4, бид iaam-d8b.4.3).
//!
//! Правило одно: возврат окончателен, когда накопленная сумма долей
//! достигает 100 %. Код источника не читается — у шести бумаг из
//! пятидесяти проверенных строки погашения нет вовсе.
//!
//! Признак наблюдением не записывается: он свойство проекции (ADR-0002).
//! Инвариант полноты в `iaam_market::schedule::completeness` считает ту
//! же сумму, но принадлежит ПРОФИЛЮ ИСТОЧНИКА и отвечает на другой
//! вопрос — цела ли выгрузка.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::bond::PrincipalReturn;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Окончателен ли возврат номинала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrincipalReturnFinality {
    /// Накопленная доля достигла 100 %: номинал возвращён целиком.
    Final,
    /// Часть номинала, после которой останется непогашенный остаток.
    Partial,
    /// Доли не дают 100 %: сказать нечего ни про одну строку.
    Unknown,
}

/// Разметить ряд возвратов признаком окончательности.
pub fn finality_of(
    returns: &[PrincipalReturn],
) -> Result<Vec<(PrincipalReturn, PrincipalReturnFinality)>, NumericError> {
    let shares = returns.iter().map(|r| r.share_percent).collect::<Vec<_>>();
    let total = Dec::sum(&shares)?;
    let hundred = Dec::new(Decimal::ONE_HUNDRED);
    if total != hundred {
        return Ok(returns
            .iter()
            .map(|r| (*r, PrincipalReturnFinality::Unknown))
            .collect());
    }

    // Порядок источника не гарантирован, а накопление зависит от него
    // целиком: без сортировки окончательной окажется случайная строка.
    let mut ordered = returns.to_vec();
    ordered.sort_by_key(|r| r.repayment_date);

    let mut accumulated = Dec::zero();
    let mut marked = Vec::with_capacity(ordered.len());
    for item in ordered {
        accumulated = accumulated.checked_add(item.share_percent)?;
        let finality = if accumulated == hundred {
            PrincipalReturnFinality::Final
        } else {
            PrincipalReturnFinality::Partial
        };
        marked.push((item, finality));
    }
    Ok(marked)
}
```

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-core --lib bond::finality`
Expected: PASS, три теста.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/bond/finality.rs crates/iaam-core/src/bond/mod.rs
git commit -m "feat(core): окончательность возврата номинала выводится из долей (iaam-d8b.4.3)"
```

---

### Задача 9: ближайшая выплата

**Файлы:**
- Создать: `crates/iaam-core/src/bond/posting.rs`
- Изменить: `crates/iaam-core/src/bond/mod.rs` (`pub mod posting;`)

**Интерфейсы:**
- Потребляет: `AccrualPeriod`, `PrincipalReturn` (Задача 6)
- Отдаёт: `fn next_posting_date(periods: &[AccrualPeriod], returns: &[PrincipalReturn], settled_offers: &[Date], as_of: Date) -> Option<Date>`

**Приёмка:**
- Купон берётся по `payment_date`, а не по `accrual_end`.
- Возврат номинала участвует наравне с купоном.
- Дата расчёта по поданной оферте участвует наравне.
- Ни одной выплаты впереди — `None`.

- [ ] **Шаг 1: написать падающий тест**

`crates/iaam-core/src/bond/posting.rs`, блок `mod tests`:

```rust
    #[test]
    fn a_coupon_is_taken_by_its_payment_date_not_by_its_accrual_end() {
        // Перенос с выходного двигает платёж на 3 декабря, начисление
        // остаётся на 2-е. Взять accrual_end значит обещать деньги
        // на день раньше, чем они придут.
        let periods = vec![AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 03),
            coupon_per_unit: None,
        }];
        assert_eq!(
            next_posting_date(&periods, &[], &[], date!(2026 - 08 - 20)),
            Some(date!(2026 - 12 - 03))
        );
    }

    #[test]
    fn an_amortisation_competes_with_the_coupon_on_equal_terms() {
        // Выбор только из купонного графика был бы неполон: на
        // амортизируемой бумаге ближайшие деньги — возврат номинала.
        let periods = vec![AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 02),
            coupon_per_unit: None,
        }];
        let returns = vec![PrincipalReturn {
            repayment_date: date!(2026 - 09 - 15),
            share_percent: Dec::new(Decimal::from(25)),
        }];
        assert_eq!(
            next_posting_date(&periods, &returns, &[], date!(2026 - 08 - 20)),
            Some(date!(2026 - 09 - 15))
        );
    }

    #[test]
    fn a_submitted_offer_settlement_competes_too() {
        // Окно оферты из графика — право, а не платёж (E3.4.6).
        // Уже ПОДАННАЯ заявка — платёж, и она приходит из проекции.
        assert_eq!(
            next_posting_date(&[], &[], &[date!(2026 - 09 - 01)], date!(2026 - 08 - 20)),
            Some(date!(2026 - 09 - 01))
        );
    }

    #[test]
    fn nothing_ahead_is_none_not_a_far_future_guess() {
        assert_eq!(next_posting_date(&[], &[], &[], date!(2026 - 08 - 20)), None);
    }
```

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-core --lib bond::posting 2>&1 | tail -20`
Expected: FAIL — модуля нет.

- [ ] **Шаг 3: реализовать**

`crates/iaam-core/src/bond/posting.rs`:

```rust
//! Ближайшая выплата по бумаге (§5 спеки E3.4.4).

use time::Date;

use crate::bond::{AccrualPeriod, PrincipalReturn};

/// Дата ближайшей ЛЮБОЙ выплаты не раньше `as_of`.
///
/// Купон берётся по `payment_date`: перенос с выходного двигает платёж,
/// но не начисление, и `accrual_end` обещал бы деньги раньше срока.
///
/// Окно оферты из графика сюда НЕ входит — это право, а не платёж
/// (E3.4.6). Входит расчёт по уже поданной заявке: она приходит из
/// проекции заявок, а не из графика источника.
#[must_use]
pub fn next_posting_date(
    periods: &[AccrualPeriod],
    returns: &[PrincipalReturn],
    settled_offers: &[Date],
    as_of: Date,
) -> Option<Date> {
    periods
        .iter()
        .map(|period| period.payment_date)
        .chain(returns.iter().map(|item| item.repayment_date))
        .chain(settled_offers.iter().copied())
        .filter(|date| *date >= as_of)
        .min()
}
```

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-core --lib bond::posting`
Expected: PASS, четыре теста.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/bond/posting.rs crates/iaam-core/src/bond/mod.rs
git commit -m "feat(core): ближайшая выплата — купон, амортизация и поданная оферта (iaam-d8b)"
```

---

### Задача 10: три величины в отчёте и сверка с наблюдением

**Файлы:**
- Изменить: `crates/iaam-core/src/returns/mod.rs`

**Интерфейсы:**
- Потребляет: Задачи 6-9
- Отдаёт: `struct BondPositionAttributes { account, custody, instrument, accrued_interest: Computed<Dec>, accrued_interest_payable_on_termination: Computed<Dec>, next_posting_date: Option<Date>, next_principal_return_finality: Option<PrincipalReturnFinality> }`
- Потребляет: `finality_of` (Задача 8) — здесь у признака окончательности появляется потребитель
- Отдаёт: `ReturnsReport.bond_attributes: Vec<BondPositionAttributes>`
- Отдаёт: `AppliedRules.accrued_interest_rule: AccruedInterestRuleVersion`
- Отдаёт: `NotComputable::{ScheduleMissing, CouponUndetermined, OutsideScheduleCoverage, ExitNotExecutable}`
- Отдаёт: `MaterialIssue::AccruedInterestMismatch { instrument, computed: Dec, observed: Dec, currency: CurrencyCode, date: Date }`
- Отдаёт: `LiquidationEstimate.accrued_interest_payable_on_termination: Computed<Dec>`

**Приёмка:**
- Величина позиции — деньги: `per_unit × quantity`, умножение явное.
- Расхождение расчёта и наблюдения больше минорной единицы попадает в
  `data_quality` и называет обе величины.
- Расхождение внутри минорной единицы материальной проблемой не считается.
- `accrued_interest_payable_on_termination` без исполнимой цены — `NotComputable`,
  не ноль и не НКД.
- Неизвестная реализуемая сумма делает неполноценной **ликвидационную оценку**,
  а `terminal_value` не трогает (§4.2).
- Если ближайшая выплата — возврат номинала, отчёт говорит, окончателен ли он.

- [ ] **Шаг 1: написать падающий тест**

В `mod tests` файла `crates/iaam-core/src/returns/mod.rs`:

```rust
    #[test]
    fn a_kopeck_of_disagreement_with_the_exchange_is_rounding_not_an_issue() {
        // И источник, и мы округляем до копейки. Объявить копейку
        // проблемой значит утопить data_quality в шуме по каждой
        // облигации каждый день.
        assert!(!accrued_mismatch_is_material(
            dec("15.17"),
            dec("15.18"),
            CurrencyCode::Rub
        ));
    }

    #[test]
    fn a_real_disagreement_names_both_numbers() {
        // Молчаливое разрешение в чью-то пользу прячет ошибку правила.
        assert!(accrued_mismatch_is_material(
            dec("15.17"),
            dec("22.40"),
            CurrencyCode::Rub
        ));
    }

    #[test]
    fn termination_value_without_an_executable_exit_is_unknown_not_the_accrual() {
        // §5.3 запрещает считать ликвидность НКД гарантией: без
        // исполнимого выхода получить его сегодня нельзя.
        let value = payable_on_termination(
            &Computed::Value(dec("15.17")),
            SourceExecutability::IndicativePreviousClose,
        );
        assert!(matches!(
            value,
            Computed::NotComputable { reason: NotComputable::ExitNotExecutable }
        ));
    }

    #[test]
    fn the_last_amortisation_is_reported_as_the_final_one() {
        // Бид iaam-d8b.4.3: признак обязан ДОЙТИ до потребителя.
        // Выведенный, но никем не прочитанный признак — мёртвый код,
        // который нечем проверить на верность.
        let attributes = bond_attributes_for_a_bond_ending_next_month();
        assert_eq!(
            attributes.next_principal_return_finality,
            Some(PrincipalReturnFinality::Final)
        );
    }

    #[test]
    fn an_unknown_termination_value_degrades_the_liquidation_estimate_only() {
        // §4.2 прямо: неполноценной помечается ЛИКВИДАЦИОННАЯ оценка,
        // а не NAV целиком. Утащить неизвестность в terminal_value
        // значило бы обнулить портфель из-за неторгуемой облигации.
        let report = report_with_one_inexecutable_bond();
        assert!(
            report
                .liquidation_value_before_exit_costs_and_tax
                .accrued_interest_payable_on_termination
                .value()
                .is_none()
        );
        assert!(
            report.terminal_value.value().is_some(),
            "стоимость контура остаётся вычисленной"
        );
    }
```

Вспомогательную `report_with_one_inexecutable_bond` собрать по образцу
уже существующих строителей отчёта в этом же блоке тестов
(`grep -n "fn report" crates/iaam-core/src/returns/mod.rs`).

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-core --lib returns 2>&1 | tail -20`
Expected: FAIL — функций нет.

- [ ] **Шаг 3: реализовать**

Добавить в `crates/iaam-core/src/returns/mod.rs`:

```rust
/// Атрибуты облигационной позиции (§5.1: атрибуты, не оценочная база).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BondPositionAttributes {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    /// Начисленный на дату доход по позиции: НКД на бумагу × количество.
    pub accrued_interest: Computed<Dec>,
    /// Фактически реализуемая сегодня сумма (§4.2). Не договорная.
    pub accrued_interest_payable_on_termination: Computed<Dec>,
    /// Ближайшая любая выплата.
    pub next_posting_date: Option<Date>,
    /// Окончателен ли ближайший возврат номинала, если он и есть
    /// ближайшая выплата (`iaam-d8b.4.3`). `None` — ближайшая выплата
    /// не возврат номинала либо её нет вовсе.
    pub next_principal_return_finality: Option<PrincipalReturnFinality>,
}

/// Материально ли расхождение расчёта с наблюдением.
///
/// Допуск — одна минорная единица валюты: обе стороны округляют до
/// копейки, и копейка есть округление, а не ошибка правила.
fn accrued_mismatch_is_material(computed: Dec, observed: Dec, currency: CurrencyCode) -> bool {
    let Ok(difference) = computed.checked_sub(observed) else {
        return true;
    };
    let tolerance = Dec::new(Decimal::new(1, currency.minor_units()));
    difference.inner().abs() > tolerance.inner()
}

/// Реализуемая при выходе сумма (§4.2).
///
/// Равна НКД только при исполнимом выходе. Без него величина не ноль
/// и не НКД, а незнание: §5.3 прямо запрещает считать ликвидность НКД
/// гарантией.
fn payable_on_termination(
    accrued: &Computed<Dec>,
    executability: SourceExecutability,
) -> Computed<Dec> {
    match executability {
        SourceExecutability::Executable => accrued.clone(),
        SourceExecutability::IndicativePreviousClose => Computed::NotComputable {
            reason: NotComputable::ExitNotExecutable,
        },
    }
}
```

Добавить варианты в `NotComputable` и в его `code()` (дispatcher один —
проверить `grep -n "match self" crates/iaam-core/src/returns/mod.rs`
в окрестности `impl NotComputable`):

```rust
    /// Снимка графика выпуска на координату знания нет.
    ScheduleMissing { instrument: InstrumentId },
    /// Сумма купона текущего периода не определена.
    CouponUndetermined { instrument: InstrumentId },
    /// Дата отчёта вне покрытия графика.
    OutsideScheduleCoverage { instrument: InstrumentId },
    /// Исполнимого выхода нет: реализовать НКД сегодня нельзя.
    ExitNotExecutable,
```

Добавить вариант в `MaterialIssue` и в его `makes_incomplete()`
(расхождение — материальная проблема, но ответ неполным **не** делает:
обе величины известны, спорна их согласованность):

```rust
    /// Расчётный и наблюдённый НКД разошлись больше допуска.
    AccruedInterestMismatch {
        instrument: InstrumentId,
        computed: Dec,
        observed: Dec,
        currency: CurrencyCode,
        date: Date,
    },
```

Добавить поле в `AppliedRules`:

```rust
    /// Версия правила расчёта НКД.
    pub accrued_interest_rule: AccruedInterestRuleVersion,
```

и поле в `ReturnsReport`:

```rust
    /// Атрибуты облигационных позиций (§4 спеки E3.4.4).
    pub bond_attributes: Vec<BondPositionAttributes>,
```

Поле в `LiquidationEstimate` — там, и только там, живёт неполнота §4.2:

```rust
    /// Реализуемый сегодня НКД по всем облигационным позициям.
    ///
    /// `NotComputable` здесь делает неполноценной ИМЕННО эту оценку.
    /// В `terminal_value` неизвестность не уходит: неторгуемая
    /// облигация не обнуляет портфель (§4.2).
    pub accrued_interest_payable_on_termination: Computed<Dec>,
```

Заполняется рядом с остальными полями `LiquidationEstimate` в
`crates/iaam-core/src/returns/mod.rs` (там, где сейчас
`exit_costs: AmountQualification::Unknown`): сумма по позициям, где
величина вычислена; хотя бы одна `NotComputable` — вся агрегированная
величина `NotComputable` с той же причиной.

Умножение на количество делается явно там, где собирается
`BondPositionAttributes`:

```rust
        // Правило считает на одну бумагу; атрибут позиции — деньги.
        // Умножение явное: §4.1 предупреждает, что молчаливое
        // сопоставление величин разной размерности читается как
        // ошибка источника.
        let accrued = per_unit.checked_mul_quantity(quantity)?;
```

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-core && cargo build --workspace`
Expected: PASS. Все места конструирования `AppliedRules` и `ReturnsReport`
обновлены — компилятор их перечислит.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/returns/mod.rs
git commit -m "feat(core): три величины §5.1 атрибутами позиции и сверка НКД с источником (iaam-d8b)"
```

---

### Задача 11: структурный перевод снимка в доменные типы

**Файлы:**
- Изменить: `crates/iaam-app/src/market_candidate.rs`

**Интерфейсы:**
- Потребляет: `StoredSnapshot`, `CouponPeriodRow`, `PrincipalRepaymentRow` (`iaam-store`), `AccrualPeriod`, `PrincipalReturn` (Задача 6)
- Отдаёт: `fn accrual_periods_from_snapshot(snapshot: &StoredSnapshot) -> Result<Vec<AccrualPeriod>, AppError>`
- Отдаёт: `fn principal_returns_from_snapshot(snapshot: &StoredSnapshot) -> Result<Vec<PrincipalReturn>, AppError>`

**Приёмка:**
- Перевод не содержит ни одного условия, зависящего от даты отчёта.
- Строка со статусом суммы, отличным от «сумма известна», даёт
  `coupon_per_unit: None`.

- [ ] **Шаг 1: написать падающий тест**

В `mod tests` файла `crates/iaam-app/src/market_candidate.rs`:

```rust
    #[test]
    fn a_row_without_a_fixed_amount_translates_to_none_not_zero() {
        // Перевод обязан оставаться структурным. Подставить ноль здесь
        // значит принять решение правила в переводчике — и обойти
        // версию, под которой это решение должно стоять.
        let snapshot = StoredSnapshot {
            snapshot_id: "s1".to_owned(),
            observed_at: "2026-08-27T12:00:00Z".to_owned(),
            coupon_periods: vec![CouponPeriodRow {
                period_start: "2026-06-03".to_owned(),
                accrual_end: "2026-12-02".to_owned(),
                payment_date: "2026-12-02".to_owned(),
                record_date: None,
                amount_status: "undetermined".to_owned(),
                amount_per_unit: None,
                amount_currency: None,
                rate_percent: None,
                source_entry_id: None,
            }],
            principal_repayments: Vec::new(),
            offer_windows: Vec::new(),
        };
        let periods = accrual_periods_from_snapshot(&snapshot).unwrap();
        assert!(periods[0].coupon_per_unit.is_none());
    }
```

Значение `amount_status` сверить с тем, которое пишет
`record_schedule_snapshot` (`grep -n "amount_status" crates/iaam-store/src/schedule.rs`),
и подставить фактическое.

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-app --lib market_candidate 2>&1 | tail -20`
Expected: FAIL — функции нет.

- [ ] **Шаг 3: реализовать**

Две функции в `crates/iaam-app/src/market_candidate.rs`. Разбор дат через
`Date::parse(&text, &Iso8601::DEFAULT)`, чисел — через
`Decimal::from_str_exact`, валюты — тем же способом, что уже применён
в `market_candidate_from_row` (`crates/iaam-app/src/scenarios/reports.rs`).
Никаких ветвлений по дате отчёта: перевод структурный.

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-app --lib market_candidate`
Expected: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-app/src/market_candidate.rs
git commit -m "feat(app): структурный перевод снимка графика в доменные типы ядра (iaam-d8b)"
```

---

### Задача 12: отчёт собирает атрибуты

**Файлы:**
- Изменить: `crates/iaam-app/src/scenarios/reports.rs`

**Интерфейсы:**
- Потребляет: Задачи 4, 10, 11; `SqliteStore::schedule_at_or_before`
- Отдаёт: `ReportInputs` с полями `schedules` и `accrued_observations`

**Приёмка:**
- Снимок графика читается на ту же координату знания, что и цены.
- Наблюдение НКД читается на ту же координату.
- Повторная синхронизация при неизменной координате не меняет ни одного
  из трёх атрибутов.

- [ ] **Шаг 1: написать падающий тест**

В `crates/iaam-app/tests/` рядом с существующим метаморфным тестом графика
(`schedule_metamorphic.rs`):

```rust
#[tokio::test]
async fn resyncing_changes_no_bond_attribute_at_a_fixed_coordinate() {
    // Метаморфное свойство: снимок с тем же содержимым новой записи
    // не порождает, значит и атрибуты обязаны совпасть до поля.
    let services = fixture_services().await;
    sync_schedule(&services).await;
    let before = returns(&services, &principal(), &query()).await.unwrap();
    sync_schedule(&services).await;
    let after = returns(&services, &principal(), &query()).await.unwrap();
    assert_eq!(before.bond_attributes, after.bond_attributes);
}
```

Вспомогательные функции переиспользовать из существующего
`schedule_metamorphic.rs`.

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-app --test schedule_metamorphic 2>&1 | tail -20`
Expected: FAIL — поля `bond_attributes` нет в сборке отчёта.

- [ ] **Шаг 3: реализовать**

Добавить в `reports.rs` функцию по образцу `market_price_candidates`:
она проходит те же инструменты позиций, для каждого зовёт
`store.schedule_at_or_before(&instrument_id, "moex-iss", &knowledge_as_of)`
и `store.accrued_interest_at_or_before(...)`, переводит результат
функциями Задачи 11 и складывает в `ReportInputs`. Идентификатор
источника взять тем же способом, что и в `market_price_candidates`
(`"moex-iss"`), а не заводить второй.

Расширить `ReportInputs`:

```rust
struct ReportInputs<'a> {
    fx: &'a FxTable,
    market_prices: &'a [PriceCandidate],
    /// График на координату знания, по инструменту.
    schedules: &'a BTreeMap<InstrumentId, BondSchedule>,
    /// Наблюдённый НКД на одну бумагу, по инструменту.
    accrued_observations: &'a BTreeMap<InstrumentId, PerUnitAmount>,
    knowledge_as_of: OffsetDateTime,
}
```

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-app && cargo build --workspace`
Expected: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-app/src/scenarios/reports.rs
git commit -m "feat(app): отчёт читает график и наблюдения НКД на координату знания (iaam-d8b)"
```

---

### Задача 13: три величины в API

**Файлы:**
- Изменить: `crates/iaam-server/src/dto.rs`

**Интерфейсы:**
- Потребляет: `BondPositionAttributes` (Задача 10)
- Отдаёт: `BondPositionAttributesDto`

**Приёмка:**
- `NotComputable` доходит до API машиночитаемым кодом, а не текстом.
- Неизвестная величина в JSON — не ноль.

- [ ] **Шаг 1: написать падающий тест**

В `mod tests` файла `crates/iaam-server/src/dto.rs`:

```rust
    #[test]
    fn an_unknown_termination_value_serialises_with_a_reason_not_a_zero() {
        // Ноль в JSON означал бы «при выходе не получите ничего»,
        // а мы говорим «не знаем». Внешний агент разбирает код.
        let dto = BondPositionAttributesDto::from_domain(&BondPositionAttributes {
            account: AccountId::new_random(),
            custody: None,
            instrument: InstrumentId::new_random(),
            accrued_interest: Computed::Value(Dec::new(Decimal::from_str_exact("15.17").unwrap())),
            accrued_interest_payable_on_termination: Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable,
            },
            next_posting_date: Some(date!(2026 - 12 - 02)),
        });
        let json = serde_json::to_value(&dto).unwrap();
        assert!(json["accruedInterestPayableOnTermination"]["value"].is_null());
        assert_eq!(
            json["accruedInterestPayableOnTermination"]["reason"],
            "exit_not_executable"
        );
    }
```

Имена полей и форму DTO для `Computed` сверить с уже существующим
`AmountQualificationDto` / `LiquidationEstimateDto` в этом файле и
повторить их, а не изобретать второй стиль.

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-server --lib dto 2>&1 | tail -20`
Expected: FAIL — DTO нет.

- [ ] **Шаг 3: реализовать**

`BondPositionAttributesDto` по образцу `LiquidationEstimateDto`, поле
`bondAttributes` в DTO отчёта, заполнение в `from_domain`.

- [ ] **Шаг 4: тесты проходят**

Run: `cargo test -p iaam-server`
Expected: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-server/src/dto.rs
git commit -m "feat(server): три величины §5.1 в ответе отчёта (iaam-d8b)"
```

---

### Задача 14: фикстуры и заслоны

**Файлы:**
- Создать: `tests/fixtures/market/moex-iss-history-ofz.json` (файл политики)
- Изменить: `tests/fixtures/market/MANIFEST.sha256` (файл политики)
- Изменить: `scripts/check-mutants.sh` (файл политики)

**Приёмка:**
- Фикстура снята живым вызовом, а не сконструирована.
- Новые модули стоят в мутационном заслоне, выживших нет.
- Тест Задачи 5 больше не помечен `#[ignore]`.

- [ ] **Шаг 1: снять фикстуру живым вызовом**

```bash
curl -s "https://iss.moex.com/iss/history/engines/stock/markets/bonds/boards/TQOB/securities/SU26238RMFS4.json?from=2026-08-20&till=2026-08-26&iss.only=history" \
  -o tests/fixtures/market/moex-iss-history-ofz.json
python3 -c "import json;d=json.load(open('tests/fixtures/market/moex-iss-history-ofz.json'));assert 'ACCINT' in d['history']['columns'];print('ACCINT на месте, строк:',len(d['history']['data']))"
```

Expected: `ACCINT на месте, строк: 5`

- [ ] **Шаг 2: обновить манифест**

```bash
./scripts/check-fixtures.sh --update 2>/dev/null || \
  (cd tests/fixtures/market && sha256sum *.json > MANIFEST.sha256)
```

Точную команду сверить с `scripts/check-fixtures.sh` — она там одна.

- [ ] **Шаг 3: снять `#[ignore]` с теста Задачи 5 и прогнать всё**

```bash
make check
```

Expected: зелено.

- [ ] **Шаг 4: мутационный заслон**

Добавить в `scripts/check-mutants.sh` модули
`crates/iaam-core/src/rules/accrued_interest.rs`,
`crates/iaam-core/src/bond/finality.rs`,
`crates/iaam-core/src/bond/posting.rs`,
`crates/iaam-market/src/moex/parse.rs` — по образцу уже перечисленных.

Run: `make mutants`
Expected: выживших по новым модулям нет. Выживший мутант — недостающий
тест, а не шум.

- [ ] **Шаг 5: два раздельных коммита**

```bash
git add crates/iaam-app/src/scenarios/sync.rs
git commit -m "test(app): облигационный ответ даёт цены и НКД одним разбором (iaam-d8b)"

git add tests/fixtures/market/moex-iss-history-ofz.json \
        tests/fixtures/market/MANIFEST.sha256 scripts/check-mutants.sh
POLICY_CHANGE_APPROVED=1 git commit -m "chore(policy): фикстура истории облигации с ACCINT и заслон новых модулей (iaam-d8b)"
```

---

## Порядок и зависимости

```
З1 (округление) ─────────────────────────────┐
З6 (типы графика) ─┬─> З7 (правило НКД) ──────┼─> З10 (отчёт) ─> З12 (сборка) ─> З13 (API)
                   ├─> З8 (окончательность) ──┤        ^              ^
                   ├─> З9 (ближайшая выплата) ─┘        │              │
                   └─> З11 (перевод) ───────────────────┘              │
                                                                        │
З2 (наблюдение) ─> З3 (разбор) ─> З4 (хранилище) ─> З5 (синхронизация) ─┘
                                                                        │
                                                        З14 (фикстуры и заслоны)
```

Три независимые ветки открываются сразу: З1, З2 и З6. Единственная точка
схождения — З12; до неё дерево остаётся зелёным на каждом коммите.

З14 трогает файлы политики и требует отдельного коммита с
`POLICY_CHANGE_APPROVED=1` и метки PR `policy-change`. Тест З5 до З14
помечен `#[ignore]`, потому что зависит от фикстуры.

## Расхождения с текстом спеки

**Валюта наблюдения.** Спека (3.1) показывает у `AccruedInterestObservation`
поля `per_unit` и `currency` рядом. План складывает их в один
`PerUnitAmount`, который валюту уже несёт: два поля дали бы два источника
истины о валюте одного числа и возможность их рассогласовать. Требование
спеки — валюта явная и из наблюдения — выполнено полностью.

**Порт чтения.** Спека (раздел 7) называет чтение снимка «портом». В коде отчёт читает
`services.market_store` напрямую (`reports.rs::market_price_candidates`),
трейта-порта для рыночного хранилища нет. План следует коду: заводить порт
ради одной выборки значило бы развести два способа чтения одного хранилища
в одном сценарии. Смысл раздела 7 — что правило живёт в ядре, а перевод
структурный — сохранён полностью.
