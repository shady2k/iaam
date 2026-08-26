# E3.2 часть 3 — потребление и автоматизация: план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** Отчёт считается по сохранённому ряду наблюдений, а не по последней известной цене; курс приходит из хранилища, а не телом запроса; агент и веб-UI получают справочные ряды с provenance; синхронизация идёт по расписанию.

**Architecture:** Ядро получает выборку цены **на дату** взамен бездаточной `latest()`; отчёт закрепляет координату знания и возвращает хеш выбранного набора входов; `iaam-app` наполняет `FxTable` из хранилища; сервер получает справочные маршруты и подсистему заданий.

**Tech Stack:** Rust 1.98.0, `iaam-core`, `iaam-store`, `iaam-app`, `iaam-server`, `iaam-http`, `tokio`.

**Спецификация:** `.internal/specs/2026-08-26-e3-2-market-data-design.md`, разделы 4, 5, 6.3, 7
**Части 1 и 2:** сданы; `iaam-http`, `iaam-market`, `MarketStore` и сценарий `sync_market` готовы.

## Global Constraints

Те же, что в части 2 (`.internal/plans/2026-08-26-e3-2-part2-observations.md`,
раздел `## Global Constraints`) — прочитай их там целиком. Коротко, самое
дорогое из накопленного за части 1 и 2:

- **Клиппи по своей крейте обязателен**: `cargo clippy -p <крейт> --all-targets -- -D warnings`.
- **Меняешь публичный тип, трейт или сигнатуру — проверь потребителей** `grep`-ом и `cargo check -p` по каждой затронутой крейте. Тестовые заглушки трейтов дополняй, а не удаляй.
- **Фильтр `cargo test` совпадает с именем ТЕСТА, а не файла.** Для `tests/foo.rs` — `--test foo`. `0 passed; N filtered out` — несостоявшаяся проверка, а не успех.
- **Коммит обязателен перед сигналом завершения**: приёмка идёт по коммиту.
- Ослабление теста ради прохождения запрещено (§15.7).
- Тяжёлые прогоны (`make check`, мутанты, покрытие) — только у координатора, в конце эпика.

---

## Карта файлов

| Файл | Ответственность | Задача |
|---|---|---|
| `crates/iaam-core/src/valuation.rs` | выборка цены на дату | 1 |
| `crates/iaam-core/src/returns/xirr.rs:130` | `latest()` уходит | 1 |
| `crates/iaam-core/src/returns/mod.rs:245` | координата в `ReturnsRequest` | 2 |
| `crates/iaam-app/src/scenarios/reports.rs` | наполнение `FxTable` из хранилища | 3 |
| `crates/iaam-server/src/routes.rs:1191` | курс из хранилища, не из тела | 3 |
| `crates/iaam-server/src/routes.rs` | справочные маршруты | 4 |
| `docs/agent-skill/SKILL.md` | граница «факты цитируй, производные — из отчётов» | 4 |
| `crates/iaam-app/src/jobs.rs` | подсистема заданий | 5 |

**Порядок и параллельность.** Задачи 1 и 4 независимы и идут параллельно
(ядро против сервера). Задача 2 стоит на задаче 1. Задача 3 стоит на задаче 2.
Задача 5 независима от всех и идёт параллельно с любой.

---

### Task 1: Выборка цены на дату вместо `latest()`

**Files:**
- Modify: `crates/iaam-core/src/valuation.rs`
- Modify: `crates/iaam-core/src/returns/xirr.rs` (строка 130)

**Interfaces:**
- Produces: `PriceBoard::price_at_or_before(&self, instrument: InstrumentId, as_of: Date) -> Option<&InstrumentPrice>`. `PriceBoard::latest` **удаляется**.

**Acceptance Criteria:**
- `latest()` в ядре отсутствует: `grep -rn "\.latest(" crates/` не находит вызовов.
- `account_values` берёт цену **не позже** `request.as_of`.
- Наблюдение позже даты отчёта в оценку не попадает — даже если оно есть в состоянии.
- Существующие тесты доходности проходят без правки утверждений.

**Почему это делается.** Сегодня `account_values` берёт цену бездаточным
`latest()` (`crates/iaam-core/src/returns/xirr.rs:130`). Живого бага нет,
и это проверено: `price.as_of` берётся из `event.dates.effective_date()`
(`projection/mod.rs:346`), `Coverage::last_event` — максимум того же
(`state.rs:88`), а `guard_state_not_newer` отвергает состояние новее
отчёта. Но безопасность здесь **случайная**: она держится на совпадении
двух дат, а не на форме API, и станет ошибкой при первом же использовании
внешнего ряда. Раздел 5.1 спеки.

- [ ] **Step 1: Падающий тест**

В `crates/iaam-core/src/valuation.rs`, в модуль тестов:

```rust
#[test]
fn a_price_observed_after_the_report_date_is_not_used() {
    let mut board = PriceBoard::new();
    let instrument = InstrumentId::new(uuid::Uuid::nil());
    board.record(price_at(instrument, date!(2025 - 12 - 31), "100"));
    board.record(price_at(instrument, date!(2026 - 08 - 01), "200"));
    let chosen = board
        .price_at_or_before(instrument, date!(2025 - 12 - 31))
        .expect("цена на дату");
    assert_eq!(chosen.as_of, date!(2025 - 12 - 31));
}

#[test]
fn a_gap_falls_back_to_the_latest_earlier_observation() {
    // Биржа не торговала — берётся последнее наблюдение НЕ ПОЗЖЕ даты.
    // Это не carry-forward как запись: наблюдение остаётся своим днём,
    // выбор делается на чтении.
}

#[test]
fn an_instrument_without_any_earlier_observation_has_no_price() {
    // Отсутствие цены — не ноль (§4.9).
}
```

- [ ] **Step 2–5: реализация, прогон, клиппи, потребители**

`grep -rn "\.latest(" crates/` — убедиться, что вызовов не осталось.
`cargo check -p iaam-app -p iaam-server` — потребители ядра.

- [ ] **Step 6: Коммит**

---

### Task 2: Координата знания в отчёте

**Files:**
- Modify: `crates/iaam-core/src/returns/mod.rs` (строка 245, `ReturnsRequest`)
- Modify: `crates/iaam-core/src/returns/mod.rs` (`ReturnsReport`)

**Interfaces:**
- Produces: `KnowledgeCoordinate { knowledge_as_of: OffsetDateTime, source_priority_version: u32, valuation_policy_version: u32 }`; поле `coordinate` в `ReturnsRequest`; поля `coordinate` и `inputs_hash: String` в `ReturnsReport`.

**Acceptance Criteria:**
- Отчёт возвращает координату, по которой он посчитан, и **хеш выбранного набора входов**.
- Хеш меняется, когда меняется выбранный набор, и **не меняется**, когда пересчёт идёт по той же координате.
- Координата — тройка, а не список наблюдений: манифест на сотни тысяч идентификаторов сам стал бы объектом хранения (раздел 4 спеки).

**Почему хеш, а не список.** Хеш существует **не как список для чтения,
а как проверка того, что выбор детерминирован**. Если один и тот же журнал
с той же координатой дал разный хеш — сломался выбор, и это надо заметить
раньше, чем разойдутся числа.

- [ ] **Step 1: Падающий тест**

```rust
#[test]
fn the_same_coordinate_yields_the_same_inputs_hash() { /* ... */ }

#[test]
fn a_different_knowledge_time_yields_a_different_inputs_hash() { /* ... */ }
```

- [ ] **Step 2–6: реализация, прогон, клиппи, потребители, коммит**

`ReturnsRequest` — публичная структура с временем жизни; её конструируют
`crates/iaam-app/src/scenarios/reports.rs` и `crates/iaam-server/src/routes.rs`.
Оба обязаны собраться.

---

### Task 3: Курс из хранилища вместо тела запроса

**Files:**
- Modify: `crates/iaam-app/src/scenarios/reports.rs`
- Modify: `crates/iaam-server/src/routes.rs` (строки 1169, 1191–1197)
- Modify: `crates/iaam-server/tests/contract.rs`

**Acceptance Criteria:**
- `FxTable` наполняется из `MarketStore` с источником `FxSource::CbrOfficial`.
- Путь, при котором курс приходит **телом запроса**, либо исчезает, либо остаётся явно помеченным `OwnerSupplied` и **отличимым в ответе**.
- Отчёт по инструменту в валюте, курса которой нет в хранилище, даёт типизированный отказ, а не курс, равный единице.

**Почему это дыра.** Сегодня курс приходит телом запроса
(`returns_report_with_rates`, `routes.rs:1191`) с источником
`OwnerSupplied`: вызывающая сторона может назвать **любой** курс,
и отчёт его применит. Для проекта, чей заявленный тезис — достоверность
данных, это открытая дверь. `FxSource::CbrOfficial` объявлен в
`crates/iaam-core/src/valuation.rs:117` с комментарием «Появится в E3» —
заготовленное место ждёт именно этой задачи.

- [ ] **Step 1–6: тест, реализация, прогон, клиппи, потребители, коммит**

---

### Task 4: Справочная поверхность для агента и веб-UI

**Files:**
- Modify: `crates/iaam-server/src/routes.rs`, `dto.rs`, `openapi.rs`
- Modify: `crates/iaam-server/tests/contract.rs`
- Modify: `docs/agent-skill/SKILL.md`

**Interfaces:**
- Produces: `GET /v1/market/key-rate`, `GET /v1/market/fx`, `GET /v1/market/prices` — все с интервалом и provenance.

**Acceptance Criteria:**
- Ответ несёт **значение, дату, источник, момент наблюдения, флаг качества и границу полноты серии**. Голое число без provenance не отдаётся.
- **Ключевая ставка отдаётся интервалами**, и граница помечена выведенной, когда она попала в нерабочие дни (раздел 8.3 спеки).
- Маршруты требуют токена; неаутентифицированной поверхности не появляется.
- Пускают и владельческий, и агентский токен: маршруты только читают.
- `docs/agent-skill/SKILL.md` дополнен границей: **производные величины —
  только из отчётных маршрутов, справочные факты можно цитировать**.

**Почему это не противоречит §13.** §13 требует, чтобы **отчётный** контур
отдавал финальные числа, а не сырьё для досчёта. Он не запрещает API иметь
справочную поверхность — она уже есть, `/v1/accounts` и `/v1/instruments`.
Запрет на собственную арифметику агента при этом не слабеет: он
сформулирован как свойство **ответа** («число в ответе агента,
отсутствующее в ответах API, является ошибкой»), а не как ограничение
на чтение. Процитированная ставка — число из ответа API. Вычисленная
агентом реальная доходность — числа, которого в ответах API нет.

- [ ] **Step 1–6: тест, реализация, прогон, клиппи, документация, коммит**

---

### Task 5: Подсистема заданий и расписание

**Files:**
- Create: `crates/iaam-app/src/jobs.rs`
- Modify: `crates/iaam-app/src/lib.rs`
- Modify: `crates/iaam-server/src/lib.rs` (запуск планировщика)

**Acceptance Criteria:**
- **Раз в сутки после закрытия торгов, плюс ручной запуск.** Чаще незачем: дневная история внутри дня не меняется.
- **Скользящее окно исправлений**: каждый прогон перезапрашивает последние несколько недель по каждой серии. Брать только новые дни от границы полноты значило бы не увидеть исправление источника никогда — и тогда append-only теряет смысл.
- **Первичная загрузка — от даты первого события по инструменту в журнале**, с запасом на нерабочие дни. Не фиксированное окно: для давно купленной бумаги оно оборвало бы ряд раньше покупки.
- **Закрытая позиция выбывает из расписания**: прошлые наблюдения сохранены, будущие цены проданной бумаги не нужны никому. Возврат позиции возвращает и серию.
- Расписание **не является** движком процессов: ни DSL заданий, ни графа зависимостей. Синхронизация рынка — первое задание; `iaam-023.5` (брокер) позже сведётся к регистрации второго.
- Тесты не спят и не ходят в сеть: решение «пора ли запускать» — **чистая функция** от времени и состояния, как и политика повторов в `iaam-http::resilience`.

- [ ] **Step 1–6: тест, реализация, прогон, клиппи, коммит**

---

## Приёмка части 3 (оркестратор, один раз в конце)

```bash
nix develop -c make check
nix develop -c ./scripts/check-architecture.sh
nix develop -c ./scripts/check-fixtures.sh
nix develop -c cargo mutants -p iaam-core --file crates/iaam-core/src/valuation.rs --no-times
grep -rn "\.latest(" crates/ --include='*.rs' && echo "ПРОВАЛ" || echo "latest() удалён"
```
