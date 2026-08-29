# Владение с точностью расчётов — план реализации

> **Исполнителю:** ОБЯЗАТЕЛЬНАЯ ПОДСКИЛЛА — `beads-superpowers:subagent-driven-development`
> (рекомендуется) либо `beads-superpowers:executing-plans`. Каждая задача становится
> бидом (`bd create -t task --parent <epic-id>`). Шаги внутри задач — чекбоксы для чтения человеком.

**Цель:** сверка запланированных выплат обвиняет только тогда, когда владение на дату
фиксации реестра доказано, молчит только тогда, когда доказано его отсутствие, и во всех
остальных случаях честно признаётся, что проверить не может.

**Архитектура:** каждое событие, меняющее количество бумаги, несёт интервал возможной даты
расчёта; владение на дату выводится из консервативного диапазона возможного количества;
сверка идёт по одной выплате, без ранних возвратов, отдельным проходом по книге лотов.

**Стек:** Rust 2024, `nix develop -c cargo`, тесты `cargo test`/`cargo nextest`,
заслоны `make check`, `make mutants`.

**Спека:** `.internal/specs/2026-08-29-ownership-intervals-design.md`. Ссылки вида §3.2 —
на неё.

## Глобальные ограничения

- Все команды идут через `nix develop -c` — вне dev-оболочки тулчейн недоступен.
- `unsafe_code = "forbid"`, `clippy::all = "deny"` (`Cargo.toml`, `[workspace.lints]`).
  Новый `#[allow]` или `#[expect]` ловится `scripts/check-diff-lint.sh` и запрещён.
- Комментарии и сообщения об ошибках — по-русски, объясняют ПОЧЕМУ, а не что.
- Отсутствие данных выражается `Option`/явным вариантом, никогда — значением по умолчанию
  (§4.9 основной спеки). `#[serde(default)]` на новом поле состояния запрещён.
- Молчание допустимо ТОЛЬКО для доказанного `NotOwned`. Любая неопределённость выходит
  дефектной недоказуемостью (§4.2 спеки).
- Каждая задача заканчивается коммитом с идентификатором бида в сообщении.
- Порядок фаз обязателен: A → B → C. Внутри фазы задачи идут по номерам.

---

## Карта файлов

**Фаза A — `iaam-d8b.22`, приёмка Finam**

| Файл | Ответственность |
| --- | --- |
| `crates/iaam-ingest/src/report/finam.rs` | разбор XLS: дата расчётов в `settled`, `cash_posted` только из своей колонки |
| `crates/iaam-ingest/tests/report_finam.rs` | контракт: три даты различимы, отсутствие колонки не подставляет дату |

**Фаза B — `iaam-d8b.21`, сверка по одной выплате**

| Файл | Ответственность |
| --- | --- |
| `crates/iaam-core/src/returns/mod.rs` | форма `ScheduledPostingUnverifiable`, `reconcile_past_postings` без ранних возвратов |
| `crates/iaam-core/src/projection/state.rs` | `Coverage` хранит горизонт по счёту |
| `crates/iaam-server/src/dto.rs` | дата и вид в DTO недоказуемости |

**Фаза C — `iaam-d8b.14`, владение и дата права**

| Файл | Ответственность |
| --- | --- |
| `crates/iaam-core/src/bond/mod.rs` | `AccrualPeriod.record_date` |
| `crates/iaam-app/src/market_candidate.rs` | проводка `record_date` из хранилища в домен |
| `crates/iaam-core/src/rules/cashflow.rs` | `ScheduledPosting.entitlement`, `CashflowProjectionV2`, `historical_schedule_postings` |
| `crates/iaam-core/src/settlement.rs` | НОВЫЙ: `SettlementKnowledge`, `SettlementLagPolicy` |
| `crates/iaam-core/src/projection/ownership.rs` | НОВЫЙ: `OwnershipHistory`, диапазон количества, `ownership_at` |
| `crates/iaam-core/src/projection/lots.rs` | пополнение истории владения вместе с партиями |
| `crates/iaam-core/src/rules/posting_match.rs` | `PostingMatchV2`: исходы по владению и дате права |
| `crates/iaam-core/src/returns/mod.rs` | исторический проход по книге, агрегация причин источника |
| `scripts/check-mutants.sh` | новые модули в списке критичных |

---

# ФАЗА A — `iaam-d8b.22`

### Задача A1: Finam перестаёт выдумывать дату расчётов

**Файлы:**
- Изменить: `crates/iaam-ingest/src/report/finam.rs:30` (версия парсера), `:153-157`
  (подстановка), `:650-663` (конструктор операции)
- Тест: `crates/iaam-ingest/tests/report_finam.rs`

**Интерфейсы:**
- Отдаёт: `SubmittedOperation.dates` с раздельными `trade`, `settled`, `cash_posted`.
  Фаза C читает `settled` через `EventDates::settled`.

**Критерии приёмки:**
- `settlement_date` попадает в `EventDates.settled`, а не в `cash_posted`.
- Отчёт без колонки расчётов даёт `settled: None`, а не `settled == trade`.
- `cash_posted` заполняется только из собственной колонки движения денег; её нет — `None`.
- Версия парсера поднята до `finam-xls/2`.

- [ ] **Шаг 1: тест на три различимые даты**

В `crates/iaam-ingest/tests/report_finam.rs` добавить:

```rust
#[test]
fn the_settlement_date_reaches_settled_and_not_cash_posted() {
    // Дата расчётов и дата движения денег — разные факты (dates.rs:34,38).
    // Права переходят при расчётах, поэтому подмена одной другой ломает
    // не только сверку, но и отчётный период: effective_date() ставит
    // cash_posted выше trade.
    let parsed = разобрать_фикстуру("finam-three-dates.xls");
    let сделка = parsed.accepted.first().expect("сделка разобрана");
    assert_eq!(сделка.dates.trade, Some(date!(2026 - 03 - 10)));
    assert_eq!(сделка.dates.settled, Some(date!(2026 - 03 - 12)));
    assert_ne!(
        сделка.dates.cash_posted,
        сделка.dates.settled,
        "дата движения денег не обязана совпадать с датой расчётов"
    );
}

#[test]
fn a_report_without_a_settlement_column_admits_it_does_not_know() {
    // Отсутствие колонки — незнание, а не утверждение «расчёты в день
    // сделки». Подстановка выдумала бы факт (§4.9).
    let parsed = разобрать_фикстуру("finam-no-settlement-column.xls");
    let сделка = parsed.accepted.first().expect("сделка разобрана");
    assert_eq!(сделка.dates.settled, None);
}
```

- [ ] **Шаг 2: убедиться, что тесты падают**

Выполнить: `nix develop -c cargo test -p iaam-ingest --test report_finam settlement`
Ожидается: FAIL — `settled` равен `None` в первом тесте и `Some(trade)` во втором.

- [ ] **Шаг 3: развести даты в конструкторе**

В `crates/iaam-ingest/src/report/finam.rs` заменить сигнатуру `operation`:

```rust
fn operation(
    account: iaam_core::ids::AccountId,
    kind: OperationKind,
    trade_date: Date,
    // Дата расчётов, а не «денежная дата»: именно при расчётах
    // переходят права (dates.rs:34). None — колонки в отчёте нет,
    // и подставлять дату сделки нельзя: это утверждение, а не пропуск.
    settlement_date: Option<Date>,
    cash_date: Option<Date>,
    source_id: Option<&str>,
) -> SubmittedOperation {
    SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: Some(trade_date),
            settled: settlement_date,
            cash_posted: cash_date,
            ..OperationDates::default()
        },
        idempotency_key: None,
        source_operation_id: source_id.map(str::to_owned),
    }
}
```

- [ ] **Шаг 4: снять подстановку**

Там же, в разборе строки, заменить `.unwrap_or(trade_date)`:

```rust
            let settlement_date = settlement_date_col
                .map(|column| date_value(cell(row, column), "settlement_date"))
                .transpose()?;
```

и передавать `settlement_date` в `operation` отдельным аргументом.

- [ ] **Шаг 5: поднять версию парсера**

`crates/iaam-ingest/src/report/finam.rs:30`: `finam-xls/1` → `finam-xls/2`. Семантика
разбора изменилась, значит меняется и отпечаток строки.

- [ ] **Шаг 6: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-ingest`
Ожидается: PASS, включая оба новых теста.

- [ ] **Шаг 7: коммит**

```bash
git add crates/iaam-ingest/src/report/finam.rs crates/iaam-ingest/tests/report_finam.rs tests/fixtures/reports/
git commit -m "fix(ingest): Finam отдаёт дату расчётов, а не выдумывает её (iaam-d8b.22)"
```

---

# ФАЗА B — `iaam-d8b.21`

### Задача B1: недоказуемость называет выплату

**Файлы:**
- Изменить: `crates/iaam-core/src/returns/mod.rs:273` (форма проблемы),
  `crates/iaam-server/src/dto.rs`
- Тест: там же, модуль `tests` в `returns/mod.rs`

**Интерфейсы:**
- Отдаёт: `MaterialIssue::ScheduledPostingUnverifiable { account, instrument, date, kind, reason }`.
  Задачи B2, C7 и C9 конструируют именно эту форму.

**Критерии приёмки:**
- Проблема несёт дату и вид выплаты наравне с `ScheduledPostingNotReceived`.
- DTO выдаёт оба новых поля; контрактный тест их проверяет.
- Существующие тесты, сверяющие только причину, продолжают проходить.

- [ ] **Шаг 1: тест на форму**

```rust
#[test]
fn an_unverifiable_posting_names_which_posting_it_is() {
    // Без даты и вида нельзя объявить недоказуемой ОДНУ выплату:
    // причина уровня пары глушит соседние доказуемые (iaam-d8b.21).
    let issue = MaterialIssue::ScheduledPostingUnverifiable {
        account: AccountId::new_random(),
        instrument: InstrumentId::new_random(),
        date: date!(2026 - 06 - 15),
        kind: PostingKind::Coupon,
        reason: UnverifiableReason::AcquisitionDateUnknown,
    };
    assert!(issue.is_defect());
}
```

- [ ] **Шаг 2: убедиться, что не компилируется**

Выполнить: `nix develop -c cargo test -p iaam-core --lib an_unverifiable_posting_names`
Ожидается: ошибка компиляции — у варианта нет полей `date` и `kind`.

- [ ] **Шаг 3: расширить вариант и всех его конструкторов**

В `crates/iaam-core/src/returns/mod.rs` добавить `date: Date` и `kind: PostingKind`
в вариант. Компилятор укажет все места конструирования — исправить каждое, беря дату и
вид из обрабатываемой выплаты. Замыкание `unverifiable` в `reconcile_past_postings`
принимает выплату аргументом.

- [ ] **Шаг 4: провести поля до DTO**

В `crates/iaam-server/src/dto.rs` добавить `date` и `kind` в представление недоказуемости
рядом с полями `ScheduledPostingNotReceived`.

- [ ] **Шаг 5: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core -p iaam-server`
Ожидается: PASS.

- [ ] **Шаг 6: коммит**

```bash
git add crates/iaam-core/src/returns/mod.rs crates/iaam-server/src/dto.rs
git commit -m "feat(core): недоказуемость называет выплату, а не только пару (iaam-d8b.21)"
```

### Задача B2: сверка идёт по одной выплате

**Файлы:**
- Изменить: `crates/iaam-core/src/returns/mod.rs:1584-1655` (`reconcile_past_postings`)
- Тест: там же

**Интерфейсы:**
- Потребляет: форму из B1.
- Отдаёт: `reconcile_past_postings`, возвращающая проблемы по каждой выплате отдельно.
  Задача C7 подключает к ней проверку владения.

**Критерии приёмки:**
- Выплата раньше горизонта журнала даёт `HistoryStartsAfterSchedule` только для себя.
- Доказуемый пропуск после горизонта выдаётся, даже если раньше горизонта есть выплаты.
- Неизвестный вид дохода и неизвестная дата приобретения тоже перестают глушить пару.
- Статус качества отражает доказуемый пропуск.

- [ ] **Шаг 1: тест на дефект**

```rust
#[test]
fn a_posting_before_the_journal_does_not_silence_a_provable_miss_after_it() {
    // Восстановленная позиция 2021 года, журнал с 01.01.2026,
    // купоны 15.12.2025 и 15.03.2026, мартовский не пришёл.
    // Ранний возврат объявлял недоказуемой ВСЮ пару, и мартовский
    // пропуск исчезал вместе со статусом качества.
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let schedule = график_купонов(
        &[date!(2025 - 12 - 15), date!(2026 - 03 - 15)],
        date!(2026 - 12 - 15),
    );
    let events = vec![восстановленная_позиция(
        account,
        instrument,
        date!(2026 - 01 - 01),
        date!(2021 - 05 - 01),
    )];
    let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

    let даты: Vec<_> = непринятые(&report)
        .into_iter()
        .filter_map(|issue| match issue {
            MaterialIssue::ScheduledPostingNotReceived { date, .. } => Some(*date),
            _ => None,
        })
        .collect();
    assert!(
        даты.contains(&date!(2026 - 03 - 15)),
        "пропуск внутри покрытия обязан быть назван: {даты:?}"
    );
    assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-core --lib does_not_silence_a_provable_miss`
Ожидается: FAIL — список непринятых пуст, статус `Complete`.

- [ ] **Шаг 3: переписать функцию на разбор по выплате**

```rust
fn reconcile_past_postings(inputs: &PastReconciliationInputs<'_>) -> Vec<MaterialIssue> {
    let PastReconciliationInputs { key, plan, lots, income, history_starts, rule, as_of } = *inputs;

    // Причина, названная один раз для всей пары, глушила выплаты,
    // по которым ответ доказуем (iaam-d8b.21). Поэтому кандидаты
    // сначала собираются целиком, и каждый получает свой исход.
    let mut issues = Vec::new();
    let mut verifiable = Vec::new();

    let gap = income.gap(&key);
    let acquired = lots.and_then(crate::projection::lots::InstrumentLots::earliest_acquired);

    for posting in plan.past.iter().copied().filter(|p| rule.is_due(p, as_of)) {
        if let Some(gap) = gap {
            issues.push(недоказуемо(key, posting, причина_пробела(gap)));
            continue;
        }
        let Some(acquired) = acquired else {
            issues.push(недоказуемо(key, posting, UnverifiableReason::AcquisitionDateUnknown));
            continue;
        };
        if posting.date < acquired.0 {
            // Купон за период, когда бумаги ещё не было, владельцу
            // не причитается: молчим, а не обвиняем.
            continue;
        }
        if history_starts.is_some_and(|start| posting.date < start) {
            issues.push(недоказуемо(key, posting, UnverifiableReason::HistoryStartsAfterSchedule));
            continue;
        }
        verifiable.push(posting);
    }

    issues.extend(
        rule.unreceived(&verifiable, income.postings(&key), as_of)
            .into_iter()
            .map(|posting| MaterialIssue::ScheduledPostingNotReceived {
                account: key.account,
                instrument: key.instrument,
                date: posting.date,
                kind: posting.kind,
            }),
    );
    issues
}
```

- [ ] **Шаг 4: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core -p iaam-server`
Ожидается: PASS. Тест
`a_restored_history_reports_that_it_cannot_verify_rather_than_crying_wolf` продолжает
проходить: выплаты 2021–2025 по-прежнему недоказуемы, но каждая называется своей.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/returns/mod.rs
git commit -m "fix(core): сверка идёт по одной выплате, а не глушит пару (iaam-d8b.21)"
```

### Задача B3: горизонт истории по счёту

**Файлы:**
- Изменить: `crates/iaam-core/src/projection/state.rs:44-95` (`Coverage`),
  `crates/iaam-core/src/returns/mod.rs:1664`
- Тест: `crates/iaam-core/src/projection/state.rs`, модуль `tests`

**Интерфейсы:**
- Отдаёт: `Coverage::first_event_for(account) -> Option<Date>`. Задача C8 вызывает её
  вместо глобальной `first_event()`.

**Критерии приёмки:**
- Счёт, впервые появившийся в 2026 году, не наследует горизонт 2020 года от другого счёта.
- Глобальная `first_event()` сохраняется: она используется в покрытии отчёта (§10.7).
- `PROJECTION_VERSION` поднимается здесь, если форма `Coverage` меняется.

- [ ] **Шаг 1: тест**

```rust
#[test]
fn each_account_carries_its_own_history_horizon() {
    // Глобальный горизонт объявлял бы историю счёта B покрытой
    // с 2020 года только потому, что счёт A существует с 2020-го.
    let a = AccountId::new_random();
    let b = AccountId::new_random();
    let mut coverage = Coverage::default();
    coverage.observe(&событие(a, date!(2020 - 01 - 15)));
    coverage.observe(&событие(b, date!(2026 - 01 - 01)));

    assert_eq!(coverage.first_event_for(a), Some(date!(2020 - 01 - 15)));
    assert_eq!(coverage.first_event_for(b), Some(date!(2026 - 01 - 01)));
    assert_eq!(coverage.first_event(), Some(date!(2020 - 01 - 15)));
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-core --lib each_account_carries_its_own`
Ожидается: ошибка компиляции — метода `first_event_for` нет.

- [ ] **Шаг 3: хранить горизонт по счёту**

В `Coverage` добавить `first_event_by_account: BTreeMap<AccountId, Date>`, заполнять в
`observe` по счёту события, глобальный `first_event` оставить как есть.

- [ ] **Шаг 4: подключить в сверке**

`crates/iaam-core/src/returns/mod.rs:1664`: `history_starts` берётся по счёту пары.

- [ ] **Шаг 5: поднять версию проекции**

`crates/iaam-core/src/projection/mod.rs:37`: `PROJECTION_VERSION` 4 → 5. Форма
сериализованного `Coverage` изменилась.

- [ ] **Шаг 6: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core -p iaam-store -p iaam-app`
Ожидается: PASS; тесты снимков подтверждают отказ читать снимок версии 4.

- [ ] **Шаг 7: коммит**

```bash
git add crates/iaam-core/src/projection/state.rs crates/iaam-core/src/projection/mod.rs crates/iaam-core/src/returns/mod.rs
git commit -m "fix(core): горизонт истории считается по счёту (iaam-d8b.21)"
```

---

# ФАЗА C — `iaam-d8b.14`

### Задача C1: дата фиксации доезжает до домена

**Файлы:**
- Изменить: `crates/iaam-core/src/bond/mod.rs:32-43` (`AccrualPeriod`),
  `crates/iaam-app/src/market_candidate.rs:95`
- Тест: `crates/iaam-app/src/market_candidate.rs`, модуль `tests`

**Интерфейсы:**
- Отдаёт: `AccrualPeriod.record_date: Option<Date>`. Задача C2 переносит её в
  `ScheduledPosting.entitlement`.

**Критерии приёмки:**
- Снимок графика с датой фиксации доводит её до `AccrualPeriod`.
- Снимок без даты фиксации даёт `None`, а не подстановку `payment_date`.

- [ ] **Шаг 1: тест**

```rust
#[test]
fn the_record_date_survives_the_trip_from_the_snapshot() {
    // Дата регламентирована и уже лежит в хранилище
    // (iaam-store/src/schedule.rs), терялась только здесь.
    let snapshot = снимок_с_купоном(Some("2026-06-14"), "2026-06-15");
    let periods = accrual_periods_from_snapshot(&snapshot).expect("периоды построены");
    assert_eq!(periods[0].record_date, Some(date!(2026 - 06 - 14)));
}

#[test]
fn a_snapshot_without_a_record_date_says_so() {
    let snapshot = снимок_с_купоном(None, "2026-06-15");
    let periods = accrual_periods_from_snapshot(&snapshot).expect("периоды построены");
    assert_eq!(
        periods[0].record_date, None,
        "подстановка даты платежа выдала бы предположение за факт"
    );
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-app record_date`
Ожидается: ошибка компиляции — поля нет.

- [ ] **Шаг 3: добавить поле**

```rust
pub struct AccrualPeriod {
    pub period_start: Date,
    pub accrual_end: Date,
    pub payment_date: Date,
    /// Дата фиксации реестра — она решает, КОМУ платят.
    ///
    /// `None` означает «источник не сообщил», и подставлять вместо неё
    /// дату платежа запрещено: зазор между ними непостоянен (0–5 дней
    /// по фикстурам), и в 157 случаях из 275 он равен одному дню —
    /// ровно тем дням, когда сделка меняет ответ.
    pub record_date: Option<Date>,
    pub coupon_per_unit: Option<PerUnitAmount>,
}
```

- [ ] **Шаг 4: заполнить в проводке**

В `crates/iaam-app/src/market_candidate.rs` разобрать `row.record_date` тем же способом,
что и остальные даты, и положить в новое поле.

- [ ] **Шаг 5: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core -p iaam-app -p iaam-market`
Ожидается: PASS.

- [ ] **Шаг 6: коммит**

```bash
git add crates/iaam-core/src/bond/mod.rs crates/iaam-app/src/market_candidate.rs
git commit -m "feat(core): дата фиксации доезжает до AccrualPeriod (iaam-d8b.14)"
```

### Задача C2: план выплат несёт дату права

**Файлы:**
- Изменить: `crates/iaam-core/src/rules/cashflow.rs:78` (`ScheduledPosting`),
  `crates/iaam-core/src/rules/mod.rs:76` (реестр)

**Интерфейсы:**
- Потребляет: `AccrualPeriod.record_date` из C1.
- Отдаёт: `ScheduledPosting { date, kind, entitlement: Option<Date> }` и
  `CashflowProjectionV2`. Задачи C3 и C7 работают с этой формой.

**Критерии приёмки:**
- Купон несёт дату фиксации из графика.
- `CashflowProjectionV2` зарегистрирован в `RuleRegistry::with_defaults`.
- `CashflowProjectionV1` остаётся в реестре: снимки и отчёты, построенные им, читаются.

- [ ] **Шаг 1: тест**

```rust
#[test]
fn a_scheduled_coupon_carries_the_entitlement_date_from_the_schedule() {
    let schedule = график_с_фиксацией(date!(2026 - 06 - 15), Some(date!(2026 - 06 - 14)));
    let plan = CashflowProjectionV2.project(&вход(&schedule)).expect("поток построен");
    let купон = plan.past.iter().find(|p| p.kind == PostingKind::Coupon).expect("купон");
    assert_eq!(купон.entitlement, Some(date!(2026 - 06 - 14)));
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-core --lib carries_the_entitlement_date`
Ожидается: ошибка компиляции.

- [ ] **Шаг 3: добавить поле и версию правила**

Поле `entitlement: Option<Date>` в `ScheduledPosting`; `CashflowProjectionV2` копирует
логику V1 и заполняет новое поле; в `RuleRegistry::with_defaults` добавить
`cashflow_rules.insert(CashflowProjectionVersion(2), Box::new(CashflowProjectionV2));`.

- [ ] **Шаг 4: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core`
Ожидается: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/rules/cashflow.rs crates/iaam-core/src/rules/mod.rs
git commit -m "feat(core): CashflowProjectionV2 несёт дату права на выплату (iaam-d8b.14)"
```

### Задача C3: прошлые выплаты строятся из графика

**Файлы:**
- Изменить: `crates/iaam-core/src/rules/cashflow.rs`

**Интерфейсы:**
- Отдаёт: `historical_schedule_postings(schedule: &BondSchedule, as_of: Date) -> Result<Vec<ScheduledPosting>, ScheduleTrustError>`.
  Задача C8 вызывает её вместо `scenario_plan`.

**Критерии приёмки:**
- Функция не требует номинала, `PrincipalState`, количества и НКД.
- График не `Validated` → отказ с причиной, а не пустой список.
- Объявленный дефолт выпуска → отказ с причиной.
- Прошлые выплаты те же, что даёт `CashflowProjectionV2.past` при известном номинале.

- [ ] **Шаг 1: тест на независимость от номинала**

```rust
#[test]
fn past_postings_do_not_need_the_face_value() {
    // Сверке нужны даты и виды, а не суммы. Пока прошлое строилось
    // сценарием, неизвестный номинал молча выключал сверку целиком
    // (iaam-d8b.15).
    let schedule = график_купонов(&[date!(2026 - 03 - 15)], date!(2026 - 12 - 15));
    let postings = historical_schedule_postings(&schedule, date!(2026 - 08 - 26))
        .expect("прошлое построено без номинала");
    assert_eq!(postings.len(), 1);
    assert_eq!(postings[0].kind, PostingKind::Coupon);
}

#[test]
fn an_incomplete_schedule_refuses_instead_of_returning_nothing() {
    // Пустой список неотличим от «выплат не было» и погасил бы
    // сверку молча; обвинение по неполному ряду обвиняло бы
    // по выдуманному графику.
    let mut schedule = график_купонов(&[date!(2026 - 03 - 15)], date!(2026 - 12 - 15));
    schedule.completeness = crate::bond::ScheduleCompleteness::Unknown;
    assert!(historical_schedule_postings(&schedule, date!(2026 - 08 - 26)).is_err());
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-core --lib past_postings_do_not_need`
Ожидается: ошибка компиляции — функции нет.

- [ ] **Шаг 3: реализовать**

Вынести из `CashflowProjectionV2` ветви построения прошлых купонов и возвратов; перед
построением проверить `ScheduleCompleteness::Validated` и `DefaultFlags.declared`,
вернув `ScheduleTrustError` с причиной. Номинал, количество и НКД не читать.

- [ ] **Шаг 4: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core`
Ожидается: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/rules/cashflow.rs
git commit -m "feat(core): прошлые выплаты строятся из графика, без номинала (iaam-d8b.14)"
```

### Задача C4: знание о дате расчёта

**Файлы:**
- Создать: `crates/iaam-core/src/settlement.rs`
- Изменить: `crates/iaam-core/src/lib.rs` (объявление модуля)

**Интерфейсы:**
- Отдаёт:
  ```rust
  pub enum SettlementKnowledge {
      Exact(Date),
      Bounded { earliest: Date, latest: Date },
      Unbounded,
  }
  impl SettlementKnowledge {
      pub fn applied_before(&self, day: Date) -> Applied; // Yes | No | Maybe
  }
  pub struct SettlementLagPolicyVersion(pub u32);
  pub struct SettlementLagPolicy { /* профиль -> максимум календарных дней */ }
  ```
  Задача C5 строит по ним диапазон количества.

**Критерии приёмки:**
- Интервал замкнут с обоих концов; «точно применилось» — строго `latest < day`.
- `Exact(d)` ведёт себя как `[d, d]`: на самой `d` ответ `Maybe`, не `Yes`.
- `Unbounded` даёт `Maybe` на любой день.
- Полоса берётся из политики по профилю источника, единого числа на всех нет.
- Начальное заполнение политики соответствует таблице §3.4: Finam с заполненным `settled`
  — `Exact`; профиль с контрактным верхним пределом — `Bounded`; T-Invest API и
  произвольный CSV — `Unbounded`. «Обычно T+1» основанием для `Bounded` не является:
  нужна верхняя граница, включая выходные и праздники.

- [ ] **Шаг 1: тест границы дня**

```rust
#[test]
fn an_exact_date_is_a_degenerate_closed_interval() {
    // Внутридневного времени нет, поэтому расчёт ровно в d возможен,
    // и объявить его состоявшимся до d нельзя. Асимметрия между
    // Exact(d) и Bounded{latest: d} была бы неоправданной: верхняя
    // календарная граница у них одна.
    let знание = SettlementKnowledge::Exact(date!(2026 - 06 - 10));
    assert_eq!(знание.applied_before(date!(2026 - 06 - 09)), Applied::No);
    assert_eq!(знание.applied_before(date!(2026 - 06 - 10)), Applied::Maybe);
    assert_eq!(знание.applied_before(date!(2026 - 06 - 11)), Applied::Yes);
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-core --lib degenerate_closed_interval`
Ожидается: ошибка компиляции.

- [ ] **Шаг 3: реализовать модуль**

```rust
pub fn applied_before(&self, day: Date) -> Applied {
    match *self {
        Self::Exact(date) => Self::judge(date, date, day),
        Self::Bounded { earliest, latest } => Self::judge(earliest, latest, day),
        // Смысл даты источника не доказан: событие могло примениться
        // когда угодно, и утверждать обратное нечем.
        Self::Unbounded => Applied::Maybe,
    }
}

const fn judge(earliest: Date, latest: Date, day: Date) -> Applied {
    // Строгое `<`: интервал замкнут, расчёт ровно в latest возможен.
    if latest < day {
        Applied::Yes
    } else if day < earliest {
        Applied::No
    } else {
        Applied::Maybe
    }
}
```

- [ ] **Шаг 4: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core --lib settlement`
Ожидается: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/settlement.rs crates/iaam-core/src/lib.rs
git commit -m "feat(core): знание о дате расчёта с замкнутым интервалом (iaam-d8b.14)"
```

### Задача C5: владение из диапазона количества

**Файлы:**
- Создать: `crates/iaam-core/src/projection/ownership.rs`
- Изменить: `crates/iaam-core/src/projection/lots.rs` (пополнение истории),
  `crates/iaam-core/src/projection/mod.rs` (объявление модуля)

**Интерфейсы:**
- Потребляет: `SettlementKnowledge` из C4.
- Отдаёт: `OwnershipHistory::ownership_at(day) -> Ownership` где
  `Ownership = Owned | NotOwned | Unknown`. Задача C7 вызывает её.

**Критерии приёмки:**
- Перекрывающиеся полосы дают `Unknown`, а не `Owned` (контрпример §3.6).
- `Owned` только при минимально возможном количестве больше нуля.
- `NotOwned` только при максимально возможном количестве равном нулю.
- Восстановленное количество (`unpriced`) входит в подсчёт.

- [ ] **Шаг 1: тест на контрпример**

```rust
#[test]
fn overlapping_settlement_windows_are_not_proof_of_ownership() {
    // На руках 1 бумага. 10 марта покупка с полосой [10,12],
    // 11 марта продажа с полосой [11,13]. Журнальное количество
    // идёт 1 -> 2 -> 1 и через ноль не переходит, но расчёты могли
    // лечь в обратном порядке и дать фактический ноль.
    let mut история = OwnershipHistory::default();
    история.observe(Quantity(dec("1")), SettlementKnowledge::Exact(date!(2026 - 03 - 01)));
    история.observe(
        Quantity(dec("1")),
        SettlementKnowledge::Bounded { earliest: date!(2026 - 03 - 10), latest: date!(2026 - 03 - 12) },
    );
    история.observe(
        Quantity(dec("-1")),
        SettlementKnowledge::Bounded { earliest: date!(2026 - 03 - 11), latest: date!(2026 - 03 - 13) },
    );

    assert_eq!(история.ownership_at(date!(2026 - 03 - 11)), Ownership::Unknown);
    assert_eq!(история.ownership_at(date!(2026 - 03 - 12)), Ownership::Unknown);
    assert_eq!(история.ownership_at(date!(2026 - 03 - 05)), Ownership::Owned);
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-core --lib overlapping_settlement_windows`
Ожидается: ошибка компиляции.

- [ ] **Шаг 3: реализовать диапазон**

```rust
pub fn ownership_at(&self, day: Date) -> Ownership {
    let mut минимум = Dec::zero();
    let mut максимум = Dec::zero();
    for change in &self.changes {
        match change.settlement.applied_before(day) {
            // Точно применилось — двигает обе границы.
            Applied::Yes => { минимум += change.delta; максимум += change.delta; }
            Applied::No => {}
            // Могло примениться: выбытие идёт в минимум,
            // приобретение — в максимум. Оценка сознательно
            // пессимистична с обеих сторон.
            Applied::Maybe => {
                if change.delta.is_negative() { минимум += change.delta; }
                else { максимум += change.delta; }
            }
        }
    }
    if минимум > Dec::zero() { Ownership::Owned }
    else if максимум.is_zero() { Ownership::NotOwned }
    else { Ownership::Unknown }
}
```

- [ ] **Шаг 4: пополнять историю вместе с партиями**

В `crates/iaam-core/src/projection/lots.rs` вызывать `observe` во всех местах, меняющих
количество: `push_lot`, восстановленное количество, `dispose`. Знание о расчёте строится
из `event.dates.settled` (`Exact`), иначе из `event.dates.trade` и политики полос
(`Bounded`), иначе `Unbounded`.

- [ ] **Шаг 5: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core`
Ожидается: PASS.

- [ ] **Шаг 6: коммит**

```bash
git add crates/iaam-core/src/projection/ownership.rs crates/iaam-core/src/projection/lots.rs crates/iaam-core/src/projection/mod.rs
git commit -m "feat(core): владение выводится из диапазона возможного количества (iaam-d8b.14)"
```

### Задача C6: восстановленная позиция через уверенность в дате

**Файлы:**
- Изменить: `crates/iaam-core/src/projection/lots.rs:1007`,
  `crates/iaam-server/src/dto.rs:414`

**Критерии приёмки:**
- Дата берётся из `OpeningAssertions.acquisition_date` при `DateCertainty::Known`.
- `Estimated` и `Unknown` дают `SettlementKnowledge::Unbounded`.
- DTO принимает `assertions`; тест идёт через контракт, а не ручную сборку `Event`.

- [ ] **Шаг 1: тест**

```rust
#[test]
fn an_estimated_acquisition_date_does_not_prove_ownership() {
    // Оценка — не доказательство. Приписать ей Known значило бы
    // задним числом объявить документированным то, чего никто не видел.
    let событие = восстановление_через_контракт(DateCertainty::Estimated, date!(2021 - 05 - 01));
    let состояние = проекция(&[событие]);
    let история = состояние.book().entry(&ключ).expect("запись").ownership();
    assert_eq!(история.ownership_at(date!(2022 - 06 - 15)), Ownership::Unknown);
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-server --test contract estimated_acquisition`
Ожидается: FAIL — DTO не принимает `assertions`.

- [ ] **Шаг 3: провести assertions через DTO и читать их в книге лотов**

В `crates/iaam-server/src/dto.rs:414` заменить принудительный `assertions: None` на разбор
поля запроса; отсутствие поля даёт `OpeningAssertions::default()`, где по каждому пункту
стоит «неизвестно» — это не заглушка, а правда: событие, записанное без утверждений,
действительно ничего не утверждало.

В `crates/iaam-core/src/projection/lots.rs:1007` брать дату не из `event.dates.trade`,
а из утверждений:

```rust
// Дата из утверждений, а не из дат события: у восстановления
// нет сделки, и trade там — то, что записал импортёр, а не то,
// что владелец утверждает о происхождении позиции.
let знание = match (assertions.acquisition_date, assertions.acquisition_date_certainty) {
    (Some(day), DateCertainty::Known) => SettlementKnowledge::Exact(day),
    // Оценка не превращается в доказанное начало: непрерывность
    // владения до открытия журнала недоказуема в принципе (§3.5).
    _ => SettlementKnowledge::Unbounded,
};
```

- [ ] **Шаг 4: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core -p iaam-server`
Ожидается: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/projection/lots.rs crates/iaam-server/src/dto.rs
git commit -m "feat(core): восстановленная позиция уважает уверенность в дате (iaam-d8b.14)"
```

### Задача C7: `PostingMatchV2` судит по дате права

**Файлы:**
- Изменить: `crates/iaam-core/src/rules/posting_match.rs`,
  `crates/iaam-core/src/returns/mod.rs` (новые причины)

**Интерфейсы:**
- Потребляет: `ScheduledPosting.entitlement` (C2), `ownership_at` (C5).
- Отдаёт: `PostingMatchV2` и причины `OwnershipUnknown`, `EntitlementDateUnknown`.

**Критерии приёмки:**
- Таблица исходов §4.2 выполняется во всех четырёх строках.
- `Final` у `finality_of` заполняет `entitlement` датой возврата; `Partial` и `Unknown`
  дают `EntitlementDateUnknown`.
- Молчание выдаётся только при доказанном `NotOwned`.

- [ ] **Шаг 1: тест на все четыре исхода**

```rust
#[test]
fn the_four_outcomes_of_a_due_posting() {
    let фиксация = date!(2026 - 06 - 14);
    let выплата = ScheduledPosting {
        date: date!(2026 - 06 - 15),
        kind: PostingKind::Coupon,
        entitlement: Some(фиксация),
    };

    // Владел на дату фиксации, факта нет — обвинение.
    assert_eq!(
        PostingMatchV2::new().judge(&выплата, Ownership::Owned, &[]),
        Verdict::NotReceived
    );
    // Не владел — выплата не причиталась, молчим.
    assert_eq!(
        PostingMatchV2::new().judge(&выплата, Ownership::NotOwned, &[]),
        Verdict::Silent
    );
    // Владение недоказуемо — признаёмся, а не обвиняем.
    assert_eq!(
        PostingMatchV2::new().judge(&выплата, Ownership::Unknown, &[]),
        Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown)
    );
    // Даты фиксации нет — судить по дате платежа запрещено:
    // зазор в 157 случаях из 275 равен одному дню.
    let без_фиксации = ScheduledPosting { entitlement: None, ..выплата };
    assert_eq!(
        PostingMatchV2::new().judge(&без_фиксации, Ownership::Owned, &[]),
        Verdict::Unverifiable(UnverifiableReason::EntitlementDateUnknown)
    );
}

#[test]
fn an_incomplete_amortisation_series_is_unknown_not_partial() {
    // finality_of даёт три исхода. Подставлять Partial по умолчанию
    // запрещено: неполный ряд долей — это незнание, а не частичность.
    let schedule = график_с_долями(&[dec("30"), dec("30")]); // 60%, не 100%
    let выплата = историческая_выплата(&schedule, PostingKind::PrincipalReturn);
    assert_eq!(выплата.entitlement, None);
    assert_eq!(
        PostingMatchV2::new().judge(&выплата, Ownership::Owned, &[]),
        Verdict::Unverifiable(UnverifiableReason::EntitlementDateUnknown)
    );
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-core --lib the_four_outcomes_of_a_due_posting`
Ожидается: ошибка компиляции — `PostingMatchV2` и `Verdict` не объявлены.

- [ ] **Шаг 3: реализовать `PostingMatchV2`**

```rust
pub enum Verdict {
    NotReceived,
    Silent,
    Unverifiable(UnverifiableReason),
}

impl PostingMatchV2 {
    pub fn judge(
        &self,
        posting: &ScheduledPosting,
        ownership: Ownership,
        facts: &[ReceivedPosting],
    ) -> Verdict {
        // Дата права проверяется ПЕРВОЙ: без неё вопрос о владении
        // не имеет смысла — неизвестно, на какой день смотреть.
        let Some(entitlement) = posting.entitlement else {
            return Verdict::Unverifiable(UnverifiableReason::EntitlementDateUnknown);
        };
        let _ = entitlement;
        match ownership {
            // Молчание допустимо ТОЛЬКО здесь: владение доказано
            // отсутствующим, значит выплата не причиталась.
            Ownership::NotOwned => Verdict::Silent,
            Ownership::Unknown => {
                Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown)
            }
            Ownership::Owned if self.confirmed(posting, facts) => Verdict::Silent,
            Ownership::Owned => Verdict::NotReceived,
        }
    }
}
```

Заполнение `entitlement` для возврата номинала — в `historical_schedule_postings`
(задача C3): `finality_of` даёт `Final` → дата возврата, `Partial`/`Unknown`/ошибка →
`None`, и тогда `judge` вернёт `EntitlementDateUnknown`.

- [ ] **Шаг 4: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core`
Ожидается: PASS.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/rules/posting_match.rs crates/iaam-core/src/returns/mod.rs
git commit -m "feat(core): PostingMatchV2 судит владение на дату фиксации (iaam-d8b.14)"
```

### Задача C8: исторический проход по книге лотов

**Файлы:**
- Изменить: `crates/iaam-core/src/returns/mod.rs:1660-1725`

**Критерии приёмки:**
- Полностью проданная бумага попадает в сверку.
- `reconciled: BTreeSet<LotKey>` удалён вместе с надобностью.
- Метрики по местам хранения не изменились: их обход остался прежним.
- Прошлое строится `historical_schedule_postings`, а не `scenario_plan`.

- [ ] **Шаг 1: тест на проданную бумагу**

```rust
#[test]
fn a_bond_sold_before_as_of_is_still_reconciled() {
    // Обход по позициям отсекал нулевое количество ДО сверки,
    // поэтому купон за период, когда бумага была на руках,
    // не проверялся ничем.
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let schedule = график_купонов(&[date!(2026 - 03 - 15)], date!(2026 - 12 - 15));
    let custody = CustodyId::new_random();
    let events = vec![
        пополнение(account, date!(2026 - 01 - 05)),
        покупка_облигации_в_депозитарии(account, instrument, date!(2026 - 01 - 10), custody, 2),
        // Продажа ПОСЛЕ купона: на дату фиксации бумага была на руках.
        продажа_облигации(account, instrument, date!(2026 - 05 - 20), Some(date!(2026 - 05 - 20)), custody, 3),
    ];
    let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

    let даты: Vec<_> = непринятые(&report)
        .into_iter()
        .filter_map(|issue| match issue {
            MaterialIssue::ScheduledPostingNotReceived { date, .. } => Some(*date),
            _ => None,
        })
        .collect();
    assert!(
        даты.contains(&date!(2026 - 03 - 15)),
        "купон по проданной позже бумаге обязан быть сверен: {даты:?}"
    );
}

#[test]
fn one_instrument_in_two_custodies_reports_the_problem_once() {
    // Раньше дедуп держался отметкой reconciled: BTreeSet<LotKey>
    // в обходе позиций. Проход по книге лотов делает её ненужной,
    // но свойство обязано сохраниться.
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let schedule = график_купонов(&[date!(2026 - 03 - 15)], date!(2026 - 12 - 15));
    let events = журнал_в_двух_депозитариях(account, instrument);
    let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

    assert_eq!(непринятые(&report).len(), 1, "одна проблема, а не по одной на депозитарий");
}
```

- [ ] **Шаг 2: убедиться, что падает**

Выполнить: `nix develop -c cargo test -p iaam-core --lib sold_before_as_of_is_still_reconciled`
Ожидается: FAIL — список непринятых пуст: до сверки дело не дошло.

- [ ] **Шаг 3: вынести проход**

В `returns_report` добавить отдельный проход ДО обхода позиций:

```rust
// Сверка прошлого идёт по книге лотов, а не по позициям: полностью
// проданная бумага в книге остаётся, а из позиций исчезает вместе
// с пропущенной по ней выплатой. Метрики по-прежнему считаются
// по позициям с местом хранения — здесь только проблемы.
let mut issues = Vec::new();
for (key, lots) in state.book().iter() {
    if !request.contour.contains(key.account) {
        continue;
    }
    let Some(schedule) = request.bond_schedules.get(&key.instrument) else {
        continue;
    };
    // Прошлое строится из графика: сценарий требует номинала,
    // и его отказ молча выключал сверку (iaam-d8b.15).
    let past = match historical_schedule_postings(schedule, request.as_of) {
        Ok(past) => past,
        Err(reason) => {
            issues.extend(недоказуемо_по_графику(*key, schedule, reason));
            continue;
        }
    };
    issues.extend(reconcile_past_postings(&PastReconciliationInputs {
        key: *key,
        past: &past,
        lots: Some(lots),
        income: state.income(),
        history_starts: state.coverage().first_event_for(key.account),
        rule: &posting_match,
        as_of: request.as_of,
    }));
}
```

Отметку `reconciled: BTreeSet<LotKey>` и вызов `reconcile_past_postings` из обхода
позиций удалить: ключ книги и так один на пару.

- [ ] **Шаг 4: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core -p iaam-server`
Ожидается: PASS, включая существующий тест двух депозитариев.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-core/src/returns/mod.rs
git commit -m "feat(core): сверка прошлого идёт по книге лотов (iaam-d8b.14)"
```

### Задача C9: агрегация причин уровня источника

**Файлы:**
- Изменить: `crates/iaam-core/src/returns/mod.rs`

**Критерии приёмки:**
- Одинаковые причины уровня источника сворачиваются по ключу
  `(счёт, инструмент, профиль, причина)` и несут число выплат и диапазон дат.
- Свёртка идёт ПОСЛЕ расчёта каждой выплаты: `Unbounded`-событие на малое количество
  не делает неизвестными выплаты, где минимум всё равно положителен.
- Недоказуемые выплаты остаются в сопоставлении: их факт не закрывает соседнюю выплату.
- Свёрнутая проблема не прекращает обход пары.

- [ ] **Шаг 1: тест «факт не уходит соседу»**

```rust
#[test]
fn an_unverifiable_posting_still_consumes_its_own_fact() {
    // Сопоставитель расходует факты жадно и однократно
    // (posting_match.rs:79). Убрать недоказуемую выплату до
    // сопоставления значит отдать её факт соседней выплате
    // и закрыть настоящий пропуск.
    //
    // График: купоны 15.03 и 15.06. Факт один — 16.03.
    // Владение на 15.03 недоказуемо, на 15.06 доказано.
    // Верный ответ: 15.03 недоказуема, 15.06 НЕ ПОЛУЧЕНА.
    // Неверный: факт 16.03 уходит июньскому купону и гасит пропуск.
    let report = отчёт_с_недоказуемым_мартом();

    let непринятые_даты: Vec<_> = непринятые(&report)
        .into_iter()
        .filter_map(|issue| match issue {
            MaterialIssue::ScheduledPostingNotReceived { date, .. } => Some(*date),
            _ => None,
        })
        .collect();
    assert_eq!(
        непринятые_даты,
        vec![date!(2026 - 06 - 15)],
        "мартовский факт обязан остаться за мартовской выплатой"
    );
}

#[test]
fn one_bad_source_does_not_make_every_posting_unknown() {
    // При точном количестве 100 и неограниченно датированной
    // продаже 10 минимум равен 90: владение доказано, сворачивать
    // нечего. Сворачиваются только выплаты, фактически получившие
    // Unknown, а не вся пара при обнаружении плохого события.
    let report = отчёт_с_крупной_позицией_и_мелкой_безымянной_продажей();
    assert!(
        !содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            }
        )),
        "минимум положителен — владение доказано"
    );
}

#[test]
fn an_aggregated_cause_does_not_swallow_a_provable_miss() {
    // Пара может нести свёрнутую недоказуемость И доказуемый
    // пропуск на другой дате одновременно.
    let report = отчёт_со_свёрнутой_причиной_и_пропуском();
    assert!(!непринятые(&report).is_empty());
    assert!(содержит(&report, |issue| matches!(
        issue,
        MaterialIssue::ScheduledPostingUnverifiable { .. }
    )));
}
```

- [ ] **Шаг 2: убедиться, что падают**

Выполнить: `nix develop -c cargo test -p iaam-core --lib still_consumes_its_own_fact`
Ожидается: FAIL на первом же тесте.

- [ ] **Шаг 3: реализовать свёртку после расчёта**

Сопоставление получает ВСЕ выплаты, срок которых наступил, включая недоказуемые:
недоказуемость меняет вывод по выплате, но не выводит её из очереди на факт. Свёртка
идёт последним шагом, уже над готовым списком проблем, по ключу
`(счёт, инструмент, профиль источника, причина)`, и заменяет группу одинаковых
недоказуемостей одной с числом выплат и диапазоном дат.

- [ ] **Шаг 4: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core -p iaam-server`
Ожидается: PASS.
- [ ] **Шаг 5: коммит**

```bash
git commit -m "feat(core): причины уровня источника сворачиваются после расчёта (iaam-d8b.14)"
```

### Задача C10: версии входов и применённых правил

**Файлы:**
- Изменить: `crates/iaam-core/src/returns/mod.rs:788` (тег отпечатка), `:969` (`AppliedRules`)

**Критерии приёмки:**
- Тег отпечатка входов поднят до `iaam/returns-inputs/v2`: форма `BondSchedule` изменилась.
- `AppliedRules` отдаёт фактически применённые версии, а не жёсткие единицы.
- Тест: два отчёта с одинаковыми входами дают одинаковый отпечаток; отчёт по графику
  с датой фиксации и без неё — разный.

- [ ] **Шаг 1: тест на отпечаток**

```rust
#[test]
fn the_inputs_fingerprint_changes_when_the_schedule_gains_a_record_date() {
    // record_date входит в BondSchedule, который целиком
    // сериализуется в SelectedInputs. Оставить прежнюю метку v1
    // значило бы объявить два разных набора входов одинаковыми.
    let без = отпечаток_входов(&график_с_фиксацией(date!(2026 - 06 - 15), None));
    let с = отпечаток_входов(&график_с_фиксацией(date!(2026 - 06 - 15), Some(date!(2026 - 06 - 14))));
    assert_ne!(без, с, "разные входы обязаны давать разный отпечаток");
    assert!(с.starts_with("iaam/returns-inputs/v2"));
}

#[test]
fn applied_rules_report_the_versions_actually_used() {
    // Жёсткие единицы врали бы о том, каким правилом посчитан отчёт.
    let report = отчёт_сверки_по_умолчанию();
    assert_eq!(report.applied_rules.cashflow, CashflowProjectionVersion(2));
    assert_eq!(report.applied_rules.posting_match, PostingMatchVersion(2));
}
```

- [ ] **Шаг 2: убедиться, что падают**

Выполнить: `nix develop -c cargo test -p iaam-core --lib inputs_fingerprint_changes`
Ожидается: FAIL — метка `v1`, версии равны единице.

- [ ] **Шаг 3: поднять тег и вернуть настоящие версии**

`returns/mod.rs:788`: `iaam/returns-inputs/v1` → `v2`. `returns/mod.rs:969`: `AppliedRules`
заполняется версиями, которыми отчёт действительно посчитан, а не константами.

- [ ] **Шаг 4: тесты зелёные**

Выполнить: `nix develop -c cargo test -p iaam-core -p iaam-server`
Ожидается: PASS; золотые отчёты обновлены вместе с меткой.
- [ ] **Шаг 5: коммит**

```bash
git commit -m "feat(core): отпечаток входов v2 и настоящие версии правил (iaam-d8b.14)"
```

### Задача C11: мутационный заслон на новые модули

**Файлы:**
- Изменить: `scripts/check-mutants.sh` (список критичных модулей)

**Критерии приёмки:**
- `crates/iaam-core/src/settlement.rs`, `crates/iaam-core/src/projection/ownership.rs`
  и `crates/iaam-core/src/rules/posting_match.rs` в списке, с письменным обоснованием.
- Прогон `make mutants-diff BASE=main` не оставляет выживших в новых модулях.

- [ ] **Шаг 1: добавить модули с обоснованием**

В `scripts/check-mutants.sh`, в массив `MODULES`:

```bash
  # Владение и дата расчёта решают, кому причиталась выплата. Мутант
  # здесь не меняет ни одной суммы — он меняет то, чему сумма
  # соответствует, ровно как в instrument.rs и reconciliation.
  # `latest < day` против `latest <= day` — граница дня из §3.3 спеки:
  # сдвиг делает недоказуемое доказанным.
  "crates/iaam-core/src/settlement.rs"
  "crates/iaam-core/src/projection/ownership.rs"
  "crates/iaam-core/src/rules/posting_match.rs"
```
- [ ] **Шаг 2: прогон**

Выполнить: `nix develop -c env POLICY_CHANGE_APPROVED=1 make mutants-diff BASE=main`
Ожидается: выживших нет. Первый кандидат — `latest < day` из C4: мутант `<=` не меняет
ни одной суммы, он меняет то, чему сумма соответствует.

- [ ] **Шаг 3: коммит**

```bash
git add scripts/check-mutants.sh
git commit -m "test(core): новые модули владения под мутационным заслоном (iaam-d8b.14)"
```

---

## Что этот план сознательно НЕ делает

- `iaam-d8b.23` (T-Invest выдаёт поручение за сделку и не проверяет состояние операции) —
  отдельный бид. До него профилю `tinkoff-api/1` в `SettlementLagPolicy` присваивается
  `Unbounded`, и проблема уровня источника называется честно: семантика торгового события
  не доказана, а не «дата расчётов неизвестна».
- `iaam-d8b.19` (прошлые расчёты по оферте) и `iaam-d8b.20` (дата отсечки из брокерского
  отчёта) — вне периметра спеки, §2.
- `iaam-d8b.15` (номинал) — после задачи C3 сверку больше не блокирует, но сам остаётся.
