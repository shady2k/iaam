# E3.3 часть 1 — Типы кандидата и граница legacy

> **For agentic workers:** каждая задача ниже — отдельный бид. Реализация по TDD:
> сначала падающий тест, потом минимальный код. **Тяжёлые проверки не запускать**
> — `cargo mutants`, полный `cargo test --workspace` и живые сетевые тесты гоняет
> супервайзер в конце эпика. Воркеру достаточно тестов своего крейта.

**Goal:** закрыть путь, которым наш собственный вывод может попасть в хранилище
или журнал под видом наблюдения источника, и завести общий тип кандидата, через
который обе цены доходят до оценки.

**Architecture:** три независимых изменения. Первое убирает из рыночного
хранилища значение, которого источник не даёт. Второе закрывает публичный API,
через который клиент мог записать наш вывод фактом. Третье заводит общий тип
кандидата и адаптер, который раскладывает старое смешанное качество цены на две
оси, не переоценивая унаследованные производные значения.

**Tech Stack:** Rust 2024, `rustc 1.98.0`, SQLite STRICT, `serde`, `utoipa`
(`ToSchema`), тесты — штатный `cargo test` по крейтам.

## Global Constraints

- Все команды выполняются внутри `nix develop`.
- `unsafe_code = "forbid"`, `clippy::all` на уровне `deny` (см. `Cargo.toml`).
- Журнал фактов append-only: `EventKind::Valuation` и `SCHEMA_VERSION = 3` в этом
  плане **не трогаются вовсе**. Задача, которая захочет их изменить, — остановка
  и эскалация к супервайзеру.
- Существующие тесты не переписываются. Механическая адаптация к изменившемуся
  типу разрешена; изменение того, **что** тест утверждает, — запрещено и является
  поводом остановиться.
- Дизайн: `.internal/specs/2026-08-26-e3-3-valuation-policy-design.md`.
  Решение: `docs/decisions/0002-polnota-ocenki-i-ispolnimost-ceny-dve-osi.md`.

---

### Task 1: `Executability::Stale` уходит из рыночного хранилища

Источник не сообщает, что наблюдение устарело: устаревание — это сравнение
возраста с порогом, а порог принадлежит политике оценки. Хранить такой вывод
наблюдением запрещено решением 0002. Значение сегодня не производится ни одним
разборщиком, поэтому удаление ничего не ломает.

**Files:**
- Modify: `crates/iaam-market/src/observation.rs:36-43` — вариант `Stale`
- Modify: `crates/iaam-app/src/scenarios/sync.rs:578-584` — функция `executability`
- Create: `crates/iaam-store/migrations/0007_executability_without_stale.sql`
- Test: `crates/iaam-store/tests/` — новый тест рядом с существующими тестами миграций

**Interfaces:**
- Consumes: ничего. Задача независима, начинать можно сразу.
- Produces: `iaam_market::Executability` с двумя вариантами — `Executable`,
  `IndicativePreviousClose`. На этот набор опирается часть 2 плана, где
  появляется отображение в доменный `SourceExecutability` внутри `iaam-app`.
  Задача 3 этого плана от него **не** зависит.

**Acceptance Criteria:**
- `Executability` не содержит варианта `Stale`; сборка воркспейса проходит.
- Миграция `0007` сужает `CHECK` до двух значений, сохраняя все существующие
  строки, индекс и триггеры таблицы `price_observations`.
- Тест доказывает, что после `0007` вставка строки с `executability = 'stale'`
  отклоняется базой, а ранее записанные строки читаются без изменений.

- [ ] **Шаг 1: падающий тест на отказ базы**

Тест в стиле уже существующих тестов `crates/iaam-store/tests/`: применить
миграции, вставить наблюдение с `executability = 'stale'` напрямую через SQL,
ожидать ошибку CHECK.

```rust
#[test]
fn stale_executability_is_rejected_by_the_store() {
    let store = test_store();
    let err = store
        .raw_execute(
            "INSERT INTO price_observations (instrument_id, board, session, \
             trade_date, kind, source_id, observed_at, price, currency, \
             executability, raw_hash, sync_run_id) \
             VALUES (?1, 'TQBR', 3, '2026-08-03', 'close', 'moex', \
             '2026-08-03T18:00:00Z', '281.39', 'RUB', 'stale', 'h', ?2)",
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK"));
}
```

Точные имена помощников (`test_store`, способ выполнить сырой SQL) взять из
соседних тестов в `crates/iaam-store/tests/` — свои не заводить.

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-store stale_executability`
Expected: FAIL — вставка проходит, ошибки нет.

- [ ] **Шаг 3: миграция 0007**

SQLite не умеет менять `CHECK` на месте. Таблица пересоздаётся: прочитать
определение `price_observations`, индекс и **все** триггеры в
`crates/iaam-store/migrations/0006_market_observations.sql`, воспроизвести их
дословно с единственным изменением в `CHECK`, скопировать строки, удалить старую
таблицу, переименовать новую, пересоздать индекс и триггеры.

```sql
-- 0007: устаревание — вывод политики оценки (E3.3), а не атрибут наблюдения.
-- Решение: docs/decisions/0002-polnota-ocenki-i-ispolnimost-ceny-dve-osi.md
DROP TRIGGER price_observations_are_immutable;
-- ... остальные триггеры таблицы, если их больше одного
DROP INDEX price_observations_by_series;

CREATE TABLE price_observations_new (
    -- колонки дословно из 0006
    CHECK (executability IN ('executable', 'indicative_previous_close'))
) STRICT;

INSERT INTO price_observations_new SELECT * FROM price_observations;
DROP TABLE price_observations;
ALTER TABLE price_observations_new RENAME TO price_observations;
-- индекс и триггеры пересоздаются дословно из 0006
```

Если в существующих строках найдётся `executability = 'stale'`, `INSERT` упадёт —
это правильное поведение: значит утверждение дизайна неверно, **остановиться и
эскалировать**, а не чистить данные.

- [ ] **Шаг 4: убрать вариант из типа**

В `crates/iaam-market/src/observation.rs` удалить вариант `Stale` и его
doc-комментарий. В doc-комментарии самого `enum Executability` дописать абзац:
устаревания здесь нет по той же причине, по какой нет переноса, — это вывод
правила оценки, а не то, что прислал источник.

Тест `crates/iaam-market/src/observation.rs:135-146`, перечисляющий варианты,
механически адаптировать: массив из двух элементов, `assert_eq!(all.len(), 2)`.
Смысл утверждения не менять.

В `crates/iaam-app/src/scenarios/sync.rs` убрать рукав `Stale` из `executability`.

- [ ] **Шаг 5: прогнать тесты двух крейтов**

Run: `cargo test -p iaam-store -p iaam-market`
Expected: PASS. Тест из шага 1 проходит.

**Workspace-тесты не запускать** — это делает супервайзер.

- [ ] **Шаг 6: коммит**

```bash
git add crates/iaam-market/src/observation.rs crates/iaam-app/src/scenarios/sync.rs \
        crates/iaam-store/migrations/0007_executability_without_stale.sql \
        crates/iaam-store/tests
git commit -m "refactor(market): устаревание — вывод политики, не атрибут наблюдения"
```

---

### Task 2: публичный API перестаёт принимать наш собственный вывод

`PriceQualityDto` (`crates/iaam-server/src/dto.rs:75`) выставляет наружу все пять
значений `PriceQuality`, включая `CarriedForward` и `Stale`. Сегодня клиент может
прислать цену, объявленную перенесённой, и записать в журнал фактов наш вывод под
видом факта источника. Это живая дыра.

CSV-канал уже безопасен: `crates/iaam-ingest/src/csv_source.rs:334` жёстко ставит
`OwnerEstimate`. Трогать его не нужно.

**Files:**
- Modify: `crates/iaam-server/src/dto.rs:72-93` — `PriceQualityDto` и `to_domain`
- Modify: `scripts/check-architecture.sh` — новое правило рядом с правилом 11
- Test: `crates/iaam-server/tests/` — рядом с существующими тестами маршрутов

**Interfaces:**
- Produces: `PriceQualityDto` с вариантами `Executable`, `PreviousClose`,
  `OwnerEstimate`. Домённый `PriceQuality` **не меняется**: пять вариантов в ядре
  остаются, потому что журнал append-only и старые события обязаны читаться.

**Acceptance Criteria:**
- `POST` операции оценки с `"quality": "carried_forward"` отклоняется разбором
  тела как неизвестное значение, а не принимается.
- То же для `"stale"`.
- `"executable"`, `"previous_close"` и `"owner_estimate"` продолжают работать —
  существующие приёмочные тесты не ослабляются.
- `CarriedForward` и `Stale` исчезают из `/v1/openapi.json`.
- `scripts/check-architecture.sh` падает, если `PriceQualityDto` снова начнёт
  перечислять запрещённые значения.

- [ ] **Шаг 1: падающий тест на отказ**

```rust
#[tokio::test]
async fn a_carried_forward_price_is_not_accepted_from_the_api() {
    let app = test_app().await;
    let response = app
        .post_operation(json!({
            "type": "valuation",
            "quality": "carried_forward"
            // остальные поля — как в соседнем тесте успешной оценки
        }))
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
```

Имена помощников (`test_app`, способ отправить операцию, ожидаемый код отказа для
нераспознанного тела) взять из соседних тестов `crates/iaam-server/tests/`. Если
разбор тела в этом проекте отвечает другим кодом — использовать тот, что уже
используется для нераспознанного значения перечисления, и не заводить новый.

- [ ] **Шаг 2: убедиться, что тест падает**

Run: `cargo test -p iaam-server carried_forward_price_is_not_accepted`
Expected: FAIL — запрос принимается.

- [ ] **Шаг 3: сузить DTO**

```rust
/// Качество цены в транспорте.
///
/// Уже вычисленные нами величины — перенос на нерабочий день и
/// устаревание по порогу — представимым вводом не являются: это выводы
/// политики оценки, а не то, что утверждает источник. Записать их фактом
/// значит стереть различие между наблюдением и нашим выводом
/// (docs/decisions/0002-polnota-ocenki-i-ispolnimost-ceny-dve-osi.md).
/// Доменный PriceQuality шире: он обязан читать старый журнал.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceQualityDto {
    Executable,
    PreviousClose,
    OwnerEstimate,
}

impl PriceQualityDto {
    #[must_use]
    pub const fn to_domain(self) -> PriceQuality {
        match self {
            Self::Executable => PriceQuality::Executable,
            Self::PreviousClose => PriceQuality::PreviousClose,
            Self::OwnerEstimate => PriceQuality::OwnerEstimate,
        }
    }
}
```

Если где-то есть обратное преобразование `PriceQuality -> PriceQualityDto` (для
чтения событий наружу), оно обязано остаться тотальным: `CarriedForward` и
`Stale` при выдаче наружу отображаются, потому что старые события существуют.
Найти его `grep -rn "PriceQualityDto" crates/iaam-server/src` и, если оно есть,
добавить для этих двух значений отдельный вариант вывода — **не паниковать и не
терять значение молча**. Если такого преобразования нет — шаг пропустить.

- [ ] **Шаг 4: заслон в check-architecture.sh**

Рядом с правилом про `reqwest` (`scripts/check-architecture.sh:235-243`), тем же
стилем:

```bash
hits=$(grep -nE '^[[:space:]]*(CarriedForward|Stale),' crates/iaam-server/src/dto.rs || true)
if [ -n "$hits" ]; then
  err "dto.rs выставляет наружу вывод политики как качество цены (решение 0002)"
  echo "$hits" >&2
fi
```

- [ ] **Шаг 5: тесты крейта и заслон**

Run: `cargo test -p iaam-server && bash scripts/check-architecture.sh`
Expected: PASS оба.

- [ ] **Шаг 6: коммит**

```bash
git add crates/iaam-server/src/dto.rs scripts/check-architecture.sh crates/iaam-server/tests
git commit -m "fix(api): перенос и устаревание перестают быть представимым вводом"
```

---

### Task 3: общий кандидат, порт выборки и legacy-адаптер

Дизайн E3.2 обещал `PriceObservation`, `PriceQuery` и `SelectedPrice`; в коде есть
только первый. Задача закрывает долг и заводит границу, через которую обе цены —
биржевая и журнальная — попадают в оценку.

Правило выбора здесь **не реализуется**: это часть 2. Здесь только типы и
адаптер.

**Files:**
- Create: `crates/iaam-core/src/valuation/candidate.rs`
- Modify: `crates/iaam-core/src/valuation.rs` — объявить модуль, реэкспорт
- Test: тесты в самом `candidate.rs`, как принято в этом крейте

**Interfaces:**
- Consumes: ничего от задач 1 и 2. Задача полностью независима — начинать можно
  сразу.
- **`iaam-core` не зависит ни от одного крейта проекта** (см. его `Cargo.toml`):
  это чистый домен. `iaam_market::Executability` здесь **недоступен и не должен
  импортироваться** — попытка добавить зависимость уронит
  `scripts/check-architecture.sh`. `SourceExecutability` — собственный тип ядра;
  отображение из рыночного `Executability` живёт в `iaam-app` и относится к
  части 2 плана.
- Produces: `PriceCandidate`, `PriceOrigin`, `SourceExecutability`, `PriceQuery`,
  `SelectedPrice`, `PriceSelection`, `PriceFreshness`, `Uncovered`,
  `candidate_from_legacy_valuation`. На них опирается часть 2 плана.

**Acceptance Criteria:**
- `SourceExecutability` имеет ровно три значения: `Executable`,
  `IndicativePreviousClose`, `Unknown`.
- `PriceSelection` и `PriceFreshness` — **разные** типы: цена может быть
  одновременно перенесённой и устаревшей, и тест это доказывает.
- `candidate_from_legacy_valuation` для `PriceQuality::OwnerEstimate` даёт
  происхождение `OwnerAsserted` и исполнимость `Unknown`.
- `candidate_from_legacy_valuation` для `CarriedForward` и `Stale` **не даёт
  кандидата**: возвращает терминальное `LegacyDerived`, потому что исходная дата
  наблюдения в событии не сохранена и переоценка выдала бы чужой перенос за
  свежее наблюдение.
- Ни один конструктор не позволяет собрать `PriceCandidate` с исполнимостью,
  вычисленной нами: `PriceSelection` в кандидат не входит.

- [ ] **Шаг 1: падающие тесты**

```rust
#[test]
fn a_legacy_owner_estimate_becomes_an_owner_asserted_candidate() {
    let outcome = candidate_from_legacy_valuation(PriceQuality::OwnerEstimate, price());
    let candidate = outcome.candidate().expect("оценка владельца — кандидат");
    assert_eq!(candidate.origin, PriceOrigin::OwnerAsserted);
    assert_eq!(candidate.executability, SourceExecutability::Unknown);
}

#[test]
fn a_legacy_carried_forward_price_is_never_re_derived() {
    let outcome = candidate_from_legacy_valuation(PriceQuality::CarriedForward, price());
    assert!(outcome.candidate().is_none());
    assert_eq!(
        outcome.legacy(),
        Some(PriceQuality::CarriedForward),
        "исходная дата наблюдения потеряна: переоценка выдала бы перенос за наблюдение"
    );
}

#[test]
fn carried_forward_and_stale_are_independent_facts() {
    let selected = SelectedPrice {
        selection: PriceSelection::CarriedForward { observed_on: date!(2026-07-01), days: 40 },
        freshness: PriceFreshness::Stale { days: 40 },
        // остальные поля
    };
    assert!(matches!(selected.selection, PriceSelection::CarriedForward { .. }));
    assert!(matches!(selected.freshness, PriceFreshness::Stale { .. }));
}
```

- [ ] **Шаг 2: убедиться, что тесты падают**

Run: `cargo test -p iaam-core candidate`
Expected: FAIL — типы не объявлены.

- [ ] **Шаг 3: типы**

```rust
//! Общий кандидат на оценку и порт выборки (E3.3, дизайн раздел 3).
//!
//! Два канала цены — биржевое наблюдение и утверждение владельца или
//! документа — приходят сюда одним типом. Исполнимость в кандидате
//! принадлежит ИСТОЧНИКУ; всё, что вывели мы, живёт в `SelectedPrice`
//! и в кандидат не попадает по построению.

/// Откуда пришёл кандидат. Не выводится: канал известен в точке сборки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceOrigin {
    // Вид цены — своя строка ядра, а не iaam_market::PriceKind:
    // ядро не зависит от крейт-источников (см. Interfaces).
    Market { venue: String, kind: String },
    ReportParsed { source: SourceId },
    OwnerAsserted,
}

/// Исполнимость по утверждению источника.
///
/// `Unknown` обязателен: владелец, вводя цену неликвида, не утверждает
/// ни того, что по ней можно выйти, ни того, что это цена закрытия.
/// Без этого варианта ручной канал вынужден лгать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceExecutability {
    Executable,
    IndicativePreviousClose,
    Unknown,
}

/// Способ выбора — почему дата наблюдения не совпала с датой оценки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSelection {
    AsObserved,
    CarriedForward { observed_on: Date, days: u16 },
    LegacyDerived { quality: PriceQuality },
}

/// Свежесть — отдельная ось: цена бывает перенесённой И устаревшей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceFreshness {
    Fresh,
    Stale { days: u16 },
}

/// Почему позиция осталась без цены.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UncoveredReason {
    NoObservation,
    TooOld,
    AmbiguousVenue,
    AmbiguousCandidate,
}
```

`PriceCandidate` — инструмент, цена (`Dec`), валюта, `trade_date`, `origin`,
`executability`. `PriceQuery` — инструмент, дата оценки, `knowledge_as_of`.
`SelectedPrice` — выбранный кандидат, `selection`, `freshness`.

Точные типы идентификаторов и дат брать из `crate::ids` и `crate::dates`, свои не
заводить.

- [ ] **Шаг 4: legacy-адаптер**

Возвращаемый тип — перечисление с двумя рукавами: кандидат либо терминальное
унаследованное значение. Отображение — таблицей из раздела 3.4 дизайна.

- [ ] **Шаг 5: тесты крейта**

Run: `cargo test -p iaam-core`
Expected: PASS.

- [ ] **Шаг 6: коммит**

```bash
git add crates/iaam-core/src/valuation crates/iaam-core/src/valuation.rs
git commit -m "feat(core): общий кандидат на оценку и граница legacy-качества"
```
