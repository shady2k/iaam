# E2 · Достоверность данных — план реализации

> **Для агентов-исполнителей:** ОБЯЗАТЕЛЬНЫЙ СУБ-СКИЛЛ: используйте
> `beads-superpowers:subagent-driven-development` (рекомендуется) или
> `beads-superpowers:executing-plans` для исполнения по задачам. Каждая
> задача становится бидом (`bd create -t task --parent iaam-023`).
> Чекбоксы `- [ ]` внутри задач — для человека, отслеживание идёт бидами.

**Goal:** приёмка данных без ручного подтверждения операций: получение
данных двумя независимыми каналами (API брокера и построчный импорт
брокерских отчётов), многомерная сверка со статусом по паре
интервал×измерение, три уровня достоверности с восемью основаниями
автоматического повышения, шесть вердиктов приёмки, правила
классификации с пересчётом истории, блок `dataQuality` и построчная
обработка непокрытого периметра (§10, §11).

**Architecture:** сверка — **чистая проекция журнала**, а не отдельная
подсистема. Контрольные величины (остатки, обороты, количества, суммы
комиссий, доходов и удержанного налога) записываются в журнал фактов
новым вариантом события `ControlAssertion` — фактом источника
с provenance, без денежных ног, по образцу `Valuation`. Ядро считает те
же величины из событий за тот же интервал и сравнивает; из совпадений
рождаются основания (`Evidence`), из оснований — статус по паре
интервал×измерение.

**Каналов получения данных два, и это принципиально.** Брокерский отчёт
разбирается парсером на связку брокер+формат; те же операции и остатки
система получает **сама** через API брокера отдельным клиентом и
отдельным кодом разбора. Совпадение двух каналов — основание 3 (§10.3),
и только оно даёт `accepted_independent` на реальных данных: следующий
отчёт того же брокера тем же парсером — непрерывность, а не
независимость. Ручная загрузка отчёта остаётся полноправным входом:
API покрывает не всё и не всегда доступен.

Оболочка хранит сырьё документов и строк, чтобы повторный разбор новой
версией парсера и пересчёт истории после правки правила классификации
были возможны без обращения к источнику.

**Tech Stack:** Rust 2024, `rustc 1.98.0`; `calamine` для чтения XLS/XLSX
брокерских отчётов (одна библиотека читает и выгрузку Т-Инвестиций, и
выгрузку Финама); `reqwest` + `serde` для REST-шлюзов брокерских API
(gRPC-стек с генерацией protobuf в сборку не вносится: HTTP/JSON-шлюз
покрывает нужные методы, а лишний генератор кода — это лишний способ
разойтись с заслонами); `rusqlite` (STRICT-таблицы, триггеры
append-only), `axum` + `utoipa`, `proptest`, `cargo-nextest`,
`cargo-mutants`, `trybuild`.

---

## Что уже готово от E1 и не переделывается

Эти вещи существуют и являются фундаментом; задачи ниже их дополняют,
а не заменяют.

| Готово | Где | Что из E2 на это опирается |
|---|---|---|
| `Provenance` с хешом сырья, версией парсера и локатором строки | `iaam-core/src/event/provenance.rs` | канал источника и правило независимости (задача 4) |
| Уникальные индексы по `idempotency_key` и `(source, source_operation_id)` | `iaam-store/migrations/0001_initial.sql` | уровни 1–2 иерархии дедупликации (задача 12) |
| `Verdict` с `Provisional`/`Duplicate`/`NeedsClassification`/`Unsupported`/`Rejected` | `iaam-ingest/src/verdict.rs` | добавляются `Accepted`, `Discrepancy`, `NeedsReconciliation` (задача 3) |
| `Confidence::{Known,Estimated,Unknown}` в конверте события | `iaam-core/src/event/mod.rs` | **не является** уровнем сверки и им не становится (задача 5) |
| `EventKind::Valuation` — факт без ног | `iaam-core/src/event/kind.rs` | образец для `ControlAssertion` (задача 1) |
| `FeeOrigin::MarginInterest` | там же | §11, проценты по марже (задача 6) |
| `Coverage.restored_accounts`, `first_event` | `iaam-core/src/projection/state.rs` | §10.7 и `materialIssues` (задачи 7, 8) |
| `DataQuality`, `Computed<T>`, `NotComputable` | `iaam-core/src/returns/mod.rs` | расширяются `navCoverage` и новыми причинами (задача 7) |
| Разбор CSV построчно с вердиктом на строку | `iaam-ingest/src/csv_source.rs` | образец построчности для парсеров отчётов (задача 14) |
| Append-only журнал, `Relation::{Reversal,Replacement}` | store + core | пересчёт истории после правки правила (задача 13) |

---

## Global Constraints

Действуют для **каждой** задачи. Нарушение любого — основание отклонить
задачу на ревью. Первые семнадцать строк перенесены из планов E0/E1 без
изменений: они не перестали действовать оттого, что план закончился.

| Правило | Источник |
|---|---|
| Все команды выполняются внутри dev-shell: `nix develop -c <команда>` | E0 |
| `unsafe` запрещён во всех крейтах первой стороны: `[workspace.lints.rust]` **плюс** `[lints] workspace = true` в каждой крейте, включая новые | §15.1 |
| `f64` запрещён в доменных величинах. Допустим только в объявленных заслоном файлах приближённого режима | §6.6, §15.1 |
| `iaam-core` — синхронная, без `async`, без `Mutex`, без ввода-вывода, без зависимостей на другие крейты воркспейса | §3.1, §3.2 |
| Строковые дискриминаторы запрещены там, где возможен `enum` | §15.1 |
| Неизвестное значение — `Option<T>`, **никогда** не нулевая заглушка | §4.9 |
| Проведённые суммы (`PostedMinor`) и расчётные величины (`Dec`) — разные типы, не смешиваются | §3.4 |
| Каждое событие несёт `provenance` | §4.1 |
| Ожидаемое значение теста **никогда** не берётся из вывода программы | §15.5 |
| Замороженную фикстуру нельзя править, чтобы починить тест | §15.7 |
| Нарушение инварианта — типизированная ошибка и `not_computable`, а не число с предупреждением | §15.2 |
| Общего крейта `shared` / `common` / `utils` не существует | §3.2 |
| `clippy -D warnings` обязателен; новые `allow`, `expect`, `ignore`, `todo!`, `unimplemented!` и `_ =>` в доменных `enum` запрещены | §15.7 |
| **Логика конструктора не живёт в `new`** — `cargo-mutants` молча пропускает функции с этим именем | §15.7 |
| **Литералы вида `100_00` запрещены** — пишите `10_000` | §15.1 |
| **Оболочка не считает.** `iaam-app`, `iaam-server`, `iaam-ingest` не содержат арифметики над деньгами | §3.1, §13 |
| Секрет не попадает ни в лог, ни в ответ, ни в базу в открытом виде | §14 |
| Коммит после каждой задачи, с идентификатором бида в сообщении | — |

Добавляется этим планом:

| Правило | Источник |
|---|---|
| **Статус присваивается утверждению о полноте счёта на интервале по измерению, а не операции.** Поля «уровень достоверности» у события не существует и не заводится | §10.3 |
| **`Confidence` события не является уровнем сверки** и никогда не конвертируется в него. `Confidence` описывает уверенность в значении (§4.9), сверка — утверждение о полноте интервала | §10.3 |
| **`independent` только при доказанной независимости канала:** другая версия парсера **и** другой документ. Следующий отчёт того же брокера тем же парсером — непрерывность, а не независимость | §10.3 |
| **Сверка — чистая функция от журнала.** Ни одного статуса, вычисленного оболочкой или хранимого отдельно от фактов, из которых он выводится | §3.1, §10.3 |
| **Строка, которую не поняли, не отменяет документ.** Любой парсер возвращает вердикт на строку и продолжает разбор | §10.1 |
| **Вероятностный дубликат не удаляется автоматически** — показывается владельцу | §10.6 |
| **Неподдерживаемая операция сохраняет денежный эффект и не достраивает экономику.** Расхождение по причине вне периметра — исключение, а не «почини это» | §11 |
| **Отказ считать за период не отменяет остальные счета и периоды** | §11 |
| Сырьё документа и строки хранятся: без них ни повторный разбор новой версией парсера, ни пересчёт истории после правки правила невозможны | §10.1, §10.4 |
| **Брокерский токен шифруется на диске ключом вне базы.** Утечка файла БД не даёт доступа к брокерскому счёту. В логи, ответы API и сообщения об ошибках токен не попадает никогда | §14 |
| **У брокера запрашивается только доступ на чтение**, где брокер это различает. Торговые права не запрашиваются ни при каких условиях | §14 |
| **Канал API и канал отчёта не делят код разбора.** Общая функция нормализации между ними уничтожила бы независимость, ради которой второй канал и заводится: общая ошибка исказит обе стороны, и сверка её не заметит | §10.3, §15.4 |

---

## Сознательные сокращения

Требования спеки, которых этот план **не** закрывает. Ни одно не забыто.

| Требование спеки | Куда отнесено | Что делает E2 вместо этого |
|---|---|---|
| Депозитарный отчёт (основание 4) | E7 (`iaam-3ju`) | Основание `DepositaryReportConfirms` **реализовано и протестировано** в ядре; парсера депозитарного отчёта нет. Тип не ждёт E7 — иначе E7 потребует правки ядра сверки |
| Периодическая синхронизация по расписанию (§10.1) | E7 | Синхронизация запускается вызовом маршрута; фонового планировщика нет. Клиент API, шифрование токена и сведение каналов — здесь (задачи 17–20) |
| Брокеры, кроме Т-Инвестиций и Финама | отдельные биды | Реестр каналов и парсеров открыт: новый брокер добавляется реализацией трейта, без правки ядра сверки |
| Справка налогового агента (основание 8) | E5 (`iaam-c55`) | Основание реализовано; удержанный налог сравнивать пока не с чем — измерение `taxBasis` честно отдаёт `NotComparable`, а не «сошлось» |
| Параметры выпуска из MOEX (основание 7) | E3 (`iaam-d8b`) | Основание реализовано; источника параметров выпуска нет |
| Подтверждение графика выплат (основание 6) | E3 | Основание реализовано; графиков вкладов и облигаций ещё нет |
| Порог невязки из алгоритма округления вклада (§8.3) | E3 | Сверка E2 сравнивает проведённые суммы **точно**: обе стороны — `PostedMinor`, и допуск здесь означал бы разрешение на потерю копеек |
| Налоговый отчёт с диапазоном при неизвестной стоимости (§10.7) | E5 | Утверждения `opening_position` записываются полностью (задача 8); диапазон считать нечем, пока нет налогового движка |
| ЛДВ и `ldvEligibility` как расчёт | E5 | Признак записывается как утверждение с уверенностью, не используется в расчёте |
| Веб-интерфейс правил классификации | E8 (`iaam-ebz`) | Правила видимы и редактируемы через REST (задача 17) |
| Шифрование брокерских токенов (§14) | E7 | Брокерских токенов ещё нет |

---

## Открытые решения этого плана

Три решения приняты внутри плана, потому что спека их не фиксирует.
Каждое видно на ревью и может быть отклонено.

1. **Уровень 3 иерархии дедупликации (отпечаток нормализованной записи)
   действует жёстко только внутри одного документа.** Спека ставит его
   выше уровня 4 (хеш документа + строка), но прямо запрещает считать
   дубликатом две законные одинаковые покупки в один день (§10.6).
   Отпечаток нормализованной записи у них совпадает. Разрешение: внутри
   одного документа повтор отпечатка означает повторную подачу того же
   файла и даёт `Duplicate`; между разными документами тот же отпечаток
   понижается до уровня 5 — вероятностной подсказки, не удаляющей ничего.
2. **Названный владельцем остаток даёт `accepted_internal`, не
   `accepted_independent`.** §10.4 ограничивает его измерениями `cash`
   и `positions`, но уровня не называет. Владелец мог прочитать ту же
   цифру в том же отчёте, который мы разобрали, — независимость не
   доказана, а §10.3 требует доказательства, а не типа основания.
3. **`navCoverage` содержит четвёртую долю — `discrepant`.** §10.5
   показывает в примере три. Без четвёртой расходящийся счёт попадал бы
   в `provisional` и выглядел как «просто пока не подтверждён»: проблема
   пряталась бы ровно в той цифре, которая существует, чтобы её
   показывать. Добавление поля аддитивно и потребителя не ломает.
4. **Окно расчётов для `temporary_settlement_deficit` — параметр
   политики, по умолчанию 5 календарных дней.** «Допустимый срок» (§11)
   без торгового календаря не вычисляется, а календарь — это E3.
   Параметр задаётся в запросе отчёта и попадает в `AppliedRules`:
   цифра, зависящая от порога, обязана нести порог рядом с собой.

---

## Статус детализации задач

План написан с разной глубиной, и это видно намеренно.

| Задачи | Глубина | Что это значит для исполнителя |
|---|---|---|
| 1–10 | полная | Каждый шаг несёт готовый код и точное место правки. Задачу можно отдать исполнителю без контекста |
| 11–23 | контурная | Файлы, интерфейсы, критерии приёмки и порядок шагов заданы; тела функций и тексты тестов — нет |

**Задачи 11–23 обязаны быть развёрнуты до полной глубины перед
исполнением** — по одной, непосредственно перед взятием в работу, когда
типы предыдущих задач уже существуют в коде и их можно не угадывать.
Развёртывание контурной задачи — это не новая работа поверх плана, а
последний шаг планирования, который дешевле делать с готовым кодом
на руках, чем предсказывать за десять задач вперёд.

Часть A доведена до полной глубины целиком, потому что она вводит все
новые понятия эпика: ошибка в типах сверки распространится на все
остальные задачи, а ошибка в теле парсера Финама не выйдет за пределы
одного файла.

---

## File Structure

```
crates/
  iaam-core/                            дополняется
    src/event/kind.rs                   + ControlAssertion, OpeningAssertions   задачи 1, 8
    src/event/mod.rs                    + валидация ControlAssertion, SCHEMA_VERSION = 3
    src/reconciliation/mod.rs           НОВЫЙ  статус, измерения, уровни        задача 5
    src/reconciliation/claim.rs         НОВЫЙ  контрольные утверждения          задача 1
    src/reconciliation/observed.rs      НОВЫЙ  те же величины из журнала        задача 2
    src/reconciliation/check.rs         НОВЫЙ  сопоставление и расхождения      задача 3
    src/reconciliation/evidence.rs      НОВЫЙ  восемь оснований, независимость  задача 4
    src/perimeter.rs                    НОВЫЙ  §11: маржа, РЕПО, минусовой кэш  задача 6
    src/returns/mod.rs                  + navCoverage, новые materialIssues     задача 7
    tests/reconciliation_grounds.rs     НОВЫЙ  восемь оснований построчно       задача 4
    tests/reconciliation_properties.rs  НОВЫЙ  свойства сверки                  задача 9
    tests/perimeter.rs                  НОВЫЙ  §11 целиком                      задача 6
    tests/acceptance_stage2.rs          НОВЫЙ  приёмка эпика                    задача 9
    tests/metamorphic_reconciliation.rs НОВЫЙ  метаморфные §15.6                задача 9
  iaam-store/                           дополняется
    migrations/0002_sources_and_rules.sql  НОВЫЙ  документы, строки, правила    задачи 10, 11
    migrations/0003_broker_access.sql   НОВЫЙ  зашифрованный доступ к брокеру  задача 17
    src/documents.rs                    НОВЫЙ  сырьё документа и строк          задача 10
    src/rules.rs                        НОВЫЙ  правила классификации            задача 11
    src/broker_access.rs                НОВЫЙ  хранение шифротекста токена      задача 17
  iaam-ingest/                          дополняется
    src/dedup.rs                        НОВЫЙ  иерархия ключей §10.6            задача 12
    src/classification.rs               НОВЫЙ  правила и пересчёт истории       задача 13
    src/report/mod.rs                   НОВЫЙ  реестр парсеров и контракт       задача 14
    src/report/sections.rs              НОВЫЙ  контрольные секции               задача 14
    src/report/tinkoff.rs               НОВЫЙ  парсер Т-Инвестиций              задача 15
    src/report/finam.rs                 НОВЫЙ  парсер Финама                    задача 16
    tests/report_tinkoff.rs             НОВЫЙ                                   задача 15
    tests/report_finam.rs               НОВЫЙ                                   задача 16
  iaam-broker/                          НОВАЯ  адаптер брокерских API          задачи 17-19
    src/lib.rs                          ошибки, устойчивость, общий HTTP-клиент задача 17
    src/credentials.rs                  шифрование токена ключом вне базы (§14) задача 17
    src/tinkoff.rs                      клиент T-Invest: операции и портфель    задача 18
    src/finam.rs                        клиент Finam: транзакции и портфель     задача 19
    tests/tinkoff_mapping.rs            разбор ответов по замороженным образцам задача 18
    tests/finam_mapping.rs              то же для Финама                        задача 19
  iaam-app/                             дополняется
    src/ports.rs                        + порт BrokerChannel и хранение доступа задача 17
    src/scenarios/documents.rs          НОВЫЙ  загрузка, разбор, повторный разбор задача 21
    src/scenarios/sync.rs               НОВЫЙ  синхронизация и сведение каналов  задача 20
    src/scenarios/reconciliation.rs     НОВЫЙ  статусы и ответ владельца         задача 21
  iaam-server/                          дополняется
    src/dto.rs, src/routes.rs, src/openapi.rs  + маршруты E2                    задача 21
docs/
  agent-skill/SKILL.md                  обновляется                             задача 19
  decisions/                            ADR по трём открытым решениям, если приняты
scripts/
  check-architecture.sh                 + iaam-broker, заслон «каналы не делят разбор» задачи 17, 21
  check-mutants.sh                      + пороги новых модулей                  задача 9
tests/fixtures/
  reports/tinkoff-synthetic.xlsx        синтетика по структуре реального        задача 15
  reports/finam-synthetic.xls           синтетика по структуре реального        задача 16
  api/tinkoff-operations.json           замороженный образец ответа API         задача 18
  api/finam-transactions.json           замороженный образец ответа API         задача 19
  reconciliation_cases.json             ожидаемые статусы, посчитанные вручную  задача 9
  MANIFEST.sha256                       дополняется                             задачи 15, 16, 18, 19, 9
```

**Почему сверка разбита на пять файлов.** Утверждение источника
(`claim.rs`) и наша проекция того же (`observed.rs`) обязаны считаться
независимо: если обе стороны позовут один хелпер, проверка станет
тавтологией и перестанет ловить ошибку разбора — ровно то, ради чего
§10.3 вводит три уровня, а не два (§15.4). Сопоставление (`check.rs`)
не знает, откуда взялись стороны; основания (`evidence.rs`) не знают,
как считалось совпадение; статус (`mod.rs`) не знает, какие бывают
основания, кроме их уровня и измерений.

---

## Граф задач

Двадцать три задачи, три части. Каждая — один бид
(`bd create -t task --parent iaam-023`), каждая заканчивается зелёной
сборкой и коммитом.

```
Часть A — ядро сверки
  1 контрольные утверждения (SCHEMA_VERSION 3)
      └─→ 2 наблюдаемые величины из журнала
             └─→ 3 сопоставление и вердикты расхождения
                    └─→ 4 восемь оснований и независимость
                           └─→ 5 статус по интервал×измерение
  6 §11 периметр (зависит от 2)  ─┐
  8 §10.7 восстановленные начала ─┼─→ 7 dataQuality и navCoverage
  5 ──────────────────────────────┘        └─→ 9 приёмка ядра E2

Часть B — источники данных
  ветка «отчёты»    14 реестр парсеров ─┬─→ 15 Т-Инвестиции (файл)
                                        └─→ 16 Финам (файл)
  ветка «API»       17 iaam-broker: порт, шифрование доступа (§14)
                        ├─→ 18 клиент T-Invest
                        └─→ 19 клиент Finam
  ветка «хранилище» 10 сырьё документов ─┐
                    11 правила           ├─→ 13 классификация и пересчёт истории
                    12 дедупликация ─────┘
  18, 19 ──→ 20 сведение каналов и синхронизация
  13, 15, 16, 20, 5 ──→ 21 app + server: маршруты, статусы, правила

Часть C — сдача
  22 золотые сценарии E2 и приёмка эпика → 23 документация и закрытие
```

**Порядок внутри части A жёсткий:** каждая следующая задача потребляет
типы предыдущей. Задачи 6 и 8 можно делать параллельно с 3–5.

**Часть B параллелится по трём веткам** — отчёты (14–16), API (17–19) и
хранилище с приёмкой (10–13). Ветки не пересекаются по файлам, и это
не случайность: **канал API и канал отчёта не делят код разбора**.
Общая функция нормализации между ними уничтожила бы независимость,
ради которой второй канал заводится (§10.3). Заслон на это ставится
в задаче 21.

**Задача 20 — точка сведения.** Она единственная знает про оба канала
сразу, и её работа — не «слить», а сопоставить: одна и та же сделка,
пришедшая из API и из отчёта, обязана быть распознана как один факт
(дедупликация, задача 12) и одновременно дать основание 3 (задача 4).
Слияние без основания потеряло бы подтверждение; основание без
дедупликации удвоило бы позицию.

---

# Часть A — ядро сверки

## Задача 1: Контрольные утверждения источника

**Files:**
- Create: `crates/iaam-core/src/reconciliation/mod.rs`
- Create: `crates/iaam-core/src/reconciliation/claim.rs`
- Modify: `crates/iaam-core/src/lib.rs` — добавить `pub mod reconciliation;`
- Modify: `crates/iaam-core/src/event/kind.rs` — вариант `ControlAssertion`, ветки в `discriminant` и `flow_endpoints`
- Modify: `crates/iaam-core/src/event/mod.rs` — `SCHEMA_VERSION = 3`, ветка диспетчера, `validate_control_assertion`
- Modify: `crates/iaam-core/src/projection/lots.rs:236` — ветка «лотов не трогает»
- Test: тесты в `claim.rs` и в `event/mod.rs`

**Interfaces:**
- Produces: `iaam_core::reconciliation::Dimension` (`Cash`, `Positions`, `TaxBasis`, `Income`; `code()`, `all()`); `iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim}`; `AssertionPeriod::between(from, to) -> Option<Self>`, `AssertionPeriod::contains(&self, Date) -> bool`; `ControlClaim::dimension(&self) -> Dimension`, `ControlClaim::discriminant(&self) -> &'static str`; `EventKind::ControlAssertion { period: AssertionPeriod, claim: ControlClaim }`.

**Acceptance Criteria:**
- Контрольное утверждение записывается как факт журнала с provenance и без единой ноги; событие с ногой отклоняется валидацией формы
- Интервал с началом позже конца не создаётся конструктором и отклоняется валидацией после десериализации
- Отрицательная сумма комиссий, доходов, налога и отрицательный оборот отклоняются; отрицательный **денежный остаток** принимается (§11)
- Отрицательное количество бумаг отклоняется: периметр long-only (§11)
- `SCHEMA_VERSION` поднята до 3, и в комментарии сказано, чем версия 3 отличается от версии 2
- Каждое измерение имеет машиночитаемый код; `Dimension::all()` перечисляет все четыре

- [ ] **Шаг 1: Написать падающий тест на форму утверждения**

Добавьте в конец `crates/iaam-core/src/event/mod.rs` в модуль `mod tests`:

```rust
    #[test]
    fn a_control_assertion_carries_no_legs() {
        // Утверждение о полноте интервала не двигает денег. Нога у него
        // означала бы, что контрольная секция отчёта попала в остаток
        // вторым экземпляром и удвоила его.
        use crate::money::PostedMinor;
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};

        let account = AccountId::new_random();
        let period = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        let claim = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(1_000_000),
            at: BalancePoint::Closing,
        };
        let kind = EventKind::ControlAssertion { period, claim };

        let clean = test_support::event_with(account, date!(2026 - 03 - 31), 1, kind, vec![]);
        assert!(clean.validate_structure().is_ok());

        let money = Money::new(PostedMinor::new(1_000_000), CurrencyCode::Rub);
        let with_leg =
            test_support::event_with(account, date!(2026 - 03 - 31), 2, kind, vec![Leg::cash(account, money)]);
        assert!(matches!(
            with_leg.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
    }

    #[test]
    fn a_control_assertion_with_an_inverted_period_is_rejected() {
        // Конструктор такой интервал не создаёт, но событие приходит
        // и из JSON, где конструктор не вызывался. Валидация формы —
        // второй рубеж, и он обязан ловить состояние.
        use crate::money::PostedMinor;
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};

        assert!(AssertionPeriod::between(date!(2026 - 03 - 31), date!(2026 - 03 - 01)).is_none());

        let inverted = AssertionPeriod {
            from: date!(2026 - 03 - 31),
            to: date!(2026 - 03 - 01),
        };
        let kind = EventKind::ControlAssertion {
            period: inverted,
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(1),
                at: BalancePoint::Opening,
            },
        };
        let event = test_support::event_with(AccountId::new_random(), date!(2026 - 03 - 01), 1, kind, vec![]);
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::NonPositive { field: "period", .. })
        ));
    }

    #[test]
    fn negative_totals_are_rejected_but_a_negative_cash_balance_is_not() {
        // Отрицательный остаток — законное состояние (§11): технический
        // овердрафт и тайминги расчётов. Отрицательная **сумма комиссий**
        // законным состоянием не является: это ошибка разбора знака,
        // и принять её значит внести её в журнал навсегда.
        use crate::money::PostedMinor;
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};

        let account = AccountId::new_random();
        let period = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();

        let overdraft = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-5_000),
                at: BalancePoint::Closing,
            },
        };
        assert!(
            test_support::event_with(account, date!(2026 - 03 - 31), 1, overdraft, vec![])
                .validate_structure()
                .is_ok()
        );

        let negative_fees = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::FeesTotal {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-100),
            },
        };
        assert!(matches!(
            test_support::event_with(account, date!(2026 - 03 - 31), 2, negative_fees, vec![])
                .validate_structure(),
            Err(EventValidationError::NonPositive { field: "amount", .. })
        ));
    }

    #[test]
    fn a_negative_position_quantity_is_outside_the_perimeter() {
        // Шорты вне периметра (§11). Отрицательное количество в контрольной
        // секции означает либо шорт, либо перепутанный знак — принимать
        // нельзя ни то, ни другое.
        use crate::numeric::decimal::Dec;
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
        use rust_decimal::Decimal;

        let period = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        let kind = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::PositionQuantity {
                instrument: InstrumentId::new_random(),
                custody: crate::ids::CustodyId::new_random(),
                quantity: Quantity(Dec::new(Decimal::from(-10))),
                at: BalancePoint::Closing,
            },
        };
        assert!(matches!(
            test_support::event_with(AccountId::new_random(), date!(2026 - 03 - 31), 1, kind, vec![])
                .validate_structure(),
            Err(EventValidationError::NonPositive { field: "quantity", .. })
        ));
    }
```

- [ ] **Шаг 2: Запустить и убедиться, что не компилируется**

Run: `nix develop -c cargo test -p iaam-core --lib event::tests 2>&1 | head -20`
Expected: FAIL — `unresolved module or unlinked crate reconciliation` и `no variant named ControlAssertion`.

- [ ] **Шаг 3: Завести модуль сверки и измерения**

Создайте `crates/iaam-core/src/reconciliation/mod.rs`:

```rust
//! Сверка: статус полноты счёта на интервале по измерению (§10.3).
//!
//! **Статус присваивается не операции.** Операция либо записана, либо
//! нет; утверждать про неё «подтверждена» бессмысленно — подтверждается
//! полнота интервала: что за март по деньгам учтено всё и ничего
//! лишнего. Поэтому единицей статуса является пара интервал×измерение,
//! а не событие, и поля «уровень достоверности» у события не существует.

pub mod claim;

use serde::{Deserialize, Serialize};

/// Измерение, о полноте которого делается утверждение (§10.3).
///
/// Разделение обязательно: подтверждённый остаток принимает деньги и
/// количества, но **не подтверждает** налоговую стоимость и
/// классификацию доходов. Одно измерение на всё превратило бы
/// «остаток сошёлся» в «налоги посчитаны верно».
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Dimension {
    Cash,
    Positions,
    TaxBasis,
    Income,
}

impl Dimension {
    /// Машиночитаемый код для API (§13).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Cash => "cash",
            Self::Positions => "positions",
            Self::TaxBasis => "tax_basis",
            Self::Income => "income",
        }
    }

    /// Все измерения одним списком.
    ///
    /// Обход по измерениям пишется через него, а не литералом на месте
    /// вызова: литерал с пропущенным вариантом компилируется, и
    /// пропавшее измерение молча не получает статуса.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Cash, Self::Positions, Self::TaxBasis, Self::Income]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dimension_has_a_distinct_machine_readable_code() {
        let codes: Vec<&str> = Dimension::all().iter().map(|d| d.code()).collect();
        assert_eq!(codes, vec!["cash", "positions", "tax_basis", "income"]);
    }

    #[test]
    fn the_list_of_dimensions_covers_every_variant() {
        // Список задан руками, поэтому он обязан быть проверен:
        // забытое измерение не получает статуса и выглядит как
        // «подтверждать нечего».
        for dimension in Dimension::all() {
            let found = Dimension::all().iter().filter(|d| **d == dimension).count();
            assert_eq!(found, 1, "измерение {dimension:?} встречается не один раз");
        }
        assert_eq!(Dimension::all().len(), 4);
    }
}
```

- [ ] **Шаг 4: Написать контрольные утверждения**

Создайте `crates/iaam-core/src/reconciliation/claim.rs`:

```rust
//! Контрольные утверждения источника (§10.3).
//!
//! Отчёт брокера содержит не только операции, но и контрольные секции:
//! остатки на начало и конец периода, обороты Dt/Kt, количества бумаг,
//! суммы комиссий, купонов и дивидендов, удержанный налог. Это **факты
//! источника**, а не расчёт, поэтому они записываются в журнал наравне
//! с операциями — с provenance, версией парсера и локатором строки.
//!
//! Утверждение денег не двигает: ног у события нет, как у `Valuation`.
//! Нога здесь означала бы, что контрольная секция попала в остаток
//! вторым экземпляром.

use serde::{Deserialize, Serialize};
use time::Date;

use super::Dimension;
use crate::ids::{CustodyId, InstrumentId};
use crate::money::{CurrencyCode, PostedMinor, Quantity};

/// Интервал, о котором говорит утверждение. Границы включаются с обеих
/// сторон: отчёт за март говорит и о первом, и о тридцать первом марта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssertionPeriod {
    pub from: Date,
    pub to: Date,
}

impl AssertionPeriod {
    /// Интервал с началом позже конца не создаётся.
    ///
    /// Такой интервал — не «пустой период», а неверно разобранный
    /// документ: перепутанные местами даты дают сверку, которая никогда
    /// ни с чем не сойдётся и потому вечно висит расхождением.
    ///
    /// Проверка живёт не в `new`: `cargo-mutants` молча пропускает
    /// функции с этим именем (§15.7).
    #[must_use]
    pub fn between(from: Date, to: Date) -> Option<Self> {
        (from <= to).then_some(Self { from, to })
    }

    /// Корректен ли интервал. Нужен отдельно от конструктора, потому
    /// что событие приходит и из JSON, где конструктор не вызывался.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.from <= self.to
    }

    #[must_use]
    pub fn contains(&self, date: Date) -> bool {
        self.from <= date && date <= self.to
    }
}

/// На какой момент интервала сделано утверждение об остатке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BalancePoint {
    /// Остаток на начало: состояние **до** первого события интервала.
    Opening,
    /// Остаток на конец: состояние, включающее последнее событие интервала.
    Closing,
}

impl BalancePoint {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Opening => "opening",
            Self::Closing => "closing",
        }
    }
}

/// Что именно утверждает контрольная секция.
///
/// Величины оборотов и итогов — **модули**: знак несёт сторона
/// (дебет/кредит) и смысл поля, а не само число. Денежный остаток —
/// исключение: он может быть отрицательным, и это законное состояние
/// (§11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlClaim {
    /// Остаток денег на начало или конец интервала.
    CashBalance {
        currency: CurrencyCode,
        amount: PostedMinor,
        at: BalancePoint,
    },
    /// Количество бумаг на начало или конец интервала.
    PositionQuantity {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Quantity,
        at: BalancePoint,
    },
    /// Обороты по счёту за интервал, обе стороны модулями.
    CashTurnover {
        currency: CurrencyCode,
        debit: PostedMinor,
        credit: PostedMinor,
    },
    /// Сумма комиссий за интервал.
    FeesTotal {
        currency: CurrencyCode,
        amount: PostedMinor,
    },
    /// Сумма купонов и дивидендов за интервал.
    IncomeTotal {
        currency: CurrencyCode,
        amount: PostedMinor,
    },
    /// Удержанный налоговым агентом налог за интервал.
    TaxWithheldTotal {
        currency: CurrencyCode,
        amount: PostedMinor,
    },
}

impl ControlClaim {
    /// Какое измерение ограничивает это утверждение (§10.3).
    ///
    /// Комиссии отнесены к деньгам, а не к доходам: комиссия — это
    /// денежное списание, и сходится она с денежной проекцией.
    /// Удержанный налог — единственное, что говорит о `TaxBasis`,
    /// и говорит он только об агрегате (основание 8).
    #[must_use]
    pub const fn dimension(&self) -> Dimension {
        match self {
            Self::CashBalance { .. } | Self::CashTurnover { .. } | Self::FeesTotal { .. } => {
                Dimension::Cash
            }
            Self::PositionQuantity { .. } => Dimension::Positions,
            Self::IncomeTotal { .. } => Dimension::Income,
            Self::TaxWithheldTotal { .. } => Dimension::TaxBasis,
        }
    }

    /// Машиночитаемое имя вида утверждения.
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::CashBalance { .. } => "cash_balance",
            Self::PositionQuantity { .. } => "position_quantity",
            Self::CashTurnover { .. } => "cash_turnover",
            Self::FeesTotal { .. } => "fees_total",
            Self::IncomeTotal { .. } => "income_total",
            Self::TaxWithheldTotal { .. } => "tax_withheld_total",
        }
    }

    /// Величина, которая обязана быть неотрицательной, и имя её поля.
    ///
    /// `None` означает «отрицательное значение законно» — это только
    /// денежный остаток (§11). Возвращается имя поля, чтобы ошибка
    /// валидации называла именно то поле, которое не прошло (§13).
    #[must_use]
    pub const fn non_negative_field(&self) -> Option<(&'static str, i64)> {
        match self {
            // Отрицательный денежный остаток — законное состояние.
            Self::CashBalance { .. } => None,
            Self::PositionQuantity { .. } => None,
            Self::CashTurnover { debit, credit, .. } => {
                // Проверяется меньшая из двух: если она неотрицательна,
                // неотрицательны обе.
                let (min, _) = if debit.raw() <= credit.raw() {
                    (debit.raw(), credit.raw())
                } else {
                    (credit.raw(), debit.raw())
                };
                Some(("turnover", min))
            }
            Self::FeesTotal { amount, .. }
            | Self::IncomeTotal { amount, .. }
            | Self::TaxWithheldTotal { amount, .. } => Some(("amount", amount.raw())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(amount: i64) -> PostedMinor {
        PostedMinor::new(amount)
    }

    #[test]
    fn an_inverted_period_is_not_constructed() {
        assert!(AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).is_some());
        assert!(AssertionPeriod::between(date!(2026 - 03 - 31), date!(2026 - 03 - 01)).is_none());
    }

    #[test]
    fn a_single_day_period_is_valid() {
        // Отчёт за один день — законный документ, а не вырожденный случай.
        let day = date!(2026 - 03 - 15);
        let period = AssertionPeriod::between(day, day).unwrap();
        assert!(period.contains(day));
    }

    #[test]
    fn period_boundaries_are_inclusive_on_both_ends() {
        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        assert!(period.contains(date!(2026 - 03 - 01)));
        assert!(period.contains(date!(2026 - 03 - 31)));
        assert!(!period.contains(date!(2026 - 02 - 28)));
        assert!(!period.contains(date!(2026 - 04 - 01)));
    }

    #[test]
    fn cash_claims_constrain_cash_and_quantity_claims_constrain_positions() {
        // Измерение выводится из вида утверждения, а не назначается
        // вызывающим: назначаемое измерение позволило бы объявить
        // сошедшийся остаток подтверждением налоговой базы.
        let cash = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: rub(100),
            at: BalancePoint::Closing,
        };
        let fees = ControlClaim::FeesTotal {
            currency: CurrencyCode::Rub,
            amount: rub(100),
        };
        let position = ControlClaim::PositionQuantity {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: Quantity(Dec::new(Decimal::from(10))),
            at: BalancePoint::Closing,
        };
        let income = ControlClaim::IncomeTotal {
            currency: CurrencyCode::Rub,
            amount: rub(100),
        };
        let tax = ControlClaim::TaxWithheldTotal {
            currency: CurrencyCode::Rub,
            amount: rub(13),
        };

        assert_eq!(cash.dimension(), Dimension::Cash);
        assert_eq!(fees.dimension(), Dimension::Cash);
        assert_eq!(position.dimension(), Dimension::Positions);
        assert_eq!(income.dimension(), Dimension::Income);
        assert_eq!(tax.dimension(), Dimension::TaxBasis);
    }

    #[test]
    fn every_claim_kind_has_a_distinct_discriminant() {
        let claims = [
            ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: rub(1),
                at: BalancePoint::Opening,
            },
            ControlClaim::PositionQuantity {
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                quantity: Quantity(Dec::one()),
                at: BalancePoint::Opening,
            },
            ControlClaim::CashTurnover {
                currency: CurrencyCode::Rub,
                debit: rub(1),
                credit: rub(1),
            },
            ControlClaim::FeesTotal {
                currency: CurrencyCode::Rub,
                amount: rub(1),
            },
            ControlClaim::IncomeTotal {
                currency: CurrencyCode::Rub,
                amount: rub(1),
            },
            ControlClaim::TaxWithheldTotal {
                currency: CurrencyCode::Rub,
                amount: rub(1),
            },
        ];
        let mut names: Vec<&str> = claims.iter().map(ControlClaim::discriminant).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), claims.len(), "имена видов утверждений совпали");
    }

    #[test]
    fn a_turnover_reports_the_smaller_side_for_the_sign_check() {
        // Проверяется меньшая из двух сторон: неотрицательная меньшая
        // означает неотрицательные обе. Взять первую попавшуюся значило
        // бы пропускать отрицательный кредит при положительном дебете.
        let claim = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: rub(500),
            credit: rub(-1),
        };
        assert_eq!(claim.non_negative_field(), Some(("turnover", -1)));

        let mirrored = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: rub(-1),
            credit: rub(500),
        };
        assert_eq!(mirrored.non_negative_field(), Some(("turnover", -1)));
    }

    #[test]
    fn a_negative_cash_balance_is_not_a_sign_violation() {
        // §11: технический овердрафт и тайминги расчётов дают минус,
        // и он обязан войти в NAV обязательством, а не быть отвергнут.
        let claim = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: rub(-5_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(claim.non_negative_field(), None);
    }
}
```

- [ ] **Шаг 5: Подключить модуль и завести вариант события**

В `crates/iaam-core/src/lib.rs` добавьте объявление модуля рядом с остальными:

```rust
pub mod reconciliation;
```

В `crates/iaam-core/src/event/kind.rs` добавьте импорт и вариант в конец
`enum EventKind` (после `Valuation`):

```rust
use crate::reconciliation::claim::{AssertionPeriod, ControlClaim};
```

```rust
    /// Контрольное утверждение источника о полноте интервала (§10.3).
    ///
    /// Факт с provenance, а не расчёт: контрольная секция отчёта — это
    /// то, что источник о себе сказал. Сверка сравнивает её с тем, что
    /// насчитала проекция, и из совпадения рождается основание повышения
    /// статуса. Денег не двигает: ног у события нет.
    ControlAssertion {
        period: AssertionPeriod,
        claim: ControlClaim,
    },
```

В `discriminant`:

```rust
            Self::ControlAssertion { .. } => "control_assertion",
```

В `flow_endpoints` добавьте вариант в существующую ветку `WithinAccount`:

```rust
            Self::Trade { .. }
            | Self::Income { .. }
            | Self::Fee { .. }
            | Self::OpeningPosition { .. }
            | Self::OpeningCash { .. }
            | Self::Valuation { .. }
            | Self::ControlAssertion { .. } => FlowEndpoints::WithinAccount,
```

- [ ] **Шаг 6: Поднять версию схемы и написать валидацию формы**

В `crates/iaam-core/src/event/mod.rs` замените константу и её комментарий:

```rust
/// Текущая версия схемы события.
///
/// Версия 3 отличается от версии 2 добавленным вариантом
/// [`EventKind::ControlAssertion`]. Уже записанные факты версий 1 и 2
/// читаются без изменений — новый вариант в них просто не встречается, —
/// но программа, знающая только версию 2, не разберёт контрольное
/// утверждение и потому не должна притворяться, что разобрала (§4.1).
pub const SCHEMA_VERSION: u32 = 3;
```

В диспетчер `validate_structure` добавьте ветку рядом с `Valuation`:

```rust
            EventKind::ControlAssertion { period, claim } => {
                self.validate_control_assertion(name, *period, *claim)
            }
```

И сам метод рядом с `validate_valuation`:

```rust
    /// Контрольное утверждение: ног нет, интервал корректен, величины,
    /// которые обязаны быть модулями, — неотрицательны.
    ///
    /// Отрицательный денежный остаток пропускается намеренно: это
    /// законное состояние (§11). Отрицательное количество бумаг —
    /// нет: шорты вне периметра, и минус здесь означает либо шорт,
    /// либо перепутанный знак при разборе.
    fn validate_control_assertion(
        &self,
        name: &'static str,
        period: crate::reconciliation::claim::AssertionPeriod,
        claim: crate::reconciliation::claim::ControlClaim,
    ) -> Result<(), EventValidationError> {
        use crate::reconciliation::claim::ControlClaim;

        if !period.is_well_formed() {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "period",
                value: format!("{} .. {}", period.from, period.to),
            });
        }
        if let ControlClaim::PositionQuantity { quantity, .. } = claim
            && quantity.0.is_negative()
        {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "quantity",
                value: quantity.0.inner().to_string(),
            });
        }
        if let Some((field, value)) = claim.non_negative_field()
            && value < 0
        {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field,
                value: value.to_string(),
            });
        }
        if self.legs.is_empty() {
            Ok(())
        } else {
            Err(EventValidationError::LegCount {
                kind: name,
                expected: "ни одной ноги",
                found: self.legs.len(),
            })
        }
    }
```

- [ ] **Шаг 7: Починить исчерпывающие `match` проекций**

В `crates/iaam-core/src/projection/lots.rs` добавьте вариант в ветку
«книгу лотов не трогает»:

```rust
            EventKind::CashIn { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Income { .. }
            | EventKind::Fee { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. } => Ok(()),
```

`Balances` и `FlowLog` правки не требуют: первая ходит по ногам,
которых нет, вторая классифицирует через `flow_endpoints`, уже
дополненный на шаге 5. Если сборка укажет на другой исчерпывающий
`match` — это ровно то, ради чего он исчерпывающий; добавьте вариант
в ветку «эффекта нет».

- [ ] **Шаг 8: Прогнать тесты**

Run: `nix develop -c cargo test -p iaam-core 2>&1 | tail -20`
Expected: PASS, все тесты крейта зелёные.

- [ ] **Шаг 9: Прогнать заслоны**

```bash
nix develop -c cargo fmt --all -- --check
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c ./scripts/check-architecture.sh
```
Expected: без замечаний.

- [ ] **Шаг 10: Коммит**

```bash
git add crates/iaam-core/src/reconciliation crates/iaam-core/src/lib.rs \
        crates/iaam-core/src/event crates/iaam-core/src/projection/lots.rs
git commit -m "feat(core): контрольные утверждения источника как факт журнала (iaam-023)"
```

---

## Задача 2: Наблюдаемые величины из журнала

**Files:**
- Create: `crates/iaam-core/src/reconciliation/observed.rs`
- Modify: `crates/iaam-core/src/reconciliation/mod.rs` — `pub mod observed;`
- Test: тесты в `observed.rs`

**Interfaces:**
- Consumes: `AssertionPeriod`, `Dimension` (задача 1); `Balances`, `Event`, `Leg`, `LegKind`.
- Produces: `ObservedTotals` с методами `cash_at(BalancePoint, CurrencyCode) -> Option<PostedMinor>`, `position_at(BalancePoint, InstrumentId, CustodyId) -> Option<Quantity>`, `turnover(CurrencyCode) -> Option<Turnover>`, `fees(CurrencyCode) -> Option<PostedMinor>`, `income(CurrencyCode) -> Option<PostedMinor>`, `tax_withheld(CurrencyCode) -> Option<PostedMinor>`, `tax_facts_recorded() -> bool`, `events_seen() -> u64`; `Turnover { debit: PostedMinor, credit: PostedMinor }`; `observe(events: &[Event], account: AccountId, period: AssertionPeriod) -> Result<ObservedTotals, ObserveError>`; `ObserveError`.

**Acceptance Criteria:**
- Остаток на начало не включает события интервала, остаток на конец включает; события позже конца интервала не влияют ни на что
- Обороты считаются по **всем** денежным ногам счёта: комиссия и налог входят в оборот наравне с ногой типа `Cash`
- Сумма комиссий собирается по ногам `LegKind::Fee` из событий **любого** типа, включая комиссию внутри сделки
- Отсутствие движения и ноль различаются: `None` против `Some(0)`
- Пока в журнале нет ни одной ноги налога, `tax_facts_recorded()` ложно — сравнивать удержанный налог не с чем, и это не расхождение
- Событие без даты — типизированная ошибка, а не молчаливый пропуск

- [ ] **Шаг 1: Написать падающие тесты**

Создайте `crates/iaam-core/src/reconciliation/observed.rs` с одним лишь
блоком тестов (реализация появится на шаге 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::{FeeOrigin, TradeSide};
    use crate::event::test_support::event_with;
    use crate::money::{Money, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::reconciliation::claim::BalancePoint;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }

    #[test]
    fn opening_excludes_the_period_and_closing_includes_it() {
        // Остаток на начало марта — это состояние до первого мартовского
        // события. Включить март в «начало» значит сверять отчёт с самим
        // собой: обе стороны съедут одинаково, и расхождение исчезнет.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 02 - 20),
                1,
                EventKind::CashIn { amount: rub(100_000) },
                vec![Leg::cash(account, rub(100_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 10),
                1,
                EventKind::CashIn { amount: rub(50_000) },
                vec![Leg::cash(account, rub(50_000))],
            ),
            event_with(
                account,
                date!(2026 - 04 - 05),
                1,
                EventKind::CashIn { amount: rub(7) },
                vec![Leg::cash(account, rub(7))],
            ),
        ];

        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.cash_at(BalancePoint::Opening, CurrencyCode::Rub),
            Some(PostedMinor::new(100_000))
        );
        assert_eq!(
            observed.cash_at(BalancePoint::Closing, CurrencyCode::Rub),
            Some(PostedMinor::new(150_000)),
            "апрельское событие не имеет права попасть в остаток на конец марта"
        );
    }

    #[test]
    fn turnover_counts_every_cash_leg_including_fees_and_tax() {
        // Оборот по счёту — это всё движение денег, а не только ноги
        // типа Cash. Комиссия, списанная с того же счёта, в обороте
        // брокерского отчёта присутствует, и не учесть её значит
        // получить расхождение на ровном месте.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 03 - 02),
                1,
                EventKind::CashIn { amount: rub(100_000) },
                vec![Leg::cash(account, rub(100_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 03),
                1,
                EventKind::Fee {
                    amount: rub(-350),
                    origin: FeeOrigin::Brokerage,
                },
                vec![Leg::fee(account, rub(-350))],
            ),
        ];

        let observed = observe(&events, account, march()).unwrap();
        let turnover = observed.turnover(CurrencyCode::Rub).unwrap();
        assert_eq!(turnover.debit, PostedMinor::new(100_000), "приход");
        assert_eq!(turnover.credit, PostedMinor::new(350), "расход модулем");
    }

    #[test]
    fn fees_are_collected_from_trades_too() {
        // Комиссия внутри сделки — та же комиссия. Контрольная секция
        // отчёта суммирует все, и собирать только отдельные события Fee
        // значит недосчитать ровно на комиссии сделок.
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let quantity = Quantity(Dec::new(Decimal::from(10)));
        let trade = event_with(
            account,
            date!(2026 - 03 - 04),
            1,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity,
                gross: rub(-50_000),
                fee: Some(rub(-120)),
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, rub(-50_000)),
                Leg::fee(account, rub(-120)),
                Leg::security(account, custody, instrument, quantity),
            ],
        );
        let standalone = event_with(
            account,
            date!(2026 - 03 - 05),
            1,
            EventKind::Fee {
                amount: rub(-80),
                origin: FeeOrigin::Depositary,
            },
            vec![Leg::fee(account, rub(-80))],
        );

        let observed = observe(&[trade, standalone], account, march()).unwrap();
        assert_eq!(
            observed.fees(CurrencyCode::Rub),
            Some(PostedMinor::new(200)),
            "120 внутри сделки плюс 80 отдельным событием, модулем"
        );
    }

    #[test]
    fn absence_of_movement_is_not_zero() {
        // `None` и `Some(0)` — разные утверждения. Первое означает
        // «данных нет», второе «данные есть, и остаток нулевой».
        // Схлопнуть их значит выдать отсутствие истории за подтверждённый
        // ноль (§4.9, §10.7).
        let account = AccountId::new_random();
        let observed = observe(&[], account, march()).unwrap();
        assert_eq!(observed.cash_at(BalancePoint::Closing, CurrencyCode::Rub), None);
        assert_eq!(observed.turnover(CurrencyCode::Rub), None);
        assert_eq!(observed.events_seen(), 0);
    }

    #[test]
    fn tax_is_not_comparable_until_a_tax_leg_exists() {
        // Ног налога в E1 не производит ни один путь записи: налоги — E5.
        // Пока их нет, удержанный налог сравнивать не с чем, и ноль
        // с нашей стороны означает «не считаем», а не «брокер не удержал».
        let account = AccountId::new_random();
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 02),
            1,
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(account, rub(1))],
        )];
        let observed = observe(&events, account, march()).unwrap();
        assert!(!observed.tax_facts_recorded());
        assert_eq!(observed.tax_withheld(CurrencyCode::Rub), None);
    }

    #[test]
    fn income_is_summed_from_income_events_only() {
        // Приход денег и доход — разные вещи. Пополнение счёта владельцем
        // деньгами является, доходом — нет, и попасть в контрольную сумму
        // купонов и дивидендов не должно.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 03 - 06),
                1,
                EventKind::CashIn { amount: rub(500_000) },
                vec![Leg::cash(account, rub(500_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 07),
                1,
                EventKind::Income {
                    instrument: None,
                    gross: rub(4_000),
                },
                vec![Leg::cash(account, rub(4_000))],
            ),
        ];
        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(observed.income(CurrencyCode::Rub), Some(PostedMinor::new(4_000)));
    }

    #[test]
    fn another_account_does_not_leak_into_the_totals() {
        // Утверждение делается о счёте. Ноги чужого счёта в обороте —
        // это подтверждение, полученное чужими деньгами.
        let ours = AccountId::new_random();
        let theirs = AccountId::new_random();
        let events = vec![event_with(
            theirs,
            date!(2026 - 03 - 08),
            1,
            EventKind::CashIn { amount: rub(999) },
            vec![Leg::cash(theirs, rub(999))],
        )];
        let observed = observe(&events, ours, march()).unwrap();
        assert_eq!(observed.turnover(CurrencyCode::Rub), None);
        assert_eq!(observed.events_seen(), 0);
    }

    #[test]
    fn an_event_without_a_date_is_a_typed_error() {
        // Событие без даты не попадает ни в один период. Пропустить его
        // молча значит посчитать сверку по неполному срезу и объявить
        // расхождение там, где его нет.
        let account = AccountId::new_random();
        let mut event = event_with(
            account,
            date!(2026 - 03 - 09),
            1,
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(account, rub(1))],
        );
        event.dates = crate::dates::EventDates::default();
        assert!(matches!(
            observe(&[event], account, march()),
            Err(ObserveError::EventWithoutDate { .. })
        ));
    }
}
```

- [ ] **Шаг 2: Убедиться, что тесты не собираются**

Run: `nix develop -c cargo test -p iaam-core reconciliation::observed 2>&1 | head -20`
Expected: FAIL — `cannot find function observe in this scope`.

- [ ] **Шаг 3: Написать реализацию**

Добавьте в начало `observed.rs`, перед блоком тестов:

```rust
//! Те же величины, посчитанные из журнала (§10.3).
//!
//! Это **вторая сторона** сверки. Первая — то, что сказал источник
//! (`claim`). Стороны обязаны считаться независимо: общий помощник
//! между ними превратил бы проверку в тавтологию, и компенсирующая
//! ошибка разбора перестала бы ловиться — ровно то, ради чего §10.3
//! вводит три уровня достоверности, а не два.
//!
//! Остатки берутся у `Balances` — уже проверенной проекции. Это не
//! нарушает независимость: `Balances` считает по журналу, а не по
//! контрольной секции документа, и общего кода с разбором отчёта
//! у неё нет.

use std::collections::BTreeMap;

use crate::event::leg::LegKind;
use crate::event::{Event, kind::EventKind};
use crate::ids::{AccountId, CustodyId, EventId, InstrumentId};
use crate::money::{CurrencyCode, PostedMinor, Quantity};
use crate::numeric::NumericError;
use crate::projection::balances::{BalanceError, Balances, PositionKey};
use crate::reconciliation::claim::{AssertionPeriod, BalancePoint};

/// Обороты по счёту за интервал.
///
/// Обе стороны — **модули**. `debit` — приход, `credit` — расход;
/// соответствие колонкам конкретного отчёта устанавливает парсер,
/// а не эта структура.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Turnover {
    pub debit: PostedMinor,
    pub credit: PostedMinor,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObserveError {
    #[error("событие {event:?} не имеет ни одной даты и не попадает ни в один период")]
    EventWithoutDate { event: EventId },
    #[error("переполнение при подсчёте величины {field}")]
    Overflow { field: &'static str },
    #[error(transparent)]
    Balance(#[from] BalanceError),
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Наблюдаемые величины за интервал.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedTotals {
    cash_opening: BTreeMap<CurrencyCode, PostedMinor>,
    cash_closing: BTreeMap<CurrencyCode, PostedMinor>,
    positions_opening: BTreeMap<(InstrumentId, CustodyId), Quantity>,
    positions_closing: BTreeMap<(InstrumentId, CustodyId), Quantity>,
    turnover: BTreeMap<CurrencyCode, Turnover>,
    fees: BTreeMap<CurrencyCode, PostedMinor>,
    income: BTreeMap<CurrencyCode, PostedMinor>,
    tax_withheld: BTreeMap<CurrencyCode, PostedMinor>,
    tax_facts_recorded: bool,
    events_seen: u64,
}

impl ObservedTotals {
    #[must_use]
    pub fn cash_at(&self, at: BalancePoint, currency: CurrencyCode) -> Option<PostedMinor> {
        match at {
            BalancePoint::Opening => self.cash_opening.get(&currency).copied(),
            BalancePoint::Closing => self.cash_closing.get(&currency).copied(),
        }
    }

    #[must_use]
    pub fn position_at(
        &self,
        at: BalancePoint,
        instrument: InstrumentId,
        custody: CustodyId,
    ) -> Option<Quantity> {
        match at {
            BalancePoint::Opening => self.positions_opening.get(&(instrument, custody)).copied(),
            BalancePoint::Closing => self.positions_closing.get(&(instrument, custody)).copied(),
        }
    }

    #[must_use]
    pub fn turnover(&self, currency: CurrencyCode) -> Option<Turnover> {
        self.turnover.get(&currency).copied()
    }

    #[must_use]
    pub fn fees(&self, currency: CurrencyCode) -> Option<PostedMinor> {
        self.fees.get(&currency).copied()
    }

    #[must_use]
    pub fn income(&self, currency: CurrencyCode) -> Option<PostedMinor> {
        self.income.get(&currency).copied()
    }

    #[must_use]
    pub fn tax_withheld(&self, currency: CurrencyCode) -> Option<PostedMinor> {
        self.tax_withheld.get(&currency).copied()
    }

    /// Записан ли в журнале хоть один факт удержанного налога.
    ///
    /// Ложь означает «сравнивать не с чем», а не «налог равен нулю».
    /// Налоговые факты появляются в E5; до тех пор утверждение отчёта
    /// об удержанном налоге не является расхождением.
    #[must_use]
    pub const fn tax_facts_recorded(&self) -> bool {
        self.tax_facts_recorded
    }

    /// Сколько событий счёта видел журнал за интервал и до него.
    /// Ноль означает, что подтверждать нечего: истории нет.
    #[must_use]
    pub const fn events_seen(&self) -> u64 {
        self.events_seen
    }
}

/// Подсчёт наблюдаемых величин.
///
/// Логика вынесена из конструктора с именем `new` намеренно:
/// `cargo-mutants` молча пропускает функции с этим именем (§15.7).
pub fn observe(
    events: &[Event],
    account: AccountId,
    period: AssertionPeriod,
) -> Result<ObservedTotals, ObserveError> {
    let mut opening = Balances::new();
    let mut closing = Balances::new();
    let mut totals = ObservedTotals::default();

    for event in events {
        let date = event
            .dates
            .effective_date()
            .ok_or(ObserveError::EventWithoutDate { event: event.id })?;

        let touches_us = event.legs.iter().any(|leg| leg.account == account);
        if date < period.from {
            opening.apply(event)?;
            closing.apply(event)?;
            if touches_us {
                totals.events_seen += 1;
            }
        } else if period.contains(date) {
            closing.apply(event)?;
            if touches_us {
                totals.events_seen += 1;
                accumulate(&mut totals, event, account)?;
            }
        }
        // События позже конца интервала не применяются ни к чему:
        // остаток на конец марта не знает про апрель.
    }

    snapshot_cash(&opening, account, &mut totals.cash_opening);
    snapshot_cash(&closing, account, &mut totals.cash_closing);
    snapshot_positions(&opening, account, &mut totals.positions_opening);
    snapshot_positions(&closing, account, &mut totals.positions_closing);
    Ok(totals)
}

fn snapshot_cash(
    balances: &Balances,
    account: AccountId,
    into: &mut BTreeMap<CurrencyCode, PostedMinor>,
) {
    for (owner, money) in balances.iter_cash() {
        if owner == account {
            into.insert(money.currency(), money.amount());
        }
    }
}

fn snapshot_positions(
    balances: &Balances,
    account: AccountId,
    into: &mut BTreeMap<(InstrumentId, CustodyId), Quantity>,
) {
    for (key, quantity) in balances.iter_positions() {
        let PositionKey {
            account: owner,
            custody,
            instrument,
        } = key;
        if *owner != account {
            continue;
        }
        // Место хранения не указано — это тот же перечень позиций,
        // но без разбиения по депозитариям. Утверждение отчёта всегда
        // называет депозитарий, поэтому позиция без него сверке
        // не подлежит и в срез не попадает.
        if let Some(custody) = custody {
            into.insert((*instrument, *custody), quantity);
        }
    }
}

/// Накопление величин интервала по ногам **нашего** счёта.
fn accumulate(
    totals: &mut ObservedTotals,
    event: &Event,
    account: AccountId,
) -> Result<(), ObserveError> {
    let is_income = matches!(event.kind, EventKind::Income { .. });
    for leg in &event.legs {
        if leg.account != account {
            continue;
        }
        let Some(money) = leg.cash_effect() else {
            continue;
        };
        let currency = money.currency();
        let raw = money.amount().raw();

        let turnover = totals.turnover.entry(currency).or_default();
        if raw >= 0 {
            turnover.debit = turnover
                .debit
                .checked_add(PostedMinor::new(raw))
                .ok_or(ObserveError::Overflow { field: "debit" })?;
        } else {
            let magnitude = raw
                .checked_neg()
                .ok_or(ObserveError::Overflow { field: "credit" })?;
            turnover.credit = turnover
                .credit
                .checked_add(PostedMinor::new(magnitude))
                .ok_or(ObserveError::Overflow { field: "credit" })?;
        }

        match leg.kind {
            LegKind::Fee => add_magnitude(&mut totals.fees, currency, raw, "fees")?,
            LegKind::Tax => {
                totals.tax_facts_recorded = true;
                add_magnitude(&mut totals.tax_withheld, currency, raw, "tax_withheld")?;
            }
            LegKind::Cash => {
                if is_income {
                    add_magnitude(&mut totals.income, currency, raw, "income")?;
                }
            }
            LegKind::SecurityQuantity | LegKind::Principal => {}
        }
    }
    Ok(())
}

/// Прибавление модуля величины: контрольные суммы отчёта — модули,
/// знак в них несёт название колонки, а не число.
fn add_magnitude(
    into: &mut BTreeMap<CurrencyCode, PostedMinor>,
    currency: CurrencyCode,
    raw: i64,
    field: &'static str,
) -> Result<(), ObserveError> {
    let magnitude = raw.checked_abs().ok_or(ObserveError::Overflow { field })?;
    let slot = into.entry(currency).or_insert_with(|| PostedMinor::new(0));
    *slot = slot
        .checked_add(PostedMinor::new(magnitude))
        .ok_or(ObserveError::Overflow { field })?;
    Ok(())
}
```

В `crates/iaam-core/src/reconciliation/mod.rs` добавьте объявление:

```rust
pub mod observed;
```

Если `PositionKey` или `Balances` не экспортированы из
`crate::projection::balances` — экспортируйте их там (`pub use`), не
копируя структуру: копия разойдётся с оригиналом при первой же правке.

- [ ] **Шаг 4: Прогнать тесты**

Run: `nix develop -c cargo test -p iaam-core reconciliation 2>&1 | tail -15`
Expected: PASS, восемь тестов модуля зелёные.

- [ ] **Шаг 5: Заслоны и коммит**

```bash
nix develop -c cargo fmt --all -- --check
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/iaam-core/src/reconciliation
git commit -m "feat(core): наблюдаемые величины интервала из журнала (iaam-023)"
```

---

## Задача 3: Сопоставление сторон и шесть вердиктов приёмки

**Files:**
- Create: `crates/iaam-core/src/reconciliation/check.rs`
- Modify: `crates/iaam-core/src/reconciliation/mod.rs` — `pub mod check;`
- Modify: `crates/iaam-ingest/src/verdict.rs` — недостающие вердикты §10.4
- Test: тесты в `check.rs`; тесты вердиктов в `verdict.rs`

**Interfaces:**
- Consumes: `ControlClaim`, `BalancePoint` (задача 1); `ObservedTotals`, `Turnover` (задача 2).
- Produces: `ClaimValue`, `Discrepancy`, `NotComparable`, `ReconciliationException`, `ClaimOutcome`, `check_claim(&ControlClaim, &ObservedTotals) -> ClaimOutcome`; в `iaam-ingest` — `Verdict::{Accepted, Discrepancy, NeedsReconciliation}`.

**Acceptance Criteria:**
- Совпадение проведённых сумм проверяется **точно**: допуска нет, обе стороны — `PostedMinor`
- Расхождение сообщает счёт-агрегат, заявленное, наблюдаемое и разницу; по оборотам называет сторону (`debit`/`credit`)
- Пустой журнал даёт `NotComparable`, а не расхождение на всю сумму утверждения
- Удержанный налог при отсутствии налоговых фактов даёт `NotComparable`, а не расхождение
- Валюта, по которой у счёта не было движения, при непустом журнале сверяется как ноль
- Вердиктов ровно шесть по §10.4 плюс служебные `Duplicate` и `Rejected`; у каждого машиночитаемый код, и `is_recorded` честно отвечает про запись

- [ ] **Шаг 1: Написать падающие тесты сопоставления**

Создайте `crates/iaam-core/src/reconciliation/check.rs` с блоком тестов:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::Money;
    use crate::numeric::decimal::Dec;
    use crate::reconciliation::observed::observe;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }

    fn journal_with_one_deposit(account: AccountId, minor: i64) -> Vec<crate::event::Event> {
        vec![event_with(
            account,
            date!(2026 - 03 - 10),
            1,
            EventKind::CashIn { amount: rub(minor) },
            vec![Leg::cash(account, rub(minor))],
        )]
    }

    #[test]
    fn an_exact_match_is_accepted_and_one_kopeck_is_not() {
        // Допуска нет. Обе стороны — проведённые суммы в минимальных
        // единицах; «почти сошлось» на копейку означает потерянную
        // копейку, а потерянная копейка — это ошибка разнесения,
        // которая на длинной истории вырастает.
        let account = AccountId::new_random();
        let observed = observe(&journal_with_one_deposit(account, 100_000), account, march()).unwrap();

        let exact = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&exact, &observed), ClaimOutcome::Matched);

        let off_by_one = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_001),
            at: BalancePoint::Closing,
        };
        let outcome = check_claim(&off_by_one, &observed);
        let ClaimOutcome::Discrepant(discrepancy) = outcome else {
            panic!("расхождение в одну копейку обязано быть расхождением: {outcome:?}");
        };
        assert_eq!(discrepancy.field, "amount");
        assert_eq!(
            discrepancy.delta,
            ClaimValue::Money {
                amount: PostedMinor::new(1),
                currency: CurrencyCode::Rub
            },
            "разница считается как заявленное минус наблюдаемое"
        );
    }

    #[test]
    fn an_empty_journal_is_not_comparable_rather_than_wrong() {
        // Утверждение «на счёте 100 000» при пустом журнале не является
        // расхождением на 100 000: сверять не с чем. Расхождение здесь
        // отправило бы владельца искать ошибку там, где её нет,
        // а нужен ему вердикт needs_reconciliation.
        let account = AccountId::new_random();
        let observed = observe(&[], account, march()).unwrap();
        let claim = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(
            check_claim(&claim, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::NoJournalCoverage
            }
        );
    }

    #[test]
    fn a_currency_without_movement_is_compared_as_zero_when_history_exists() {
        // История счёта есть, движения в долларах — нет. Утверждение
        // «на счёте 0 USD» подтверждается, а «на счёте 500 USD» —
        // расходится. Отдать здесь NotComparable значило бы навсегда
        // оставить непроверяемой любую валюту, в которой ничего не было.
        let account = AccountId::new_random();
        let observed = observe(&journal_with_one_deposit(account, 100_000), account, march()).unwrap();

        let zero = ControlClaim::CashBalance {
            currency: CurrencyCode::Usd,
            amount: PostedMinor::new(0),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&zero, &observed), ClaimOutcome::Matched);

        let nonzero = ControlClaim::CashBalance {
            currency: CurrencyCode::Usd,
            amount: PostedMinor::new(50_000),
            at: BalancePoint::Closing,
        };
        assert!(matches!(
            check_claim(&nonzero, &observed),
            ClaimOutcome::Discrepant(_)
        ));
    }

    #[test]
    fn a_turnover_names_the_side_that_disagrees() {
        // «Обороты не сошлись» без указания стороны заставляет владельца
        // сверять обе колонки вручную — ровно та работа, которую §10.2
        // отказывается на него перекладывать.
        let account = AccountId::new_random();
        let observed = observe(&journal_with_one_deposit(account, 100_000), account, march()).unwrap();

        let claim = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(100_000),
            credit: PostedMinor::new(700),
        };
        let ClaimOutcome::Discrepant(discrepancy) = check_claim(&claim, &observed) else {
            panic!("расход 700 против нуля обязан быть расхождением");
        };
        assert_eq!(discrepancy.field, "credit");
    }

    #[test]
    fn tax_without_tax_facts_is_not_comparable() {
        // Налоговых фактов не производит ни один путь записи до E5.
        // Ноль с нашей стороны означает «не считаем», и объявить
        // удержанные брокером 1 300 расхождением было бы ложью.
        let account = AccountId::new_random();
        let observed = observe(&journal_with_one_deposit(account, 100_000), account, march()).unwrap();
        let claim = ControlClaim::TaxWithheldTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(130_000),
        };
        assert_eq!(
            check_claim(&claim, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::TaxFactsNotRecorded
            }
        );
    }

    #[test]
    fn a_position_quantity_is_compared_per_custody() {
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let quantity = Quantity(Dec::new(Decimal::from(10)));
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 11),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: None,
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        )];
        let observed = observe(&events, account, march()).unwrap();

        let matching = ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&matching, &observed), ClaimOutcome::Matched);

        let elsewhere = ControlClaim::PositionQuantity {
            instrument,
            custody: CustodyId::new_random(),
            quantity,
            at: BalancePoint::Closing,
        };
        assert!(
            matches!(check_claim(&elsewhere, &observed), ClaimOutcome::Discrepant(_)),
            "то же количество в другом депозитарии — это другая позиция"
        );
    }
}
```

- [ ] **Шаг 2: Убедиться, что тесты не собираются**

Run: `nix develop -c cargo test -p iaam-core reconciliation::check 2>&1 | head -20`
Expected: FAIL — `cannot find function check_claim in this scope`.

- [ ] **Шаг 3: Написать сопоставление**

Добавьте в начало `check.rs`, перед тестами:

```rust
//! Сопоставление утверждения источника с наблюдаемым (§10.3, §10.4).
//!
//! **Допуска нет.** Обе стороны — проведённые суммы в минимальных
//! единицах валюты, и различие в копейку является различием.
//! Порог невязки существует там, где сравниваются расчётная величина
//! и проведённая (начисления по вкладу, §8.3), — это E3, и порог там
//! берётся из алгоритма округления договора, а не назначается здесь.

use crate::money::{CurrencyCode, PostedMinor, Quantity};
use crate::numeric::decimal::Dec;
use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use crate::reconciliation::observed::{ObservedTotals, Turnover};

/// Величина одной стороны сравнения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimValue {
    Money {
        amount: PostedMinor,
        currency: CurrencyCode,
    },
    Quantity(Quantity),
}

/// Расхождение: что заявлено, что наблюдается, какова разница.
///
/// Разница считается как заявленное минус наблюдаемое: положительная
/// означает «источник видит больше, чем мы».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discrepancy {
    /// Поле утверждения, которое не сошлось. Для оборотов называет
    /// сторону: `debit` или `credit`.
    pub field: &'static str,
    pub claimed: ClaimValue,
    pub observed: ClaimValue,
    pub delta: ClaimValue,
}

/// Почему сравнение невозможно.
///
/// Невозможность сравнить — **не** расхождение. Расхождение означает
/// «цифры разошлись, разберитесь»; невозможность означает «сверять
/// не с чем», и это разные ответы владельцу (§10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotComparable {
    /// У счёта нет ни одного события: подтверждать нечего.
    NoJournalCoverage,
    /// Налоговых фактов система пока не записывает (E5).
    TaxFactsNotRecorded,
}

impl NotComparable {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoJournalCoverage => "no_journal_coverage",
            Self::TaxFactsNotRecorded => "tax_facts_not_recorded",
        }
    }
}

/// Расхождение, объяснённое границей периметра (§11).
///
/// Существует, чтобы владелец не получал задание «починить» то, что
/// система намеренно не поддерживает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationException {
    /// Количества разошлись из-за обременения бумаг по РЕПО.
    UnsupportedRepoEncumbrance,
    /// В периоде присутствует финансирование вне периметра (маржа).
    UnsupportedFinancingPresent,
}

impl ReconciliationException {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedRepoEncumbrance => "unsupported_repo_encumbrance",
            Self::UnsupportedFinancingPresent => "unsupported_financing_present",
        }
    }
}

/// Итог сверки одного утверждения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Matched,
    Discrepant(Discrepancy),
    NotComparable { reason: NotComparable },
    /// Расхождение объяснено границей периметра и не требует действий
    /// владельца (§11). Основанием повышения статуса не является.
    Excepted { exception: ReconciliationException },
}

impl ClaimOutcome {
    /// Даёт ли исход право повысить статус измерения.
    ///
    /// Исключение периметра не даёт: «мы знаем, почему не сходится» —
    /// это не «сошлось».
    #[must_use]
    pub const fn confirms(&self) -> bool {
        match self {
            Self::Matched => true,
            Self::Discrepant(_) | Self::NotComparable { .. } | Self::Excepted { .. } => false,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::Discrepant(_) => "discrepant",
            Self::NotComparable { .. } => "not_comparable",
            Self::Excepted { .. } => "excepted",
        }
    }
}

/// Сверка одного утверждения с наблюдаемыми величинами.
#[must_use]
pub fn check_claim(claim: &ControlClaim, observed: &ObservedTotals) -> ClaimOutcome {
    if observed.events_seen() == 0 {
        return ClaimOutcome::NotComparable {
            reason: NotComparable::NoJournalCoverage,
        };
    }
    match *claim {
        ControlClaim::CashBalance {
            currency,
            amount,
            at,
        } => compare_money(
            "amount",
            currency,
            amount,
            observed.cash_at(at, currency).unwrap_or(PostedMinor::new(0)),
        ),
        ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at,
        } => compare_quantity(
            quantity,
            observed
                .position_at(at, instrument, custody)
                .unwrap_or_else(Quantity::zero),
        ),
        ControlClaim::CashTurnover {
            currency,
            debit,
            credit,
        } => {
            let seen = observed.turnover(currency).unwrap_or_default();
            let Turnover {
                debit: seen_debit,
                credit: seen_credit,
            } = seen;
            match compare_money("debit", currency, debit, seen_debit) {
                ClaimOutcome::Matched => {
                    compare_money("credit", currency, credit, seen_credit)
                }
                other => other,
            }
        }
        ControlClaim::FeesTotal { currency, amount } => compare_money(
            "amount",
            currency,
            amount,
            observed.fees(currency).unwrap_or(PostedMinor::new(0)),
        ),
        ControlClaim::IncomeTotal { currency, amount } => compare_money(
            "amount",
            currency,
            amount,
            observed.income(currency).unwrap_or(PostedMinor::new(0)),
        ),
        ControlClaim::TaxWithheldTotal { currency, amount } => {
            if observed.tax_facts_recorded() {
                compare_money(
                    "amount",
                    currency,
                    amount,
                    observed.tax_withheld(currency).unwrap_or(PostedMinor::new(0)),
                )
            } else {
                ClaimOutcome::NotComparable {
                    reason: NotComparable::TaxFactsNotRecorded,
                }
            }
        }
    }
}

fn compare_money(
    field: &'static str,
    currency: CurrencyCode,
    claimed: PostedMinor,
    observed: PostedMinor,
) -> ClaimOutcome {
    if claimed == observed {
        return ClaimOutcome::Matched;
    }
    // Переполнение разницы означает величины, между которыми разрыв
    // больше диапазона денежного типа: это расхождение в любом случае,
    // и сообщается оно насыщением, а не паникой.
    let delta = claimed.raw().saturating_sub(observed.raw());
    ClaimOutcome::Discrepant(Discrepancy {
        field,
        claimed: ClaimValue::Money {
            amount: claimed,
            currency,
        },
        observed: ClaimValue::Money {
            amount: observed,
            currency,
        },
        delta: ClaimValue::Money {
            amount: PostedMinor::new(delta),
            currency,
        },
    })
}

fn compare_quantity(claimed: Quantity, observed: Quantity) -> ClaimOutcome {
    if claimed == observed {
        return ClaimOutcome::Matched;
    }
    // Разница количеств не вычисляется через checked_sub с обработкой
    // ошибки наверх: невычислимая разница всё равно является
    // расхождением, и сообщается она нулём с уже названными сторонами.
    let delta = claimed.0.checked_sub(observed.0).unwrap_or_else(|_| Dec::zero());
    ClaimOutcome::Discrepant(Discrepancy {
        field: "quantity",
        claimed: ClaimValue::Quantity(claimed),
        observed: ClaimValue::Quantity(observed),
        delta: ClaimValue::Quantity(Quantity(delta)),
    })
}

/// Интервал утверждения нужен потребителям сверки рядом с исходом,
/// поэтому переэкспортируется отсюда: искать его в двух модулях —
/// лишний повод сослаться не на тот.
pub use crate::reconciliation::claim::AssertionPeriod as CheckedPeriod;
```

В `crates/iaam-core/src/reconciliation/mod.rs`:

```rust
pub mod check;
```

- [ ] **Шаг 4: Дописать недостающие вердикты §10.4**

В `crates/iaam-ingest/src/verdict.rs` замените `enum Verdict` и его
реализацию:

```rust
/// Вердикт по одной строке.
///
/// Отдельного шага подтверждения в нормальном пути нет: есть отправка
/// и вердикт (§10.4). Шесть вердиктов спеки — `Accepted`,
/// `Provisional`, `Discrepancy`, `NeedsReconciliation`,
/// `NeedsClassification`, `Unsupported`. `Duplicate` и `Rejected`
/// служебные: первый отвечает на повтор (§10.6), второй — на строку,
/// которую не удалось разобрать (§10.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Записано, сверка сошлась.
    Accepted { event: EventId },
    /// Записано, независимого подтверждения пока нет.
    Provisional { event: EventId },
    /// Записано, но сверка не сходится: владелец разбирается.
    Discrepancy {
        event: EventId,
        account: AccountId,
        dimension: Dimension,
        detail: String,
    },
    /// Сверять не с чем: требуется остаток от владельца.
    NeedsReconciliation {
        account: AccountId,
        dimension: Dimension,
    },
    /// Уже записано ранее по ключу идемпотентности (§10.6).
    Duplicate { existing: EventId },
    /// Классификация неоднозначна: нужен ответ владельца.
    NeedsClassification { question: String },
    /// Операция вне периметра (§11): денежный эффект сохранён,
    /// экономика не достраивается.
    Unsupported { reason: String },
    /// Строка не разобрана.
    Rejected { rejection: Rejection },
}

impl Verdict {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Accepted { .. } => "accepted",
            Self::Provisional { .. } => "provisional",
            Self::Discrepancy { .. } => "discrepancy",
            Self::NeedsReconciliation { .. } => "needs_reconciliation",
            Self::Duplicate { .. } => "duplicate",
            Self::NeedsClassification { .. } => "needs_classification",
            Self::Unsupported { .. } => "unsupported",
            Self::Rejected { .. } => "rejected",
        }
    }

    /// Была ли строка записана в журнал.
    ///
    /// Расхождение записано: факт получен, и скрывать его до выяснения
    /// значило бы терять данные. Требование сверки — нет: там записывать
    /// нечего, вопрос задан владельцу.
    #[must_use]
    pub const fn is_recorded(&self) -> bool {
        match self {
            Self::Accepted { .. }
            | Self::Provisional { .. }
            | Self::Discrepancy { .. }
            | Self::Duplicate { .. } => true,
            Self::NeedsReconciliation { .. }
            | Self::NeedsClassification { .. }
            | Self::Unsupported { .. }
            | Self::Rejected { .. } => false,
        }
    }
}
```

Добавьте импорты в шапку файла:

```rust
use iaam_core::ids::{AccountId, EventId};
use iaam_core::reconciliation::Dimension;
```

Добавьте тест в тот же файл:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verdict_has_a_distinct_code_and_six_of_them_are_the_spec_verdicts() {
        // §10.4 называет шесть вердиктов. Duplicate и Rejected служебные:
        // они отвечают на повтор и на неразобранную строку, а не на
        // результат приёмки. Проверяется и то, и другое — потерянный
        // вердикт превращается в молчание там, где владелец ждёт ответа.
        let event = EventId::new_random();
        let account = AccountId::new_random();
        let all = [
            Verdict::Accepted { event },
            Verdict::Provisional { event },
            Verdict::Discrepancy {
                event,
                account,
                dimension: Dimension::Cash,
                detail: "остаток на конец марта".to_owned(),
            },
            Verdict::NeedsReconciliation {
                account,
                dimension: Dimension::Cash,
            },
            Verdict::Duplicate { existing: event },
            Verdict::NeedsClassification {
                question: "перевод внутренний?".to_owned(),
            },
            Verdict::Unsupported {
                reason: "РЕПО".to_owned(),
            },
            Verdict::Rejected {
                rejection: Rejection {
                    field: "date".to_owned(),
                    expected: "ДД.ММ.ГГГГ".to_owned(),
                    actual: "вчера".to_owned(),
                },
            },
        ];
        let mut codes: Vec<&str> = all.iter().map(Verdict::code).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count, "коды вердиктов совпали");

        let spec_verdicts = [
            "accepted",
            "provisional",
            "discrepancy",
            "needs_reconciliation",
            "needs_classification",
            "unsupported",
        ];
        for verdict in spec_verdicts {
            assert!(codes.contains(&verdict), "вердикт {verdict} потерян");
        }
    }

    #[test]
    fn a_discrepancy_is_recorded_and_a_reconciliation_request_is_not() {
        // Расхождение — записанный факт с открытым вопросом. Требование
        // сверки — вопрос без факта. Слить их значит либо потерять
        // данные, либо записать в журнал то, чего не было.
        let event = EventId::new_random();
        let account = AccountId::new_random();
        assert!(
            Verdict::Discrepancy {
                event,
                account,
                dimension: Dimension::Positions,
                detail: String::new(),
            }
            .is_recorded()
        );
        assert!(
            !Verdict::NeedsReconciliation {
                account,
                dimension: Dimension::Positions,
            }
            .is_recorded()
        );
    }
}
```

- [ ] **Шаг 5: Починить места, где `Verdict` разбирается исчерпывающе**

Сборка укажет на `iaam-app/src/scenarios/ingest.rs` и
`iaam-server/src/dto.rs`. Новые варианты там пока не производятся:
в `dto.rs` добавьте их сериализацию по образцу существующих, в
сценарии оставьте прежнее поведение — `Accepted` появится только
после сверки (задача 21). Ветку `_ =>` не добавляйте.

- [ ] **Шаг 6: Прогнать тесты и заслоны**

```bash
nix develop -c cargo test --workspace 2>&1 | tail -20
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```
Expected: PASS.

- [ ] **Шаг 7: Коммит**

```bash
git add crates/iaam-core/src/reconciliation crates/iaam-ingest/src/verdict.rs \
        crates/iaam-server/src/dto.rs crates/iaam-app/src/scenarios/ingest.rs
git commit -m "feat(core): сопоставление сторон сверки и шесть вердиктов приёмки (iaam-023)"
```

---

## Задача 4: Восемь оснований повышения и правило независимости

**Files:**
- Create: `crates/iaam-core/src/reconciliation/evidence.rs`
- Modify: `crates/iaam-core/src/reconciliation/mod.rs` — `pub mod evidence;`, `ConfidenceLevel`
- Test: `crates/iaam-core/tests/reconciliation_grounds.rs`

**Interfaces:**
- Consumes: `Dimension` (задача 1), `ClaimOutcome` (задача 3), `Provenance`/`ParserVersion`/`RawHash` из `event::provenance`.
- Produces: `ConfidenceLevel` (`Provisional < AcceptedInternal < AcceptedIndependent`, `Ord`); `SourceChannel { source, parser_version, document }` с `is_independent_of(&self, &Self) -> bool`; `Ground` (девять вариантов), `Ground::{ceiling, dimensions, code}`; `Evidence` с `Evidence::from_match(...) -> Option<Self>`, `level()`, `dimensions()`, `ground()`.

**Acceptance Criteria:**
- Все восемь оснований §10.3 представлены вариантами; девятый — названный владельцем остаток (§10.4)
- `independent` присваивается **только** когда подтверждающий канал отличается от подтверждаемого и версией парсера, и документом; иначе основание понижается до `internal`
- Следующий отчёт того же брокера, разобранный тем же парсером, даёт `internal`, а не `independent` — отдельный тест именно на этот случай
- Депозитарный отчёт повышает только `Positions`; справка налогового агента — только `Income` и `TaxBasis`; названный владельцем остаток — только `Cash` и `Positions`
- Основание, построенное из несошедшегося исхода, не создаётся вовсе
- Каждое основание имеет машиночитаемый код

- [ ] **Шаг 1: Написать падающий тест на таблицу оснований**

Создайте `crates/iaam-core/tests/reconciliation_grounds.rs`:

```rust
//! Восемь оснований автоматического повышения статуса (§10.3, таблица).
//!
//! Тест перечисляет основания по спеке построчно. Ожидаемые уровни
//! взяты из таблицы §10.3, а не из вывода программы (§15.5).

use std::collections::BTreeSet;

use iaam_core::event::provenance::{ParserVersion, RawHash};
use iaam_core::ids::SourceId;
use iaam_core::reconciliation::evidence::{Evidence, Ground, SourceChannel};
use iaam_core::reconciliation::{ConfidenceLevel, Dimension};

fn hash(seed: &str) -> RawHash {
    RawHash::parse(&seed.repeat(64)).unwrap()
}

fn report_channel(parser: &str, document: &str) -> SourceChannel {
    SourceChannel {
        source: SourceId::new_random(),
        parser_version: ParserVersion(parser.to_owned()),
        document: Some(hash(document)),
    }
}

fn api_channel(parser: &str) -> SourceChannel {
    // У ответа API документа нет: это поток, а не файл.
    SourceChannel {
        source: SourceId::new_random(),
        parser_version: ParserVersion(parser.to_owned()),
        document: None,
    }
}

fn dims(list: &[Dimension]) -> BTreeSet<Dimension> {
    list.iter().copied().collect()
}

#[test]
fn ground_one_opening_matches_prior_closing_is_internal_only() {
    // Тот же брокер и тот же парсер: общая ошибка разбора исказит
    // обе стороны одинаково, и сверка её не заметит. Это непрерывность,
    // а не независимость.
    let confirmed = report_channel("tinkoff-xlsx/1", "a");
    let confirming = report_channel("tinkoff-xlsx/1", "b");
    let evidence = Evidence::from_match(
        Ground::OpeningMatchesPriorClosing,
        confirming,
        confirmed,
        dims(&[Dimension::Cash, Dimension::Positions]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedInternal);
}

#[test]
fn ground_two_continuity_is_internal_only() {
    let confirmed = report_channel("finam-xls/1", "a");
    let confirming = report_channel("finam-xls/1", "b");
    let evidence = Evidence::from_match(
        Ground::ContinuityBetweenStatements,
        confirming,
        confirmed,
        dims(&[Dimension::Cash]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedInternal);
}

#[test]
fn ground_three_broker_api_against_a_parsed_report_is_independent() {
    // Другой канал получения и другой код разбора — условие §10.3
    // выполнено, и только здесь появляется independent.
    let confirmed = report_channel("tinkoff-xlsx/1", "a");
    let confirming = api_channel("tinkoff-api/1");
    let evidence = Evidence::from_match(
        Ground::BrokerApiAgreesWithStatement,
        confirming,
        confirmed,
        dims(&[Dimension::Cash, Dimension::Positions]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedIndependent);
}

#[test]
fn ground_three_degrades_to_internal_when_the_channel_is_not_independent() {
    // Ключевая проверка §10.3: уровень определяется независимостью
    // канала, а не типом основания. Если «API» разобран тем же кодом
    // и тем же документом, никакой независимости нет.
    let confirmed = report_channel("tinkoff-xlsx/1", "a");
    let confirming = report_channel("tinkoff-xlsx/1", "a");
    let evidence = Evidence::from_match(
        Ground::BrokerApiAgreesWithStatement,
        confirming,
        confirmed,
        dims(&[Dimension::Cash]),
    )
    .unwrap();
    assert_eq!(
        evidence.level(),
        ConfidenceLevel::AcceptedInternal,
        "тот же парсер и тот же документ не дают независимости"
    );
}

#[test]
fn a_later_statement_of_the_same_broker_never_reaches_independent() {
    // Прямая формулировка спеки: «Следующий отчёт того же брокера,
    // разобранный тем же парсером, — это непрерывность, а не
    // независимость». Документы разные, парсер один.
    let confirmed = report_channel("tinkoff-xlsx/3", "march");
    let confirming = report_channel("tinkoff-xlsx/3", "april");
    assert!(!confirming.is_independent_of(&confirmed));

    for ground in [
        Ground::OpeningMatchesPriorClosing,
        Ground::ContinuityBetweenStatements,
        Ground::BrokerApiAgreesWithStatement,
        Ground::DepositaryReportConfirms,
    ] {
        let evidence =
            Evidence::from_match(ground, confirming.clone(), confirmed.clone(), dims(&[Dimension::Positions]));
        if let Some(evidence) = evidence {
            assert!(
                evidence.level() <= ConfidenceLevel::AcceptedInternal,
                "основание {ground:?} выдало independent на одном парсере"
            );
        }
    }
}

#[test]
fn a_reparse_of_the_same_document_by_a_new_parser_is_not_independent() {
    // Новая версия парсера по тому же документу — это исправленный
    // разбор, а не второй источник. Документ один, и ошибка в нём
    // самом останется незамеченной обеими сторонами.
    let confirmed = report_channel("tinkoff-xlsx/1", "march");
    let confirming = report_channel("tinkoff-xlsx/2", "march");
    assert!(!confirming.is_independent_of(&confirmed));
}

#[test]
fn ground_four_depositary_report_raises_positions_only() {
    // Депозитарий подтверждает количества и место хранения. О деньгах
    // он не говорит ничего, и повысить ими денежное измерение значило
    // бы выдать подтверждение, которого не было.
    let confirmed = report_channel("tinkoff-xlsx/1", "a");
    let confirming = report_channel("depositary-pdf/1", "b");
    let evidence = Evidence::from_match(
        Ground::DepositaryReportConfirms,
        confirming,
        confirmed,
        dims(&[Dimension::Cash, Dimension::Positions]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedIndependent);
    assert_eq!(evidence.dimensions(), dims(&[Dimension::Positions]));
}

#[test]
fn ground_five_separate_sections_agree_is_internal_across_dimensions() {
    // Независимые уравнения, но один документ и один парсер.
    let channel = report_channel("tinkoff-xlsx/1", "march");
    let evidence = Evidence::from_match(
        Ground::SeparateSectionsAgree,
        channel.clone(),
        channel,
        dims(&[Dimension::Cash, Dimension::Positions, Dimension::Income]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedInternal);
    assert_eq!(
        evidence.dimensions(),
        dims(&[Dimension::Cash, Dimension::Positions, Dimension::Income])
    );
}

#[test]
fn ground_six_payout_is_independent_only_through_another_channel() {
    // Выписка банка против нашей проекции — независимо. Та же выписка,
    // что дала условия договора, — нет.
    let terms = report_channel("bank-statement/1", "contract");
    let other_channel = report_channel("bank-api/1", "statement");
    let independent = Evidence::from_match(
        Ground::PayoutConfirmsSchedule,
        other_channel,
        terms.clone(),
        dims(&[Dimension::Income]),
    )
    .unwrap();
    assert_eq!(independent.level(), ConfidenceLevel::AcceptedIndependent);

    let same = Evidence::from_match(
        Ground::PayoutConfirmsSchedule,
        terms.clone(),
        terms,
        dims(&[Dimension::Income]),
    )
    .unwrap();
    assert_eq!(same.level(), ConfidenceLevel::AcceptedInternal);
}

#[test]
fn ground_seven_corporate_action_terms_raise_positions() {
    let confirmed = report_channel("tinkoff-xlsx/1", "a");
    let confirming = api_channel("moex-iss/1");
    let evidence = Evidence::from_match(
        Ground::CorporateActionMatchesIssueTerms,
        confirming,
        confirmed,
        dims(&[Dimension::Positions]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedIndependent);
    assert_eq!(evidence.dimensions(), dims(&[Dimension::Positions]));
}

#[test]
fn ground_eight_tax_certificate_raises_income_and_tax_basis_only() {
    // Отдельный документ, отдельный парсер — independent, но только
    // по агрегатам: справка не подтверждает ни остаток, ни количества.
    let confirmed = report_channel("tinkoff-xlsx/1", "a");
    let confirming = report_channel("tax-certificate/1", "b");
    let evidence = Evidence::from_match(
        Ground::TaxAgentCertificate,
        confirming,
        confirmed,
        dims(&[Dimension::Cash, Dimension::Income, Dimension::TaxBasis]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedIndependent);
    assert_eq!(
        evidence.dimensions(),
        dims(&[Dimension::Income, Dimension::TaxBasis])
    );
}

#[test]
fn an_owner_stated_balance_is_internal_and_touches_cash_and_positions_only() {
    // §10.4: названный владельцем остаток подтверждает снимок и
    // не трогает налоговую стоимость и доходы. Уровень — internal:
    // владелец мог прочитать ту же цифру в том же отчёте, и
    // независимость здесь не доказана, а §10.3 требует доказательства.
    let confirmed = report_channel("tinkoff-xlsx/1", "a");
    let owner = SourceChannel {
        source: SourceId::new_random(),
        parser_version: ParserVersion("owner/1".to_owned()),
        document: None,
    };
    let evidence = Evidence::from_match(
        Ground::OwnerStatedBalance,
        owner,
        confirmed,
        dims(&[
            Dimension::Cash,
            Dimension::Positions,
            Dimension::TaxBasis,
            Dimension::Income,
        ]),
    )
    .unwrap();
    assert_eq!(evidence.level(), ConfidenceLevel::AcceptedInternal);
    assert_eq!(
        evidence.dimensions(),
        dims(&[Dimension::Cash, Dimension::Positions])
    );
}

#[test]
fn evidence_without_any_confirmed_dimension_does_not_exist() {
    // Основание, ничего не подтверждающее, — это не основание.
    // Пустое множество измерений здесь опаснее ошибки: оно молча
    // добавляет строку в список доказательств.
    let channel = report_channel("tinkoff-xlsx/1", "a");
    assert!(
        Evidence::from_match(
            Ground::DepositaryReportConfirms,
            channel.clone(),
            channel,
            dims(&[Dimension::Cash]),
        )
        .is_none(),
        "депозитарий не подтверждает деньги — основания нет"
    );
}

#[test]
fn every_ground_has_a_distinct_machine_readable_code() {
    let grounds = [
        Ground::OpeningMatchesPriorClosing,
        Ground::ContinuityBetweenStatements,
        Ground::BrokerApiAgreesWithStatement,
        Ground::DepositaryReportConfirms,
        Ground::SeparateSectionsAgree,
        Ground::PayoutConfirmsSchedule,
        Ground::CorporateActionMatchesIssueTerms,
        Ground::TaxAgentCertificate,
        Ground::OwnerStatedBalance,
    ];
    let mut codes: Vec<&str> = grounds.iter().map(|g| g.code()).collect();
    let count = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), count);
    assert_eq!(count, 9, "восемь оснований §10.3 плюс остаток от владельца");
}
```

- [ ] **Шаг 2: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-core --test reconciliation_grounds 2>&1 | head -20`
Expected: FAIL — `unresolved import iaam_core::reconciliation::evidence`.

- [ ] **Шаг 3: Завести уровень достоверности**

В `crates/iaam-core/src/reconciliation/mod.rs` добавьте после `Dimension`:

```rust
/// Уровень достоверности утверждения (§10.3).
///
/// Порядок значим: `PartialOrd` используется для повышения статуса.
/// Уровней три, а не два, потому что операции и контрольные остатки
/// извлекаются одним парсером из одного документа: общая ошибка разбора
/// исказит обе стороны проверки одинаково, и сверка её не заметит.
/// Средний уровень существует ровно для этого случая и называет вещи
/// своими именами — «сошлось внутри одного источника».
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ConfidenceLevel {
    Provisional,
    AcceptedInternal,
    AcceptedIndependent,
}

impl ConfidenceLevel {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::AcceptedInternal => "accepted_internal",
            Self::AcceptedIndependent => "accepted_independent",
        }
    }
}
```

И объявление модуля:

```rust
pub mod evidence;
```

- [ ] **Шаг 4: Написать основания и правило независимости**

Создайте `crates/iaam-core/src/reconciliation/evidence.rs`:

```rust
//! Основания автоматического повышения статуса (§10.3).
//!
//! Восемь оснований спеки плюс девятое — названный владельцем остаток
//! (§10.4). Участия человека ни одно из первых восьми не требует.
//!
//! **Уровень определяется независимостью канала, а не типом
//! основания.** Это главное правило модуля: основание лишь задаёт
//! потолок, а фактический уровень получается понижением потолка до
//! `internal`, если независимость не доказана.

use std::collections::BTreeSet;

use crate::event::provenance::{ParserVersion, RawHash};
use crate::ids::SourceId;
use crate::reconciliation::{ConfidenceLevel, Dimension};

/// Канал, которым получены данные.
///
/// Документ — хеш файла, из которого разобраны данные. У ответа API
/// документа нет: это поток, а не файл, и `None` здесь означает
/// именно «файла не было», а не «хеш не посчитали».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceChannel {
    pub source: SourceId,
    pub parser_version: ParserVersion,
    pub document: Option<RawHash>,
}

impl SourceChannel {
    /// Независим ли этот канал от другого (§10.3).
    ///
    /// Критерий спеки: подтверждающие данные не должны проходить через
    /// **тот же код разбора** и **тот же документ**. Оба условия
    /// обязательны, поэтому здесь конъюнкция:
    ///
    /// - тот же парсер, другой документ — следующий отчёт того же
    ///   брокера: непрерывность, но не независимость;
    /// - другой парсер, тот же документ — повторный разбор новой
    ///   версией: исправленный разбор, но источник тот же.
    ///
    /// Идентификатор источника в критерий **не входит**: два источника
    /// могут делить код разбора, и тогда общая ошибка исказит обе
    /// стороны, сколько бы разных идентификаторов у них ни было.
    #[must_use]
    pub fn is_independent_of(&self, other: &Self) -> bool {
        self.parser_version != other.parser_version && self.document != other.document
    }
}

/// Основание повышения статуса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ground {
    /// 1. Начальный остаток следующего отчёта совпал с вычисленным
    ///    остатком предыдущего периода.
    OpeningMatchesPriorClosing,
    /// 2. Конечный остаток одного отчёта совпал с начальным следующего.
    ContinuityBetweenStatements,
    /// 3. API брокера совпал с разобранным отчётом.
    BrokerApiAgreesWithStatement,
    /// 4. Депозитарный отчёт подтвердил количества и место хранения.
    DepositaryReportConfirms,
    /// 5. Раздельные контрольные секции одного документа сошлись
    ///    одновременно.
    SeparateSectionsAgree,
    /// 6. Фактическая выплата подтвердила график предшествующего периода.
    PayoutConfirmsSchedule,
    /// 7. Количества после корпоративного действия совпали с параметрами
    ///    выпуска.
    CorporateActionMatchesIssueTerms,
    /// 8. Справка налогового агента подтвердила агрегаты.
    TaxAgentCertificate,
    /// Названный владельцем остаток (§10.4). Не входит в восемь
    /// автоматических оснований: требует участия человека.
    OwnerStatedBalance,
}

impl Ground {
    /// Потолок уровня, который основание может дать в принципе.
    ///
    /// Основания 1, 2 и 5 ограничены `internal` **по устройству**:
    /// они сравнивают данные, прошедшие через один и тот же парсер.
    /// Опустить это ограничение и положиться на проверку независимости
    /// нельзя: у оснований 1 и 2 документы разные, и проверка их
    /// пропустила бы, если бы парсер вдруг тоже отличался.
    #[must_use]
    pub const fn ceiling(self) -> ConfidenceLevel {
        match self {
            Self::OpeningMatchesPriorClosing
            | Self::ContinuityBetweenStatements
            | Self::SeparateSectionsAgree
            | Self::OwnerStatedBalance => ConfidenceLevel::AcceptedInternal,
            Self::BrokerApiAgreesWithStatement
            | Self::DepositaryReportConfirms
            | Self::PayoutConfirmsSchedule
            | Self::CorporateActionMatchesIssueTerms
            | Self::TaxAgentCertificate => ConfidenceLevel::AcceptedIndependent,
        }
    }

    /// Какие измерения основание вправе повысить.
    ///
    /// Ограничение существенно: депозитарий не говорит о деньгах,
    /// справка налогового агента — только об агрегатах, названный
    /// владельцем остаток — только о снимке (§10.4).
    #[must_use]
    pub fn dimensions(self) -> BTreeSet<Dimension> {
        let list: &[Dimension] = match self {
            Self::OpeningMatchesPriorClosing
            | Self::ContinuityBetweenStatements
            | Self::BrokerApiAgreesWithStatement
            | Self::OwnerStatedBalance => &[Dimension::Cash, Dimension::Positions],
            Self::DepositaryReportConfirms | Self::CorporateActionMatchesIssueTerms => {
                &[Dimension::Positions]
            }
            Self::SeparateSectionsAgree => &[
                Dimension::Cash,
                Dimension::Positions,
                Dimension::Income,
                Dimension::TaxBasis,
            ],
            Self::PayoutConfirmsSchedule => &[Dimension::Income],
            Self::TaxAgentCertificate => &[Dimension::Income, Dimension::TaxBasis],
        };
        list.iter().copied().collect()
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::OpeningMatchesPriorClosing => "opening_matches_prior_closing",
            Self::ContinuityBetweenStatements => "continuity_between_statements",
            Self::BrokerApiAgreesWithStatement => "broker_api_agrees_with_statement",
            Self::DepositaryReportConfirms => "depositary_report_confirms",
            Self::SeparateSectionsAgree => "separate_sections_agree",
            Self::PayoutConfirmsSchedule => "payout_confirms_schedule",
            Self::CorporateActionMatchesIssueTerms => "corporate_action_matches_issue_terms",
            Self::TaxAgentCertificate => "tax_agent_certificate",
            Self::OwnerStatedBalance => "owner_stated_balance",
        }
    }
}

/// Состоявшееся подтверждение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    ground: Ground,
    confirming: SourceChannel,
    confirmed: SourceChannel,
    dimensions: BTreeSet<Dimension>,
}

impl Evidence {
    /// Построение основания из состоявшегося совпадения.
    ///
    /// Возвращает `None`, когда основание не подтверждает ни одного
    /// из сошедшихся измерений: основание, ничего не подтверждающее,
    /// является не пустым основанием, а его отсутствием, и попадание
    /// такого в список доказательств создавало бы видимость проверки.
    ///
    /// Логика живёт не в `new`: `cargo-mutants` пропускает это имя.
    #[must_use]
    pub fn from_match(
        ground: Ground,
        confirming: SourceChannel,
        confirmed: SourceChannel,
        matched_dimensions: BTreeSet<Dimension>,
    ) -> Option<Self> {
        let dimensions: BTreeSet<Dimension> = ground
            .dimensions()
            .intersection(&matched_dimensions)
            .copied()
            .collect();
        (!dimensions.is_empty()).then_some(Self {
            ground,
            confirming,
            confirmed,
            dimensions,
        })
    }

    /// Уровень, который даёт это основание.
    ///
    /// Потолок основания понижается до `internal`, если независимость
    /// канала не доказана. Обратного хода нет: `internal` никогда не
    /// повышается проверкой канала — основание, ограниченное `internal`
    /// по устройству, ограничено им всегда.
    #[must_use]
    pub fn level(&self) -> ConfidenceLevel {
        let ceiling = self.ground.ceiling();
        if ceiling == ConfidenceLevel::AcceptedIndependent
            && !self.confirming.is_independent_of(&self.confirmed)
        {
            return ConfidenceLevel::AcceptedInternal;
        }
        ceiling
    }

    #[must_use]
    pub fn dimensions(&self) -> BTreeSet<Dimension> {
        self.dimensions.clone()
    }

    #[must_use]
    pub const fn ground(&self) -> Ground {
        self.ground
    }

    #[must_use]
    pub const fn confirming(&self) -> &SourceChannel {
        &self.confirming
    }

    #[must_use]
    pub const fn confirmed(&self) -> &SourceChannel {
        &self.confirmed
    }
}
```

- [ ] **Шаг 5: Прогнать тесты**

Run: `nix develop -c cargo test -p iaam-core --test reconciliation_grounds 2>&1 | tail -20`
Expected: PASS — четырнадцать тестов зелёные.

- [ ] **Шаг 6: Заслоны и коммит**

```bash
nix develop -c cargo fmt --all -- --check
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/iaam-core/src/reconciliation crates/iaam-core/tests/reconciliation_grounds.rs
git commit -m "feat(core): восемь оснований повышения и правило независимости канала (iaam-023)"
```

---

## Задача 5: Статус по паре интервал×измерение

**Files:**
- Modify: `crates/iaam-core/src/reconciliation/mod.rs` — `DimensionStatus`, `ReconciliationStatus`, `ReconciliationLedger`, `reconcile`
- Test: `crates/iaam-core/tests/reconciliation_ledger.rs`

**Interfaces:**
- Consumes: всё из задач 1–4.
- Produces: `DimensionStatus` (`Provisional`, `AcceptedInternal`, `AcceptedIndependent`, `Discrepant`); `ReconciliationStatus` с `account()`, `period()`, `dimension(Dimension) -> DimensionStatus`, `evidence() -> &[Evidence]`, `outcomes() -> &[ClaimCheck]`; `ClaimCheck { claim, outcome }`; `ReconciliationLedger` с `build(events: &[Event]) -> Result<Self, ObserveError>`, `with_external_evidence(Vec<(AccountId, AssertionPeriod, Evidence)>) -> Self`, `statuses() -> impl Iterator`, `status_for(AccountId, Date, Dimension) -> DimensionStatus`.

**Acceptance Criteria:**
- Статус вычисляется **чисто** из журнала: тот же журнал всегда даёт тот же статус, хранимого состояния нет
- Расхождение по измерению делает его `Discrepant` независимо от других оснований — подтверждение не затирает несошедшуюся цифру
- Основание 5 требует, чтобы в документе сошлись **все** утверждения и присутствовали как остаток, так и оборот; одной сошедшейся секции недостаточно
- Основание 1 повышает **предыдущий** период, а не тот, в котором пришёл начальный остаток
- Два независимых канала за один период дают `AcceptedIndependent`; два отчёта одного парсера — не выше `AcceptedInternal`
- Измерение без единого утверждения остаётся `Provisional`, а не становится `Clean` по умолчанию
- Исход `Excepted` (§11) не подтверждает и не делает измерение расходящимся

- [ ] **Шаг 1: Написать падающий тест реестра**

Создайте `crates/iaam-core/tests/reconciliation_ledger.rs`:

```rust
//! Статус полноты счёта на интервале по измерению (§10.3).

use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use time::Date;
use time::macros::date;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn hash(seed: &str) -> RawHash {
    RawHash::parse(&seed.repeat(64)).unwrap()
}

/// Событие с заданным каналом происхождения. Канал — это то, чем
/// разобрали и из какого документа; именно он решает вопрос
/// независимости (§10.3).
fn event_from(
    owner: OwnerId,
    account: AccountId,
    day: Date,
    sequence: u32,
    kind: EventKind,
    legs: Vec<Leg>,
    parser: &str,
    document: &str,
) -> Event {
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind,
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs,
        provenance: Provenance::new(
            SourceId::new_random(),
            hash(document),
            ParserVersion(parser.to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn march() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
}

/// Полный набор контрольных секций одного документа: остаток на начало,
/// остаток на конец и обороты. Именно такой набор даёт основание 5.
fn full_sections(
    owner: OwnerId,
    account: AccountId,
    opening: i64,
    closing: i64,
    debit: i64,
    credit: i64,
    parser: &str,
    document: &str,
) -> Vec<Event> {
    let period = march();
    [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(opening),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(closing),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(debit),
            credit: PostedMinor::new(credit),
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(index, claim)| {
        event_from(
            owner,
            account,
            date!(2026 - 03 - 31),
            u32::try_from(index).unwrap() + 10,
            EventKind::ControlAssertion { period, claim },
            vec![],
            parser,
            document,
        )
    })
    .collect()
}

#[test]
fn separate_sections_that_all_agree_raise_the_period_to_internal() {
    // Основание 5: независимые уравнения, но один документ и один
    // парсер. Выше internal подняться не может по устройству.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut events = vec![event_from(
        owner,
        account,
        date!(2026 - 03 - 10),
        1,
        EventKind::CashIn { amount: rub(100_000) },
        vec![Leg::cash(account, rub(100_000))],
        "tinkoff-xlsx/1",
        "march",
    )];
    events.extend(full_sections(
        owner, account, 0, 100_000, 100_000, 0, "tinkoff-xlsx/1", "march",
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::TaxBasis),
        DimensionStatus::Provisional,
        "налоговая стоимость денежным остатком не подтверждается"
    );
}

#[test]
fn one_agreeing_section_is_not_enough_for_ground_five() {
    // Один сошедшийся остаток не является совпадением независимых
    // уравнений: он подтверждает сам себя. Основание 5 требует, чтобы
    // сошлись и остаток, и оборот — величины, считающиеся по-разному.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let period = march();
    let events = vec![
        event_from(
            owner,
            account,
            date!(2026 - 03 - 10),
            1,
            EventKind::CashIn { amount: rub(100_000) },
            vec![Leg::cash(account, rub(100_000))],
            "tinkoff-xlsx/1",
            "march",
        ),
        event_from(
            owner,
            account,
            date!(2026 - 03 - 31),
            10,
            EventKind::ControlAssertion {
                period,
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(100_000),
                    at: BalancePoint::Closing,
                },
            },
            vec![],
            "tinkoff-xlsx/1",
            "march",
        ),
    ];

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Provisional
    );
}

#[test]
fn a_discrepancy_wins_over_any_amount_of_confirmation() {
    // Подтверждение не затирает несошедшуюся цифру. Иначе достаточно
    // было бы приложить второй документ, чтобы расхождение исчезло
    // с экрана, оставшись в данных.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut events = vec![event_from(
        owner,
        account,
        date!(2026 - 03 - 10),
        1,
        EventKind::CashIn { amount: rub(100_000) },
        vec![Leg::cash(account, rub(100_000))],
        "tinkoff-xlsx/1",
        "march",
    )];
    // Обороты сойдутся, а конечный остаток — нет.
    events.extend(full_sections(
        owner, account, 0, 999_999, 100_000, 0, "tinkoff-xlsx/1", "march",
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Discrepant
    );
}

#[test]
fn two_independent_channels_over_the_same_period_reach_independent() {
    // Основание 3. Тот же период, те же цифры, другой парсер и другой
    // документ — условие независимости §10.3 выполнено.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut events = vec![event_from(
        owner,
        account,
        date!(2026 - 03 - 10),
        1,
        EventKind::CashIn { amount: rub(100_000) },
        vec![Leg::cash(account, rub(100_000))],
        "tinkoff-xlsx/1",
        "march",
    )];
    events.extend(full_sections(
        owner, account, 0, 100_000, 100_000, 0, "tinkoff-xlsx/1", "march",
    ));
    events.extend(full_sections(
        owner, account, 0, 100_000, 100_000, 0, "tinkoff-api/1", "api-march",
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );
}

#[test]
fn two_statements_of_the_same_parser_never_reach_independent() {
    // Прямая формулировка §10.3. Два разных документа одного брокера,
    // разобранные одним парсером, — это непрерывность.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut events = vec![event_from(
        owner,
        account,
        date!(2026 - 03 - 10),
        1,
        EventKind::CashIn { amount: rub(100_000) },
        vec![Leg::cash(account, rub(100_000))],
        "tinkoff-xlsx/1",
        "march",
    )];
    events.extend(full_sections(
        owner, account, 0, 100_000, 100_000, 0, "tinkoff-xlsx/1", "march-first-copy",
    ));
    events.extend(full_sections(
        owner, account, 0, 100_000, 100_000, 0, "tinkoff-xlsx/1", "march-second-copy",
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );
}

#[test]
fn a_period_without_assertions_stays_provisional() {
    // Отсутствие утверждений — это отсутствие подтверждения, а не
    // подтверждение отсутствия проблем.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let events = vec![event_from(
        owner,
        account,
        date!(2026 - 03 - 10),
        1,
        EventKind::CashIn { amount: rub(100_000) },
        vec![Leg::cash(account, rub(100_000))],
        "manual/1",
        "none",
    )];
    let ledger = ReconciliationLedger::build(&events).unwrap();
    for dimension in Dimension::all() {
        assert_eq!(
            ledger.status_for(account, date!(2026 - 03 - 15), dimension),
            DimensionStatus::Provisional
        );
    }
}

#[test]
fn the_ledger_is_a_pure_function_of_the_journal() {
    // Тот же журнал — тот же статус. Иначе воспроизвести показанную
    // владельцу цифру невозможно, а §3.1 требует именно этого.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut events = vec![event_from(
        owner,
        account,
        date!(2026 - 03 - 10),
        1,
        EventKind::CashIn { amount: rub(100_000) },
        vec![Leg::cash(account, rub(100_000))],
        "tinkoff-xlsx/1",
        "march",
    )];
    events.extend(full_sections(
        owner, account, 0, 100_000, 100_000, 0, "tinkoff-xlsx/1", "march",
    ));

    let first = ReconciliationLedger::build(&events).unwrap();
    let second = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        first.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        second.status_for(account, date!(2026 - 03 - 15), Dimension::Cash)
    );
}
```

- [ ] **Шаг 2: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-core --test reconciliation_ledger 2>&1 | head -20`
Expected: FAIL — `no ReconciliationLedger in iaam_core::reconciliation`.

- [ ] **Шаг 3: Написать статус измерения и статус интервала**

Добавьте в `crates/iaam-core/src/reconciliation/mod.rs` (после
`ConfidenceLevel`):

```rust
use std::collections::{BTreeMap, BTreeSet};

use time::Date;

use crate::event::{Event, kind::EventKind};
use crate::ids::AccountId;
use check::{ClaimOutcome, check_claim};
use claim::{AssertionPeriod, BalancePoint, ControlClaim};
use evidence::{Evidence, Ground, SourceChannel};
use observed::{ObserveError, observe};

/// Статус измерения на интервале (§10.3).
///
/// Четыре значения спеки. `Discrepant` — не уровень, а поглощающее
/// состояние: несошедшаяся цифра не перестаёт быть несошедшейся оттого,
/// что рядом сошлась другая.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DimensionStatus {
    Provisional,
    AcceptedInternal,
    AcceptedIndependent,
    Discrepant,
}

impl DimensionStatus {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::AcceptedInternal => "accepted_internal",
            Self::AcceptedIndependent => "accepted_independent",
            Self::Discrepant => "discrepant",
        }
    }

    const fn from_level(level: ConfidenceLevel) -> Self {
        match level {
            ConfidenceLevel::Provisional => Self::Provisional,
            ConfidenceLevel::AcceptedInternal => Self::AcceptedInternal,
            ConfidenceLevel::AcceptedIndependent => Self::AcceptedIndependent,
        }
    }
}

/// Одно проверенное утверждение вместе с исходом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimCheck {
    pub claim: ControlClaim,
    pub outcome: ClaimOutcome,
}

/// Утверждение о полноте счёта на интервале.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationStatus {
    account: AccountId,
    period: AssertionPeriod,
    dimensions: BTreeMap<Dimension, DimensionStatus>,
    evidence: Vec<Evidence>,
    outcomes: Vec<ClaimCheck>,
}

impl ReconciliationStatus {
    #[must_use]
    pub const fn account(&self) -> AccountId {
        self.account
    }

    #[must_use]
    pub const fn period(&self) -> AssertionPeriod {
        self.period
    }

    /// Статус измерения. Отсутствие записи означает `Provisional`:
    /// об измерении, о котором ничего не утверждали, ничего и не
    /// известно.
    #[must_use]
    pub fn dimension(&self, dimension: Dimension) -> DimensionStatus {
        self.dimensions
            .get(&dimension)
            .copied()
            .unwrap_or(DimensionStatus::Provisional)
    }

    #[must_use]
    pub fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }

    #[must_use]
    pub fn outcomes(&self) -> &[ClaimCheck] {
        &self.outcomes
    }
}
```

- [ ] **Шаг 4: Написать сборку реестра**

Дальше в том же файле:

```rust
/// Группа утверждений одного документа об одном счёте за один интервал.
///
/// Группируется линейным поиском, а не картой: канал не упорядочен
/// (хеш документа сравним, но не сортируем осмысленно), а документов
/// у владельца единицы. Карта потребовала бы порядка ради порядка.
#[derive(Debug, Clone)]
struct StatementGroup {
    account: AccountId,
    period: AssertionPeriod,
    channel: SourceChannel,
    claims: Vec<ControlClaim>,
}

impl StatementGroup {
    fn matches(&self, account: AccountId, period: AssertionPeriod, channel: &SourceChannel) -> bool {
        self.account == account && self.period == period && &self.channel == channel
    }
}

/// Реестр статусов: чистая функция от журнала (§3.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconciliationLedger {
    statuses: Vec<ReconciliationStatus>,
}

impl ReconciliationLedger {
    /// Сборка реестра из журнала.
    ///
    /// Логика вынесена из `new` намеренно (§15.7).
    pub fn build(events: &[Event]) -> Result<Self, ObserveError> {
        let groups = collect_groups(events);
        let mut statuses: Vec<ReconciliationStatus> = Vec::new();

        // Шаг 1: сверка каждой группы со своей проекцией.
        let mut checked: Vec<(usize, Vec<ClaimCheck>)> = Vec::new();
        for (index, group) in groups.iter().enumerate() {
            let observed = observe(events, group.account, group.period)?;
            let outcomes = group
                .claims
                .iter()
                .map(|claim| ClaimCheck {
                    claim: *claim,
                    outcome: check_claim(claim, &observed),
                })
                .collect();
            checked.push((index, outcomes));
        }

        // Шаг 2: основания.
        let mut evidence: Vec<(AccountId, AssertionPeriod, Evidence)> = Vec::new();
        for (index, outcomes) in &checked {
            let group = &groups[*index];
            if let Some(item) = ground_five(group, outcomes) {
                evidence.push((group.account, group.period, item));
            }
            if let Some((period, item)) = ground_one(group, outcomes, &groups) {
                evidence.push((group.account, period, item));
            }
        }
        evidence.extend(ground_three(&groups, &checked));
        evidence.extend(ground_two(&groups));

        // Шаг 3: статусы.
        for (index, outcomes) in checked {
            let group = &groups[index];
            let status = build_status(group, outcomes, &evidence);
            merge_status(&mut statuses, status);
        }
        Ok(Self { statuses })
    }

    /// Добавление оснований, которые журнал породить пока не может:
    /// депозитарный отчёт, параметры выпуска, справка налогового агента,
    /// подтверждение графика выплат (задачи E3, E5, E7).
    #[must_use]
    pub fn with_external_evidence(
        mut self,
        items: Vec<(AccountId, AssertionPeriod, Evidence)>,
    ) -> Self {
        for (account, period, item) in items {
            let level = DimensionStatus::from_level(item.level());
            let dimensions = item.dimensions();
            if let Some(status) = self
                .statuses
                .iter_mut()
                .find(|status| status.account == account && status.period == period)
            {
                raise(&mut status.dimensions, &dimensions, level);
                status.evidence.push(item);
            } else {
                let mut map = BTreeMap::new();
                raise(&mut map, &dimensions, level);
                self.statuses.push(ReconciliationStatus {
                    account,
                    period,
                    dimensions: map,
                    evidence: vec![item],
                    outcomes: Vec::new(),
                });
            }
        }
        self
    }

    pub fn statuses(&self) -> impl Iterator<Item = &ReconciliationStatus> {
        self.statuses.iter()
    }

    /// Статус измерения на дату.
    ///
    /// Берётся **худший** статус среди интервалов, накрывающих дату:
    /// два утверждения об одном дне, одно из которых не сошлось, дают
    /// расхождение. Взять лучший значило бы позволить лишнему документу
    /// закрыть собой проблему.
    #[must_use]
    pub fn status_for(
        &self,
        account: AccountId,
        date: Date,
        dimension: Dimension,
    ) -> DimensionStatus {
        let mut result: Option<DimensionStatus> = None;
        for status in &self.statuses {
            if status.account != account || !status.period.contains(date) {
                continue;
            }
            let candidate = status.dimension(dimension);
            result = Some(match result {
                None => candidate,
                Some(current) => worst(current, candidate),
            });
        }
        result.unwrap_or(DimensionStatus::Provisional)
    }
}

/// Худший из двух статусов. `Discrepant` поглощает всё.
const fn worst(left: DimensionStatus, right: DimensionStatus) -> DimensionStatus {
    match (left, right) {
        (DimensionStatus::Discrepant, _) | (_, DimensionStatus::Discrepant) => {
            DimensionStatus::Discrepant
        }
        (DimensionStatus::Provisional, _) | (_, DimensionStatus::Provisional) => {
            DimensionStatus::Provisional
        }
        (DimensionStatus::AcceptedInternal, _) | (_, DimensionStatus::AcceptedInternal) => {
            DimensionStatus::AcceptedInternal
        }
        (DimensionStatus::AcceptedIndependent, DimensionStatus::AcceptedIndependent) => {
            DimensionStatus::AcceptedIndependent
        }
    }
}

fn collect_groups(events: &[Event]) -> Vec<StatementGroup> {
    let mut groups: Vec<StatementGroup> = Vec::new();
    for event in events {
        let EventKind::ControlAssertion { period, claim } = event.kind else {
            continue;
        };
        let channel = SourceChannel {
            source: event.provenance.source(),
            parser_version: event.provenance.parser_version().clone(),
            document: Some(event.provenance.raw_hash().clone()),
        };
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.matches(event.account, period, &channel))
        {
            group.claims.push(claim);
        } else {
            groups.push(StatementGroup {
                account: event.account,
                period,
                channel,
                claims: vec![claim],
            });
        }
    }
    groups
}

/// Основание 5: раздельные контрольные секции одного документа сошлись
/// одновременно.
///
/// Требуется и остаток, и оборот: это величины, считающиеся по-разному,
/// и совпадение обеих является независимым уравнением. Один сошедшийся
/// остаток подтверждает сам себя и основанием не является.
fn ground_five(group: &StatementGroup, outcomes: &[ClaimCheck]) -> Option<Evidence> {
    if outcomes.is_empty() || !outcomes.iter().all(|check| check.outcome.confirms()) {
        return None;
    }
    let has_balance = group
        .claims
        .iter()
        .any(|claim| matches!(claim, ControlClaim::CashBalance { .. } | ControlClaim::PositionQuantity { .. }));
    let has_turnover = group.claims.iter().any(|claim| {
        matches!(
            claim,
            ControlClaim::CashTurnover { .. }
                | ControlClaim::FeesTotal { .. }
                | ControlClaim::IncomeTotal { .. }
        )
    });
    if !has_balance || !has_turnover {
        return None;
    }
    let dimensions: BTreeSet<Dimension> = group.claims.iter().map(ControlClaim::dimension).collect();
    Evidence::from_match(
        Ground::SeparateSectionsAgree,
        group.channel.clone(),
        group.channel.clone(),
        dimensions,
    )
}

/// Основание 1: начальный остаток следующего отчёта совпал с
/// вычисленным остатком предыдущего периода.
///
/// Повышается **предыдущий** период: подтверждается именно он.
/// Повысить текущий значило бы засчитать подтверждение данных,
/// которых в нём ещё нет.
fn ground_one(
    group: &StatementGroup,
    outcomes: &[ClaimCheck],
    groups: &[StatementGroup],
) -> Option<(AssertionPeriod, Evidence)> {
    let opening_matched: BTreeSet<Dimension> = outcomes
        .iter()
        .filter(|check| {
            check.outcome.confirms()
                && matches!(
                    check.claim,
                    ControlClaim::CashBalance { at: BalancePoint::Opening, .. }
                        | ControlClaim::PositionQuantity { at: BalancePoint::Opening, .. }
                )
        })
        .map(|check| check.claim.dimension())
        .collect();
    if opening_matched.is_empty() {
        return None;
    }
    let prior = groups
        .iter()
        .filter(|other| other.account == group.account && other.period.to < group.period.from)
        .max_by_key(|other| other.period.to)?;
    let evidence = Evidence::from_match(
        Ground::OpeningMatchesPriorClosing,
        group.channel.clone(),
        prior.channel.clone(),
        opening_matched,
    )?;
    Some((prior.period, evidence))
}

/// Основание 2: конечный остаток одного отчёта совпал с начальным
/// следующего. Сравниваются два **утверждения источника**, а не
/// утверждение с проекцией: это проверка непрерывности документов.
fn ground_two(groups: &[StatementGroup]) -> Vec<(AccountId, AssertionPeriod, Evidence)> {
    let mut found = Vec::new();
    for earlier in groups {
        for later in groups {
            if earlier.account != later.account || later.period.from <= earlier.period.to {
                continue;
            }
            let mut dimensions = BTreeSet::new();
            for closing in &earlier.claims {
                for opening in &later.claims {
                    if continuous(*closing, *opening) {
                        dimensions.insert(closing.dimension());
                    }
                }
            }
            if let Some(evidence) = Evidence::from_match(
                Ground::ContinuityBetweenStatements,
                later.channel.clone(),
                earlier.channel.clone(),
                dimensions,
            ) {
                found.push((earlier.account, earlier.period, evidence));
            }
        }
    }
    found
}

/// Совпадают ли конечное утверждение одного отчёта и начальное другого.
fn continuous(closing: ControlClaim, opening: ControlClaim) -> bool {
    match (closing, opening) {
        (
            ControlClaim::CashBalance {
                currency: left_currency,
                amount: left,
                at: BalancePoint::Closing,
            },
            ControlClaim::CashBalance {
                currency: right_currency,
                amount: right,
                at: BalancePoint::Opening,
            },
        ) => left_currency == right_currency && left == right,
        (
            ControlClaim::PositionQuantity {
                instrument: left_instrument,
                custody: left_custody,
                quantity: left,
                at: BalancePoint::Closing,
            },
            ControlClaim::PositionQuantity {
                instrument: right_instrument,
                custody: right_custody,
                quantity: right,
                at: BalancePoint::Opening,
            },
        ) => {
            left_instrument == right_instrument && left_custody == right_custody && left == right
        }
        _ => false,
    }
}

/// Основание 3: два независимых канала за один интервал.
///
/// Пара берётся один раз (`i < j`): отношение независимости
/// симметрично, и вторая копия того же основания удвоила бы список
/// доказательств, ничего не добавив.
fn ground_three(
    groups: &[StatementGroup],
    checked: &[(usize, Vec<ClaimCheck>)],
) -> Vec<(AccountId, AssertionPeriod, Evidence)> {
    let mut found = Vec::new();
    for (position, (left_index, left_outcomes)) in checked.iter().enumerate() {
        for (right_index, right_outcomes) in checked.iter().skip(position + 1) {
            let left = &groups[*left_index];
            let right = &groups[*right_index];
            if left.account != right.account
                || left.period != right.period
                || !left.channel.is_independent_of(&right.channel)
            {
                continue;
            }
            let confirmed: BTreeSet<Dimension> = confirmed_dimensions(left_outcomes)
                .intersection(&confirmed_dimensions(right_outcomes))
                .copied()
                .collect();
            if let Some(evidence) = Evidence::from_match(
                Ground::BrokerApiAgreesWithStatement,
                right.channel.clone(),
                left.channel.clone(),
                confirmed,
            ) {
                found.push((left.account, left.period, evidence));
            }
        }
    }
    found
}

/// Измерения, по которым в группе сошлось всё и не разошлось ничего.
fn confirmed_dimensions(outcomes: &[ClaimCheck]) -> BTreeSet<Dimension> {
    let mut confirmed = BTreeSet::new();
    let mut broken = BTreeSet::new();
    for check in outcomes {
        let dimension = check.claim.dimension();
        match check.outcome {
            ClaimOutcome::Matched => {
                confirmed.insert(dimension);
            }
            ClaimOutcome::Discrepant(_) => {
                broken.insert(dimension);
            }
            // Несравнимое и исключённое периметром не подтверждают
            // и не ломают: они молчат.
            ClaimOutcome::NotComparable { .. } | ClaimOutcome::Excepted { .. } => {}
        }
    }
    confirmed.retain(|dimension| !broken.contains(dimension));
    confirmed
}

fn build_status(
    group: &StatementGroup,
    outcomes: Vec<ClaimCheck>,
    evidence: &[(AccountId, AssertionPeriod, Evidence)],
) -> ReconciliationStatus {
    let mut dimensions: BTreeMap<Dimension, DimensionStatus> = BTreeMap::new();
    let mut own_evidence = Vec::new();
    for (account, period, item) in evidence {
        if *account == group.account && *period == group.period {
            raise(
                &mut dimensions,
                &item.dimensions(),
                DimensionStatus::from_level(item.level()),
            );
            own_evidence.push(item.clone());
        }
    }
    // Расхождение поглощает: ставится после повышений и не снимается.
    for check in &outcomes {
        if matches!(check.outcome, ClaimOutcome::Discrepant(_)) {
            dimensions.insert(check.claim.dimension(), DimensionStatus::Discrepant);
        }
    }
    ReconciliationStatus {
        account: group.account,
        period: group.period,
        dimensions,
        evidence: own_evidence,
        outcomes,
    }
}

/// Повышение статуса измерений до уровня основания. Понижения нет:
/// основание слабее уже достигнутого ничего не меняет.
fn raise(
    dimensions: &mut BTreeMap<Dimension, DimensionStatus>,
    of: &BTreeSet<Dimension>,
    level: DimensionStatus,
) {
    for dimension in of {
        let slot = dimensions
            .entry(*dimension)
            .or_insert(DimensionStatus::Provisional);
        if rank(level) > rank(*slot) {
            *slot = level;
        }
    }
}

const fn rank(status: DimensionStatus) -> u8 {
    match status {
        DimensionStatus::Discrepant => 0,
        DimensionStatus::Provisional => 1,
        DimensionStatus::AcceptedInternal => 2,
        DimensionStatus::AcceptedIndependent => 3,
    }
}

/// Слияние статусов одного счёта и интервала, пришедших из разных
/// документов: берётся лучшее подтверждение и все расхождения.
fn merge_status(into: &mut Vec<ReconciliationStatus>, status: ReconciliationStatus) {
    if let Some(existing) = into
        .iter_mut()
        .find(|item| item.account == status.account && item.period == status.period)
    {
        for (dimension, value) in &status.dimensions {
            let slot = existing
                .dimensions
                .entry(*dimension)
                .or_insert(DimensionStatus::Provisional);
            *slot = if *value == DimensionStatus::Discrepant || *slot == DimensionStatus::Discrepant
            {
                DimensionStatus::Discrepant
            } else if rank(*value) > rank(*slot) {
                *value
            } else {
                *slot
            };
        }
        existing.evidence.extend(status.evidence);
        existing.outcomes.extend(status.outcomes);
    } else {
        into.push(status);
    }
}
```

- [ ] **Шаг 5: Прогнать тесты**

Run: `nix develop -c cargo test -p iaam-core 2>&1 | tail -20`
Expected: PASS — семь тестов реестра плюс все прежние.

- [ ] **Шаг 6: Заслоны и коммит**

```bash
nix develop -c cargo fmt --all -- --check
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c ./scripts/check-architecture.sh
git add crates/iaam-core
git commit -m "feat(core): статус полноты по паре интервал×измерение (iaam-023)"
```

---

## Задача 6: Периметр §11 — маржа, РЕПО, отрицательный денежный остаток

**Files:**
- Create: `crates/iaam-core/src/perimeter.rs`
- Modify: `crates/iaam-core/src/lib.rs` — `pub mod perimeter;`
- Modify: `crates/iaam-core/src/reconciliation/mod.rs` — `build_with(events, &PerimeterExceptions)`
- Test: `crates/iaam-core/tests/perimeter.rs`

**Interfaces:**
- Consumes: `Event`, `Balances`, `FeeOrigin::MarginInterest`, `ReconciliationException` (задача 3), `Dimension`.
- Produces: `PerimeterPolicy { settlement_window_days: u16 }` с `PerimeterPolicy::default()` = 5; `NegativeCashClassification` (три варианта §11); `NegativeCashSpan { account, currency, from, resolved, classification }`; `PerimeterAssessment` с `spans()`, `financing_present(AccountId) -> bool`, `blocks_period_reports(AccountId) -> bool`, `exceptions() -> PerimeterExceptions`; `assess(events, policy) -> Result<PerimeterAssessment, PerimeterError>`; `PerimeterExceptions` с `none()`, `add(AccountId, Dimension, ReconciliationException)`, `covers(AccountId, Dimension) -> Option<ReconciliationException>`; `ReconciliationLedger::build_with`.

**Acceptance Criteria:**
- Минус, закрывшийся расчётом в пределах окна политики и без процентов по марже, классифицируется `temporary_settlement_deficit`, и расчёты по счёту разрешены
- Минус при наличии `FeeOrigin::MarginInterest` в том же промежутке — `unsupported_margin_liability`; незакрывшийся минус без признаков кредита — `unclassified_negative_cash`
- В двух последних случаях отчёты за период по счёту не считаются, а **остальные счета и периоды считаются** — отдельный тест
- Окно расчётов берётся из политики и попадает в результат: цифра, зависящая от порога, несёт порог рядом
- Расхождение, накрытое исключением периметра, даёт `Excepted`, а не `Discrepant`, и не требует действий владельца
- Отрицательный остаток остаётся в остатках и NAV: он обязательство, а не ошибка

- [ ] **Шаг 1: Написать падающий тест**

Создайте `crates/iaam-core/tests/perimeter.rs`:

```rust
//! Периметр: шорты, маржа, РЕПО вне периметра (§11).

use iaam_core::event::kind::{EventKind, FeeOrigin};
use iaam_core::event::leg::Leg;
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::perimeter::{
    NegativeCashClassification, PerimeterPolicy, assess,
};
use iaam_core::ids::AccountId;
use time::macros::date;

mod support;
use support::event_at;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

#[test]
fn a_deficit_closed_within_the_window_is_temporary() {
    // Минус из-за тайминга расчётов — нормальная работа, а не событие
    // вне периметра. Расчёты по счёту продолжаются.
    let account = AccountId::new_random();
    let events = vec![
        event_at(account, date!(2026 - 03 - 10), 1, EventKind::CashOut { amount: rub(-50_000) },
                 vec![Leg::cash(account, rub(-50_000))]),
        event_at(account, date!(2026 - 03 - 12), 1, EventKind::CashIn { amount: rub(50_000) },
                 vec![Leg::cash(account, rub(50_000))]),
    ];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    let span = assessment.spans().first().expect("минус обязан быть замечен");
    assert_eq!(span.classification, NegativeCashClassification::TemporarySettlementDeficit);
    assert!(!assessment.blocks_period_reports(account));
}

#[test]
fn margin_interest_in_the_span_makes_it_an_unsupported_liability() {
    // Признак кредита есть — экономику финансирования система не
    // достраивает и отчёты за период не выдаёт (§11).
    let account = AccountId::new_random();
    let events = vec![
        event_at(account, date!(2026 - 03 - 10), 1, EventKind::CashOut { amount: rub(-50_000) },
                 vec![Leg::cash(account, rub(-50_000))]),
        event_at(account, date!(2026 - 03 - 11), 1,
                 EventKind::Fee { amount: rub(-120), origin: FeeOrigin::MarginInterest },
                 vec![Leg::fee(account, rub(-120))]),
        event_at(account, date!(2026 - 03 - 12), 1, EventKind::CashIn { amount: rub(50_120) },
                 vec![Leg::cash(account, rub(50_120))]),
    ];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    let span = assessment.spans().first().unwrap();
    assert_eq!(span.classification, NegativeCashClassification::UnsupportedMarginLiability);
    assert!(assessment.financing_present(account));
    assert!(assessment.blocks_period_reports(account));
}

#[test]
fn an_unexplained_deficit_outside_the_window_is_unclassified() {
    let account = AccountId::new_random();
    let events = vec![event_at(
        account, date!(2026 - 03 - 10), 1, EventKind::CashOut { amount: rub(-50_000) },
        vec![Leg::cash(account, rub(-50_000))],
    )];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    let span = assessment.spans().first().unwrap();
    assert_eq!(span.classification, NegativeCashClassification::UnclassifiedNegativeCash);
    assert!(span.resolved.is_none());
}

#[test]
fn other_accounts_keep_computing() {
    // Ключевое требование §11: отказ считать один счёт не отменяет
    // остальные. Иначе одна непонятая строка гасит весь портфель.
    let broken = AccountId::new_random();
    let healthy = AccountId::new_random();
    let events = vec![
        event_at(broken, date!(2026 - 03 - 10), 1, EventKind::CashOut { amount: rub(-50_000) },
                 vec![Leg::cash(broken, rub(-50_000))]),
        event_at(broken, date!(2026 - 03 - 11), 1,
                 EventKind::Fee { amount: rub(-120), origin: FeeOrigin::MarginInterest },
                 vec![Leg::fee(broken, rub(-120))]),
        event_at(healthy, date!(2026 - 03 - 10), 1, EventKind::CashIn { amount: rub(70_000) },
                 vec![Leg::cash(healthy, rub(70_000))]),
    ];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    assert!(assessment.blocks_period_reports(broken));
    assert!(!assessment.blocks_period_reports(healthy));
}

#[test]
fn the_settlement_window_comes_from_the_policy() {
    // Порог обязан быть параметром: «допустимый срок» без торгового
    // календаря не вычисляется, и цифра, зависящая от порога, обязана
    // нести порог рядом с собой.
    let account = AccountId::new_random();
    let events = vec![
        event_at(account, date!(2026 - 03 - 10), 1, EventKind::CashOut { amount: rub(-50_000) },
                 vec![Leg::cash(account, rub(-50_000))]),
        event_at(account, date!(2026 - 03 - 20), 1, EventKind::CashIn { amount: rub(50_000) },
                 vec![Leg::cash(account, rub(50_000))]),
    ];
    let narrow = assess(&events, PerimeterPolicy { settlement_window_days: 5 }).unwrap();
    assert_eq!(
        narrow.spans().first().unwrap().classification,
        NegativeCashClassification::UnclassifiedNegativeCash
    );
    let wide = assess(&events, PerimeterPolicy { settlement_window_days: 30 }).unwrap();
    assert_eq!(
        wide.spans().first().unwrap().classification,
        NegativeCashClassification::TemporarySettlementDeficit
    );
    assert_eq!(wide.policy().settlement_window_days, 30);
}
```

Создайте общий помощник `crates/iaam-core/tests/support/mod.rs` (он же
понадобится задачам 7 и 9):

```rust
//! Сборка событий для интеграционных тестов ядра.
//!
//! Живёт отдельным модулем, потому что `test_support` внутри крейта
//! доступен только модульным тестам: интеграционные тесты — внешний
//! потребитель и обязаны собирать событие через публичный интерфейс.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use time::Date;

#[must_use]
pub fn event_at(
    account: AccountId,
    day: Date,
    sequence: u32,
    kind: EventKind,
    legs: Vec<Leg>,
) -> Event {
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner: OwnerId::new_random(),
        account,
        kind,
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs,
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"a".repeat(64)).unwrap(),
            ParserVersion("test/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}
```

- [ ] **Шаг 2: Убедиться, что тест не собирается**

Run: `nix develop -c cargo test -p iaam-core --test perimeter 2>&1 | head -20`
Expected: FAIL — `unresolved import iaam_core::perimeter`.

- [ ] **Шаг 3: Написать модуль периметра**

Создайте `crates/iaam-core/src/perimeter.rs`:

```rust
//! Периметр: шорты, маржа, РЕПО и ПФИ вне периметра (§11).
//!
//! Граница **возможностная, а не документная**: встретив
//! неподдерживаемую операцию, система не отклоняет отчёт. Наблюдаемый
//! денежный эффект сохраняется всегда; выдумывать экономику
//! неподдерживаемого финансирования система отказывается.
//!
//! Отрицательный денежный остаток поддерживается и в long-only
//! системе: он возникает из-за таймингов расчётов, комиссий и
//! технического овердрафта. В NAV он входит обязательством.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use time::Date;

use crate::event::kind::{EventKind, FeeOrigin};
use crate::event::{Event, leg::LegKind};
use crate::ids::{AccountId, EventId};
use crate::money::{CurrencyCode, PostedMinor};
use crate::reconciliation::Dimension;
use crate::reconciliation::check::ReconciliationException;

/// Политика периметра.
///
/// Окно расчётов задаётся параметром, а не константой: «допустимый
/// срок» (§11) без торгового календаря не вычисляется, а календарь —
/// это E3. Значение по умолчанию покрывает T+2 с выходными.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerimeterPolicy {
    pub settlement_window_days: u16,
}

impl Default for PerimeterPolicy {
    fn default() -> Self {
        Self {
            settlement_window_days: 5,
        }
    }
}

/// Классификация отрицательного остатка (§11, таблица).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NegativeCashClassification {
    /// Закрывается известным расчётом в допустимый срок: расчёты разрешены.
    TemporarySettlementDeficit,
    /// Присутствуют проценты по марже или признак кредита.
    UnsupportedMarginLiability,
    /// Причина неизвестна.
    UnclassifiedNegativeCash,
}

impl NegativeCashClassification {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TemporarySettlementDeficit => "temporary_settlement_deficit",
            Self::UnsupportedMarginLiability => "unsupported_margin_liability",
            Self::UnclassifiedNegativeCash => "unclassified_negative_cash",
        }
    }

    /// Блокирует ли классификация налоговые и финансовые отчёты за
    /// период (§11). Временный дефицит — нет.
    #[must_use]
    pub const fn blocks_reports(self) -> bool {
        match self {
            Self::TemporarySettlementDeficit => false,
            Self::UnsupportedMarginLiability | Self::UnclassifiedNegativeCash => true,
        }
    }
}

/// Промежуток отрицательного остатка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegativeCashSpan {
    pub account: AccountId,
    pub currency: CurrencyCode,
    pub from: Date,
    /// Дата возврата в неотрицательный остаток. `None` — не закрылся.
    pub resolved: Option<Date>,
    pub classification: NegativeCashClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PerimeterError {
    #[error("событие {event:?} не имеет даты и не может быть отнесено к промежутку")]
    EventWithoutDate { event: EventId },
    #[error("переполнение остатка счёта {account:?} в {currency:?}")]
    Overflow {
        account: AccountId,
        currency: CurrencyCode,
    },
}

/// Исключения сверки, объяснённые границей периметра.
///
/// Существуют, чтобы владелец не получал задание «починить» то, что
/// система намеренно не поддерживает (§11).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerimeterExceptions {
    entries: Vec<(AccountId, Dimension, ReconciliationException)>,
}

impl PerimeterExceptions {
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    pub fn add(
        &mut self,
        account: AccountId,
        dimension: Dimension,
        exception: ReconciliationException,
    ) {
        if !self
            .entries
            .iter()
            .any(|(a, d, e)| *a == account && *d == dimension && *e == exception)
        {
            self.entries.push((account, dimension, exception));
        }
    }

    #[must_use]
    pub fn covers(
        &self,
        account: AccountId,
        dimension: Dimension,
    ) -> Option<ReconciliationException> {
        self.entries
            .iter()
            .find(|(a, d, _)| *a == account && *d == dimension)
            .map(|(_, _, exception)| *exception)
    }
}

/// Оценка периметра по журналу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerimeterAssessment {
    policy: PerimeterPolicy,
    spans: Vec<NegativeCashSpan>,
    financing: BTreeSet<AccountId>,
}

impl PerimeterAssessment {
    #[must_use]
    pub fn spans(&self) -> &[NegativeCashSpan] {
        &self.spans
    }

    #[must_use]
    pub const fn policy(&self) -> PerimeterPolicy {
        self.policy
    }

    /// Есть ли на счёте финансирование вне периметра.
    #[must_use]
    pub fn financing_present(&self, account: AccountId) -> bool {
        self.financing.contains(&account)
    }

    /// Отказываются ли налоговые и финансовые отчёты считаться по этому
    /// счёту. Про **другие** счета не говорит ничего: §11 требует,
    /// чтобы остальные продолжали считаться.
    #[must_use]
    pub fn blocks_period_reports(&self, account: AccountId) -> bool {
        self.spans
            .iter()
            .any(|span| span.account == account && span.classification.blocks_reports())
    }

    /// Исключения сверки, следующие из оценки.
    #[must_use]
    pub fn exceptions(&self) -> PerimeterExceptions {
        let mut exceptions = PerimeterExceptions::none();
        for account in &self.financing {
            exceptions.add(
                *account,
                Dimension::Cash,
                ReconciliationException::UnsupportedFinancingPresent,
            );
        }
        exceptions
    }
}

/// Оценка периметра.
///
/// Логика вынесена из конструктора с именем `new` намеренно (§15.7).
pub fn assess(
    events: &[Event],
    policy: PerimeterPolicy,
) -> Result<PerimeterAssessment, PerimeterError> {
    let mut ordered: Vec<(Date, &Event)> = Vec::with_capacity(events.len());
    for event in events {
        let date = event
            .dates
            .effective_date()
            .ok_or(PerimeterError::EventWithoutDate { event: event.id })?;
        ordered.push((date, event));
    }
    ordered.sort_by_key(|(date, event)| (*date, event.order));

    let mut financing: BTreeSet<AccountId> = BTreeSet::new();
    for (_, event) in &ordered {
        if matches!(
            event.kind,
            EventKind::Fee {
                origin: FeeOrigin::MarginInterest,
                ..
            }
        ) {
            financing.insert(event.account);
        }
    }

    let mut balances: BTreeMap<(AccountId, CurrencyCode), PostedMinor> = BTreeMap::new();
    let mut open: BTreeMap<(AccountId, CurrencyCode), Date> = BTreeMap::new();
    let mut spans: Vec<NegativeCashSpan> = Vec::new();

    for (date, event) in &ordered {
        for leg in &event.legs {
            let Some(money) = leg.cash_effect() else {
                continue;
            };
            // Нога количества денег не двигает и в остаток не входит.
            if leg.kind == LegKind::SecurityQuantity {
                continue;
            }
            let key = (leg.account, money.currency());
            let slot = balances.entry(key).or_insert_with(|| PostedMinor::new(0));
            *slot = slot
                .checked_add(money.amount())
                .ok_or(PerimeterError::Overflow {
                    account: leg.account,
                    currency: money.currency(),
                })?;

            let negative = slot.raw() < 0;
            match (negative, open.get(&key).copied()) {
                (true, None) => {
                    open.insert(key, *date);
                }
                (false, Some(start)) => {
                    open.remove(&key);
                    spans.push(classify(key, start, Some(*date), &financing, policy));
                }
                _ => {}
            }
        }
    }
    // Промежутки, не закрывшиеся до конца журнала.
    for (key, start) in open {
        spans.push(classify(key, start, None, &financing, policy));
    }
    spans.sort_by_key(|span| (span.from, span.account, span.currency));
    Ok(PerimeterAssessment {
        policy,
        spans,
        financing,
    })
}

fn classify(
    key: (AccountId, CurrencyCode),
    from: Date,
    resolved: Option<Date>,
    financing: &BTreeSet<AccountId>,
    policy: PerimeterPolicy,
) -> NegativeCashSpan {
    let (account, currency) = key;
    // Порядок ветвей значим: признак кредита сильнее срока. Минус,
    // закрывшийся за день, но сопровождённый процентами по марже,
    // остаётся маржинальным обязательством.
    let classification = if financing.contains(&account) {
        NegativeCashClassification::UnsupportedMarginLiability
    } else if resolved.is_some_and(|end| {
        (end - from).whole_days() <= i64::from(policy.settlement_window_days)
    }) {
        NegativeCashClassification::TemporarySettlementDeficit
    } else {
        NegativeCashClassification::UnclassifiedNegativeCash
    };
    NegativeCashSpan {
        account,
        currency,
        from,
        resolved,
        classification,
    }
}
```

Добавьте в `crates/iaam-core/src/lib.rs`:

```rust
pub mod perimeter;
```

- [ ] **Шаг 4: Подключить исключения к сверке**

В `crates/iaam-core/src/reconciliation/mod.rs` переименуйте `build`
в `build_with` и оставьте `build` как частный случай:

```rust
    /// Сборка реестра из журнала без исключений периметра.
    pub fn build(events: &[Event]) -> Result<Self, ObserveError> {
        Self::build_with(events, &crate::perimeter::PerimeterExceptions::none())
    }

    /// Сборка реестра с исключениями периметра (§11).
    ///
    /// Расхождение, накрытое исключением, становится `Excepted`:
    /// система знает, почему цифры не сходятся, и не отправляет
    /// владельца чинить то, что не поддерживает.
    pub fn build_with(
        events: &[Event],
        exceptions: &crate::perimeter::PerimeterExceptions,
    ) -> Result<Self, ObserveError> {
```

Внутри — там, где формируется `outcomes`, оберните исход:

```rust
            let outcomes = group
                .claims
                .iter()
                .map(|claim| {
                    let outcome = check_claim(claim, &observed);
                    let outcome = match (outcome, exceptions.covers(group.account, claim.dimension()))
                    {
                        (ClaimOutcome::Discrepant(_), Some(exception)) => {
                            ClaimOutcome::Excepted { exception }
                        }
                        (outcome, _) => outcome,
                    };
                    ClaimCheck {
                        claim: *claim,
                        outcome,
                    }
                })
                .collect();
```

- [ ] **Шаг 5: Тесты, заслоны, коммит**

```bash
nix develop -c cargo test -p iaam-core 2>&1 | tail -20
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/iaam-core
git commit -m "feat(core): периметр §11 — классификация минуса, маржа и исключения сверки (iaam-023)"
```

---

## Задача 7: `dataQuality` — покрытие NAV по уровням достоверности

**Files:**
- Modify: `crates/iaam-core/src/returns/xirr.rs` — вынести `account_values`
- Modify: `crates/iaam-core/src/returns/mod.rs` — `NavCoverage`, новые `MaterialIssue`, новая причина отказа
- Test: `crates/iaam-core/tests/data_quality.rs`

**Interfaces:**
- Consumes: `ReconciliationLedger`, `DimensionStatus` (задача 5), `PerimeterAssessment` (задача 6).
- Produces: `account_values(&LedgerState, &ReturnsRequest) -> Result<BTreeMap<AccountId, Dec>, NotComputable>`; `NavCoverage { accepted_independent, accepted_internal, provisional, discrepant }`; `DataQuality { status, nav_coverage, material_issues }`; `MaterialIssue::{NoIndependentSource, Discrepancy, UnsupportedFinancing}`; `NotComputable::UnsupportedFinancing { account }`; `ReturnsRequest` получает поля `ledger: &ReconciliationLedger` и `perimeter: &PerimeterAssessment`.

**Acceptance Criteria:**
- Доли покрытия считаются по **стоимости** счетов, а не по числу записей, и в сумме дают единицу
- Счёт с расхождением попадает в долю `discrepant`, а не растворяется в `provisional`
- Счёт с финансированием вне периметра даёт `materialIssue` и `not_computable` для отчётов за период по этому счёту; остальные счета считаются
- `Clean` достижим и выставляется только при нулевой доле `provisional` и отсутствии материальных проблем
- Начало истории сообщается всегда и **неполнотой не является**

- [ ] **Шаг 1: Вынести стоимость по счетам**

В `crates/iaam-core/src/returns/xirr.rs` замените `terminal_value`:

```rust
/// Стоимость контура **по счетам** на дату отчёта.
///
/// Вынесена из `terminal_value`, потому что покрытие NAV по уровням
/// достоверности (§10.5) взвешивается стоимостью счёта: доля,
/// посчитанная по числу записей, объявила бы счёт с одной сделкой
/// на миллион равным счёту с сотней сделок на тысячу.
pub fn account_values(
    state: &LedgerState,
    request: &ReturnsRequest,
) -> Result<BTreeMap<AccountId, Dec>, NotComputable> {
    guard_state_not_newer(state, request.as_of)?;
    let mut values: BTreeMap<AccountId, Dec> = BTreeMap::new();

    for (account, money) in state.balances().iter_cash() {
        if !request.contour.contains(account) {
            continue;
        }
        let converted = convert(money, request.report_currency, request.as_of, request.fx)?;
        let slot = values.entry(account).or_insert_with(Dec::zero);
        *slot = add(*slot, converted)?;
    }

    for (key, quantity) in state.balances().iter_positions() {
        if !request.contour.contains(key.account) || quantity.0.is_zero() {
            continue;
        }
        let price = state
            .prices()
            .latest(key.instrument)
            .ok_or(NotComputable::MissingPrice {
                instrument: key.instrument,
            })?;
        let local = mul(quantity.0, price.price)?;
        let converted = in_report_currency(local, price.currency, request)?;
        let slot = values.entry(key.account).or_insert_with(Dec::zero);
        *slot = add(*slot, converted)?;
    }
    Ok(values)
}

/// Стоимость контура на дату отчёта — сумма по счетам.
pub fn terminal_value(state: &LedgerState, request: &ReturnsRequest) -> Result<Dec, NotComputable> {
    let values = account_values(state, request)?;
    let mut total = Dec::zero();
    for value in values.values() {
        total = add(total, *value)?;
    }
    Ok(total)
}
```

Добавьте в шапку файла `use std::collections::BTreeMap;` и
`use crate::ids::AccountId;`, если их там нет.

- [ ] **Шаг 2: Переписать блок качества данных**

В `crates/iaam-core/src/returns/mod.rs` замените `DataQuality` и
`data_quality`, добавьте варианты проблем и причину отказа:

```rust
/// Покрытие стоимости портфеля уровнями достоверности (§10.5).
///
/// Доли считаются по **модулю** стоимости счёта: счёт с отрицательным
/// остатком тоже покрыт или не покрыт сверкой, и выбросить его
/// значило бы посчитать долю от неполного портфеля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavCoverage {
    pub accepted_independent: Dec,
    pub accepted_internal: Dec,
    pub provisional: Dec,
    /// Доля стоимости, по которой сверка не сходится.
    ///
    /// Спека §10.5 показывает в примере три доли. Четвёртая добавлена
    /// намеренно: без неё расходящийся счёт попадал бы в `provisional`
    /// и выглядел как «просто пока не подтверждён» — то есть проблема
    /// пряталась бы ровно в той цифре, которая существует, чтобы её
    /// показывать.
    pub discrepant: Dec,
}

/// Материальная проблема качества данных (§10.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterialIssue {
    /// Позиция восстановлена без документированной стоимости (§10.7).
    RestoredWithoutBasis { account: AccountId },
    /// Цена устарела или является оценкой владельца.
    PriceNotExecutable {
        instrument: InstrumentId,
        quality: PriceQuality,
    },
    /// Отрицательный денежный остаток — обязательство в NAV (§11).
    NegativeCash {
        account: AccountId,
        currency: CurrencyCode,
    },
    /// Данных до этой даты нет; всё, что раньше, в расчёт не вошло.
    HistoryStartsAt { date: Date },
    /// Независимого источника по счёту нет: подтверждать нечем (§10.5).
    NoIndependentSource {
        account: AccountId,
        dimension: crate::reconciliation::Dimension,
    },
    /// Сверка не сходится.
    Discrepancy {
        account: AccountId,
        dimension: crate::reconciliation::Dimension,
    },
    /// На счёте присутствует финансирование вне периметра (§11).
    UnsupportedFinancing { account: AccountId },
}

/// Блок качества данных.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataQuality {
    pub status: DataQualityStatus,
    pub nav_coverage: NavCoverage,
    pub material_issues: Vec<MaterialIssue>,
}
```

В `NotComputable` добавьте вариант и его код:

```rust
    /// На счёте финансирование вне периметра: экономику система
    /// не достраивает (§11).
    UnsupportedFinancing { account: AccountId },
```

```rust
            Self::UnsupportedFinancing { .. } => "unsupported_financing",
```

Замените функцию `data_quality`:

```rust
/// Блок качества данных строится из состояния и реестра сверки,
/// а не из желания показать зелёный статус.
fn data_quality(state: &LedgerState, request: &ReturnsRequest) -> DataQuality {
    let mut issues = Vec::new();
    for account in state.coverage().restored_accounts() {
        issues.push(MaterialIssue::RestoredWithoutBasis { account: *account });
    }
    for (instrument, price) in state.prices().iter() {
        if !price.quality.is_complete() {
            issues.push(MaterialIssue::PriceNotExecutable {
                instrument: *instrument,
                quality: price.quality,
            });
        }
    }
    for (account, money) in state.balances().negative_cash() {
        issues.push(MaterialIssue::NegativeCash {
            account,
            currency: money.currency(),
        });
    }
    if let Some(date) = state.coverage().first_event() {
        issues.push(MaterialIssue::HistoryStartsAt { date });
    }

    let values = xirr::account_values(state, request).unwrap_or_default();
    let mut coverage = Shares::default();
    for (account, value) in &values {
        if request.perimeter.financing_present(*account) {
            issues.push(MaterialIssue::UnsupportedFinancing { account: *account });
        }
        // Уровень счёта — худший среди денег и позиций: подтверждённые
        // деньги при расходящихся количествах не делают счёт покрытым.
        let mut level = DimensionStatus::AcceptedIndependent;
        for dimension in [
            crate::reconciliation::Dimension::Cash,
            crate::reconciliation::Dimension::Positions,
        ] {
            let status = request.ledger.status_for(*account, request.as_of, dimension);
            match status {
                DimensionStatus::Discrepant => {
                    issues.push(MaterialIssue::Discrepancy {
                        account: *account,
                        dimension,
                    });
                }
                DimensionStatus::Provisional => {
                    issues.push(MaterialIssue::NoIndependentSource {
                        account: *account,
                        dimension,
                    });
                }
                DimensionStatus::AcceptedInternal | DimensionStatus::AcceptedIndependent => {}
            }
            level = worse_of(level, status);
        }
        coverage.add(level, value.inner().abs());
    }

    let nav_coverage = coverage.finish();
    // `HistoryStartsAt` неполнотой не является: «данных до 01.03.2024
    // нет» — это факт о периоде, а не дефект (§10.7).
    let material = issues
        .iter()
        .any(|issue| !matches!(issue, MaterialIssue::HistoryStartsAt { .. }));
    let status = if material {
        DataQualityStatus::Incomplete
    } else if nav_coverage.provisional.is_zero() {
        DataQualityStatus::Clean
    } else {
        DataQualityStatus::Mixed
    };
    DataQuality {
        status,
        nav_coverage,
        material_issues: issues,
    }
}

const fn worse_of(left: DimensionStatus, right: DimensionStatus) -> DimensionStatus {
    match (left, right) {
        (DimensionStatus::Discrepant, _) | (_, DimensionStatus::Discrepant) => {
            DimensionStatus::Discrepant
        }
        (DimensionStatus::Provisional, _) | (_, DimensionStatus::Provisional) => {
            DimensionStatus::Provisional
        }
        (DimensionStatus::AcceptedInternal, _) | (_, DimensionStatus::AcceptedInternal) => {
            DimensionStatus::AcceptedInternal
        }
        _ => DimensionStatus::AcceptedIndependent,
    }
}

/// Накопитель долей. Считает в `rust_decimal`, потому что доля —
/// расчётная величина, а не проведённая сумма (§3.4).
#[derive(Debug, Default)]
struct Shares {
    independent: rust_decimal::Decimal,
    internal: rust_decimal::Decimal,
    provisional: rust_decimal::Decimal,
    discrepant: rust_decimal::Decimal,
}

impl Shares {
    fn add(&mut self, level: DimensionStatus, weight: rust_decimal::Decimal) {
        let slot = match level {
            DimensionStatus::AcceptedIndependent => &mut self.independent,
            DimensionStatus::AcceptedInternal => &mut self.internal,
            DimensionStatus::Provisional => &mut self.provisional,
            DimensionStatus::Discrepant => &mut self.discrepant,
        };
        *slot += weight;
    }

    /// Доли от суммы весов.
    ///
    /// Нулевая сумма означает пустой портфель: доли неопределимы,
    /// и честный ответ — «ничего не подтверждено», а не деление на ноль.
    fn finish(self) -> NavCoverage {
        let total = self.independent + self.internal + self.provisional + self.discrepant;
        if total.is_zero() {
            return NavCoverage {
                accepted_independent: Dec::zero(),
                accepted_internal: Dec::zero(),
                provisional: Dec::one(),
                discrepant: Dec::zero(),
            };
        }
        NavCoverage {
            accepted_independent: Dec::new(self.independent / total),
            accepted_internal: Dec::new(self.internal / total),
            provisional: Dec::new(self.provisional / total),
            discrepant: Dec::new(self.discrepant / total),
        }
    }
}
```

Обновите `ReturnsRequest` и вызов `data_quality(state)` →
`data_quality(state, request)`:

```rust
pub struct ReturnsRequest<'a> {
    pub contour: &'a ContourDefinition,
    pub as_of: Date,
    pub report_currency: CurrencyCode,
    pub fx: &'a FxTable,
    pub solver_policy: SolverPolicy,
    /// Реестр сверки: без него доля подтверждённого неизвестна.
    pub ledger: &'a crate::reconciliation::ReconciliationLedger,
    /// Оценка периметра: без неё отчёт не знает, где отказаться считать.
    pub perimeter: &'a crate::perimeter::PerimeterAssessment,
}
```

Добавьте в `AppliedRules` порог периметра — цифра обязана нести порог:

```rust
    pub perimeter_policy: crate::perimeter::PerimeterPolicy,
```

- [ ] **Шаг 3: Написать тест**

Создайте `crates/iaam-core/tests/data_quality.rs` с тремя проверками:
доли в сумме дают единицу; расходящийся счёт попадает в `discrepant`,
а не в `provisional`; счёт с маржинальным финансированием даёт
`MaterialIssue::UnsupportedFinancing`, а соседний счёт продолжает
считаться. Ожидаемые доли считаются вручную от стоимостей счетов,
а не берутся из вывода (§15.5).

- [ ] **Шаг 4: Починить вызовы и прогнать**

Сборка укажет на `iaam-app/src/scenarios/reports.rs` и тесты, где
собирается `ReturnsRequest`. Реестр и оценка периметра строятся там
из тех же событий, что и проекция.

```bash
nix develop -c cargo test --workspace 2>&1 | tail -20
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/iaam-core crates/iaam-app
git commit -m "feat(core): покрытие NAV по уровням достоверности в dataQuality (iaam-023)"
```

---

## Задача 8: Восстановленные начала как набор утверждений

**Files:**
- Modify: `crates/iaam-core/src/event/kind.rs` — `OpeningAssertions` и поле в `OpeningPosition`
- Modify: `crates/iaam-core/src/projection/lots.rs` — `..` в образце
- Modify: `crates/iaam-ingest/src/operation.rs` — заполнение утверждений
- Test: тесты в `kind.rs` и `crates/iaam-core/tests/opening_assertions.rs`

**Interfaces:**
- Produces: `Certainty { Known, Estimated }`, `DateCertainty { Known, Estimated, Unknown }`, `BasisCertainty { Documented, Estimated, Unknown }`, `Tristate { Yes, No, Unknown }`, `Knowledge { Known, Unknown }`, `OpeningAssertions` с `Default`; `EventKind::OpeningPosition { .., assertions }`.

**Acceptance Criteria:**
- `opening_position` несёт семь утверждений §10.7, каждое со своей уверенностью, а не одну цену
- Уже записанные события версий 1–2 читаются без миграции: поле имеет `#[serde(default)]`, и умолчание — «неизвестно», а не выдуманное значение
- Восстановленная позиция без документированной налоговой стоимости даёт `MaterialIssue::RestoredWithoutBasis`
- Тест round-trip: событие версии 2 без поля десериализуется и даёт умолчание

- [ ] **Шаг 1: Написать утверждения**

В `crates/iaam-core/src/event/kind.rs` добавьте перед `EventKind`:

```rust
/// Уверенность в количестве (§10.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Certainty {
    Known,
    Estimated,
}

/// Уверенность в дате приобретения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateCertainty {
    Known,
    Estimated,
    Unknown,
}

/// Уверенность в налоговой стоимости.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisCertainty {
    Documented,
    Estimated,
    Unknown,
}

/// Троичный ответ. `Unknown` — полноценное значение, а не «нет» (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tristate {
    Yes,
    No,
    Unknown,
}

/// Известно ли что-то вообще.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Knowledge {
    Known,
    Unknown,
}

/// Восстановленное начало как **набор утверждений с уверенностью**
/// (§10.7), а не строка с ценой.
///
/// Умолчание — «неизвестно» по каждому пункту. Это не заглушка:
/// событие, записанное до появления этого поля, действительно ничего
/// из перечисленного не утверждало, и приписать ему `Known` значило бы
/// задним числом объявить документированным то, чего никто не видел.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpeningAssertions {
    pub quantity: Certainty,
    pub acquisition_date: Option<time::Date>,
    pub acquisition_date_certainty: DateCertainty,
    pub tax_basis: BasisCertainty,
    pub basis_currency: Option<CurrencyCode>,
    pub basis_rate: Option<Dec>,
    pub fees_included: Tristate,
    pub ldv_eligibility: Knowledge,
    pub prior_corporate_actions: Knowledge,
}

impl Default for OpeningAssertions {
    fn default() -> Self {
        Self {
            quantity: Certainty::Estimated,
            acquisition_date: None,
            acquisition_date_certainty: DateCertainty::Unknown,
            tax_basis: BasisCertainty::Unknown,
            basis_currency: None,
            basis_rate: None,
            fees_included: Tristate::Unknown,
            ldv_eligibility: Knowledge::Unknown,
            prior_corporate_actions: Knowledge::Unknown,
        }
    }
}

impl OpeningAssertions {
    /// Достаточно ли известно, чтобы считать налоговую стоимость.
    ///
    /// Используется отчётом: если стоимость неизвестна, налоговый
    /// отчёт обязан вернуть диапазон или `not_computable`, но не
    /// точную цифру (§10.7). Сам расчёт появится в E5.
    #[must_use]
    pub const fn basis_is_documented(&self) -> bool {
        matches!(self.tax_basis, BasisCertainty::Documented)
    }
}
```

Измените вариант события:

```rust
    /// Восстановленная позиция для счёта без истории (§10.7).
    OpeningPosition {
        instrument: InstrumentId,
        quantity: Quantity,
        cost_basis: Option<Money>,
        /// Набор утверждений о восстановленном начале.
        ///
        /// `#[serde(default)]` обязателен: журнал append-only, и уже
        /// записанные события этого поля не содержат. Отсутствие поля
        /// означает «ничего из этого не утверждалось».
        #[serde(default)]
        assertions: OpeningAssertions,
    },
```

- [ ] **Шаг 2: Починить образцы**

В `crates/iaam-core/src/projection/lots.rs` и в `event/mod.rs` добавьте
`..` в образцы `OpeningPosition`. В `iaam-ingest/src/operation.rs`
заполняйте утверждения из присланной операции; там, где клиент ничего
не сказал, — `OpeningAssertions::default()`, **не** выдуманные значения.

- [ ] **Шаг 3: Тест обратной совместимости**

Создайте `crates/iaam-core/tests/opening_assertions.rs`: возьмите JSON
события версии 2 без поля `assertions` (запишите его в тесте строковым
литералом, а не сгенерируйте текущим кодом — иначе тест проверяет сам
себя), десериализуйте и убедитесь, что утверждения равны умолчанию,
а `basis_is_documented()` ложно.

- [ ] **Шаг 4: Прогнать и закоммитить**

```bash
nix develop -c cargo test --workspace 2>&1 | tail -20
git add crates/iaam-core crates/iaam-ingest
git commit -m "feat(core): восстановленное начало как набор утверждений с уверенностью (iaam-023)"
```

---

## Задача 9: Приёмка ядра E2 — свойства, метаморфные тесты, мутационный заслон

**Files:**
- Create: `crates/iaam-core/tests/reconciliation_properties.rs`
- Create: `crates/iaam-core/tests/metamorphic_reconciliation.rs`
- Create: `crates/iaam-core/tests/acceptance_stage2.rs`
- Modify: `scripts/check-mutants.sh` — пороги новых модулей
- Modify: `tests/fixtures/MANIFEST.sha256`

**Interfaces:**
- Consumes: всё из задач 1–8.

**Acceptance Criteria:**
- **Компенсирующая ошибка парсера не поднимает статус выше `accepted_internal`** — прямой критерий приёмки эпика, отдельным метаморфным тестом
- Перестановка событий в журнале не меняет ни одного статуса
- Ни одно измерение не достигает `accepted_independent`, пока все каналы делят версию парсера — свойство, проверяемое на сгенерированных журналах
- Расхождение по измерению поглощает любые основания того же измерения
- Мутационный заслон покрывает `reconciliation/*` и `perimeter.rs` с порогом не ниже действующего для `projection/*`

- [ ] **Шаг 1: Метаморфный тест компенсирующей ошибки**

Создайте `crates/iaam-core/tests/metamorphic_reconciliation.rs`:

```rust
//! Метаморфные тесты сверки (§15.6).
//!
//! Метаморфное отношение проверяет не конкретное число, а поведение
//! при известном преобразовании входа. Здесь это главное: сверка
//! обязана **не заметить** компенсирующую ошибку разбора — и обязана
//! не выдать за независимое подтверждение то, что ею не является.

use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use iaam_core::ids::{AccountId, OwnerId};
use time::macros::date;

mod support;
use support::event_from_channel;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn march() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
}

/// Журнал одного документа: операция и полный набор контрольных секций,
/// согласованных между собой. `shift` сдвигает **обе** стороны — это и
/// есть компенсирующая ошибка парсера.
fn statement(owner: OwnerId, account: AccountId, shift: i64) -> Vec<iaam_core::event::Event> {
    let deposit = 100_000 + shift;
    let period = march();
    let mut events = vec![event_from_channel(
        owner,
        account,
        date!(2026 - 03 - 10),
        1,
        EventKind::CashIn { amount: rub(deposit) },
        vec![Leg::cash(account, rub(deposit))],
        "tinkoff-xlsx/1",
        "march",
    )];
    for (index, claim) in [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(0),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(deposit),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(deposit),
            credit: PostedMinor::new(0),
        },
    ]
    .into_iter()
    .enumerate()
    {
        events.push(event_from_channel(
            owner,
            account,
            date!(2026 - 03 - 31),
            u32::try_from(index).unwrap() + 10,
            EventKind::ControlAssertion { period, claim },
            vec![],
            "tinkoff-xlsx/1",
            "march",
        ));
    }
    events
}

#[test]
fn a_compensating_parser_error_never_reaches_independent() {
    // Критерий приёмки эпика. Парсер ошибся на 7 копеек одинаково
    // в операции и в контрольной секции: обе стороны съехали, сверка
    // сошлась, и это ровно тот случай, ради которого §10.3 вводит
    // третий уровень. Статус обязан остаться internal.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    let honest = ReconciliationLedger::build(&statement(owner, account, 0)).unwrap();
    let skewed = ReconciliationLedger::build(&statement(owner, account, 7)).unwrap();

    let honest_status = honest.status_for(account, date!(2026 - 03 - 15), Dimension::Cash);
    let skewed_status = skewed.status_for(account, date!(2026 - 03 - 15), Dimension::Cash);

    assert_eq!(
        honest_status, skewed_status,
        "сверка внутри одного документа не отличает верный разбор от компенсирующе неверного — \
         и именно поэтому не имеет права называть его независимым"
    );
    assert_eq!(skewed_status, DimensionStatus::AcceptedInternal);
    assert_ne!(skewed_status, DimensionStatus::AcceptedIndependent);
}

#[test]
fn reordering_the_journal_does_not_change_any_status() {
    // Проекция определяется журналом, а не порядком его чтения.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let events = statement(owner, account, 0);
    let mut reversed = events.clone();
    reversed.reverse();

    let straight = ReconciliationLedger::build(&events).unwrap();
    let backwards = ReconciliationLedger::build(&reversed).unwrap();
    for dimension in Dimension::all() {
        assert_eq!(
            straight.status_for(account, date!(2026 - 03 - 15), dimension),
            backwards.status_for(account, date!(2026 - 03 - 15), dimension)
        );
    }
}
```

Допишите в `crates/iaam-core/tests/support/mod.rs` функцию
`event_from_channel(owner, account, day, sequence, kind, legs, parser,
document)` — она собирает событие с заданными версией парсера и хешом
документа (тело такое же, как у `event_at`, но `Provenance` берёт
переданные значения).

- [ ] **Шаг 2: Свойства**

Создайте `crates/iaam-core/tests/reconciliation_properties.rs` с двумя
свойствами на `proptest`:

1. **Один парсер — не выше `internal`.** Генерируется журнал из одного
   до пяти документов, все с версией парсера `"same/1"`, произвольными
   суммами и датами. Утверждение: ни для одного счёта, даты и измерения
   статус не равен `AcceptedIndependent`.
2. **Расхождение поглощает.** Генерируется журнал, в котором ровно одно
   утверждение заведомо не сходится. Утверждение: статус его измерения
   равен `Discrepant` независимо от того, сколько сошедшихся утверждений
   добавлено рядом.

Обе формулировки — свойства, а не примеры: ожидаемое значение выводится
из правила §10.3, а не из прогона программы (§15.5).

- [ ] **Шаг 3: Приёмка этапа 2**

Создайте `crates/iaam-core/tests/acceptance_stage2.rs`: один сценарий
из конца в конец — два месячных отчёта одного брокера плюс данные того
же периода вторым каналом. Проверяется по шагам:

- март по одному отчёту → `AcceptedInternal` (основание 5);
- апрельский отчёт с начальным остатком, равным вычисленному мартовскому
  → март остаётся `AcceptedInternal`, но получает основание 1;
- те же данные вторым каналом с другой версией парсера → март становится
  `AcceptedIndependent`;
- испорченная цифра в одном из документов → `Discrepant`, и доля
  `discrepant` в `navCoverage` строго положительна.

Ожидаемые остатки считаются в тесте вручную из сумм операций.

- [ ] **Шаг 4: Мутационный заслон**

В `scripts/check-mutants.sh` добавьте новые модули в список с порогами:
`reconciliation/claim.rs`, `reconciliation/observed.rs`,
`reconciliation/check.rs`, `reconciliation/evidence.rs`,
`reconciliation/mod.rs`, `perimeter.rs`. Порог — не ниже действующего
для `projection/*`.

Проверьте заслон падением: внесите в `Evidence::level` замену
понижения на потолок (то есть уберите проверку независимости) и
убедитесь, что мутационный прогон и тесты краснеют. Верните код.

- [ ] **Шаг 5: Прогнать всё и закоммитить**

```bash
nix develop -c cargo nextest run --workspace --all-features 2>&1 | tail -20
nix develop -c ./scripts/check-mutants.sh
nix develop -c ./scripts/check-architecture.sh
git add crates/iaam-core/tests scripts/check-mutants.sh tests/fixtures/MANIFEST.sha256
git commit -m "test(core): приёмка ядра E2, свойства и метаморфные тесты сверки (iaam-023)"
```

---

# Часть B — источники данных

## Задача 10: `iaam-store` — сырьё документов и строк

**Files:**
- Create: `crates/iaam-store/migrations/0002_sources_and_rules.sql`
- Create: `crates/iaam-store/src/documents.rs`
- Modify: `crates/iaam-store/src/schema.rs` — `SCHEMA_VERSION = 2`, вторая миграция
- Modify: `crates/iaam-store/src/lib.rs`
- Test: `crates/iaam-store/tests/documents.rs`

**Interfaces:**
- Produces: `DocumentRecord { id, owner, broker, format, parser_version, document_hash, uploaded_at, body }`; `RawRow { document, sheet, row, payload, status }`; `insert_document`, `load_document`, `insert_rows`, `rows_of_document`, `documents_needing_reparse(parser_version)`.

**Acceptance Criteria:**
- Сырьё документа и каждая строка хранятся: повторный разбор новой версией парсера не требует обращения к источнику (§10.1)
- Повторная загрузка того же документа тем же владельцем не создаёт второй записи — уникальность по `(owner, document_hash)`
- Документ и строки неизменяемы триггером, как и журнал: разбор можно повторить, сырьё — нет
- Строка хранит локатор (лист, номер) — без него provenance не восстановить
- Чужой документ не читается: владелец входит в каждый запрос

- [ ] **Шаг 1: Написать миграцию**

Создайте `crates/iaam-store/migrations/0002_sources_and_rules.sql`:

```sql
-- Сырьё источников (§10.1) и правила классификации (§10.4).
--
-- Сырьё хранится, потому что версия парсера пишется в provenance
-- ради повторного разбора: разбор без сырья повторить нельзя, и
-- исправленный парсер оказался бы бесполезен для уже загруженного.

CREATE TABLE source_documents (
    id             TEXT PRIMARY KEY,
    owner          TEXT NOT NULL,
    broker         TEXT NOT NULL,
    format         TEXT NOT NULL,
    parser_version TEXT NOT NULL,
    document_hash  TEXT NOT NULL,
    uploaded_at    TEXT NOT NULL,
    body           BLOB NOT NULL
) STRICT;

-- Тот же файл того же владельца — один документ. Разные владельцы
-- могут загрузить одинаковый файл: это разные факты о разных портфелях.
CREATE UNIQUE INDEX source_documents_by_hash ON source_documents (owner, document_hash);

CREATE TABLE raw_rows (
    document TEXT NOT NULL,
    sheet    TEXT,
    row      INTEGER NOT NULL,
    payload  TEXT NOT NULL,
    status   TEXT NOT NULL,
    FOREIGN KEY (document) REFERENCES source_documents (id)
) STRICT;

-- Локатор уникален, но первичным ключом быть не может: в STRICT-таблице
-- колонки первичного ключа неявно NOT NULL, а у CSV листа нет, и
-- `PRIMARY KEY (document, sheet, row)` запретил бы хранить его строки
-- вовсе. Пустой строкой лист не подменяется: неизвестное — NULL (§4.9).
--
-- `ifnull` в индексе обязателен: в обычном уникальном индексе SQLite
-- считает NULL несовпадающими, и один и тот же кусок сырья без листа
-- лёг бы в базу дважды.
CREATE UNIQUE INDEX raw_rows_by_locator
    ON raw_rows (document, ifnull(sheet, ''), row);

-- Сырьё неизменяемо наравне с журналом: «поправить строку в исходнике»
-- означает переписать факт задним числом. Разбор повторяется, сырьё —
-- никогда.
CREATE TRIGGER source_documents_are_immutable
BEFORE UPDATE ON source_documents
BEGIN
    SELECT RAISE(ABORT, 'сырьё источника неизменяемо: загрузите новый документ');
END;

CREATE TRIGGER raw_rows_are_immutable
BEFORE UPDATE ON raw_rows
BEGIN
    SELECT RAISE(ABORT, 'сырьё источника неизменяемо');
END;

CREATE TRIGGER source_documents_are_not_deletable
BEFORE DELETE ON source_documents
BEGIN
    SELECT RAISE(ABORT, 'сырьё источника неудаляемо: provenance перестанет разрешаться');
END;

CREATE TRIGGER raw_rows_are_not_deletable
BEFORE DELETE ON raw_rows
BEGIN
    SELECT RAISE(ABORT, 'сырьё источника неудаляемо');
END;

-- Правила классификации (§10.4). Меняются владельцем, поэтому обычная
-- таблица; версия нужна, чтобы пересчёт истории знал, каким правилом
-- он вызван.
CREATE TABLE classification_rules (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    version    INTEGER NOT NULL,
    matcher    TEXT NOT NULL,
    outcome    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    retired_at TEXT
) STRICT;

CREATE INDEX classification_rules_by_owner ON classification_rules (owner, retired_at);
```

В `schema.rs` поднимите `SCHEMA_VERSION` до 2 и добавьте вторую пару
в `MIGRATIONS`.

- [ ] **Шаг 2: Тест на неизменяемость и идемпотентность**

Создайте `crates/iaam-store/tests/documents.rs`: загрузка документа,
повторная загрузка того же файла тем же владельцем возвращает прежний
идентификатор; `UPDATE` и `DELETE` по обеим таблицам отбиваются
триггером (проверяется прямым SQL, а не через обёртку — заслон должен
держать и скрипт починки данных); чужой документ не читается.

- [ ] **Шаг 3: Написать `documents.rs`**

Реализуйте функции по образцу `events.rs`: параметризованные запросы,
владелец в каждом, ошибки — `StoreError`. Тело документа кладётся
`BLOB`, строки — по одной на локатор.

- [ ] **Шаг 4: Прогнать и закоммитить**

```bash
nix develop -c cargo test -p iaam-store 2>&1 | tail -20
git add crates/iaam-store
git commit -m "feat(store): сырьё документов и строк для повторного разбора (iaam-023)"
```

---

## Задача 11: `iaam-store` — правила классификации

> Развёрнута до полной глубины 2026-08-23, после задачи 10: типы
> хранилища уже существуют и не угадываются.

**Files:**
- Create: `crates/iaam-store/src/rules.rs`
- Modify: `crates/iaam-store/migrations/0002_sources_and_rules.sql` — колонка `replaces`
- Modify: `crates/iaam-core/src/ids.rs` — `ClassificationRuleId`
- Modify: `crates/iaam-store/src/lib.rs`
- Test: `crates/iaam-store/tests/rules.rs`

**Interfaces:**
- Produces: `StoredRule { id, owner, version, matcher, outcome, created_at, retired_at, replaces }`; `insert_rule(owner, matcher, outcome)`, `amend_rule(owner, previous, matcher, outcome)`, `retire_rule(owner, id)`, `list_active_rules(owner)`, `rule_history(owner)`.

**Acceptance Criteria:**
- Правило не удаляется, а выводится из обращения датой: история решений остаётся видимой
- Правка правила заводит новую версию, а не переписывает прежнюю — иначе объяснить старую классификацию будет нечем
- Активные правила выбираются одним запросом в порядке версии
- Владелец входит в каждый запрос

**Решения развёртывания.** Четыре, и каждое меняет контур:

1. **`ClassificationRuleId` заводится в `iaam-core::ids`, а не
   в хранилище.** Правило классификации — понятие §10.4, и задача 13
   всё равно им пользуется; идентичность, изобретённая хранилищем,
   к задаче 13 существовала бы в двух экземплярах. Имя полное, потому
   что `RuleId` в ядре уже занят версией правила списания лотов
   (`fifo/214.1/v1`) — это другое понятие, и короткое имя сделало бы
   их неразличимыми на месте использования.
2. **`version` — сквозной номер решения владельца, а не номер внутри
   линии правила.** Колонка `id` объявлена первичным ключом, двух строк
   с одним `id` быть не может; версия нумерует решения владельца по
   порядку, и пересчёт истории по ней узнаёт, каким решением он вызван.
3. **Правка — это `amend_rule`, а не `insert_rule` рядом.** Старая
   строка выводится из обращения и новая заводится **одной
   транзакцией**: между ними нет момента, когда действуют оба правила
   или не действует ни одного. Новая строка ссылается на прежнюю
   колонкой `replaces` — без неё вывод из обращения и заведение
   остаются двумя несвязанными строками, и «как это правило дошло до
   нынешнего вида» ответа не имеет. Миграция 0002 ещё не выпущена,
   колонка добавляется в неё же.
4. **`retire_rule(owner, id)` без параметра момента** — момент ставит
   хранилище, как `revoke_token`. Присланный клиентом момент вывода из
   обращения — это момент, которому нечем верить, а часы в крейте одни.

Повторный вывод из обращения уже выведенного правила — отказ, а не
тихое обновление даты: дата вывода не переписывается задним числом.

- [ ] **Шаг 1: `ClassificationRuleId` в ядре**

В `crates/iaam-core/src/ids.rs` добавьте `typed_id!(ClassificationRuleId)` и строку
в тест `ids_keep_the_uuid_they_wrap` — тест первым.

- [ ] **Шаг 2: Колонка `replaces` в миграции 0002**

```sql
    replaces   TEXT REFERENCES classification_rules (id),
```

- [ ] **Шаг 3: Тесты**

`crates/iaam-store/tests/rules.rs`, по одному поведению на тест:

| Тест | Что доказывает |
|---|---|
| `a_new_rule_is_active_from_the_moment_it_is_stored` | заведение и чтение активных |
| `a_retired_rule_leaves_the_active_set_but_stays_in_history` | вывод из обращения не удаляет |
| `an_amendment_adds_a_version_and_retires_the_one_it_replaces` | правка не затирает прежнюю, `replaces` указывает на неё |
| `a_rule_is_retired_once_and_the_date_is_not_rewritten` | повторный вывод — отказ |
| `versions_are_numbered_within_the_owner` | номер решения не течёт между владельцами |
| `another_owners_rules_are_neither_active_nor_in_our_history` | владелец в каждом запросе |
| `another_owners_rule_is_neither_amended_nor_retired` | чужое правило не правится и не выводится |
| `a_matcher_or_outcome_that_is_not_json_is_refused` | нечитаемое правило не доходит до задачи 13 |

- [ ] **Шаг 4: Реализация `rules.rs`**

По образцу `documents.rs`: параметризованные запросы, владелец в каждом,
номер версии назначается в той же немедленной транзакции, что и вставка
(раздельно — та же гонка, что в `append_event_in_order`). `matcher`
и `outcome` — JSON-строки доменных типов задачи 13: хранилище не знает
их устройства, оно хранит; но разбираемость как JSON проверяет при
записи, иначе правило, которое задача 13 не сможет прочитать, ляжет
в базу молча.

- [ ] **Шаг 5: Коммит**

```bash
nix develop -c cargo test -p iaam-store 2>&1 | tail -10
git add crates/iaam-store crates/iaam-core
git commit -m "feat(store): правила классификации с историей версий (iaam-023)"
```

---

## Задача 12: Иерархия ключей дедупликации

> Развёрнута до полной глубины 2026-08-23, после задач 10 и 11.

**Files:**
- Create: `crates/iaam-ingest/src/dedup.rs`
- Modify: `crates/iaam-ingest/src/lib.rs`
- Modify: `crates/iaam-ingest/src/operation.rs` — отпечаток берётся из `dedup`
- Modify: `crates/iaam-core/src/event/provenance.rs` — `RowLocator.document` становится `RawHash`
- Test: `crates/iaam-ingest/tests/dedup.rs`

**Interfaces:**
- Consumes: `SubmittedOperation`, `Provenance`.
- Produces: `DedupKey` (`SourceOperationId`, `IdempotencyKey`, `NormalizedFingerprint`, `DocumentRow`); `DedupLevel`; `DedupDecision` (`Duplicate { key, existing }`, `Fresh`, `PossibleDuplicate { of, level }`); `DocumentContext`; `KnownRecord`; `fingerprint(&SubmittedOperation) -> RawHash`; `choose_key(&SubmittedOperation, &DocumentContext) -> Option<DedupKey>`; `assess(Option<&DedupKey>, &RawHash, &DocumentContext, &[KnownRecord]) -> DedupDecision`.

**Acceptance Criteria:**
- Иерархия §10.6 реализована целиком: пять уровней, выбирается сильнейший доступный
- Две одинаковые покупки в один день **не** становятся дубликатом — прямой тест
- Повторная загрузка того же документа даёт `Duplicate` по локатору строки на каждой строке
- Совпадение отпечатка между **разными** документами даёт `PossibleDuplicate`, а не удаление
- Вероятностный дубликат никогда не приводит к автоматическому отбрасыванию — отдельный тест на то, что строка записана

### Поправка к решению наброска об уровне 3

Набросок этой задачи объявлял отпечаток **жёстким** ключом внутри
одного документа. Это противоречит его же критерию приёмки: две
законные одинаковые покупки лежат в одном документе разными строками,
отпечаток у них совпадает, и жёсткий уровень 3 объявил бы вторую
дубликатом — ровно то, что §10.6 запрещает прямым текстом. Одно из двух
обязано было уступить.

Уступает уровень 3, и вот почему. **Внутри документа тождество строки —
это её локатор, а не её содержимое.** Документ и есть свидетельство
того, что операций было две: парсер увидел две строки. Отпечаток же
описывает содержание, а одинаковое содержание — нормальное явление
(две части исполнения одной заявки). Поэтому:

| §10.6 | Ключ | Когда действует |
|---|---|---|
| 1 | `SourceOperationId` | источник дал стабильный идентификатор |
| 2 | `IdempotencyKey` | клиент назвал подачу |
| 4 | `DocumentRow` | документ известен **и** известен локатор |
| 3 | `NormalizedFingerprint` | документ известен, а локатора нет |
| 5 | подсказка по отпечатку | совпадение с записью **другого** документа или канала без файла |

**Порядок выбора — 1, 2, 4, 3**, а не 1, 2, 3, 4: локатор сильнее голого
отпечатка, потому что отпечаток не является тождеством. Отклонение от
нумерации спеки намеренное и записано здесь, а не спрятано в коде.

Уровень 3 не мёртв: канал, который даёт документ, но не даёт номера
строки (выписка, разобранная не по таблице), опирается именно на него.

**Совпадение отпечатка внутри одного документа на другом локаторе —
`Fresh`, и даже не подсказка.** Иначе отчёт с двумя одинаковыми
покупками завалил бы владельца подсказками на ровном месте.

**Но подсказку снимает только доказанное совпадение документа.** Два
канала без файла (повторная выгрузка из API без идентификаторов
операций) документа не имеют вовсе, и сравнение «`None` равно `None`,
значит один документ» объявило бы их одной строкой — молчаливое
удвоение позиции. Отсутствие документа не является совпадением
документов.

### Прочие решения развёртывания

1. **`RowLocator.document` становится `RawHash` вместо `String`.**
   Уровень 4 — «хеш документа плюс локатор»; свободная строка в этом
   поле означала бы имя файла, а имя файла не является тождеством:
   тот же отчёт, сохранённый под другим именем, перестал бы
   дедуплицироваться. По хешу локатор разрешается в `source_documents`
   задачи 10, а человеческое имя документа берётся оттуда же.
   В рабочем коде `RowLocator` пока не строится нигде — правится один
   тест ядра.
2. **Отпечаток считается один раз и живёт в `dedup`.** Сейчас он живёт
   приватной функцией в `operation.rs` и строится на `format!("{:?}")`:
   переименование поля молча меняет все отпечатки, а по ним уже
   дедуплицировано. Каноническая форма — JSON выделенной структуры
   с номером версии формата; `operation.rs` зовёт ту же функцию, второго
   экземпляра не существует.
3. **Ожидаемый отпечаток в тесте заморожен и посчитан независимо**
   (§15.5): значение получено `sha256sum` от канонической строки вне
   программы, команда воспроизведения записана в самом тесте. Из вывода
   программы оно не берётся никогда.
4. **`reason` подсказки — перечисление `DedupLevel`, а не строка.**
   Строковый дискриминатор запрещён там, где возможен `enum`.

- [ ] **Шаг 1: `RowLocator.document` — `RawHash`**

Правится `crates/iaam-core/src/event/provenance.rs` и тест
`crates/iaam-core/tests/serde_roundtrip.rs` — тест первым.

- [ ] **Шаг 2: Тесты дедупликации**

`crates/iaam-ingest/tests/dedup.rs`, по одному поведению на тест:

| Тест | Что доказывает |
|---|---|
| `the_strongest_available_key_wins` | при всех доступных ключах выбран `SourceOperationId` |
| `the_row_locator_outranks_a_bare_fingerprint` | порядок 1, 2, 4, 3 |
| `a_channel_without_a_row_number_falls_back_to_the_fingerprint` | уровень 3 не мёртв |
| `a_submission_that_nothing_identifies_has_no_key` | `choose_key` → `None` |
| `two_identical_purchases_on_one_day_are_not_a_duplicate` | §10.6 прямым текстом |
| `reloading_the_same_document_duplicates_every_row` | `Duplicate` по локатору на каждой строке |
| `the_same_fingerprint_across_documents_is_only_a_hint` | `PossibleDuplicate`, не удаление |
| `a_possible_duplicate_is_still_recorded` | вероятностная оценка не отбрасывает строку |
| `two_identical_submissions_from_a_stream_are_a_hint` | отсутствие документа не снимает подсказку |
| `the_client_key_catches_a_resubmission_without_a_document` | уровень 2 |
| `the_fingerprint_ignores_the_keys_that_name_the_submission` | ключ идемпотентности не входит в отпечаток |
| `the_canonical_fingerprint_is_frozen` | замороженное значение, посчитанное независимо |

- [ ] **Шаг 3: Реализация `dedup.rs`**

```rust
//! Идемпотентность и дедупликация (§10.6).

/// Ключ, по которому строка признаётся уже виденной.
pub enum DedupKey {
    SourceOperationId(String),
    IdempotencyKey(String),
    NormalizedFingerprint { document: RawHash, fingerprint: RawHash },
    DocumentRow { document: RawHash, sheet: Option<String>, row: u64 },
}

/// Уровень иерархии §10.6, по которому принято решение.
pub enum DedupLevel {
    SourceOperationId,
    IdempotencyKey,
    NormalizedFingerprint,
    DocumentRow,
    Probabilistic,
}

/// Что делать со строкой.
pub enum DedupDecision {
    Duplicate { key: DedupKey, existing: EventId },
    Fresh,
    /// Похоже на дубликат, но доказательства нет. **Никогда** не
    /// приводит к удалению: показывается владельцу вместе
    /// с записанной строкой (§10.6).
    PossibleDuplicate { of: EventId, level: DedupLevel },
}
```

`assess` по порядку: точное совпадение выбранного ключа → `Duplicate`;
иначе совпадение отпечатка с записью **другого** документа или канала
без файла → `PossibleDuplicate`; иначе `Fresh`.

- [ ] **Шаг 4: Прогнать и закоммитить**

```bash
nix develop -c cargo test -p iaam-ingest 2>&1 | tail -15
git add crates/iaam-ingest crates/iaam-core
git commit -m "feat(ingest): иерархия ключей дедупликации из пяти уровней (iaam-023)"
```

---

## Задача 13: Правила классификации и пересчёт истории

> Развёрнута до полной глубины 2026-08-23, после задач 10–12.

**Files:**
- Create: `crates/iaam-ingest/src/classification.rs`
- Modify: `crates/iaam-ingest/src/lib.rs`
- Test: `crates/iaam-ingest/tests/classification.rs`

**Interfaces:**
- Consumes: правила из задачи 11 (хранение), сырьё из задачи 10, `Relation::{Reversal, Replacement}`, `event::correction::resolve`.
- Produces: `Movement`; `Counterparty`; `ClassificationSubject`; `RuleMatcher`; `Classification`; `ClassificationRule`; `Basis`; `Question`; `ClassificationResult`; `classify(&ClassificationSubject, &[ClassificationRule]) -> ClassificationResult`; `classification_of(&Event) -> Option<Classification>`; `Correction`; `CorrectionStep`; `recompute_plan(&[Event], &BTreeMap<EventId, ClassificationSubject>, &[ClassificationRule]) -> Result<Vec<Correction>, CorrectionError>`.

**Acceptance Criteria:**
- Операция, классификация которой не выводится из данных и не покрыта правилом, даёт вопрос владельцу, а не догадку
- Ответ владельца становится правилом, и следующая такая же операция вопроса не вызывает
- Правка правила **пересчитывает историю**: строится план из сторнирования и замены, журнал не переписывается
- Пересчёт идемпотентен: повторный запуск с тем же правилом не создаёт новых исправлений
- Правило видимо: его формулировку можно показать владельцу и она однозначно объясняет прошлую классификацию

### Решения развёртывания

1. **Классифицируется не операция, а признаки строки.** Правило смотрит
   на счёт-контрагент, описание и слово, которым источник назвал
   операцию, — ничего этого в `SubmittedOperation` нет и быть не должно:
   к моменту, когда операция построена, тип уже выбран. Поэтому вводится
   `ClassificationSubject` — то, что видно **до** выбора типа.
2. **Вопрос владельцу — перечисление `Question`, а не строка.** Вопрос
   уходит в API и должен рендериться с человеческими именами счетов,
   которых у чистой функции нет. Строка с UUID внутри не является
   «конкретным вопросом».
3. **Перевод на собственный счёт выводится из данных и правила не
   требует.** Правило нужно там, где данных не хватает; заводить его на
   выводимое означало бы спрашивать владельца о том, что уже известно.
   Отсюда `Basis::{Derived, Rule}` в ответе: «почему так
   классифицировано» — часть ответа, а не догадка читателя.
4. **Из нескольких подошедших правил выигрывает старшая версия.**
   Правка правила заводит новую версию (задача 11), и старшая — это
   последнее решение владельца. Пустой матчер не подходит ни к чему:
   правило «на всё» заводится только по ошибке.
5. **План пересчёта не умеет выражать правку.** `Correction` раскрывается
   ровно в два шага — сторно и замену; варианта «изменить событие»
   в типе нет, поэтому append-only journal гарантирован формой типа,
   а не дисциплиной вызывающего.
6. **Идемпотентность не программируется, а следует из устройства.**
   План строится по **действующему** набору (`resolve`), и после
   применения исправлений действующим становится замещающее событие,
   классификация которого уже совпадает с правилом. Повторный запуск
   даёт пустой план сам собой.

- [ ] **Шаг 1: Тесты**

`crates/iaam-ingest/tests/classification.rs`:

| Тест | Что доказывает |
|---|---|
| `a_transfer_to_an_own_account_needs_no_rule` | выводимое не спрашивают |
| `a_transfer_to_an_unknown_counterparty_asks_the_owner` | вопрос, а не догадка |
| `the_owners_answer_becomes_a_rule_and_the_question_stops` | правило снимает вопрос |
| `the_newest_matching_rule_wins` | старшая версия — последнее решение |
| `a_matcher_that_asks_for_nothing_matches_nothing` | правила «на всё» не бывает |
| `the_description_matcher_ignores_letter_case` | брокерский текст пишут как придётся |
| `every_matcher_condition_must_hold` | условия матчера соединяются «и» |
| `a_rule_explains_itself_in_words` | правило видимо владельцу |
| `an_outflow_without_a_counterparty_asks_fee_or_withdrawal` | вопрос по делу |
| `an_inflow_without_a_counterparty_asks_income_or_return` | вопрос по делу |
| `amending_a_rule_produces_a_reversal_and_a_replacement` | пересчёт через исправления |
| `an_event_the_rule_does_not_touch_stays_out_of_the_plan` | пересчёт не трогает лишнее |
| `recomputing_twice_produces_nothing_the_second_time` | идемпотентность |
| `an_event_that_carries_no_classification_is_never_recomputed` | сделка не классифицируется |
| `an_ambiguous_subject_is_left_alone_by_the_recompute` | догадка запрещена и здесь |

- [ ] **Шаг 2: Реализация** — `classification.rs` по решениям выше.

- [ ] **Шаг 3: Прогнать и закоммитить**

```bash
nix develop -c cargo test -p iaam-ingest 2>&1 | tail -15
git add crates/iaam-ingest
git commit -m "feat(ingest): правила классификации и пересчёт истории через сторно и замену (iaam-023)"
```

---

## Задача 14: Реестр парсеров отчётов и контрольные секции

> Развёрнута до полной глубины 2026-08-23, после задач 10–13.

**Files:**
- Create: `crates/iaam-ingest/src/report/mod.rs`, `crates/iaam-ingest/src/report/workbook.rs`, `crates/iaam-ingest/src/report/sections.rs`
- Create: `scripts/gen-report-fixtures.py`, `crates/iaam-ingest/tests/fixtures/minimal_workbook.xlsx`
- Modify: `crates/iaam-ingest/Cargo.toml` — `calamine`
- Modify: `crates/iaam-ingest/src/lib.rs`
- Test: `crates/iaam-ingest/tests/report_registry.rs`

**Interfaces:**
- Produces: `Broker`; `ReportFormat`; `Cell`; `Sheet`; `Workbook`; `WorkbookError`; `trait ReportParser`; `ParsedReport`; `LocatedRow`; `Quarantined`; `UnsupportedReason`; `ParserRegistry::{builtin, of, detect}`; `DetectError`; `ControlSections` и её `claims()`.

**Acceptance Criteria:**
- Парсер выбирается **по содержимому**, а не по имени файла: имя ничего не гарантирует
- Каждая строка получает локатор (лист, номер) и собственный исход; непонятая строка не отменяет документ (§10.1)
- Строки вне периметра уходят в карантин с причиной и **не** становятся отказом всего документа (§11)
- Контрольные секции превращаются в `ControlClaim` того же интервала, что и отчёт
- Версия парсера — часть его контракта и попадает в provenance каждой строки
- Ни одна функция разбора отчёта не переиспользуется каналом API — заслон в задаче 21

### Решения развёртывания

1. **`Workbook` — собственный тип, а не тип `calamine`.** Парсеры
   и тесты не зависят от API библиотеки, а книгу можно собрать
   в памяти без файла: опознание проверяется без двоичных фикстур.
   Замена библиотеки чтения не трогает ни один парсер.
2. **`detect` принимает открытую книгу, а не байты.** Вызывающему книга
   нужна и для разбора; открывать её дважды значит разбирать один файл
   двумя разными представлениями.
3. **Опознали двое — ошибка `Ambiguous`, а не первый выигравший.** Два
   парсера на один файл означают, что признак опознания слишком слаб;
   молча взять любой значит записать факты чужим разбором.
4. **`ParserRegistry::builtin()` пока пуст** — парсеры приходят задачами
   15 и 16. Заслон на это стоит тестом: пустой реестр никого не
   опознаёт и возвращает `Unrecognised`, а не выбирает наугад.
5. **Числовая ячейка XLSX — двоичная плавающая точка, и это граница
   ввода-вывода.** Перевод в `Dec` идёт через кратчайшее обратимое
   строковое представление; доменные величины `f64` не видят. Число,
   которого десятичный тип не представляет, остаётся текстом — не нулём
   и не потерянной ячейкой (§4.9).
6. **Дата в XLSX — число со стилем даты**, эпоха 30 декабря 1899 года.
   Без стиля ячейка неотличима от обычного числа, поэтому фикстура
   содержит `styles.xml`: иначе путь чтения дат остался бы
   непроверенным.
7. **Ячейка с ошибкой вычисления — отдельный вид ячейки**, а не текст:
   `#Н/Д`, попавшее в текстовую ячейку, парсер принял бы за подпись.
8. **Настоящий отчёт брокера фикстурой быть не может** — он содержит
   персональные данные владельца. Фикстура собирается скриптом
   `gen-report-fixtures.py`: двоичный файл неизвестного происхождения
   ничем не отличается от случайных байтов.

- [ ] **Шаг 1: Фикстура и тесты**

| Тест | Что доказывает |
|---|---|
| `a_workbook_is_recognised_by_what_is_inside_it` | опознание по содержимому |
| `an_unrecognised_workbook_is_an_error_not_a_guess` | `Unrecognised`, а не случайный парсер |
| `two_parsers_claiming_one_workbook_is_an_error` | `Ambiguous` |
| `the_builtin_registry_recognises_nothing_until_parsers_arrive` | пустой реестр честен |
| `a_real_xlsx_opens_into_sheets_and_cells` | обвязка `calamine` работает |
| `a_date_cell_arrives_as_a_date_and_not_as_a_number` | стиль даты прочитан |
| `an_unreadable_stream_is_a_typed_error` | не паника |
| `a_quarantined_row_does_not_cancel_the_document` | §11 |
| `an_unparsed_row_does_not_cancel_the_document` | §10.1 |
| `an_absent_control_section_never_becomes_a_zero` | §4.9 |
| `present_control_sections_become_claims_of_the_right_dimension` | §10.3 |
| `each_parser_carries_its_own_version` | версия — часть контракта |

- [ ] **Шаг 2: Реализация** — `report/workbook.rs` (чтение и типы ячеек), `report/mod.rs` (контракт, реестр, результат), `report/sections.rs` (контрольные секции в `ControlClaim`).

- [ ] **Шаг 3: Коммит**

```bash
nix develop -c cargo test -p iaam-ingest 2>&1 | tail -15
git add crates/iaam-ingest scripts
git commit -m "feat(ingest): реестр парсеров отчётов и контрольные секции (iaam-023)"
```

---

## Задача 15: Парсер отчёта Т-Инвестиций

**Files:**
- Create: `crates/iaam-ingest/src/report/tinkoff.rs`
- Create: `tests/fixtures/reports/tinkoff-synthetic.xlsx`
- Modify: `tests/fixtures/MANIFEST.sha256`
- Test: `crates/iaam-ingest/tests/report_tinkoff.rs`

**Acceptance Criteria:**
- Фикстура **синтетическая, но по структуре реального отчёта**: листы, заголовки и порядок колонок повторяют выгрузку, суммы и бумаги вымышлены
- Ожидаемые значения теста посчитаны вручную из фикстуры, а не сняты с вывода парсера (§15.5)
- Разбираются сделки, зачисления и списания, комиссии, купоны и дивиденды, НКД в сделке
- Контрольные секции превращаются в утверждения; интервал берётся из шапки отчёта
- Строка РЕПО уходит в карантин с причиной `Repo` и не применяется к лотам (§11)
- Фикстура заморожена: правка её ради зелёного теста запрещена заслоном

- [ ] **Шаг 1: Собрать фикстуру**

Постройте `tests/fixtures/reports/tinkoff-synthetic.xlsx` по структуре
реального отчёта владельца: те же листы и заголовки, вымышленные
суммы. Внутри — не меньше: две сделки (одна с НКД), пополнение,
вывод, комиссия, купон, одна строка РЕПО, полный набор контрольных
секций. Посчитайте ожидаемые остатки и обороты **вручную** и
запишите их в тест константами.

Внесите файл в `MANIFEST.sha256` — с этого момента он заморожен.

- [ ] **Шаг 2: Тест, потом парсер**

Тест разбирает фикстуру и сверяет: число строк с исходом, суммы
операций, интервал отчёта, набор утверждений, строку в карантине.
Затем пишется `tinkoff.rs` до зелёного.

- [ ] **Шаг 3: Коммит**

```bash
nix develop -c cargo test -p iaam-ingest --test report_tinkoff 2>&1 | tail -10
nix develop -c ./scripts/check-fixtures.sh
git add crates/iaam-ingest tests/fixtures
git commit -m "feat(ingest): парсер брокерского отчёта Т-Инвестиций (iaam-023)"
```

---

## Задача 16: Парсер отчёта Финама

**Files:**
- Create: `crates/iaam-ingest/src/report/finam.rs`
- Create: `tests/fixtures/reports/finam-synthetic.xls`
- Modify: `tests/fixtures/MANIFEST.sha256`
- Test: `crates/iaam-ingest/tests/report_finam.rs`

**Acceptance Criteria:**
- Те же критерии, что у задачи 15, для формата и структуры Финама
- **Версия парсера отличается** от версии парсера Т-Инвестиций: это разные коды разбора, и общая версия сделала бы их «одним каналом» в смысле §10.3
- Общего с `tinkoff.rs` кода разбора нет; общими остаются только типы результата
- Отчёт Финама, поданный как отчёт Т-Инвестиций, не разбирается молча: `recognises` отвергает его

- [ ] **Шаги 1–3:** зеркально задаче 15.

```bash
git commit -m "feat(ingest): парсер брокерского отчёта Финама (iaam-023)"
```

---

## Задача 17: `iaam-broker` — порт, доступ к брокеру и шифрование токена

**Files:**
- Create: `crates/iaam-broker/{Cargo.toml,src/lib.rs,src/credentials.rs}`
- Create: `crates/iaam-store/migrations/0003_broker_access.sql`, `crates/iaam-store/src/broker_access.rs`
- Modify: `crates/iaam-app/src/ports.rs` — порт `BrokerChannel`
- Modify: `Cargo.toml`, `scripts/check-architecture.sh`, `deny.toml` при необходимости
- Test: `crates/iaam-broker/tests/credentials.rs`

**Interfaces:**
- Produces: `trait BrokerChannel: Send + Sync { async fn fetch_operations(&self, account, from, to) -> Result<Vec<SubmittedOperation>, BrokerError>; async fn fetch_portfolio(&self, account, at) -> Result<Vec<ControlClaim>, BrokerError>; fn channel(&self) -> SourceChannel; }`; `SealedToken { ciphertext, nonce }`; `seal(&Key, &str) -> SealedToken`, `open(&Key, &SealedToken) -> Result<Zeroizing<String>, CryptoError>`; `Key::from_env(&str) -> Result<Key, CryptoError>`.

**Acceptance Criteria:**
- Токен хранится **только** шифротекстом; ключ берётся вне базы (переменная окружения или файл, путь — из конфигурации)
- Утечка файла БД не даёт токена: тест открывает базу и убеждается, что подстроки токена в ней нет
- Токен не попадает ни в `Debug`, ни в лог, ни в ответ API: `Debug` для `SealedToken` и для расшифрованного значения печатает заглушку — отдельный тест
- Расшифрованный токен живёт в `Zeroizing` и не копируется в `String` без нужды
- У брокера запрашивается только доступ на чтение; область прав записывается рядом с доступом и проверяется перед вызовом
- Новая крейта внесена в `scripts/check-architecture.sh`: `iaam-broker` знает ядро, но не приложение и не транспорт

- [ ] **Шаг 1: Тест на то, что токена нет в базе**

```rust
#[test]
fn a_leaked_database_file_does_not_leak_the_token() {
    // §14 буквально: утечка файла БД не должна давать доступ к
    // брокерскому счёту. Проверяется не «мы вызвали шифрование»,
    // а отсутствие подстроки в байтах файла — единственная форма
    // этой проверки, которую нельзя пройти случайно.
}

#[test]
fn debug_never_prints_the_secret() {
    // Секрет утекает в лог через Debug чаще, чем через ответ API.
}
```

- [ ] **Шаг 2: Реализация**

Шифрование — `chacha20poly1305` (чистый Rust, без C-зависимостей),
случайный nonce на запись, ключ 32 байта из окружения в base64.
Ключ **не** хранится в базе и не попадает в бандл (`bundle.rs`):
архив с ключом внутри не является архивом.

- [ ] **Шаг 3: Коммит**

```bash
nix develop -c cargo test -p iaam-broker -p iaam-store 2>&1 | tail -15
nix develop -c ./scripts/check-architecture.sh
nix develop -c cargo deny check
git add crates/iaam-broker crates/iaam-store Cargo.toml scripts/check-architecture.sh
git commit -m "feat(broker): порт канала брокера и шифрование доступа (iaam-023)"
```

---

## Задача 18: Клиент T-Invest API

**Files:**
- Create: `crates/iaam-broker/src/tinkoff.rs`
- Create: `tests/fixtures/api/tinkoff-operations.json`, `tests/fixtures/api/tinkoff-portfolio.json`
- Test: `crates/iaam-broker/tests/tinkoff_mapping.rs`

**Acceptance Criteria:**
- Операции и портфель получаются по REST-шлюзу; ответы разбираются **своим** кодом, не пересекающимся с парсером XLSX
- Версия разбора канала API отличается от версии парсера отчёта — иначе основание 3 никогда не даст `independent`
- Портфель превращается в контрольные утверждения на дату запроса: денежные остатки и количества по инструментам
- Разбор проверяется на замороженных образцах ответов; ожидаемые значения выписаны из образца вручную
- Сетевые отказы, ограничение частоты и просроченный токен — типизированные ошибки; частичный ответ не записывается как полный
- Токен не попадает в сообщение об ошибке

- [ ] **Шаг 1: Заморозить образцы ответов**

Возьмите реальные ответы, замените идентификаторы и суммы на
вымышленные, сохраните в `tests/fixtures/api/`, внесите в
`MANIFEST.sha256`.

- [ ] **Шаг 2: Тест разбора, потом клиент**

Тест разбирает образцы в `SubmittedOperation` и `ControlClaim` без
сети. Сам HTTP-вызов отделён от разбора: разбор — чистая функция от
тела ответа, и именно она покрыта тестами.

- [ ] **Шаг 3: Коммит**

```bash
git commit -m "feat(broker): клиент T-Invest API как независимый канал (iaam-023)"
```

---

## Задача 19: Клиент Finam API

**Files:**
- Create: `crates/iaam-broker/src/finam.rs`
- Create: `tests/fixtures/api/finam-transactions.json`, `tests/fixtures/api/finam-portfolio.json`
- Test: `crates/iaam-broker/tests/finam_mapping.rs`

**Acceptance Criteria:** те же, что у задачи 18, плюс: версия разбора
отличается и от отчёта Финама, и от канала Т-Инвестиций; общего кода
разбора с `tinkoff.rs` нет.

```bash
git commit -m "feat(broker): клиент Finam API как независимый канал (iaam-023)"
```

---

## Задача 20: Сведение каналов и синхронизация

**Files:**
- Create: `crates/iaam-app/src/scenarios/sync.rs`
- Modify: `crates/iaam-app/src/lib.rs`, `crates/iaam-app/src/ports.rs`
- Test: `crates/iaam-app/tests/sync.rs`

**Interfaces:**
- Produces: `sync_broker(services, principal, broker, account, from, to) -> Result<SyncOutcome, AppError>`; `SyncOutcome { recorded: Vec<Verdict>, duplicates: usize, assertions: usize }`.

**Acceptance Criteria:**
- Одна и та же сделка, пришедшая из API и из отчёта, распознаётся как **один факт** дедупликацией и **не** удваивает позицию
- Она же порождает основание 3 и повышает статус до `accepted_independent` — оба следствия проверяются одним тестом
- Синхронизация идемпотентна: повторный запуск за тот же интервал не создаёт новых событий
- Частичная выгрузка не выдаётся за полную: интервал, который брокер отдал не полностью, не порождает контрольных утверждений
- Отказ одного брокера не мешает синхронизации другого

- [ ] **Шаг 1: Тест сведения**

Ключевой тест: журнал, где март пришёл и отчётом, и API. Проверяется,
что число событий сделки равно одному, а `status_for(... Cash)` равен
`AcceptedIndependent`. Это единственное место, где обе половины
основания 3 видны вместе, и потому оно проверяется вместе.

- [ ] **Шаг 2: Реализация и коммит**

```bash
nix develop -c cargo test -p iaam-app 2>&1 | tail -15
git commit -m "feat(app): синхронизация с брокером и сведение двух каналов (iaam-023)"
```

---

## Задача 21: `iaam-app` и `iaam-server` — маршруты, статусы, правила

**Files:**
- Create: `crates/iaam-app/src/scenarios/documents.rs`, `crates/iaam-app/src/scenarios/reconciliation.rs`
- Modify: `crates/iaam-server/src/{dto.rs,routes.rs,openapi.rs}`
- Modify: `scripts/check-architecture.sh` — заслон «каналы не делят разбор»
- Test: `crates/iaam-server/tests/contract.rs` (дополняется)

**Interfaces:**
- Маршруты: `POST /v1/documents` (загрузка отчёта, построчный вердикт), `POST /v1/documents/{id}/reparse`, `GET /v1/reconciliation?account&from&to`, `POST /v1/reconciliation/balance` (ответ владельца на `needs_reconciliation`), `GET|POST|DELETE /v1/classification-rules`, `POST /v1/brokers/{broker}/sync`, `PUT /v1/brokers/{broker}/access`.

**Acceptance Criteria:**
- Загрузка отчёта возвращает **вердикт на строку**, а не один статус на документ (§10.1)
- Ответ владельца на `needs_reconciliation` записывается контрольным утверждением и повышает только `cash` и `positions` (§10.4)
- Статусы сверки отдаются с основаниями: владелец видит, **почему** цифре можно верить
- `dataQuality` в отчёте о доходности содержит `navCoverage` и материальные проблемы
- Правила классификации видимы и редактируемы; правка запускает пересчёт истории
- OpenAPI описывает все новые маршруты и коды; контрактные тесты покрывают каждый вердикт
- Секрет доступа к брокеру не возвращается ни одним маршрутом — контрактный тест на это
- Заслон архитектуры проверяет, что `iaam-broker` и `report/*` не делят функций разбора

- [ ] **Шаг 1: Заслон независимости каналов**

Допишите в `scripts/check-architecture.sh`:

```bash
# --- Каналы получения данных не делят код разбора (§10.3) ---
# Независимость канала — это не декларация, а свойство кода. Если
# клиент API начнёт звать функцию парсера отчёта, общая ошибка исказит
# обе стороны сверки, и уровень accepted_independent станет ложью,
# которую ни один тест не поймает: тесты сверки увидят совпадение.
bad=$(grep -rn 'iaam_ingest::report' crates/iaam-broker/src 2>/dev/null || true)
if [ -n "$bad" ]; then
  err "iaam-broker использует парсер отчётов: каналы обязаны быть независимы (§10.3)
$bad"
fi
```

Проверьте заслон падением: временно добавьте такой вызов и убедитесь,
что заслон краснеет.

- [ ] **Шаг 2: Сценарии, DTO, маршруты, OpenAPI**

Оболочка **не считает** (§3.1): сценарии собирают срез журнала, зовут
`ReconciliationLedger::build_with` и `assess`, сериализуют результат.
Ни одной арифметики над деньгами в `iaam-app` и `iaam-server`.

- [ ] **Шаг 3: Контрактные тесты и коммит**

```bash
nix develop -c cargo test --workspace 2>&1 | tail -20
nix develop -c ./scripts/check-architecture.sh
git commit -m "feat(server): маршруты приёмки, статусы сверки и правила классификации (iaam-023)"
```

---

# Часть C — сдача

## Задача 22: Золотые сценарии E2 и приёмка эпика

**Files:**
- Create: `crates/iaam-core/tests/golden_stage2.rs`
- Modify: `tests/fixtures/MANIFEST.sha256`

**Acceptance Criteria (это и есть критерии приёмки эпика `iaam-023`):**
- **Импорт реального брокерского отчёта владельца проходит построчно** — на синтетике, повторяющей структуру реального отчёта; ни одна строка не отменяет документ
- **Сверка даёт корректный уровень достоверности** — по одному отчёту `internal`, по двум каналам `independent`, при расхождении `discrepant`
- **Компенсирующая ошибка парсера не повышает статус выше `accepted_internal`** — метаморфный тест задачи 9 включён в набор приёмки
- Непокрытый периметр обработан построчно: маржа, РЕПО и отрицательный кэш дают исходы §11, а остальные счета продолжают считаться
- Правило классификации, изменённое после импорта, пересчитывает историю, и итоговые остатки совпадают с посчитанными вручную

- [ ] **Шаг 1: Один сценарий из конца в конец**

Отчёт → построчные вердикты → сверка → статусы → `dataQuality` →
второй канал → повышение до `independent` → испорченная цифра →
`discrepant`. Все ожидаемые значения посчитаны вручную.

- [ ] **Шаг 2: Прогнать всё**

```bash
nix develop -c cargo nextest run --workspace --all-features
nix develop -c ./scripts/check-mutants.sh
nix develop -c ./scripts/check-fixtures.sh
nix develop -c cargo deny check
git commit -m "test: золотые сценарии E2 и приёмка эпика (iaam-023)"
```

---

## Задача 23: Документация, скилл агента и закрытие эпика

**Files:**
- Modify: `README.md`, `docs/agent-skill/SKILL.md`, `docs/deployment.md`, `docs/irreversible-core.md`
- Create: `docs/decisions/` — ADR по решениям, которые владелец согласился зафиксировать

**Acceptance Criteria:**
- README честно описывает, что система умеет **теперь**: два канала, сверка, уровни достоверности, — и что по-прежнему не умеет
- `SKILL.md` описывает новые маршруты, все восемь вердиктов и то, как агент обязан вести себя при `needs_classification` и `needs_reconciliation`
- `deployment.md` описывает ключ шифрования брокерских токенов: где хранится, как ротируется, что происходит при потере
- Заведены биды на всё отложенное: депозитарный отчёт, справка налогового агента, планировщик синхронизации
- Эпик `iaam-023` закрыт с перечислением того, что вошло, и того, что вынесено

- [ ] **Шаг 1: Документация**
- [ ] **Шаг 2: Биды на отложенное** (`bd create ... --parent iaam-3ju` для каналов E7)
- [ ] **Шаг 3: Закрытие**

```bash
bd close iaam-023 --reason "E2: два канала, многомерная сверка, уровни достоверности, классификация, §11"
git commit -m "docs: документация E2 и закрытие эпика (iaam-023)"
```

---

## Самопроверка плана

**Покрытие спеки.** Каждое требование §10 и §11 отнесено к задаче:

| Требование | Задача |
|---|---|
| §10.1 единый вход, построчный разбор, версия парсера в provenance | 14, 15, 16, 21 |
| §10.2 отказ от ручного подтверждения операций | 3 (вердикты), 13 (вопрос только о классификации) |
| §10.3 многомерная сверка, три уровня, восемь оснований, независимость | 1–5 |
| §10.4 шесть вердиктов, правила классификации, пересчёт истории | 3, 13, 21 |
| §10.4 ограничение `needs_reconciliation` (только cash и positions) | 4 (`OwnerStatedBalance`), 21 |
| §10.5 блок качества данных, `navCoverage`, материальные проблемы | 7 |
| §10.6 иерархия дедупликации из пяти уровней | 12 |
| §10.7 частичная история, набор утверждений восстановленного начала | 8, 7 |
| §11 маржа, РЕПО, отрицательный кэш, `not_computable` за период | 6, 14, 21 |
| §14 шифрование брокерского токена, только чтение | 17 |
| §15.4 независимые эталоны, §15.6 метаморфные, §15.9 золотые | 9, 22 |

**Отложенное** перечислено в «Сознательных сокращениях» и оформляется
бидами в задаче 23. Ни одно требование не осталось без адреса.

**Согласованность типов.** `Dimension`, `ConfidenceLevel`,
`DimensionStatus`, `ClaimOutcome`, `SourceChannel`, `Evidence`,
`ReconciliationLedger`, `PerimeterExceptions` объявлены по одному разу
и используются под теми же именами во всех последующих задачах.
`ReconciliationLedger::build` появляется в задаче 5 и расширяется до
`build_with` в задаче 6 — это единственное место, где сигнатура
задачи меняется позже, и оно названо явно.
