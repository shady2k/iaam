# E1 (часть 2): проекции, XIRR, хранилище и REST

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent iaam-1fk`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.
>
> **Эпик:** E1 = `iaam-1fk` (переоткрыт после первого плана)
> **Спека:** `.internal/specs/2026-08-22-investment-tracker-design.md`
> **Первый план:** `.internal/plans/2026-08-22-e0-e1-foundation-and-core-ledger.md` — выполнен целиком, схема журнала заморожена (`docs/irreversible-core.md`)

**Goal:** По одному счёту с ручным вводом система отвечает на три вопроса — сколько внесено, сколько выведено, какова доходность XIRR **до налога** — через REST API, которым пользуется внешний агент, и не выдаёт числа, за которые не может отвечать.

**Границы плана.** Всё, что первый план вынес во второй: проекции лотов, позиций и потоков со снимками `project`/`advance`; классификация границы контура в денежные потоки; XIRR с явной политикой решателя; инварианты §15.2 как типизированная ошибка; `iaam-store` на SQLite; `iaam-app` с портами; `iaam-server` с axum, аутентификацией и OpenAPI через `utoipa`; ручной ввод и CSV; скилл для внешнего агента. Плюс три незакрытых вопроса первого плана (задача 19).

**Явно вне плана** (обоснование — в разделе «Сознательные сокращения»): налоги и налоговые лоты (E5), рыночные данные и MOEX (E3), многомерная сверка и уровни достоверности (E2), TWR и ряд NAV (E4/E6), `iaam-cli`, веб-UI (E8).

**Architecture:** Функциональное ядро, императивная оболочка (§3.1). Ядро получает готовый срез журнала и возвращает проекцию вместе с проверенными инвариантами; снимки и кэш хранит оболочка. Транспорт (`iaam-server`) не знает про адаптеры: конкретные реализации собираются в `iaam-bootstrap`. Журнал append-only не только по дисциплине кода, но и по триггерам SQLite.

**Tech Stack:** Rust 1.98 (закреплён Nix-флейком), `rust_decimal`, `sha2`, `rusqlite` (bundled), `tokio`, `axum` 0.8, `utoipa` 5 + `utoipa-axum`, `async-trait`, `tower-http`, `tower_governor`, `csv`, `serde`, `thiserror`, `tracing`, `proptest`, `insta`, `trybuild`, `cargo-nextest`, `cargo-mutants`, `cargo-deny`, `cargo-llvm-cov`.

---

## ⚠️ Статус проверки кода

**Весь код этого плана скомпилирован и исполнен, а затем прошёл состязательное ревью и был исправлен.** В отличие от первого плана, тулчейн на машине, где план писался, доступен (`cargo 1.98.0`), и сеть тоже. Порядок был такой: код реализовывался в отдельной копии репозитория, проходил все заслоны, затем переносился сюда; после этого копия и план целиком ушли на ревью (раунд 5, `.internal/specs/2026-08-22-codex-review-round5-plan2.md`), и найденные дефекты были исправлены в коде и в тексте.

Фактически проверено на копии репозитория **после правок по ревью**:

- собираются и проходят тесты все семь крейт: `iaam-core`, `iaam-oracle`, `iaam-store`, `iaam-ingest`, `iaam-app`, `iaam-server`, `iaam-bootstrap`; **351 тест, ноль падений**;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — без замечаний;
- `cargo fmt --all -- --check` — без расхождений (код в плане уже отформатирован);
- `./scripts/check-architecture.sh` — пройден, включая **десять** заслонов: два новых (оболочка не считает деньги; один механизм асинхронных трейтов) и переписанные 5 и 8. Отдельно проверено исполнением, что каждый изменённый заслон **по-прежнему ловит** нарушение, а не просто проходит;
- `./scripts/check-fixtures.sh` — пройден;
- `cargo deny check` — `advisories ok, bans ok, licenses ok, sources ok`;
- **`./scripts/check-mutants.sh` — пройден полностью: шестнадцать модулей, 461 мутант, ноль выживших.** Прогон занял около двадцати минут на машине разработчика;
- **приёмочный критерий эпика проходит через REST API целиком**: контрактный тест поднимает сервер, отправляет пять операций и получает отчёт, в котором ставка совпадает с независимым эталоном на Python до 1e-7;
- ставка XIRR сверена с эталоном, реализованным другим методом и на другом языке (бисекция на десятичной арифметике 50 знаков) — расхождение около 1e-13;
- версии внешних крейт сверены с реестром: `axum 0.8.9`, `tower-http 0.7`, `tokio 1.53`, `rusqlite 0.40`, `utoipa 5.5`, `utoipa-axum 0.2`, `ciborium 0.2`, `csv 1.4`, `sha2 0.11`, `trybuild 1.0`, `rand 0.10`, `insta 1.48`, `proptest 1.11`.

**Что НЕ проверялось и остаётся риском исполнителя:**

- порог покрытия по диффу (`diff-cover --fail-under=90`) не проверялся;
- **стоимость мутационного прогона в CI не измерена.** На машине разработчика полный прогон занимает около двадцати минут при таймауте `mutants.yml` в шестьдесят; на более медленном раннере запас может оказаться тоньше, чем кажется. **Решение владельца (2026-08-23): полный прогон остаётся на каждом PR.** Отбор по диффу отклонён: регрессия в незатронутом модуле находилась бы с задержкой до суток, а мутационный заслон нужен именно там, где автор правки не смотрит. Если прогон упрётся в таймаут, сокращать надо время одного мутанта (`cargo-nextest`), а не охват; уменьшение охвата требует нового решения владельца;

- живой запуск двоичного файла с базой на диске не выполнялся: проверка велась на базе в памяти. Ручной прогон вынесен отдельным шагом задачи 17;
- содержимое документов задачи 20 (скилл агента, инструкция по развёртыванию) написано, но не сверялось с поднятым сервисом;
- **конкурентная запись проверена рассуждением и схемой, а не нагрузочным тестом.** Транзакция с немедленным захватом и уникальный индекс `(owner, effective_date, sequence)` закрывают гонку выдачи порядкового номера; теста на два одновременных запроса нет, и его стоит написать при первом же появлении второго писателя.

Правило прежнее: **любое расхождение исправляется в пользу компилятора, а не в пользу плана.** Но если код компилируется, а тест приходится ослабить, чтобы он прошёл, — останавливайтесь и эскалируйте (§15.7 спеки).

---

## Что изменило ревью

Состязательное ревью (раунд 5) нашло в первой редакции этого плана пять дефектов, каждый из которых давал неверную денежную цифру или необратимую порчу журнала **при полностью зелёной сборке**. Все исправлены; подробности — в задачах, ниже сводка.

| Дефект первой редакции | Чем это грозило | Где исправлено |
|---|---|---|
| `advance` принимал «пачку новых событий», и событие, добавленное задним числом до границы снимка, молча исчезало из расчёта | Правдоподобные, но неверные остатки, лоты и доходность | Задача 5: срез передаётся целиком, снимок несёт отпечаток свёрнутого префикса |
| Отпечаток состояния перечислял поля вручную и не покрывал реализованный результат, стоимости и версию правила | Подменённое состояние проходило проверку | Задача 5: отпечаток по канонической сериализации целиком |
| Единственность корня XIRR «доказывалась» подсчётом интервалов на сетке с шагом около десяти процентных пунктов | Произвольно выбранная ставка вместо обязательного отказа | Задача 6: правило знаков; сетка только ищет интервал |
| Контрольная сумма архивного бандла покрывала только идентификаторы событий | Подменённая сумма проходила, повреждённый архив выглядел целым | Задача 18: сумма по всему содержимому, импорт одной транзакцией |
| Структурная проверка не сверяла ногу с событием и не проверяла знаки величин | Противоречивый факт навсегда в append-only журнале | Задача 4: сверка инструмента, счёта, количества и знаков |

**Мутационный заслон, запущенный после правок, дал 86 выживших мутантов** — 86 мест, где код можно испортить незамеченно. Все закрыты тестами; **выживших не осталось ни одного**. Два из них оказались настоящими дефектами решателя: метод Ньютона вырождался в бисекцию (тридцать семь итераций вместо единиц), и два допуска конфликтовали так, что ослабление одного давало отказ. Уточнение корня переписано на метод Илинойса, допуск оставлен один. Последние три мутанта закрыты не объявлением эквивалентности, а выносом недостижимой проверки в отдельную функцию, которую можно вызвать напрямую. Подробности — в задаче 6 и в документе ревью.

Плюс семь исправлений меньшего масштаба: граница владельца в справочниках, контурах и снимках; запрет `DELETE` на составе контура; транзакционная выдача порядкового номера; предел памяти ограничителя частоты и отказ от записи неизвестных токенов в базу; удаление фиктивного «постоянного по времени» сравнения; версия схемы события 2; заслоны на денежную арифметику в оболочке и на единственный механизм асинхронных трейтов.

**Одно замечание ревью отклонено.** Ревьюер предложил считать ручной ввод `Confidence::Estimated`, потому что независимого подтверждения у него нет. Это смешение двух разных вещей: `Confidence` (§4.9) описывает уверенность в **значении** — владелец, вводящий пополнение вручную, знает его сумму, — а отсутствие сверки (§10.3) является утверждением о счёте и интервале и полем события не является. Неверным был комментарий рядом с кодом, а не сам код; комментарий исправлен, поле `Coverage::unconfirmed_events` переименовано в `estimated_events`, а смысл `unconfirmed_share` в отчёте описан явно.

---

## Global Constraints

Действуют для **каждой** задачи. Нарушение любого — основание отклонить задачу на ревью. Первые тринадцать строк перенесены из первого плана без изменений: они не перестали действовать оттого, что план закончился.

| Правило | Источник |
|---|---|
| `unsafe` запрещён во всех крейтах первой стороны: таблица `[workspace.lints.rust]` **плюс** `[lints] workspace = true` в каждой крейте, включая новые | §15.1 |
| `f64` запрещён в доменных величинах. Допустим только в объявленных заслоном файлах приближённого режима с документированной границей погрешности | §6.6, §15.1 |
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
| **Логика конструктора не живёт в `new`.** `cargo-mutants` молча пропускает функции с именем `new`: конструктор с проверками делегирует приватной функции (`from_parts`, `from_ratio`, …) | §15.7 |
| **Литералы вида `100_00` запрещены** — `clippy::inconsistent_digit_grouping` входит в `all`, а `all = deny`. Пишите `10_000` | §15.1 |
| Коммит после каждой задачи, с идентификатором бида в сообщении | — |

Добавляется этим планом:

| Правило | Источник |
|---|---|
| **Все команды выполняются внутри dev-shell:** `nix develop -c <команда>`. Вне его нет ни `cargo`, ни `sqlite`, ни `jq` | E0/задача 1 |
| **Приближённая величина не входит ни в одну денежную сумму.** XIRR, NPV и день-фактор живут в приближённом режиме и попадают в отчёт как отдельное поле рядом с политикой решателя | §6.6 |
| **Пересчёт денег через курс даёт расчётную величину, а не проведённую сумму.** Результат конвертации — `Dec`, никогда не `PostedMinor` | §3.4 |
| **Ядро не решает, чего оно не знает.** Отсутствие цены, курса или налоговой стоимости даёт `Computed::NotComputable { reason }`, а не подстановку нуля и не «примерно» | §5.4, §10.7 |
| **Оболочка не считает.** Ни `iaam-app`, ни `iaam-server`, ни `iaam-ingest` не содержат арифметики над деньгами: они собирают срез, зовут ядро и сериализуют результат. Проверяется заслоном (задача 15) | §3.1, §13 |
| **Секрет не попадает ни в лог, ни в ответ, ни в базу в открытом виде** | §14 |
| Новая крейта добавляется вместе со строкой в `scripts/check-architecture.sh` — иначе она вне заслонов | §3.2 |

---

## Сознательные сокращения

Требования спеки, которые этот план **не** закрывает. Ни одно не забыто — каждое отнесено к своему эпику, и там, где сокращение видно снаружи, оно видно и пользователю API.

| Требование спеки | Куда отнесено | Что делает этап 1 вместо этого |
|---|---|---|
| Налоги, налоговые базы, ЛДВ, ИИС (§9) | E5 (`iaam-c55`) | Отчёт называется «XIRR **до налога**» в самом поле ответа, а не в сноске |
| Рыночные данные, MOEX, ЦБ (§12) | E3 (`iaam-d8b`) | Цена приходит событием `Valuation` с provenance и флагом качества (задача 7) |
| Многомерная сверка, уровни достоверности (§10.3) | E2 (`iaam-023`) | Всё, введённое руками, имеет уровень `provisional`; `dataQuality` отдаётся с первого дня, но с одним уровнем |
| TWR, ряд NAV, бенчмарки (§6.2, §5) | E4/E6 | Только XIRR; `liquidation_value` вычисляется на дату отчёта, `contractual_hold_value` — нет |
| Тождество результата §6.3 целиком | E4 (`iaam-xtn`) | Проверяется его денежная часть: остатки, лоты, потоки. Разложение `RP/UP/FX/Interaction` появится вместе с оценкой по рынку |
| Облигации и вклады целиком (§7, §8) | E3 | НКД сохраняется в событии сделки (уже в схеме), но амортизация и график начислений не проецируются |
| Построчный импорт отчётов брокера (§10.1) | E2 | Только CSV собственного формата и ручной ввод — оба через приёмку с вердиктами |
| `iaam-cli` (§3.2) | Отдельный бид, создаётся в задаче 20 | Всё делается через REST; локальные команды не нужны, пока нет второго потребителя |
| Параллельные сценарии через `ScenarioInput` (§3.1) | E5/E6 | Сценарии нужны налоговому планированию и флоатерам; пока считается один вариант. Базовая проекция уже неизменяема, поэтому наложение добавится без смены интерфейсов |
| `value_range` и ряд NAV (§3.1) | E4/E6 | Стоимость считается на одну дату отчёта |
| Флаг граничного эффекта `crossesUnpostedAccrual` (§5.2) | E3 | Начислений, которые он маркирует, ещё нет: вклады и НКД появляются в E3 |
| Политика оценки по классам активов целиком (§5.4) | E3 | Флаг качества у каждой цены есть с первого дня; выбор правила по классу требует рыночных данных |
| Эталоны против реальности — реальные отчёты брокера (§15.5) | E2 | Отчётов брокера этап 1 не разбирает. Эталонами служат независимая реализация на Python и посчитанные вручную числа |
| ZCYC, DCF, календари из `invest_calculator` (§17.5) | E3 | Из математики перенесён XIRR; остальное нужно облигациям |
| API брокеров, шифрование брокерских токенов (§14) | E7 (`iaam-3ju`) | Брокерских токенов ещё нет. Агентские токены — есть, и они хранятся хешем с первого дня |

---

## File Structure

```
crates/
  iaam-core/                       СУЩЕСТВУЕТ, дополняется
    src/lib.rs                     + pub mod projection; returns; valuation;
    src/money.rs                   + Money::to_calc_dec               задача 4
    src/numeric/decimal.rs         + Dec::checked_add                 задача 1
    src/numeric/decimal.rs         + остальная арифметика Dec         задача 4
    src/event/kind.rs              + вариант Valuation (§5.4)         задача 4
    src/event/mod.rs               + форма Valuation, конструктор для тестов
    src/numeric/xirr.rs            НОВЫЙ  решатель ставки             задача 6
    src/projection/balances.rs     НОВЫЙ  остатки и позиции по ногам  задача 1
    src/projection/lots.rs         НОВЫЙ  книга лотов                 задача 2
    src/projection/flows.rs        НОВЫЙ  потоки границы контура      задача 3
    src/valuation.rs               НОВЫЙ  цены, качество, курсы       задача 4
    src/projection/state.rs        НОВЫЙ  LedgerState и отпечаток     задача 5
    src/projection/invariants.rs   НОВЫЙ  инварианты §15.2            задача 5
    src/projection/mod.rs          НОВЫЙ  Snapshot, project, advance  задача 5
    src/returns/mod.rs             НОВЫЙ  Computed<T>, dataQuality    задача 8
    src/returns/xirr.rs            НОВЫЙ  доменная обёртка решателя   задача 8
    tests/xirr_fixtures.rs         НОВЫЙ  сверка с эталоном           задача 7
    tests/acceptance_stage1.rs     НОВЫЙ  приёмка ядра                задача 9
    tests/properties.rs            + свойства проекций                задача 9
    tests/xirr_solver.rs           НОВЫЙ  отказы и единственность корня задача 6
    tests/returns_boundaries.rs    НОВЫЙ  границы дат отчёта          задача 8
    tests/metamorphic.rs           НОВЫЙ  метаморфные тесты §15.6     задача 9
    tests/golden_stage1.rs         НОВЫЙ  золотые сценарии §15.9      задача 19
    tests/serde_roundtrip.rs       НОВЫЙ  round-trip журнала          задача 19
    tests/ui.rs, tests/ui/*.rs     НОВЫЙ  trybuild                    задача 19
  iaam-store/                      НОВАЯ                              задачи 10, 11, 18
    migrations/0001_initial.sql    схема с триггерами append-only
    src/lib.rs, src/schema.rs, src/events.rs
    src/snapshots.rs, src/reference.rs, src/tokens.rs, src/bundle.rs
    tests/journal.rs, tests/snapshots_and_reference.rs, tests/bundle.rs
  iaam-ingest/                     НОВАЯ                              задачи 12, 13
    src/lib.rs, src/verdict.rs, src/operation.rs, src/csv_source.rs
    tests/normalization.rs, tests/csv_rows.rs
  iaam-app/                        НОВАЯ                              задача 14
    src/lib.rs, src/error.rs, src/ports.rs
    src/adapters/mod.rs, src/adapters/sqlite.rs
    src/scenarios/mod.rs, src/scenarios/ingest.rs, src/scenarios/reports.rs
  iaam-server/                     НОВАЯ                              задачи 15, 16
    src/lib.rs, src/error.rs, src/dto.rs, src/routes.rs, src/openapi.rs
    src/auth.rs, src/rate_limit.rs
    tests/contract.rs, tests/snapshots/                                задача 17
  iaam-bootstrap/                  НОВАЯ  точка сборки                задача 17
    src/main.rs, src/config.rs
docs/
  irreversible-core.md             обновляется                        задачи 4, 19
  deployment.md                    НОВЫЙ                              задача 20
  agent-skill/SKILL.md             НОВЫЙ  скилл внешнего агента (§13) задача 20
scripts/
  check-architecture.sh            дополняется                        задачи 6, 17
  check-mutants.sh                 дополняется                        задача 9
  gen-xirr-fixtures.py             НОВЫЙ  эталон XIRR на Python       задача 7
tests/fixtures/
  xirr_cases.json, MANIFEST.sha256                                    задача 7
deny.toml                          точечное исключение advisory       задача 17
Cargo.toml                         пять новых крейт в members
```

**`iaam-oracle` в этом плане не растёт.** Эталон списания лотов остаётся как есть; эталоном для XIRR служит независимая реализация на Python с замороженными фикстурами, а не вторая реализация на Rust — вторая реализация на том же языке, делящая с продакшеном типы и структуру серии, поймала бы опечатку, но не ошибку в модели дисконтирования (§15.4).

**Почему проекция разбита на пять файлов, а не лежит одним модулем.** Лоты, остатки и потоки — три независимых способа читать один и тот же журнал, и инвариант «сумма лотов равна позиции» имеет смысл ровно потому, что количество считается двумя из них независимо. В одном файле с общими вспомогательными функциями эта независимость исчезает незаметно: достаточно, чтобы обе стороны позвали один хелпер, и проверка станет тавтологией (§15.4).

---

## Граф задач

Двадцать задач, три части. Каждая задача — один бид (`bd create -t task --parent iaam-1fk`), каждая заканчивается зелёной сборкой и коммитом.

```
Часть A — ядро
  4 оценка и Valuation ─┬─→ 2 книга лотов ──┐
                        └─→ 3 потоки контура┤
  1 остатки и позиции ───────────────────────┼─→ 5 снимок, project/advance, инварианты ─┐
                                             ┘                                          │
  6 решатель XIRR → 7 эталон XIRR ──────────────────────────────────────────────────────┼─→ 8 отчёт → 9 приёмка ядра
                                                                                        ┘
Часть B — оболочка
  10 store: журнал → 11 store: снимки ─┐
  12 ingest: операции → 13 ingest: CSV ┼─→ 14 app: порты и сценарии → 15 server: DTO и OpenAPI
                                       ┘                                    → 16 server: аутентификация
                                                                            → 17 bootstrap и контракт
Часть C — сдача
  18 архивный бандл → 19 золотые сценарии и незакрытые вопросы → 20 скилл, документация, закрытие
```

**Исправление при исполнении (2026-08-23).** Первая редакция объявляла задачи 1–4 независимыми. Это неверно: задача 4 заводит вариант `EventKind::Valuation` и арифметику `Dec`, а исчерпывающие `match` и подсчёт количеств задач 2 и 3 без них не собираются. Порядок в части A: **4 → 2, 3**; задача 1 действительно независима (после того как `Dec::checked_add` переехал в неё); задача 6 не зависит ни от чего в части A. Задача 5 — самая крупная: три файла и две группы тестов; разделять её нельзя, потому что снимок без проверки инвариантов является ровно тем, что §15.2 запрещает выдавать наружу.


# Часть A — ядро

## Задача 1: Денежные остатки и позиции

**Files:**
- Create: `crates/iaam-core/src/projection/balances.rs`
- Create: `crates/iaam-core/src/projection/mod.rs` (пока только объявления модулей)
- Modify: `crates/iaam-core/src/lib.rs` — `pub mod projection;`
- Modify: `crates/iaam-core/src/numeric/decimal.rs` — `Dec::checked_add`

> **Исправление при исполнении (2026-08-23).** Первая редакция объявляла задачи 1–4
> независимыми, но код этой задачи складывает количества через `Dec::checked_add`,
> которого в крейте нет: план заводил его в задаче 4. Расхождение исправлено
> в пользу компилятора — `checked_add` вместе с тестами переехал сюда, задача 4
> добавляет остальную арифметику `Dec`. Независимость задач 1–4 восстановлена.

**Interfaces:**
- Consumes: `event::Event`, `event::leg::{Leg, LegKind}`, `money::{Money, PostedMinor, Quantity, CurrencyCode}`, `ids::*` — всё уже существует.
- Produces: `projection::balances::{Balances, PositionKey, BalanceError}`; методы `Balances::apply(&Event)`, `cash(account, currency) -> Option<Money>`, `iter_cash()`, `iter_positions()`, `quantity_of(account, instrument) -> Result<Quantity, NumericError>`, `negative_cash()`.

**Acceptance Criteria:**
- Остатки и позиции считаются **только по ногам события**, без разбора типа события.
- Остаток, по которому не было движений, отличается от нулевого остатка (`Option`, а не ноль).
- Отрицательный денежный остаток не является ошибкой и доступен отдельным перечислением.
- Нога с количеством без инструмента даёт типизированную ошибку, а не молчаливый пропуск.

**Почему по ногам, а не по типу события.** Лоты (задача 2) считаются по типу события и правилу списания. Если бы количество бумаг приходило туда же, откуда и лоты, инвариант «сумма лотов равна позиции» проверял бы одно вычисление против самого себя (§15.4). Две независимые дороги к одному количеству — единственная причина, по которой этот инвариант что-то значит.

- [ ] **Шаг 1: Написать падающий тест**

Создайте `crates/iaam-core/src/projection/balances.rs` **только** с тестами внизу файла (реализации ещё нет) и `mod.rs` с `pub mod balances;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::provenance::{ParserVersion, Provenance, RawHash};
    use crate::event::{Confidence, Event, Relation, SCHEMA_VERSION};
    use crate::ids::{CustodyId, EventId, OwnerId, SourceId};
    use crate::money::PostedMinor;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn cash_event(account: AccountId, amount: Money) -> Event {
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 01 - 10))),
            order: EffectiveOrder::new(date!(2026 - 01 - 10), 1),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"c".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    #[test]
    fn cash_legs_accumulate_per_account_and_currency() {
        let account = AccountId::new_random();
        let mut balances = Balances::new();
        balances.apply(&cash_event(account, rub(10_000))).unwrap();
        balances.apply(&cash_event(account, rub(2_500))).unwrap();
        assert_eq!(balances.cash(account, CurrencyCode::Rub), Some(rub(12_500)));
    }

    #[test]
    fn an_account_without_movements_is_not_a_zero_balance() {
        // Разница между «движений не было» и «остаток ноль» видна
        // в отчёте о полноте данных (§10.7), поэтому она в типе.
        let balances = Balances::new();
        assert_eq!(
            balances.cash(AccountId::new_random(), CurrencyCode::Rub),
            None
        );
    }

    #[test]
    fn negative_cash_is_reported_not_hidden() {
        let account = AccountId::new_random();
        let mut balances = Balances::new();
        balances.apply(&cash_event(account, rub(-5_000))).unwrap();
        let negative: Vec<_> = balances.negative_cash().collect();
        assert_eq!(negative, vec![(account, rub(-5_000))]);
    }

    #[test]
    fn quantity_sums_across_custodies_of_the_same_account() {
        // Лоты не различают место хранения, поэтому сравнивать с ними
        // надо сумму по всем custody, а не отдельную строку позиции.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut balances = Balances::new();
        for _ in 0..2 {
            let custody = CustodyId::new_random();
            let mut event = cash_event(account, rub(1));
            event.legs = vec![Leg::security(
                account,
                custody,
                instrument,
                Quantity(crate::numeric::decimal::Dec::new(10.into())),
            )];
            balances.apply(&event).unwrap();
        }
        assert_eq!(
            balances.quantity_of(account, instrument).unwrap(),
            Quantity(crate::numeric::decimal::Dec::new(20.into()))
        );
    }

    #[test]
    fn a_quantity_leg_without_an_instrument_is_an_error() {
        let account = AccountId::new_random();
        let mut event = cash_event(account, rub(1));
        event.legs = vec![Leg {
            kind: crate::event::leg::LegKind::SecurityQuantity,
            account,
            custody: None,
            instrument: None,
            money: None,
            quantity: Some(Quantity::zero()),
        }];
        let mut balances = Balances::new();
        assert!(matches!(
            balances.apply(&event),
            Err(BalanceError::QuantityWithoutInstrument { .. })
        ));
    }
    #[test]
    fn a_position_is_addressed_by_account_custody_and_instrument() {
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let mut balances = Balances::new();
        let mut event = cash_event(account, rub(1));
        event.legs = vec![Leg::security(
            account,
            custody,
            instrument,
            Quantity(crate::numeric::decimal::Dec::new(7.into())),
        )];
        balances.apply(&event).unwrap();

        let key = PositionKey {
            account,
            custody: Some(custody),
            instrument,
        };
        assert_eq!(
            balances.position(&key),
            Some(Quantity(crate::numeric::decimal::Dec::new(7.into())))
        );
        // Другое место хранения — другая позиция, а не та же.
        assert_eq!(
            balances.position(&PositionKey {
                custody: Some(CustodyId::new_random()),
                ..key
            }),
            None
        );
        assert_eq!(balances.iter_positions().count(), 1);
    }

    #[test]
    fn a_zero_balance_is_not_a_negative_one() {
        // Граница: ноль обязательством не является, и в блок качества
        // данных попадать не должен.
        let account = AccountId::new_random();
        let mut balances = Balances::new();
        balances.apply(&cash_event(account, rub(5_000))).unwrap();
        balances.apply(&cash_event(account, rub(-5_000))).unwrap();
        assert_eq!(balances.cash(account, CurrencyCode::Rub), Some(rub(0)));
        assert_eq!(balances.negative_cash().count(), 0);
    }
}
```

- [ ] **Шаг 2: Убедиться, что тест падает**

```bash
nix develop -c cargo test -p iaam-core balances
```

Ожидается: ошибка сборки `cannot find type `Balances` in this scope`.

- [ ] **Шаг 3: Реализация**

Полный текст `crates/iaam-core/src/projection/balances.rs` (тесты из шага 1 остаются в конце файла):

```rust
//! Денежные остатки и позиции (§3.1).
//!
//! Считаются **по ногам события**, единообразно для всех типов. Лоты
//! (`super::lots`) считаются по типу события и правилу списания. Две
//! независимые дороги к одному количеству — то, что делает инвариант
//! «сумма лотов равна позиции» проверкой, а не тавтологией (§15.4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::Event;
use crate::ids::{AccountId, CustodyId, EventId, InstrumentId};
use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
use crate::numeric::NumericError;

/// Позиция определяется тройкой: счёт, место хранения, инструмент.
/// Перевод бумаг между депозитариями внутри одного брокера — реальная
/// операция, поэтому custody входит в ключ (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PositionKey {
    pub account: AccountId,
    pub custody: Option<CustodyId>,
    pub instrument: InstrumentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BalanceError {
    #[error("переполнение денежного остатка на счёте {account:?} в {currency:?}")]
    CashOverflow {
        account: AccountId,
        currency: CurrencyCode,
    },
    #[error("нога события {event:?} несёт количество без инструмента")]
    QuantityWithoutInstrument { event: EventId },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Остатки денег и бумаг.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balances {
    cash: BTreeMap<(AccountId, CurrencyCode), PostedMinor>,
    positions: BTreeMap<PositionKey, Quantity>,
}

impl Balances {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Применение одного события. Тело вынесено из цикла проекции,
    /// чтобы порядок обхода ног был виден и проверяем.
    pub fn apply(&mut self, event: &Event) -> Result<(), BalanceError> {
        for leg in &event.legs {
            if let Some(money) = leg.cash_effect() {
                let slot = self
                    .cash
                    .entry((leg.account, money.currency()))
                    .or_insert_with(|| PostedMinor::new(0));
                *slot = slot
                    .checked_add(money.amount())
                    .ok_or(BalanceError::CashOverflow {
                        account: leg.account,
                        currency: money.currency(),
                    })?;
            }
            if let Some(quantity) = leg.quantity {
                let instrument = leg
                    .instrument
                    .ok_or(BalanceError::QuantityWithoutInstrument { event: event.id })?;
                let key = PositionKey {
                    account: leg.account,
                    custody: leg.custody,
                    instrument,
                };
                let slot = self.positions.entry(key).or_insert_with(Quantity::zero);
                *slot = Quantity(slot.0.checked_add(quantity.0)?);
            }
        }
        Ok(())
    }

    /// Остаток счёта в валюте. `None` означает «движений не было»,
    /// а не «ноль»: разница видна в отчёте о полноте данных (§10.7).
    #[must_use]
    pub fn cash(&self, account: AccountId, currency: CurrencyCode) -> Option<Money> {
        self.cash
            .get(&(account, currency))
            .map(|amount| Money::new(*amount, currency))
    }

    pub fn iter_cash(&self) -> impl Iterator<Item = (AccountId, Money)> {
        self.cash
            .iter()
            .map(|((account, currency), amount)| (*account, Money::new(*amount, *currency)))
    }

    #[must_use]
    pub fn position(&self, key: &PositionKey) -> Option<Quantity> {
        self.positions.get(key).copied()
    }

    pub fn iter_positions(&self) -> impl Iterator<Item = (&PositionKey, Quantity)> {
        self.positions.iter().map(|(key, qty)| (key, *qty))
    }

    /// Суммарное количество инструмента на счёте по всем местам хранения.
    /// Именно это сравнивается с суммой лотов: лоты не различают custody.
    pub fn quantity_of(
        &self,
        account: AccountId,
        instrument: InstrumentId,
    ) -> Result<Quantity, NumericError> {
        self.positions
            .iter()
            .filter(|(key, _)| key.account == account && key.instrument == instrument)
            .try_fold(Quantity::zero().0, |acc, (_, qty)| acc.checked_add(qty.0))
            .map(Quantity)
    }

    /// Счета с отрицательным денежным остатком (§15.9).
    /// На этапе 1 это не ошибка: маржинальный минус — обязательство,
    /// которое обязано попасть в NAV, а не исчезнуть.
    pub fn negative_cash(&self) -> impl Iterator<Item = (AccountId, Money)> {
        self.iter_cash()
            .filter(|(_, money)| money.amount().raw() < 0)
    }
}
```

И `crates/iaam-core/src/projection/mod.rs` на этом этапе:

```rust
//! Проекции журнала со снимками (§3.1).

pub mod balances;
```

Плюс строка `pub mod projection;` в `crates/iaam-core/src/lib.rs` в алфавитном порядке — то есть после `pub mod numeric;`, а не после `pub mod money;`.

В `crates/iaam-core/src/numeric/decimal.rs`, перед `to_exact`, — сложение, которого требует `apply`:

```rust
    /// Сложение с проверкой переполнения. Штатный `+` у `Decimal` паникует
    /// при выходе за диапазон; тихая паника в расчёте доходности хуже
    /// типизированного отказа.
    pub fn checked_add(self, other: Self) -> Result<Self, NumericError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(NumericError::Overflow)
    }
```

и три теста к нему в том же файле: сумма без двоичного округления (`0.1 + 0.2 == 0.3`), сумма не равна ни одному из слагаемых, переполнение даёт `NumericError::Overflow`, а не панику. Второй тест существует отдельно от первого потому, что «сложение», возвращающее операнд, иначе проходит незамеченным.

- [ ] **Шаг 4: Зелёная сборка**

```bash
nix develop -c cargo test -p iaam-core balances
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c cargo mutants --package iaam-core \
  --file crates/iaam-core/src/projection/balances.rs \
  --file crates/iaam-core/src/numeric/decimal.rs
```

Ожидается: восемь тестов `balances` проходят (первая редакция говорила «пять» — тестов в шаге 1 семь, плюс добавленный ниже), clippy молчит, выживших мутантов нет.

Мутационный прогон при исполнении дал одного выжившего: в `quantity_of` условие отбора `key.account == account && key.instrument == instrument` заменялось на `||` незамеченно — сумма молча вбирала бы чужую позицию. Закрыто тестом `quantity_of_sums_neither_a_foreign_account_nor_a_foreign_instrument`: три позиции — своя, свой счёт с чужим инструментом, чужой счёт со своим инструментом — и ответ, равный только первой.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/iaam-core/src/projection crates/iaam-core/src/lib.rs \
  crates/iaam-core/src/numeric/decimal.rs
git commit -m "feat(core): денежные остатки и позиции по ногам событий (iaam-1fk)"
```

---

## Задача 2: Книга лотов

**Files:**
- Create: `crates/iaam-core/src/projection/lots.rs`
- Modify: `crates/iaam-core/src/projection/mod.rs` — `pub mod lots;`
- Modify: `crates/iaam-core/src/event/mod.rs` — конструктор событий для тестов ядра

**Interfaces:**
- Consumes: `rules::RuleRegistry`, `rules::lot_disposal::{DisposalInput, DisposalResult, Lot, LotId, RuleId}`, `event::kind::{EventKind, TradeSide}`.
- Produces: `projection::lots::{LotBook, LotKey, InstrumentLots, LotError, BasisGap}`; методы `LotBook::new(LotRuleVersion)`, `apply(&Event, &RuleRegistry)`, `entry(&LotKey)`, `iter()`, `applied_rule()`; у `InstrumentLots` — `lots()`, `unpriced()`, `realized()`, `acquired_basis()`, `released_basis()`, `remaining_basis()`, `quantity()`, `gap()`.

**Acceptance Criteria:**
- Покупка создаёт партию со стоимостью `gross + fee`; НКД в стоимость не входит.
- Идентификатор лота выводится из идентификатора события приобретения — повторная проекция того же журнала даёт те же идентификаторы.
- Продажа списывает партии **правилом из реестра**, а не встроенным FIFO; идентификатор применённого правила доступен снаружи.
- Восстановленная позиция без документированной стоимости не превращается в лот нулевой стоимости: количество хранится отдельно, реализованный результат становится `None`.
- Продажа без позиции и неизвестная версия правила — типизированные ошибки.

> **Исправление при исполнении (2026-08-23).** Задача идёт **после задачи 4**: исчерпывающий `match` в `LotBook::apply` обязан разобрать `EventKind::Valuation`, а списание количеств пользуется `Dec::{checked_sub, is_zero}`. Мутационный прогон дал двух выживших: `remaining_basis` не проверялась ни одним тестом и могла возвращать `Ok(None)` или отказ незамеченно. Закрыто тестом `remaining_basis_completes_the_identity_acquired_equals_remaining_plus_released` — денежная часть тождества §6.3 на пустой книге и после частичной продажи.

**Три решения, которые здесь принимаются, и почему именно так.**

1. **Комиссия входит в стоимость, НКД — нет.** Уплаченный при покупке НКД возвращается ближайшим купоном, а не продажей бумаги: считать его стоимостью приобретения значит занизить купонный доход и завысить убыток от продажи. Налоговая стоимость по ст. 214.1 считается иначе, и именно поэтому списание версионировано правилом (E5 добавит версию, не переписав эту).
2. **Идентификатор лота — идентификатор события.** Ядро чисто (§3.1): `LotId::new_random()` внутри проекции сделал бы два прогона по одному журналу несравнимыми, а снимок — бессмысленным.
3. **Нет лота нулевой стоимости.** `OpeningPosition { cost_basis: None }` — это «стоимость неизвестна» (§4.9, §10.7). Лот с нулевой стоимостью объявил бы всю выручку от продажи прибылью. Количество списывается честно, а реализованный результат помечается невычислимым.

- [ ] **Шаг 1: Конструктор событий для тестов ядра**

В `crates/iaam-core/src/event/mod.rs`, внутри `mod test_support`, перед `sample_event`:

```rust
    /// Событие произвольного типа для тестов модулей ядра.
    ///
    /// Существует, чтобы тесты проекций не переписывали конверт события
    /// в каждом модуле: переписанный вручную конверт незаметно расходится
    /// с настоящим, и тест начинает проверять фикстуру, а не код.
    pub(crate) fn event_with(
        account: AccountId,
        day: time::Date,
        sequence: u32,
        kind: EventKind,
        legs: Vec<Leg>,
    ) -> Event {
        let dates = EventDates::for_cash(CashPostedDate(day));
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind,
            dates,
            order: EffectiveOrder::new(day, sequence),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"d".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
```

Пять параметров при пороге `too-many-arguments-threshold = 6` — впритык. Шестой параметр сюда добавлять нельзя: понадобятся другие даты — передавайте `EventDates` вместо `day` и правьте вызовы.

- [ ] **Шаг 2: Написать падающие тесты**

В конец `crates/iaam-core/src/projection/lots.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::CustodyId;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use crate::rules::RuleRegistry;
    use rust_decimal::Decimal;
    use time::Date;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    struct Trade {
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        units: i64,
        gross: i64,
    }

    fn buy(trade: &Trade, sequence: u32) -> Event {
        let fee = rub(10_000);
        let settlement = rub(-(trade.gross + 10_000));
        event_with(
            trade.account,
            trade.day,
            sequence,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: trade.instrument,
                quantity: qty(trade.units),
                gross: rub(trade.gross),
                fee: Some(fee),
                accrued_interest: None,
            },
            vec![
                Leg::cash(trade.account, settlement),
                Leg::security(
                    trade.account,
                    CustodyId::new_random(),
                    trade.instrument,
                    qty(trade.units),
                ),
            ],
        )
    }

    fn sell(trade: &Trade, sequence: u32) -> Event {
        let fee = rub(10_000);
        let settlement = rub(trade.gross - 10_000);
        event_with(
            trade.account,
            trade.day,
            sequence,
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument: trade.instrument,
                quantity: qty(trade.units),
                gross: rub(trade.gross),
                fee: Some(fee),
                accrued_interest: None,
            },
            vec![
                Leg::cash(trade.account, settlement),
                Leg::security(
                    trade.account,
                    CustodyId::new_random(),
                    trade.instrument,
                    qty(-trade.units),
                ),
            ],
        )
    }

    fn key(trade: &Trade) -> LotKey {
        LotKey {
            account: trade.account,
            instrument: trade.instrument,
        }
    }

    fn sample_trade() -> Trade {
        Trade {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
            day: date!(2025 - 03 - 01),
            units: 100,
            gross: 1_000_000,
        }
    }

    #[test]
    fn a_purchase_creates_a_lot_including_the_fee() {
        // Комиссия входит в стоимость приобретения; НКД — нет (§7.2).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.lots().len(), 1);
        assert_eq!(entry.lots()[0].cost_basis, rub(1_010_000));
        assert_eq!(entry.quantity().unwrap(), qty(100));
    }

    #[test]
    fn lot_identity_comes_from_the_acquisition_event_not_from_randomness() {
        // Ядро чисто: повторная проекция того же журнала обязана дать
        // те же идентификаторы лотов, иначе снимки несравнимы (§3.1).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let event = buy(&trade, 1);
        let mut first = LotBook::new(LotRuleVersion(1));
        let mut second = LotBook::new(LotRuleVersion(1));
        first.apply(&event, &rules).unwrap();
        second.apply(&event, &rules).unwrap();
        assert_eq!(
            first.entry(&key(&trade)).unwrap().lots()[0].id,
            second.entry(&key(&trade)).unwrap().lots()[0].id
        );
    }

    #[test]
    fn a_partial_sale_releases_basis_and_records_realized_result() {
        // Куплено 100 за 1 010 000, продано 40 за 500 000 минус комиссия.
        // Списанная стоимость: 1 010 000 * 40 / 100 = 404 000.
        // Реализовано: 490 000 − 404 000 = 86 000.
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        let partial = Trade {
            units: 40,
            gross: 500_000,
            day: date!(2025 - 06 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(60));
        assert_eq!(entry.released_basis(), Some(rub(404_000)));
        assert_eq!(entry.realized(), Some(rub(86_000)));
        assert_eq!(
            book.applied_rule().map(|r| r.0.as_str()),
            Some("fifo/214.1/v1")
        );
    }

    #[test]
    fn a_restored_position_without_basis_does_not_become_a_zero_cost_lot() {
        // Нулевая стоимость означала бы прибыль, равную всей выручке (§4.9).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: None,
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        book.apply(&restored, &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert!(entry.lots().is_empty());
        assert_eq!(entry.unpriced(), qty(50));
        assert_eq!(entry.gap(), Some(BasisGap::RestoredWithoutBasis));

        // Продажа из восстановленного количества уменьшает позицию,
        // но реализованный результат остаётся невычислимым.
        let partial = Trade {
            units: 20,
            gross: 300_000,
            day: date!(2025 - 02 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.unpriced(), qty(30));
        assert_eq!(entry.realized(), None);
    }

    #[test]
    fn selling_an_instrument_never_held_is_an_error() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        assert!(matches!(
            book.apply(&sell(&trade, 1), &rules),
            Err(LotError::SaleWithoutPosition { .. })
        ));
    }

    #[test]
    fn an_unknown_rule_version_is_an_error_not_a_silent_fallback() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(99));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        assert!(matches!(
            book.apply(&sell(&trade, 2), &rules),
            Err(LotError::UnknownRule { .. })
        ));
    }
    #[test]
    fn the_book_exposes_its_entries_and_the_cost_of_acquisitions() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();

        assert_eq!(book.iter().count(), 1, "книга обязана отдавать записи");
        let (found_key, entry) = book.iter().next().unwrap();
        assert_eq!(*found_key, key(&trade));
        // Приобретено = тело + комиссия; вместе со списанным образует
        // проверяемое тождество сохранения стоимости.
        assert_eq!(entry.acquired_basis(), Some(rub(1_010_000)));
        assert_eq!(entry.released_basis(), None);
        assert_eq!(book.rule_version(), LotRuleVersion(1));
    }

    #[test]
    fn the_basis_gap_has_a_machine_readable_code() {
        // Код уходит в API: агент разбирает его, а не текст.
        assert_eq!(
            BasisGap::RestoredWithoutBasis.code(),
            "restored_without_basis"
        );
    }
}
```

- [ ] **Шаг 3: Убедиться, что тесты падают**

```bash
nix develop -c cargo test -p iaam-core projection::lots
```

Ожидается: ошибка сборки — `LotBook` не существует.

- [ ] **Шаг 4: Реализация**

Начало `crates/iaam-core/src/projection/lots.rs` (тесты из шага 2 остаются в конце файла):

```rust
//! Книга лотов (§4.12).
//!
//! Лоты строятся **по типу события**: покупка добавляет партию, продажа
//! списывает её версионированным правилом из реестра. Количество бумаг
//! при этом считается независимо — по ногам события (`super::balances`).
//!
//! Восстановленная позиция без документированной стоимости (§10.7)
//! **не превращается в лот с нулевой стоимостью**: она хранится отдельным
//! количеством, списывается первой и делает реализованный результат
//! невычислимым. Нулевая заглушка здесь означала бы выдуманную прибыль,
//! равную всей выручке.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::Event;
use crate::event::kind::{EventKind, TradeSide};
use crate::ids::{AccountId, EventId, InstrumentId};
use crate::money::{Money, MoneyError, Quantity};
use crate::numeric::NumericError;
use crate::rules::lot_disposal::{
    DisposalError, DisposalInput, DisposalResult, Lot, LotId, RuleId,
};
use crate::rules::{LotRuleVersion, RuleRegistry};

/// Лоты не различают место хранения: перевод бумаги между депозитариями
/// не является приобретением и не создаёт новой партии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LotKey {
    pub account: AccountId,
    pub instrument: InstrumentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LotError {
    #[error("в реестре нет правила списания версии {version:?}")]
    UnknownRule { version: LotRuleVersion },
    #[error("продажа {event:?} без предшествующей позиции по инструменту {instrument:?}")]
    SaleWithoutPosition {
        event: EventId,
        instrument: InstrumentId,
    },
    #[error(transparent)]
    Disposal(#[from] DisposalError),
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Почему реализованный результат по инструменту не вычисляется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisGap {
    /// Позиция восстановлена без документированной стоимости (§10.7).
    RestoredWithoutBasis,
}

impl BasisGap {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RestoredWithoutBasis => "restored_without_basis",
        }
    }
}

/// Лоты одного инструмента на одном счёте.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentLots {
    /// Количество, восстановленное без стоимости. Списывается первым:
    /// оно приобретено раньше всего, что система видела.
    unpriced: Quantity,
    /// Партии в порядке приобретения.
    lots: Vec<Lot>,
    /// Реализованный результат до налога. `None`, если хотя бы одно
    /// выбытие затронуло количество без стоимости.
    realized: Option<Money>,
    /// Суммарная стоимость всех приобретений с документированной ценой.
    acquired_basis: Option<Money>,
    /// Суммарная стоимость, списанная при выбытиях.
    released_basis: Option<Money>,
    gap: Option<BasisGap>,
}

/// Пустая книга по инструменту. Пишется вручную, потому что `Quantity`
/// намеренно не реализует `Default`: нулевое количество должно возникать
/// осознанно, а не как значение по умолчанию неизвестного поля (§4.9).
impl Default for InstrumentLots {
    fn default() -> Self {
        Self {
            unpriced: Quantity::zero(),
            lots: Vec::new(),
            realized: None,
            acquired_basis: None,
            released_basis: None,
            gap: None,
        }
    }
}

impl InstrumentLots {
    #[must_use]
    pub fn lots(&self) -> &[Lot] {
        &self.lots
    }

    #[must_use]
    pub const fn unpriced(&self) -> Quantity {
        self.unpriced
    }

    #[must_use]
    pub const fn realized(&self) -> Option<Money> {
        self.realized
    }

    #[must_use]
    pub const fn gap(&self) -> Option<BasisGap> {
        self.gap
    }

    /// Стоимость приобретений. Вместе с [`Self::released_basis`] образует
    /// проверяемое тождество: приобретено = осталось + списано.
    #[must_use]
    pub const fn acquired_basis(&self) -> Option<Money> {
        self.acquired_basis
    }

    #[must_use]
    pub const fn released_basis(&self) -> Option<Money> {
        self.released_basis
    }

    /// Стоимость непроданных партий.
    pub fn remaining_basis(&self) -> Result<Option<Money>, MoneyError> {
        let Some(first) = self.lots.first() else {
            return Ok(None);
        };
        let amounts: Vec<Money> = self.lots.iter().map(|lot| lot.cost_basis).collect();
        Money::sum(&amounts, first.cost_basis.currency()).map(Some)
    }

    /// Суммарное количество: партии плюс восстановленный остаток.
    pub fn quantity(&self) -> Result<Quantity, NumericError> {
        self.lots
            .iter()
            .try_fold(self.unpriced.0, |acc, lot| acc.checked_add(lot.quantity.0))
            .map(Quantity)
    }
}

/// Факты сделки, нужные книге лотов. Отдельная структура, а не восемь
/// аргументов: порог `too-many-arguments-threshold = 6` в `clippy.toml`
/// действует, а подавлять линт запрещено (§15.7).
#[derive(Debug, Clone, Copy, PartialEq)]
struct TradeFacts {
    side: TradeSide,
    instrument: InstrumentId,
    quantity: Quantity,
    gross: Money,
    fee: Option<Money>,
}

/// Книга лотов и применённое правило.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LotBook {
    entries: BTreeMap<LotKey, InstrumentLots>,
    rule_version: LotRuleVersion,
    applied_rule: Option<RuleId>,
}

impl LotBook {
    #[must_use]
    pub fn new(rule_version: LotRuleVersion) -> Self {
        Self {
            entries: BTreeMap::new(),
            rule_version,
            applied_rule: None,
        }
    }

    #[must_use]
    pub const fn rule_version(&self) -> LotRuleVersion {
        self.rule_version
    }

    /// Идентификатор правила, которым фактически списывались лоты.
    /// Входит в отчёт и в след аудита: без него цифру не воспроизвести (§3.2).
    #[must_use]
    pub const fn applied_rule(&self) -> Option<&RuleId> {
        self.applied_rule.as_ref()
    }

    #[must_use]
    pub fn entry(&self, key: &LotKey) -> Option<&InstrumentLots> {
        self.entries.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&LotKey, &InstrumentLots)> {
        self.entries.iter()
    }

    /// Применение события к книге лотов.
    ///
    /// Диспетчер исчерпывающий: новый тип события обязан сломать сборку
    /// здесь, а не молча не создать лот.
    pub fn apply(&mut self, event: &Event, rules: &RuleRegistry) -> Result<(), LotError> {
        match &event.kind {
            EventKind::Trade {
                side,
                instrument,
                quantity,
                gross,
                fee,
                // НКД не участвует в стоимости приобретения — см. `apply_trade`.
                accrued_interest: _,
            } => self.apply_trade(
                event,
                TradeFacts {
                    side: *side,
                    instrument: *instrument,
                    quantity: *quantity,
                    gross: *gross,
                    fee: *fee,
                },
                rules,
            ),
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis,
            } => self.restore(event, *instrument, *quantity, *cost_basis),
            EventKind::CashIn { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Income { .. }
            | EventKind::Fee { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. } => Ok(()),
        }
    }

    /// Стоимость приобретения включает комиссию и **не включает НКД**:
    /// накопленный купонный доход возвращается купоном, а не продажей,
    /// поэтому он не является стоимостью бумаги (§7.2). Налоговая
    /// стоимость по ст. 214.1 считается иначе и появится в E5 —
    /// поэтому она и версионирована правилом.
    fn apply_trade(
        &mut self,
        event: &Event,
        trade: TradeFacts,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        let TradeFacts {
            side,
            instrument,
            quantity,
            gross,
            fee,
        } = trade;
        let key = LotKey {
            account: event.account,
            instrument,
        };
        match side {
            TradeSide::Buy => {
                let basis = match fee {
                    Some(f) => gross.try_add(f)?,
                    None => gross,
                };
                let entry = self.entries.entry(key).or_default();
                entry.acquired_basis = Some(match entry.acquired_basis {
                    Some(previous) => previous.try_add(basis)?,
                    None => basis,
                });
                entry.lots.push(Lot {
                    // Идентификатор лота выводится из события приобретения:
                    // ядро чисто, случайных идентификаторов в нём быть не может,
                    // иначе повторная проекция того же журнала дала бы другой
                    // результат (§3.1, §15.3).
                    id: LotId(event.id.inner()),
                    instrument,
                    acquired: event.dates.trade,
                    quantity,
                    cost_basis: basis,
                });
                Ok(())
            }
            TradeSide::Sell => {
                let proceeds = match fee {
                    Some(f) => gross.try_sub(f)?,
                    None => gross,
                };
                self.dispose(event, key, quantity, proceeds, rules)
            }
        }
    }

    fn restore(
        &mut self,
        event: &Event,
        instrument: InstrumentId,
        quantity: Quantity,
        cost_basis: Option<Money>,
    ) -> Result<(), LotError> {
        let key = LotKey {
            account: event.account,
            instrument,
        };
        let entry = self.entries.entry(key).or_default();
        match cost_basis {
            // Восстановленная партия старше всего, что система видела,
            // поэтому встаёт в голову очереди FIFO, а не в хвост.
            Some(basis) => {
                entry.acquired_basis = Some(match entry.acquired_basis {
                    Some(previous) => previous.try_add(basis)?,
                    None => basis,
                });
                entry.lots.insert(
                    0,
                    Lot {
                        id: LotId(event.id.inner()),
                        instrument,
                        acquired: event.dates.trade,
                        quantity,
                        cost_basis: basis,
                    },
                );
            }
            None => {
                entry.unpriced = Quantity(entry.unpriced.0.checked_add(quantity.0)?);
                entry.gap = Some(BasisGap::RestoredWithoutBasis);
            }
        }
        Ok(())
    }

    fn dispose(
        &mut self,
        event: &Event,
        key: LotKey,
        quantity: Quantity,
        proceeds: Money,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        let rule = rules
            .disposal_rule(self.rule_version)
            .ok_or(LotError::UnknownRule {
                version: self.rule_version,
            })?;
        let entry = self
            .entries
            .get_mut(&key)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: key.instrument,
            })?;

        // Восстановленное количество списывается первым: оно приобретено
        // раньше всего, что система наблюдала. Стоимости у него нет,
        // поэтому реализованный результат по инструменту становится
        // невычислимым — но количество списывается честно.
        let from_unpriced = entry.unpriced.0.min(quantity.0);
        if !from_unpriced.is_zero() {
            entry.unpriced = Quantity(entry.unpriced.0.checked_sub(from_unpriced)?);
            entry.realized = None;
            entry.gap = Some(BasisGap::RestoredWithoutBasis);
        }
        let left = quantity.0.checked_sub(from_unpriced)?;
        if left.is_zero() {
            return Ok(());
        }

        let result: DisposalResult = rule.apply(&DisposalInput {
            lots: entry.lots.clone(),
            quantity: Quantity(left),
        })?;
        entry.lots = result.remaining.clone();
        entry.released_basis = Some(match entry.released_basis {
            Some(previous) => previous.try_add(result.basis_released)?,
            None => result.basis_released,
        });
        self.applied_rule = Some(result.rule.clone());

        // Реализованный результат до налога: выручка минус списанная
        // стоимость. Он не суммируется с невычислимым: один разрыв делает
        // невычислимым весь инструмент, а не «почти всё».
        if entry.gap.is_none() {
            let realized = proceeds.try_sub(result.basis_released)?;
            entry.realized = Some(match entry.realized {
                Some(previous) => previous.try_add(realized)?,
                None => realized,
            });
        }
        Ok(())
    }
}
```

- [ ] **Шаг 5: Зелёная сборка**

```bash
nix develop -c cargo test -p iaam-core projection::lots
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Ожидается: шесть тестов проходят. Ожидаемые числа в тесте частичной продажи (`404 000` списанной стоимости и `86 000` реализованного результата) посчитаны вручную из условий теста и **не** берутся из вывода программы (§15.5).

- [ ] **Шаг 6: Коммит**

```bash
git add crates/iaam-core/src
git commit -m "feat(core): книга лотов со списанием версионированным правилом (iaam-1fk)"
```

---

## Задача 3: Потоки границы контура

**Files:**
- Create: `crates/iaam-core/src/projection/flows.rs`
- Modify: `crates/iaam-core/src/projection/mod.rs` — `pub mod flows;`

**Interfaces:**
- Consumes: `contour::{ContourDefinition, FlowClass, classify}`, `event::leg::Leg::cash_effect`.
- Produces: `projection::flows::{FlowLog, ExternalFlow, FlowDirection, FlowError}`; методы `FlowLog::apply(&Event, &ContourDefinition)`, `external() -> &[ExternalFlow]`, `internal()`, `irrelevant()`.

> **Исправление при исполнении (2026-08-23).** Задача идёт **после задачи 4**: `classify` разбирает `EventKind` исчерпывающе. Мутационный прогон дал одного выжившего — `moves_money` можно было заменить на `true` незамеченно, и счётчик внутренних движений стал бы счётчиком событий. Закрыто тестом `an_event_that_moved_no_money_is_not_counted_as_a_movement`: оценка внутри контура движением не считается, перевод между своими счетами — считается.

**Acceptance Criteria:**
- Внешним потоком становится только то, что пересекает границу контура; сделки, доходы и комиссии внутри контура — нет.
- Для перевода извне внутрь сумма потока равна **входящей ноге**, а не обеим.
- Каждый поток несёт идентификатор и версию контура: изменение состава контура задним числом не переписывает прошлые цифры молча (§4.10).
- Событие, чья классификация противоречит знаку суммы, — типизированная ошибка.
- Событие без даты, пересекающее границу, — типизированная ошибка, а не поток с подставленной датой.

**Почему сумма считается по ногам на счетах контура.** Одно правило покрывает все четыре случая: пополнение (одна нога внутри), вывод (одна нога внутри с минусом), перевод извне внутрь (входящая нога внутри, исходящая снаружи) и перевод изнутри наружу. Отдельная ветка на каждый тип события — это четыре места, где можно перепутать знак.

- [ ] **Шаг 1: Написать падающие тесты**

В конец `crates/iaam-core/src/projection/flows.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::ContourVersion;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, EventId, TransferId};
    use crate::money::PostedMinor;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn contour_of(accounts: [AccountId; 1]) -> ContourDefinition {
        ContourDefinition::new(
            crate::contour::ContourId::new_random(),
            ContourVersion(1),
            accounts,
        )
    }

    fn transfer(from: AccountId, to: AccountId, amount: Money) -> Event {
        event_with(
            from,
            date!(2025 - 05 - 05),
            1,
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount,
            },
            vec![
                Leg::cash(from, amount.checked_negate().unwrap()),
                Leg::cash(to, amount),
            ],
        )
    }

    #[test]
    fn money_from_outside_is_an_inbound_flow() {
        let account = AccountId::new_random();
        let contour = contour_of([account]);
        let event = event_with(
            account,
            date!(2025 - 01 - 09),
            1,
            EventKind::CashIn {
                amount: rub(50_000),
            },
            vec![Leg::cash(account, rub(50_000))],
        );
        let mut log = FlowLog::new();
        log.apply(&event, &contour).unwrap();
        assert_eq!(log.external().len(), 1);
        assert_eq!(log.external()[0].direction, FlowDirection::In);
        assert_eq!(log.external()[0].amount, rub(50_000));
        assert_eq!(log.external()[0].version, ContourVersion(1));
    }

    #[test]
    fn a_transfer_between_two_accounts_of_the_contour_is_internal() {
        // Именно из-за этой ветки в чужих сервисах перевод со вклада
        // на брокерский счёт выглядит доходом (§4.10).
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let contour = ContourDefinition::new(
            crate::contour::ContourId::new_random(),
            ContourVersion(1),
            [from, to],
        );
        let mut log = FlowLog::new();
        log.apply(&transfer(from, to, rub(30_000)), &contour)
            .unwrap();
        assert!(log.external().is_empty());
        assert_eq!(log.internal(), 1);
    }

    #[test]
    fn a_transfer_from_outside_carries_only_the_incoming_leg() {
        let outside = AccountId::new_random();
        let inside = AccountId::new_random();
        let contour = contour_of([inside]);
        let mut log = FlowLog::new();
        log.apply(&transfer(outside, inside, rub(30_000)), &contour)
            .unwrap();
        assert_eq!(log.external().len(), 1);
        assert_eq!(log.external()[0].direction, FlowDirection::In);
        assert_eq!(log.external()[0].amount, rub(30_000));
    }

    #[test]
    fn a_purchase_does_not_cross_the_boundary() {
        // Покупка бумаги меняет состав контура, а не его размер.
        let account = AccountId::new_random();
        let contour = contour_of([account]);
        let event = event_with(
            account,
            date!(2025 - 02 - 02),
            1,
            EventKind::Fee {
                amount: rub(-500),
                origin: crate::event::kind::FeeOrigin::Brokerage,
            },
            vec![Leg::fee(account, rub(-500))],
        );
        let mut log = FlowLog::new();
        log.apply(&event, &contour).unwrap();
        assert!(log.external().is_empty());
        assert_eq!(log.internal(), 1);
    }

    #[test]
    fn an_event_outside_the_contour_is_irrelevant_not_external() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let contour = contour_of([inside]);
        let event = event_with(
            outside,
            date!(2025 - 03 - 03),
            1,
            EventKind::CashIn { amount: rub(1_000) },
            vec![Leg::cash(outside, rub(1_000))],
        );
        let mut log = FlowLog::new();
        log.apply(&event, &contour).unwrap();
        assert!(log.external().is_empty());
        assert_eq!(log.irrelevant(), 1);
        assert_eq!(log.internal(), 0);
    }

    #[test]
    fn a_direction_that_contradicts_the_sign_is_an_error() {
        // Классификатор сказал «приход», а ноги показывают расход.
        // Взять модуль здесь — способ выдать вывод средств за доход.
        let account = AccountId::new_random();
        let contour = contour_of([account]);
        let mut event = event_with(
            account,
            date!(2025 - 04 - 04),
            1,
            EventKind::CashIn { amount: rub(1_000) },
            vec![Leg::cash(account, rub(1_000))],
        );
        event.legs = vec![Leg::cash(account, rub(-1_000))];
        let mut log = FlowLog::new();
        assert!(matches!(
            log.apply(&event, &contour),
            Err(FlowError::DirectionContradictsAmount { .. })
        ));
    }
    #[test]
    fn directions_have_machine_readable_codes() {
        assert_eq!(FlowDirection::In.code(), "in");
        assert_eq!(FlowDirection::Out.code(), "out");
    }

    #[test]
    fn the_sign_check_is_strict_at_zero() {
        // Нулевая сумма не является ни приходом, ни расходом. Через
        // публичный путь ноль не проходит (нулевые суммы отсеиваются
        // раньше), поэтому граница проверяется прямо на функции —
        // иначе `>` и `>=` здесь неразличимы.
        let event = EventId::new_random();
        assert!(require_sign_matches(event, FlowDirection::In, rub(1)).is_ok());
        assert!(require_sign_matches(event, FlowDirection::In, rub(0)).is_err());
        assert!(require_sign_matches(event, FlowDirection::Out, rub(-1)).is_ok());
        assert!(require_sign_matches(event, FlowDirection::Out, rub(0)).is_err());
    }

    #[test]
    fn irrelevant_events_are_counted_separately_from_internal_ones() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let contour = contour_of([inside]);
        let mut log = FlowLog::new();
        for _ in 0..3 {
            let event = event_with(
                outside,
                date!(2025 - 03 - 03),
                1,
                EventKind::CashIn { amount: rub(1_000) },
                vec![Leg::cash(outside, rub(1_000))],
            );
            log.apply(&event, &contour).unwrap();
        }
        assert_eq!(log.irrelevant(), 3);
        assert_eq!(log.internal(), 0);
        assert!(log.external().is_empty());
    }
}
```

- [ ] **Шаг 2: Убедиться, что тесты падают**

```bash
nix develop -c cargo test -p iaam-core projection::flows
```

- [ ] **Шаг 3: Реализация**

```rust
//! Денежные потоки границы контура (§4.10, §6.1).
//!
//! Из-за путаницы именно здесь сервисы показывают доходность, в которой
//! собственные пополнения выглядят заработком. Классификацию делает
//! `contour::classify`, этот модуль лишь превращает её в датированный
//! ряд сумм и следит, чтобы знак суммы не противоречил направлению.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::contour::{ContourDefinition, ContourId, ContourVersion, FlowClass, classify};
use crate::event::Event;
use crate::ids::EventId;
use crate::money::{CurrencyCode, Money, PostedMinor};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FlowDirection {
    /// Деньги вошли в контур извне.
    In,
    /// Деньги вышли из контура.
    Out,
}

impl FlowDirection {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }
}

/// Поток, пересёкший границу контура.
///
/// Сумма — **проведённая**, в валюте счёта. Перевод в валюту отчёта
/// делается позже и даёт расчётную величину (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalFlow {
    pub event: EventId,
    pub date: Date,
    pub amount: Money,
    pub direction: FlowDirection,
    pub contour: ContourId,
    pub version: ContourVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FlowError {
    #[error("событие {event:?} пересекает границу контура, но не имеет даты")]
    FlowWithoutDate { event: EventId },
    #[error(
        "событие {event:?} классифицировано как {direction:?}, \
         но денежный эффект на счетах контура равен {amount} в {currency:?}"
    )]
    DirectionContradictsAmount {
        event: EventId,
        direction: FlowDirection,
        amount: i64,
        currency: CurrencyCode,
    },
    #[error("переполнение при суммировании ног события {event:?}")]
    Overflow { event: EventId },
}

/// Ряд внешних потоков плюс счётчик внутренних движений.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowLog {
    external: Vec<ExternalFlow>,
    internal: u64,
    irrelevant: u64,
}

impl FlowLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn external(&self) -> &[ExternalFlow] {
        &self.external
    }

    /// Число **денежных** движений внутри контура.
    ///
    /// Считаются только события, двигавшие деньги: событие оценки денег
    /// не двигает и движением не является, хотя относится к контуру.
    /// Ноль внешних потоков при ненулевом внутреннем счётчике — законная
    /// ситуация: перевод между своими счетами доходность не меняет (§15.9).
    #[must_use]
    pub const fn internal(&self) -> u64 {
        self.internal
    }

    #[must_use]
    pub const fn irrelevant(&self) -> u64 {
        self.irrelevant
    }

    pub fn apply(&mut self, event: &Event, contour: &ContourDefinition) -> Result<(), FlowError> {
        let (direction, id, version) = match classify(contour, event) {
            FlowClass::ExternalIn { contour, version } => (FlowDirection::In, contour, version),
            FlowClass::ExternalOut { contour, version } => (FlowDirection::Out, contour, version),
            FlowClass::Internal => {
                if moves_money(event) {
                    self.internal += 1;
                }
                return Ok(());
            }
            FlowClass::Irrelevant => {
                if moves_money(event) {
                    self.irrelevant += 1;
                }
                return Ok(());
            }
        };
        let date = event
            .dates
            .effective_date()
            .ok_or(FlowError::FlowWithoutDate { event: event.id })?;
        for (currency, amount) in contour_cash_effect(event, contour)? {
            let money = Money::new(amount, currency);
            require_sign_matches(event.id, direction, money)?;
            self.external.push(ExternalFlow {
                event: event.id,
                date,
                amount: money,
                direction,
                contour: id,
                version,
            });
        }
        Ok(())
    }
}

/// Двигало ли событие деньги хоть где-нибудь.
///
/// Проверяется по ногам, а не по типу события: тип отвечает на вопрос
/// «что произошло», а ноги — «что при этом сдвинулось».
fn moves_money(event: &Event) -> bool {
    event.legs.iter().any(|leg| leg.cash_effect().is_some())
}

/// Денежный эффект события **на счетах контура**, по валютам.
///
/// Для перевода извне внутрь это сумма только входящей ноги: исходящая
/// нога лежит на счёте вне контура и границу не пересекает — она и есть
/// внешний мир.
fn contour_cash_effect(
    event: &Event,
    contour: &ContourDefinition,
) -> Result<BTreeMap<CurrencyCode, PostedMinor>, FlowError> {
    let mut totals: BTreeMap<CurrencyCode, PostedMinor> = BTreeMap::new();
    for leg in &event.legs {
        if !contour.contains(leg.account) {
            continue;
        }
        if let Some(money) = leg.cash_effect() {
            let slot = totals
                .entry(money.currency())
                .or_insert_with(|| PostedMinor::new(0));
            *slot = slot
                .checked_add(money.amount())
                .ok_or(FlowError::Overflow { event: event.id })?;
        }
    }
    totals.retain(|_, amount| amount.raw() != 0);
    Ok(totals)
}

/// Знак суммы обязан соответствовать направлению.
///
/// Расхождение означает, что классификатор и ноги события говорят разное,
/// и молча взять модуль здесь — способ получить доходность, в которой
/// вывод средств выглядит доходом.
fn require_sign_matches(
    event: EventId,
    direction: FlowDirection,
    money: Money,
) -> Result<(), FlowError> {
    let raw = money.amount().raw();
    let ok = match direction {
        FlowDirection::In => raw > 0,
        FlowDirection::Out => raw < 0,
    };
    if ok {
        Ok(())
    } else {
        Err(FlowError::DirectionContradictsAmount {
            event,
            direction,
            amount: raw,
            currency: money.currency(),
        })
    }
}
```

- [ ] **Шаг 4: Зелёная сборка**

```bash
nix develop -c cargo test -p iaam-core projection::flows
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Шаг 5: Коммит**

```bash
git add crates/iaam-core/src
git commit -m "feat(core): датированные потоки границы контура (iaam-1fk)"
```

---

## Задача 4: Оценка позиций, курсы и событие `Valuation`

**Files:**
- Create: `crates/iaam-core/src/valuation.rs`
- Modify: `crates/iaam-core/src/lib.rs` — `pub mod valuation;`
- Modify: `crates/iaam-core/src/event/kind.rs` — вариант `Valuation`
- Modify: `crates/iaam-core/src/event/mod.rs` — форма события `Valuation`
- Modify: `crates/iaam-core/src/money.rs` — `Money::to_calc_dec`
- Modify: `crates/iaam-core/src/numeric/decimal.rs` — арифметика `Dec`
- Modify: `docs/irreversible-core.md` — новая строка в таблице

**Interfaces:**
- Produces: `valuation::{PriceQuality, InstrumentPrice, PriceBoard, FxSource, FxTable, ValuationError, convert}`; `money::Money::to_calc_dec() -> Dec`; `numeric::decimal::Dec::{one, is_zero, is_positive, is_negative, checked_sub, checked_mul, checked_neg, sum}` (`checked_add` уже есть — его завела задача 1); `event::kind::EventKind::Valuation`.

**Acceptance Criteria:**
- Цена приходит **фактом с provenance** — событием журнала, а не параметром запроса.
- Каждая цена несёт флаг качества; полными считаются только исполнимая цена и цена закрытия.
- Событие оценки не имеет ног: переоценка не является движением.
- Цена, количество и сумма сделки обязаны быть положительными.
- Нога сделки и восстановленной позиции обязана совпадать с событием по инструменту, счёту и количеству (со знаком по направлению).
- Версия схемы события поднята до 2: добавленный вариант несовместим с читателем версии 1.
- Отсутствие курса — типизированная ошибка, а не курс, равный единице.
- Перевод суммы по курсу даёт `Dec`, а не `PostedMinor`.
- Более ранняя оценка не затирает более позднюю.

> **Исправление при исполнении (2026-08-23).** Эта задача идёт **первой в части A** после задачи 1: задачи 2 и 3 разбирают `EventKind` исчерпывающим `match` и складывают количества через арифметику `Dec` — без варианта `Valuation` и без `checked_sub`/`is_zero` они не собираются.
>
> Шаг 5 приводит `validate_valuation` в промежуточном виде, а шаг 6 её же заменяет; при исполнении достаточно сразу писать редакцию шага 6.
>
> Мутационный прогон после шага 7 дал семь выживших — все в новой арифметике `Dec`, для которой план тестов не предусматривал вовсе: `is_zero` и `is_negative` заменялись на константу незамеченно, `sum` — на отказ. Добавлены шесть тестов: границы трёх знаковых предикатов вместе с отрицательным нулём, точность и отказ `checked_sub`/`checked_mul`/`checked_neg`, сумма пустого списка и отказ суммы при переполнении.

**Почему цена — событие журнала, а не параметр отчёта.** Цена, названная владельцем, — такой же факт с источником, датой и качеством, как выписка брокера: она должна храниться, а не исчезать после запроса. В E3 тот же вариант события заполняет `iaam-market` из MOEX — меняется источник, а не схема. Это ровно тот случай, ради которого в §16.2 записано «новые варианты `EventKind` добавляются свободно».

**Почему `Valuation` не ломает необратимое ядро.** Вариант добавляется, а не меняется; существующие события сохраняют смысл; исчерпывающие `match` в `discriminant`, `flow_endpoints` и `validate_structure` ломают сборку и обязывают обработать новый вариант — это работает как задумано (§15.1).

- [ ] **Шаг 1: Арифметика расчётных величин**

В `crates/iaam-core/src/numeric/decimal.rs`, после `max_scale()`:

```rust
    #[must_use]
    pub const fn one() -> Self {
        Self(Decimal::ONE)
    }

    #[must_use]
    pub const fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    #[must_use]
    pub const fn is_positive(&self) -> bool {
        self.0.is_sign_positive() && !self.0.is_zero()
    }

    #[must_use]
    pub const fn is_negative(&self) -> bool {
        self.0.is_sign_negative() && !self.0.is_zero()
    }

    // checked_add уже добавлен задачей 1.

    pub fn checked_sub(self, other: Self) -> Result<Self, NumericError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(NumericError::Overflow)
    }

    pub fn checked_mul(self, other: Self) -> Result<Self, NumericError> {
        self.0
            .checked_mul(other.0)
            .map(Self)
            .ok_or(NumericError::Overflow)
    }

    pub fn checked_neg(self) -> Result<Self, NumericError> {
        Self::zero().checked_sub(self)
    }

    /// Сумма списка. Вынесена отдельно по той же причине, что и у `Exact`:
    /// суммирование компонентов отчёта обязано отказывать явно.
    pub fn sum(items: &[Self]) -> Result<Self, NumericError> {
        items
            .iter()
            .try_fold(Self::zero(), |acc, x| acc.checked_add(*x))
    }
```

В `crates/iaam-core/src/money.rs` добавьте `use rust_decimal::Decimal;` и метод перед `to_exact`:

```rust
    /// Переход в денежный режим: сумма как десятичная дробь.
    ///
    /// Единственная разрешённая точка перехода «проведённая сумма →
    /// расчётная величина» (§3.4). Обратного перехода нет намеренно:
    /// расчётная величина становится проведённой суммой только через
    /// факт источника, а не через округление.
    #[must_use]
    pub fn to_calc_dec(&self) -> Dec {
        Dec::new(Decimal::new(self.amount.raw(), self.currency.minor_units()))
    }
```

- [ ] **Шаг 2: Написать падающие тесты оценки**

В конец `crates/iaam-core/src/valuation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::PostedMinor;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn price(day: time::Date, value: i64, quality: PriceQuality) -> InstrumentPrice {
        InstrumentPrice {
            instrument: InstrumentId::new_random(),
            price: Dec::new(Decimal::from(value)),
            currency: CurrencyCode::Rub,
            quality,
            as_of: day,
        }
    }

    #[test]
    fn a_later_price_replaces_an_earlier_one_and_an_earlier_one_does_not() {
        let instrument = InstrumentId::new_random();
        let mut board = PriceBoard::new();
        let mut early = price(date!(2026 - 01 - 05), 100, PriceQuality::PreviousClose);
        early.instrument = instrument;
        let mut late = price(date!(2026 - 02 - 05), 120, PriceQuality::Executable);
        late.instrument = instrument;

        board.record(late);
        board.record(early);
        assert_eq!(board.latest(instrument).unwrap().price, late.price);
        assert_eq!(board.len(), 1);
    }

    #[test]
    fn only_executable_and_closing_prices_count_as_complete() {
        // Молчаливая подстановка запрещена: перенесённая, устаревшая
        // и оценочная цена помечают NAV как неполный (§5.4).
        assert!(PriceQuality::Executable.is_complete());
        assert!(PriceQuality::PreviousClose.is_complete());
        assert!(!PriceQuality::CarriedForward.is_complete());
        assert!(!PriceQuality::Stale.is_complete());
        assert!(!PriceQuality::OwnerEstimate.is_complete());
    }

    #[test]
    fn the_same_currency_needs_no_rate() {
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let amount = Money::new(PostedMinor::new(12_345), CurrencyCode::Rub);
        assert_eq!(
            convert(amount, CurrencyCode::Rub, date!(2026 - 03 - 01), &fx).unwrap(),
            Dec::new(Decimal::new(12_345, 2))
        );
    }

    #[test]
    fn a_missing_rate_is_an_error_not_an_assumed_one() {
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Usd);
        assert!(matches!(
            convert(amount, CurrencyCode::Rub, date!(2026 - 03 - 01), &fx),
            Err(ValuationError::MissingFxRate { .. })
        ));
    }

    #[test]
    fn conversion_produces_a_calculated_value() {
        // 100,00 USD по курсу 80,5 = 8050 рублей расчётной величиной,
        // а не проведённой суммой: эта сумма ни по одному счёту не прошла.
        let fx = FxTable::new(FxSource::OwnerSupplied).with_rate(
            CurrencyCode::Usd,
            CurrencyCode::Rub,
            date!(2026 - 03 - 01),
            Dec::new(Decimal::new(805, 1)),
        );
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Usd);
        assert_eq!(
            convert(amount, CurrencyCode::Rub, date!(2026 - 03 - 01), &fx).unwrap(),
            Dec::new(Decimal::new(80_500, 1))
        );
    }
    #[test]
    fn the_board_reports_what_it_holds() {
        let mut board = PriceBoard::new();
        assert!(board.is_empty());
        assert_eq!(board.len(), 0);
        assert_eq!(board.iter().count(), 0);

        board.record(price(date!(2026 - 01 - 05), 100, PriceQuality::Executable));
        board.record(price(date!(2026 - 01 - 05), 200, PriceQuality::Executable));
        assert!(!board.is_empty());
        assert_eq!(board.len(), 2, "две разные бумаги — две цены");
        assert_eq!(board.iter().count(), 2);
    }

    #[test]
    fn every_code_is_stable() {
        // Коды уходят в API и в снапшоты отчётов: их изменение —
        // изменение публичного контракта, а не переименование.
        assert_eq!(PriceQuality::Executable.code(), "executable");
        assert_eq!(PriceQuality::PreviousClose.code(), "previous_close");
        assert_eq!(PriceQuality::CarriedForward.code(), "carried_forward");
        assert_eq!(PriceQuality::Stale.code(), "stale");
        assert_eq!(PriceQuality::OwnerEstimate.code(), "owner_estimate");
        assert_eq!(FxSource::CbrOfficial.code(), "cbr_official");
        assert_eq!(FxSource::OwnerSupplied.code(), "owner_supplied");
        assert_eq!(
            ValuationError::MissingPrice {
                instrument: InstrumentId::new_random()
            }
            .code(),
            "missing_price"
        );
        assert_eq!(
            ValuationError::MissingFxRate {
                from: CurrencyCode::Usd,
                to: CurrencyCode::Rub,
                date: date!(2026 - 01 - 01),
            }
            .code(),
            "missing_fx_rate"
        );
        assert_eq!(
            ValuationError::Numeric(NumericError::Overflow).code(),
            "numeric"
        );
    }
}
```

- [ ] **Шаг 3: Убедиться, что тесты падают**

```bash
nix develop -c cargo test -p iaam-core valuation
```

- [ ] **Шаг 4: Реализация модуля оценки**

```rust
//! Оценка позиций и перевод в валюту отчёта (§5.4, §6.1).
//!
//! На этапе 1 цена приходит событием `Valuation` с provenance и флагом
//! качества, а не из рыночных данных: `iaam-market` появляется в E3.
//! Схема от этого не меняется — меняется источник цены.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::ids::InstrumentId;
use crate::money::{CurrencyCode, Money};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Флаг качества оценки (§5.4). Молчаливая подстановка запрещена:
/// оценка всегда возвращает флаг.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PriceQuality {
    /// Исполнимая цена: доступный bid.
    Executable,
    /// Цена закрытия предыдущего торгового дня.
    PreviousClose,
    /// Перенос последней цены на нерабочий день.
    CarriedForward,
    /// Цена старше порога устаревания.
    Stale,
    /// Оценка владельца для неликвида.
    OwnerEstimate,
}

impl PriceQuality {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Executable => "executable",
            Self::PreviousClose => "previous_close",
            Self::CarriedForward => "carried_forward",
            Self::Stale => "stale",
            Self::OwnerEstimate => "owner_estimate",
        }
    }

    /// Оценка считается полной, только если цена исполнима или является
    /// ценой закрытия. Всё остальное помечает NAV как неполный (§5.4).
    #[must_use]
    pub const fn is_complete(self) -> bool {
        match self {
            Self::Executable | Self::PreviousClose => true,
            Self::CarriedForward | Self::Stale | Self::OwnerEstimate => false,
        }
    }
}

/// Цена за единицу инструмента на дату.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentPrice {
    pub instrument: InstrumentId,
    pub price: Dec,
    pub currency: CurrencyCode,
    pub quality: PriceQuality,
    pub as_of: Date,
}

/// Последние известные цены. Заполняется проекцией из событий `Valuation`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBoard {
    latest: BTreeMap<InstrumentId, InstrumentPrice>,
}

impl PriceBoard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Запись цены. Более ранняя оценка не затирает более позднюю:
    /// порядок применения событий задаёт `EffectiveOrder`, но событие
    /// оценки может прийти задним числом.
    pub fn record(&mut self, price: InstrumentPrice) {
        self.latest
            .entry(price.instrument)
            .and_modify(|existing| {
                if price.as_of >= existing.as_of {
                    *existing = price;
                }
            })
            .or_insert(price);
    }

    #[must_use]
    pub fn latest(&self, instrument: InstrumentId) -> Option<&InstrumentPrice> {
        self.latest.get(&instrument)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&InstrumentId, &InstrumentPrice)> {
        self.latest.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.latest.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.latest.is_empty()
    }
}

/// Источник курса. Входит в отчёт: без источника и типа курса ставка
/// доходности не определена (§6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FxSource {
    /// Официальный курс ЦБ РФ на дату. Появится в E3.
    CbrOfficial,
    /// Курс, названный владельцем.
    OwnerSupplied,
}

impl FxSource {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CbrOfficial => "cbr_official",
            Self::OwnerSupplied => "owner_supplied",
        }
    }
}

/// Таблица курсов на даты. Неизменяемый вход ядра: добыча курсов —
/// работа оболочки, ядро только применяет их и записывает источник.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxTable {
    source: FxSource,
    rates: BTreeMap<(CurrencyCode, CurrencyCode, Date), Dec>,
}

impl FxTable {
    #[must_use]
    pub fn new(source: FxSource) -> Self {
        Self {
            source,
            rates: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_rate(
        mut self,
        from: CurrencyCode,
        to: CurrencyCode,
        date: Date,
        rate: Dec,
    ) -> Self {
        self.rates.insert((from, to, date), rate);
        self
    }

    #[must_use]
    pub const fn source(&self) -> &FxSource {
        &self.source
    }

    /// Курс на дату. Единица для одинаковых валют — не подстановка, а
    /// тождество: рубль в рублях стоит рубль.
    #[must_use]
    pub fn rate(&self, from: CurrencyCode, to: CurrencyCode, date: Date) -> Option<Dec> {
        if from == to {
            return Some(Dec::one());
        }
        self.rates.get(&(from, to, date)).copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValuationError {
    #[error("нет цены инструмента {instrument:?} — стоимость позиции неизвестна")]
    MissingPrice { instrument: InstrumentId },
    #[error("нет курса {from:?}→{to:?} на {date}")]
    MissingFxRate {
        from: CurrencyCode,
        to: CurrencyCode,
        date: Date,
    },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

impl ValuationError {
    /// Машиночитаемый код для API (§13).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingPrice { .. } => "missing_price",
            Self::MissingFxRate { .. } => "missing_fx_rate",
            Self::Numeric(_) => "numeric",
        }
    }
}

/// Перевод проведённой суммы в валюту отчёта.
///
/// Возвращает **расчётную** величину, а не проведённую сумму: результат
/// умножения на курс не проходил ни по одному счёту (§3.4).
pub fn convert(
    amount: Money,
    to: CurrencyCode,
    date: Date,
    fx: &FxTable,
) -> Result<Dec, ValuationError> {
    let rate = fx
        .rate(amount.currency(), to, date)
        .ok_or(ValuationError::MissingFxRate {
            from: amount.currency(),
            to,
            date,
        })?;
    Ok(amount.to_calc_dec().checked_mul(rate)?)
}
```

- [ ] **Шаг 5: Вариант события `Valuation`**

В `crates/iaam-core/src/event/kind.rs` расширьте импорты и `enum`:

```rust
use crate::money::{CurrencyCode, Money, Quantity};
use crate::numeric::decimal::Dec;
use crate::valuation::PriceQuality;
```

```rust
    /// Оценка инструмента по цене за единицу (§5.4).
    ///
    /// Факт с provenance, а не расчёт: цену кто-то опубликовал или назвал,
    /// и без неё стоимость позиции неизвестна. На этапе 1 источник —
    /// владелец или внешний агент; в E3 тот же вариант заполняет
    /// `iaam-market`, и схема от этого не меняется.
    ///
    /// Денег не двигает: ног у события нет.
    Valuation {
        instrument: InstrumentId,
        price: Dec,
        currency: CurrencyCode,
        quality: PriceQuality,
    },
```

Компилятор потребует обработать вариант в `discriminant()` (`"valuation"`) и в `flow_endpoints()` (`FlowEndpoints::WithinAccount`, в общей ветке с `Income`, `Fee` и `Opening*`).

В `crates/iaam-core/src/event/mod.rs` — ветка диспетчера `validate_structure` и сама проверка:

```rust
            EventKind::Valuation { .. } => self.validate_valuation(name),
```

```rust
    /// Оценка не двигает ни денег, ни бумаг: это утверждение о цене.
    /// Нога здесь означала бы, что кто-то записал переоценку как факт
    /// движения, — а нереализованный результат движением не является.
    fn validate_valuation(&self, name: &'static str) -> Result<(), EventValidationError> {
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

Добавьте тест формы события рядом с существующими тестами `event::mod`:

```rust
    #[test]
    fn a_valuation_with_a_leg_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.kind = EventKind::Valuation {
            instrument: crate::ids::InstrumentId::new_random(),
            price: crate::numeric::decimal::Dec::one(),
            currency: CurrencyCode::Rub,
            quality: crate::valuation::PriceQuality::OwnerEstimate,
        };
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
        event.legs = vec![];
        assert!(event.validate_structure().is_ok());
    }
```

- [ ] **Шаг 6: Ужесточить структурную проверку и поднять версию схемы**

Добавление варианта — повод пересмотреть заслон формы события целиком. Ревью показало две дыры в прежней проверке, и обе записываются в append-only журнал навсегда:

1. **Нога не сверялась с событием.** Событие «куплено сто бумаг X», чья нога зачисляет одну бумагу Y на чужой счёт, проходило проверку. Инвариант проекции остановил бы отчёт, но исправить записанный факт можно только сторнированием — входной заслон обязан не пропускать противоречие, а не сохранять его.
2. **Величины не проверялись на знак.** Нулевое и отрицательное количество, нулевая цена и неположительная сумма сделки проходили. Отрицательная цена даёт отрицательную стоимость позиции и внешне правдоподобную доходность; отрицательное количество — это шорт, а шорты вне периметра (§11).

Замените `validate_trade`, `validate_opening_position` и `validate_valuation` в `crates/iaam-core/src/event/mod.rs`:

```rust
    /// Сделка: ровно одна денежная и ровно одна бумажная нога, денежная
    /// нога равна расчётной сумме со знаком, заданным направлением сделки.
    /// Сделка: ровно одна денежная и ровно одна бумажная нога, денежная
    /// нога равна расчётной сумме со знаком направления, **а бумажная
    /// нога говорит ровно то же, что тип события**.
    ///
    /// Последнее — не педантизм. Без этой сверки событие «куплено сто
    /// бумаг X», чья нога зачисляет одну бумагу Y на чужой счёт, проходит
    /// проверку и попадает в append-only журнал навсегда. Инвариант
    /// проекции остановит отчёт, но исправить записанный факт можно будет
    /// только сторнированием: входной заслон обязан не пропускать
    /// противоречие, а не сохранять его (§4.3, §4.8).
    fn validate_trade(
        &self,
        name: &'static str,
        side: TradeSide,
        declared: TradeDeclaration,
    ) -> Result<(), EventValidationError> {
        let TradeDeclaration {
            instrument,
            quantity,
            gross,
            fee,
            accrued_interest,
        } = declared;
        require_positive(name, "gross", gross.amount().raw())?;
        require_positive_quantity(name, "quantity", quantity)?;

        let cash = self.cash_legs();
        let cash_money = single_leg_money(name, &cash, "ровно одна денежная нога")?;
        require_own_account(name, cash[0].account, self.account)?;
        let expected = trade_settlement(side, gross, fee, accrued_interest)?;
        require_equal(name, cash_money, expected)?;

        let security = self.security_legs();
        if security.len() != 1 {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "ровно одна бумажная нога",
                found: security.len(),
            });
        }
        let leg = security[0];
        require_own_account(name, leg.account, self.account)?;
        require_same_instrument(name, leg.instrument, instrument)?;

        // Покупка увеличивает позицию, продажа уменьшает. Шорты вне
        // периметра (§11), поэтому знак задан направлением однозначно.
        let expected_quantity = match side {
            TradeSide::Buy => quantity,
            TradeSide::Sell => Quantity(quantity.0.checked_neg()?),
        };
        match leg.quantity {
            Some(actual) if actual == expected_quantity => Ok(()),
            _ => Err(EventValidationError::LegDoesNotMatchEvent {
                kind: name,
                field: "quantity",
            }),
        }
    }

    /// Восстановленная позиция описывает только бумагу: денег в этом
    /// событии не двигалось, иначе восстановление остатка выглядело бы
    /// как реальная покупка (§10.7).
```

```rust
    fn validate_opening_position(
        &self,
        name: &'static str,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> Result<(), EventValidationError> {
        require_positive_quantity(name, "quantity", quantity)?;
        let cash = self.cash_legs();
        if !cash.is_empty() {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "ни одной денежной ноги",
                found: cash.len(),
            });
        }
        let security = self.security_legs();
        if security.len() != 1 {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "ровно одна бумажная нога",
                found: security.len(),
            });
        }
        let leg = security[0];
        require_own_account(name, leg.account, self.account)?;
        require_same_instrument(name, leg.instrument, instrument)?;
        match leg.quantity {
            Some(actual) if actual == quantity => Ok(()),
            _ => Err(EventValidationError::LegDoesNotMatchEvent {
                kind: name,
                field: "quantity",
            }),
        }
    }
```

```rust
    /// Оценка не двигает ни денег, ни бумаг: это утверждение о цене.
    /// Нога здесь означала бы, что кто-то записал переоценку как факт
    /// движения, — а нереализованный результат движением не является.
    fn validate_valuation(
        &self,
        name: &'static str,
        price: crate::numeric::decimal::Dec,
    ) -> Result<(), EventValidationError> {
        // Нулевая и отрицательная цена дают отрицательную стоимость
        // позиции и внешне правдоподобную доходность. Бумага может
        // обесцениться до нуля — но это факт делистинга (E3), а не цена.
        if !price.is_positive() {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "price",
                value: price.inner().to_string(),
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

Вспомогательные функции и структура объявленных условий сделки (`expect_single_security_leg` при этом удаляется — его работу теперь делают обе проверки по-своему):

```rust
/// Объявленные условия сделки. Отдельная структура, потому что порог
/// `too-many-arguments-threshold = 6` действует, а подавлять линт нельзя.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TradeDeclaration {
    instrument: InstrumentId,
    quantity: Quantity,
    gross: Money,
    fee: Option<Money>,
    accrued_interest: Option<Money>,
}

fn require_positive(
    name: &'static str,
    field: &'static str,
    value: i64,
) -> Result<(), EventValidationError> {
    if value > 0 {
        Ok(())
    } else {
        Err(EventValidationError::NonPositive {
            kind: name,
            field,
            value: value.to_string(),
        })
    }
}

fn require_positive_quantity(
    name: &'static str,
    field: &'static str,
    quantity: Quantity,
) -> Result<(), EventValidationError> {
    if quantity.0.is_positive() {
        Ok(())
    } else {
        Err(EventValidationError::NonPositive {
            kind: name,
            field,
            value: quantity.0.inner().to_string(),
        })
    }
}

/// Нога обязана лежать на счёте события: иначе одно событие двигало бы
/// бумаги на чужом счёте, а лоты считались бы по своему.
fn require_own_account(
    name: &'static str,
    leg: AccountId,
    event: AccountId,
) -> Result<(), EventValidationError> {
    if leg == event {
        Ok(())
    } else {
        let _ = name;
        Err(EventValidationError::WrongAccount { expected: event })
    }
}

fn require_same_instrument(
    name: &'static str,
    leg: Option<InstrumentId>,
    declared: InstrumentId,
) -> Result<(), EventValidationError> {
    match leg {
        Some(actual) if actual == declared => Ok(()),
        _ => Err(EventValidationError::LegDoesNotMatchEvent {
            kind: name,
            field: "instrument",
        }),
    }
}
```

Добавьте в `EventValidationError` два варианта и преобразование числовой ошибки:

```rust
    #[error(
        "для {kind} нога не соответствует событию по полю {field}: \
         событие говорит одно, нога другое"
    )]
    LegDoesNotMatchEvent {
        kind: &'static str,
        field: &'static str,
    },
    #[error("для {kind} величина {field} должна быть положительной, получено {value}")]
    NonPositive {
        kind: &'static str,
        field: &'static str,
        value: String,
    },
    #[error(transparent)]
    Numeric(#[from] crate::numeric::NumericError),
```

И поднимите версию схемы — вариант события добавлен после того, как версия 1 была заморожена:

```rust
/// Текущая версия схемы события.
///
/// Версия 2 отличается от версии 1 добавленным вариантом
/// [`EventKind::Valuation`]. Уже записанные факты версии 1 читаются
/// без изменений — новый вариант в них просто не встречается, — но
/// программа, знающая только версию 1, не разберёт новое событие.
/// Оставить прежний номер значило бы, что одна версия обозначает две
/// несовместимые схемы (§4.1).
pub const SCHEMA_VERSION: u32 = 2;
```

**Существующие тесты первого плана после этого падают** — и это правильный сигнал, а не помеха: они строили сделки с нулевым количеством и ногой, не связанной с событием. Почините фикстуры (количество положительное, у продажи нога отрицательная, инструмент и счёт совпадают с событием), а не проверку.

- [ ] **Шаг 7: Тесты на каждый новый отказ**

Мутационный заслон показал, что без этого шага все новые проверки можно заменить на `Ok(())` и ни один тест не заметит — проверено исполнением, выживших мутантов было семь. Добавьте в `mod tests` файла `crates/iaam-core/src/event/mod.rs`:

```rust
    // --- Сверка ноги с событием и знаки величин ---
    //
    // Каждый отказ проверяется отдельно: без этого мутационный заслон
    // показывает, что проверку можно заменить на `Ok(())` и ни один
    // тест не заметит (проверено — так и было).

    fn buy_with(acc: AccountId, instrument: InstrumentId, quantity: Quantity, leg: Leg) -> Event {
        event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity,
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![Leg::cash(acc, rub(-5_000_000)), leg],
            acc,
        )
    }

    #[test]
    fn a_trade_of_zero_quantity_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            Quantity::zero(),
            security_leg(acc, instrument, Quantity::zero()),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_trade_of_negative_quantity_is_rejected() {
        // Отрицательное количество в покупке — это шорт, а шорты вне
        // периметра (§11): их денежный эффект сохраняется отдельным
        // типом события, а не отрицательной сделкой.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(-10),
            security_leg(acc, instrument, qty(-10)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_trade_of_zero_value_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(10),
                gross: rub(0),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(0)),
                security_leg(acc, instrument, qty(10)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive { field: "gross", .. })
        ));
    }

    #[test]
    fn a_security_leg_of_another_instrument_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let other = InstrumentId::new_random();
        let ev = buy_with(acc, instrument, qty(10), security_leg(acc, other, qty(10)));
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "instrument",
                ..
            })
        ));
    }

    #[test]
    fn a_security_leg_on_another_account_is_rejected() {
        let acc = AccountId::new_random();
        let stranger = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(10),
            security_leg(stranger, instrument, qty(10)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongAccount { .. })
        ));
    }

    #[test]
    fn a_cash_leg_on_another_account_is_rejected() {
        let acc = AccountId::new_random();
        let stranger = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(10),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(stranger, rub(-5_000_000)),
                security_leg(acc, instrument, qty(10)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongAccount { .. })
        ));
    }

    #[test]
    fn a_leg_quantity_differing_from_the_event_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(10),
            security_leg(acc, instrument, qty(9)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_purchase_whose_leg_reduces_the_position_is_rejected() {
        // Знак задан направлением сделки: покупка увеличивает позицию.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(10),
            security_leg(acc, instrument, qty(-10)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_sale_whose_leg_increases_the_position_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(10),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(acc, rub(5_000_000)),
                security_leg(acc, instrument, qty(10)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn an_opening_position_disagreeing_with_its_leg_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let other = InstrumentId::new_random();

        let wrong_quantity = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(10),
                cost_basis: None,
            },
            vec![security_leg(acc, instrument, qty(11))],
            acc,
        );
        assert!(matches!(
            wrong_quantity.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));

        let wrong_instrument = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(10),
                cost_basis: None,
            },
            vec![security_leg(acc, other, qty(10))],
            acc,
        );
        assert!(matches!(
            wrong_instrument.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "instrument",
                ..
            })
        ));

        let zero = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity::zero(),
                cost_basis: None,
            },
            vec![security_leg(acc, instrument, Quantity::zero())],
            acc,
        );
        assert!(matches!(
            zero.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_valuation_at_zero_or_below_is_rejected() {
        // Нулевая цена даёт нулевую стоимость позиции и правдоподобную
        // доходность. Обесценившаяся бумага — это факт делистинга (E3),
        // а не цена.
        let acc = AccountId::new_random();
        for price in [
            crate::numeric::decimal::Dec::zero(),
            crate::numeric::decimal::Dec::new(rust_decimal::Decimal::from(-1)),
        ] {
            let ev = event(
                EventKind::Valuation {
                    instrument: InstrumentId::new_random(),
                    price,
                    currency: CurrencyCode::Rub,
                    quality: crate::valuation::PriceQuality::OwnerEstimate,
                },
                vec![],
                acc,
            );
            assert!(matches!(
                ev.validate_structure(),
                Err(EventValidationError::NonPositive { field: "price", .. })
            ));
        }
    }
```

Тестовый помощник `security_leg` при этом получает параметр количества, а `qty` добавляется рядом:

```rust
    fn qty(units: i64) -> Quantity {
        Quantity(crate::numeric::decimal::Dec::new(
            rust_decimal::Decimal::from(units),
        ))
    }

    fn security_leg(account: AccountId, instrument: InstrumentId, quantity: Quantity) -> Leg {
        Leg::security(account, CustodyId::new_random(), instrument, quantity)
    }
```

- [ ] **Шаг 8: Зелёная сборка и обновление документа необратимого ядра**

```bash
nix develop -c cargo test -p iaam-core
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

В `docs/irreversible-core.md`, в таблицу «Дополнительно зафиксировано», добавьте строку:

```markdown
| Цена — факт с источником и качеством, а не параметр запроса | `EventKind::Valuation`, `valuation::PriceQuality` |
| Нога сверяется с событием по инструменту, счёту и количеству со знаком | `Event::validate_trade`, `Event::validate_opening_position` |
| Количество, цена и сумма сделки обязаны быть положительными | `EventValidationError::NonPositive` |
| Версия схемы события — 2: вариант `Valuation` добавлен после заморозки версии 1 | `event::SCHEMA_VERSION` |
```

Тест `an_event_carries_the_current_schema_version` из первого плана закреплял `SCHEMA_VERSION == 1` — правьте ожидание на 2, это и есть смысл шага. Фикстуры сделок и восстановленных позиций в `event/mod.rs` и в `contour.rs` строились с нулевым количеством и ногой, не связанной с событием; чините фикстуры, а не проверку.

- [ ] **Шаг 9: Коммит**

```bash
git add crates/iaam-core/src docs/irreversible-core.md
git commit -m "feat(core): оценка позиций, курсы, событие Valuation и схема v2 (iaam-1fk)"
```

---

## Задача 5: Снимок, `project`/`advance` и инварианты

Самая крупная задача плана: три файла и две независимые группы тестов. Разделять её нельзя — `fold` вызывает проверку инвариантов, а снимок без проверки инвариантов является ровно тем, что §15.2 запрещает выдавать наружу.

**Files:**
- Create: `crates/iaam-core/src/projection/state.rs`
- Create: `crates/iaam-core/src/projection/invariants.rs`
- Modify: `crates/iaam-core/src/projection/mod.rs` — `Snapshot`, `project`, `advance`
- Modify: `crates/iaam-core/Cargo.toml` — `sha2 = "0.11"`, `ciborium = "0.2"`

**Interfaces:**
- Consumes: `Balances`, `LotBook`, `FlowLog`, `PriceBoard`, `event::correction::resolve`.
- Produces: `projection::{PROJECTION_VERSION, ProjectionContext, ProjectionError, Snapshot, SnapshotParts, Projection, project, advance}`; `projection::state::{LedgerState, Coverage, StateHash}`; `projection::invariants::{check, InvariantReport, CheckedInvariant, InvariantViolation}`.

**Acceptance Criteria:**
- `advance` принимает **тот же срез журнала**, что и `project`, и сам решает, что уже свёрнуто.
- `advance(снимок, весь журнал)` даёт тот же отпечаток, что `project(весь журнал)`.
- Порядок событий во входном срезе не влияет на результат.
- Снимок, собранный мимо ядра, отвергается по отпечатку состояния.
- **Событие, добавленное задним числом до границы снимка, обнаруживается** и требует полного пересчёта.
- Сторнирование события внутри снимка обнаруживается тем же механизмом.
- Снимок другого контура или другой версии правила отвергается.
- Отпечаток состояния покрывает состояние целиком, а не перечень полей, выбранный вручную.
- Нарушение инварианта отменяет проекцию целиком и отличимо от неполноты данных (`is_invariant_violation`).
- Отчёт перечисляет, **что именно** проверено, а не только факт проверки.

> **Исправление при исполнении (2026-08-23).** Мутационный прогон по трём модулям задачи дал **девятнадцать** выживших: `state.rs` не имел ни одного теста вовсе (счётчики `Coverage` можно было заменить на `*=`, границы истории — на `None`, `Display` отпечатка — на пустую строку, `feed_date` — на пустое тело), а в `mod.rs` не проверялись `projection_version`, `through` и `is_invariant_violation`. План рассчитывал, что их закроют тесты задач 8 и 9; это ровно та ставка, которую §15.7 запрещает делать. Добавлены шесть тестов в `state.rs` и два в `mod.rs`.
>
> Два мутанта потребовали не теста, а понимания: аксессор `projection_version` возвращает поле снимка, а не константу кода — иначе снимок чужой версии стал бы пригодным; проверяется через `Snapshot::restore` с чужой версией. А тождество сохранения стоимости проверялось только там, где ничего не продано: при нулевой списанной стоимости сумма неотличима от разности, и испорченный знак проходил незамеченным. Добавлен тест с частичной продажей, где обе части ненулевые и различны.

**Два разных отпечатка, и оба обязательны.**

| Отпечаток | На какой вопрос отвечает | Что ловит |
|---|---|---|
| `fingerprint` состояния | «то ли это состояние» | снимок, собранный или изменённый мимо ядра |
| `prefix_digest` журнала | «те ли это события» | событие, добавленное или сторнированное **до** границы снимка |

**Почему одного отпечатка состояния мало — и почему `advance` берёт весь срез.** Первая редакция этого модуля принимала «пачку новых событий», а оболочка отбирала всё, что позже границы снимка. Событие, импортированное задним числом с датой внутри уже свёрнутого периода, при таком отборе не попадало ни в `advance`, ни в полный пересчёт: граница снимка не менялась, состояние снимка было самосогласованным, отпечаток совпадал, и `advance` возвращал устаревшее состояние. Результат — правдоподобные, но неверные остатки, лоты и доходность при полностью зелёной сборке. Дефект найден состязательным ревью и закрыт двумя изменениями: срез передаётся целиком, а снимок несёт отпечаток свёрнутого префикса.

**Почему отпечаток состояния считается по сериализации, а не по перечню полей.** Перечень, написанный руками, был неполон: в него не вошли реализованный результат, стоимость приобретений и списаний, версия правила и границы истории. Отпечаток, покрывающий часть состояния, обещает больше, чем даёт. Сериализация покрывает всё по построению; цена — зависимость ядра от `ciborium` и привязка отпечатка к формату, что решается версией в префиксе хеша.

**Сторнирование внутри снимка** отдельной ветки не требует: оно удаляет событие из действующего набора, то есть меняет префикс, и ловится тем же сравнением.

- [ ] **Шаг 1: Написать падающие тесты**

В конец `crates/iaam-core/src/projection/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourId, ContourVersion};
    use crate::event::Relation;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::rules::RuleRegistry;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    fn contour_of(account: AccountId) -> ContourDefinition {
        ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account])
    }

    fn deposits(account: AccountId) -> Vec<Event> {
        (1..=4)
            .map(|i| {
                let amount = rub(i64::from(i) * 10_000);
                event_with(
                    account,
                    date!(2025 - 01 - 01) + time::Duration::days(i64::from(i)),
                    i,
                    EventKind::CashIn { amount },
                    vec![Leg::cash(account, amount)],
                )
            })
            .collect()
    }

    #[test]
    fn advancing_a_snapshot_equals_a_full_recompute() {
        // Инкрементальный путь обязан совпадать с эталонным: снимок —
        // оптимизация, а не другая модель (§3.1).
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);

        let full = project(&events, &ctx).unwrap();
        let head = project(&events[..2], &ctx).unwrap();
        // Срез передаётся целиком: ядро само решает, что уже свёрнуто.
        let advanced = advance(head.snapshot(), &events, &ctx).unwrap();

        assert_eq!(
            full.snapshot().fingerprint(),
            advanced.snapshot().fingerprint()
        );
        assert_eq!(full.snapshot().through(), advanced.snapshot().through());
        assert_eq!(
            full.snapshot().prefix_digest(),
            advanced.snapshot().prefix_digest()
        );
    }

    #[test]
    fn import_order_does_not_change_the_projection() {
        // Свойство §15.3: проекция зависит от EffectiveOrder, а не от того,
        // в каком порядке загрузили файлы.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let mut shuffled = events.clone();
        shuffled.reverse();

        assert_eq!(
            project(&events, &ctx).unwrap().snapshot().fingerprint(),
            project(&shuffled, &ctx).unwrap().snapshot().fingerprint()
        );
    }

    #[test]
    fn a_tampered_snapshot_is_rejected() {
        // Снимок хранит оболочка; ядро не обязано ей верить.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let snapshot = project(&events, &ctx).unwrap().into_snapshot();

        // Оболочка собрала снимок из частей и подставила чужое состояние,
        // оставив прежний отпечаток.
        let other = project(&events[..2], &ctx).unwrap().into_snapshot();
        let mut parts = snapshot.into_parts();
        parts.state = other.into_parts().state;
        let tampered = Snapshot::restore(parts);

        assert!(matches!(
            advance(&tampered, &events, &ctx),
            Err(ProjectionError::SnapshotFingerprintMismatch)
        ));
    }

    #[test]
    fn an_event_inserted_before_the_snapshot_boundary_forces_a_full_recompute() {
        // Самый опасный случай: событие пришло задним числом и встало
        // ДО границы снимка. Оно не меняет ни границу, ни состояние
        // снимка, поэтому наивное «взять всё, что позже границы» молча
        // потеряло бы его — и выдало бы правдоподобные, но неверные
        // остатки. Ядро обязано это заметить.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let snapshot = project(&events, &ctx).unwrap().into_snapshot();

        // Забытое пополнение с датой в середине уже свёрнутого периода.
        let forgotten = event_with(
            account,
            date!(2025 - 01 - 02),
            99,
            EventKind::CashIn { amount: rub(777) },
            vec![Leg::cash(account, rub(777))],
        );
        let mut with_backdated = events.clone();
        with_backdated.push(forgotten);

        let error = advance(&snapshot, &with_backdated, &ctx).unwrap_err();
        assert!(
            matches!(error, ProjectionError::PrefixChanged { .. }),
            "ожидалось PrefixChanged, получено {error}"
        );

        // Полный пересчёт видит забытое событие.
        let recomputed = project(&with_backdated, &ctx).unwrap();
        assert_eq!(
            recomputed
                .state()
                .balances()
                .cash(account, CurrencyCode::Rub),
            Some(rub(10_000 + 20_000 + 30_000 + 40_000 + 777))
        );
    }

    #[test]
    fn reversing_an_event_inside_the_snapshot_forces_a_full_recompute() {
        // Сторнирование удаляет событие из действующего набора, то есть
        // меняет уже свёрнутый префикс. Вычесть его из агрегата нельзя,
        // и притвориться, что можно, значит тихо потерять исправление.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let snapshot = project(&events[..2], &ctx).unwrap().into_snapshot();

        let mut with_reversal = events.clone();
        with_reversal[3].relation = Relation::Reversal {
            target: events[0].id,
        };

        assert!(matches!(
            advance(&snapshot, &with_reversal, &ctx),
            Err(ProjectionError::PrefixChanged { .. })
        ));
    }

    #[test]
    fn a_snapshot_of_another_contour_is_rejected() {
        let account = AccountId::new_random();
        let rules = RuleRegistry::with_defaults();
        let first = contour_of(account);
        let second = contour_of(account);
        let events = deposits(account);
        let snapshot = project(
            &events,
            &ProjectionContext {
                contour: &first,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            },
        )
        .unwrap()
        .into_snapshot();

        assert!(matches!(
            advance(
                &snapshot,
                &events,
                &ProjectionContext {
                    contour: &second,
                    rules: &rules,
                    lot_rule: LotRuleVersion(1),
                }
            ),
            Err(ProjectionError::SnapshotContourMismatch { .. })
        ));
    }

    #[test]
    fn a_leg_contradicting_the_event_never_reaches_the_projection() {
        // Событие, чья нога говорит не то, что тип события, отклоняется
        // входным заслоном — до того, как попадёт в append-only журнал.
        // Инвариант «сумма лотов равна позиции» остаётся вторым рубежом:
        // он ловит то же расхождение, если оно придёт из хранилища,
        // наполненного в обход приёмки (§15.2).
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let event = event_with(
            account,
            date!(2025 - 04 - 01),
            1,
            EventKind::Trade {
                side: crate::event::kind::TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(1_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, rub(-1_000_000)),
                // Нога говорит 90 бумаг, тип события — 100.
                Leg::security(account, CustodyId::new_random(), instrument, qty(90)),
            ],
        );

        // Заслон формы события отклоняет противоречие сам по себе.
        assert!(matches!(
            event.validate_structure(),
            Err(crate::event::EventValidationError::LegDoesNotMatchEvent { .. })
        ));

        // И проекция такого события не строит: она перепроверяет форму,
        // потому что не обязана верить тому, что лежит в хранилище.
        let error = project(&[event], &ctx).unwrap_err();
        assert!(error.is_invariant_violation(), "{error}");
        assert_eq!(error.code(), "invariant");
    }
}
```

- [ ] **Шаг 2: Убедиться, что тесты падают**

```bash
nix develop -c cargo test -p iaam-core projection::tests
```

- [ ] **Шаг 3: Состояние и отпечаток**

`crates/iaam-core/src/projection/state.rs`:

```rust
//! Состояние проекции и его отпечаток.
//!
//! Отпечаток нужен не для целостности хранилища, а для того, чтобы
//! `advance` мог отказаться продвигать снимок, который кто-то собрал
//! или изменил мимо ядра (§3.1). Считается по упорядоченным структурам:
//! порядок обхода `BTreeMap` детерминирован, поэтому один и тот же
//! журнал всегда даёт один и тот же отпечаток.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::Date;

use super::balances::Balances;
use super::flows::FlowLog;
use super::lots::LotBook;
use crate::event::{Confidence, Event};
use crate::ids::AccountId;
use crate::valuation::PriceBoard;

/// Отпечаток состояния: SHA-256 по упорядоченному обходу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateHash([u8; 32]);

impl StateHash {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for StateHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Что видел журнал: границы истории и доля непроверенного (§10.7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    events_applied: u64,
    first_event: Option<Date>,
    last_event: Option<Date>,
    /// Счета, история которых начата восстановленным остатком,
    /// а не наблюдаемой операцией.
    restored_accounts: BTreeSet<AccountId>,
    /// События, чьё значение записано как оценка, а не как известный факт
    /// (§4.9). Это **не** уровень сверки: сверка появляется в E2 и живёт
    /// отдельным утверждением о счёте и интервале, а не полем события.
    estimated_events: u64,
}

impl Coverage {
    #[must_use]
    pub const fn events_applied(&self) -> u64 {
        self.events_applied
    }

    /// Дата первого учтённого события. Отчёт обязан её показывать:
    /// «XIRR посчитан с 01.03.2024, ранее данных нет» (§10.7).
    #[must_use]
    pub const fn first_event(&self) -> Option<Date> {
        self.first_event
    }

    #[must_use]
    pub const fn last_event(&self) -> Option<Date> {
        self.last_event
    }

    #[must_use]
    pub fn restored_accounts(&self) -> &BTreeSet<AccountId> {
        &self.restored_accounts
    }

    #[must_use]
    pub const fn estimated_events(&self) -> u64 {
        self.estimated_events
    }

    fn observe(&mut self, event: &Event) {
        self.events_applied += 1;
        if let Some(date) = event.dates.effective_date() {
            self.first_event = Some(match self.first_event {
                Some(existing) => existing.min(date),
                None => date,
            });
            self.last_event = Some(match self.last_event {
                Some(existing) => existing.max(date),
                None => date,
            });
        }
        match event.confidence {
            Confidence::Known => {}
            Confidence::Estimated | Confidence::Unknown => self.estimated_events += 1,
        }
        if matches!(
            event.kind,
            crate::event::kind::EventKind::OpeningCash { .. }
                | crate::event::kind::EventKind::OpeningPosition { .. }
        ) {
            self.restored_accounts.insert(event.account);
        }
    }
}

/// Полное состояние проекции.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerState {
    balances: Balances,
    book: LotBook,
    flows: FlowLog,
    prices: PriceBoard,
    coverage: Coverage,
}

impl LedgerState {
    #[must_use]
    pub fn new(book: LotBook) -> Self {
        Self {
            balances: Balances::new(),
            book,
            flows: FlowLog::new(),
            prices: PriceBoard::new(),
            coverage: Coverage::default(),
        }
    }

    #[must_use]
    pub const fn balances(&self) -> &Balances {
        &self.balances
    }

    #[must_use]
    pub const fn book(&self) -> &LotBook {
        &self.book
    }

    #[must_use]
    pub const fn flows(&self) -> &FlowLog {
        &self.flows
    }

    #[must_use]
    pub const fn prices(&self) -> &PriceBoard {
        &self.prices
    }

    #[must_use]
    pub const fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    pub(super) const fn parts_mut(&mut self) -> (&mut Balances, &mut LotBook, &mut FlowLog) {
        (&mut self.balances, &mut self.book, &mut self.flows)
    }

    pub(super) const fn prices_mut(&mut self) -> &mut PriceBoard {
        &mut self.prices
    }

    pub(super) fn observe(&mut self, event: &Event) {
        self.coverage.observe(event);
    }

    /// Отпечаток состояния.
    ///
    /// Считается по **канонической сериализации всего состояния**, а не по
    /// перечислению полей вручную. Ручное перечисление проверено ревью
    /// и оказалось неполным: в него не попали реализованный результат,
    /// стоимость приобретений и списаний, версия правила списания
    /// и границы истории. Отпечаток, покрывающий часть состояния, обещает
    /// больше, чем даёт: снимок с изменённым непокрытым полем прошёл бы
    /// проверку. Сериализация покрывает всё, что состояние содержит,
    /// по построению.
    ///
    /// CBOR, а не JSON, по той же причине, что и в хранилище: карты
    /// состояния имеют составные ключи, которые JSON не представляет.
    /// Обход `BTreeMap` детерминирован, `Decimal` сериализуется точно,
    /// двоичной плавающей точки в состоянии нет — поэтому один и тот же
    /// журнал всегда даёт один и тот же отпечаток.
    #[must_use]
    pub fn fingerprint(&self) -> StateHash {
        let mut body = Vec::new();
        // Отказ сериализации здесь невозможен: пишем в вектор в памяти,
        // а состояние состоит из типов, у которых `Serialize` выведен.
        // Тем не менее отпечаток не подменяется заглушкой: одинаковый
        // отпечаток у разных состояний хуже, чем паника.
        ciborium::into_writer(self, &mut body)
            .unwrap_or_else(|error| panic!("состояние не сериализуется: {error}"));
        let mut hasher = Sha256::new();
        hasher.update(b"iaam/ledger-state/v2");
        hasher.update(body);
        StateHash(hasher.finalize().into())
    }
}

/// Отпечаток префикса журнала, свёрнутого в снимок.
///
/// Отвечает на вопрос, на который отпечаток состояния не отвечает:
/// «те ли это события». Событие, добавленное задним числом **до** границы
/// снимка, не меняет ни границу, ни состояние снимка — и без этой
/// проверки просто исчезло бы из расчёта.
#[must_use]
pub fn prefix_digest(events: &[&Event]) -> StateHash {
    let mut hasher = Sha256::new();
    hasher.update(b"iaam/journal-prefix/v1");
    hasher.update(
        u64::try_from(events.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for event in events {
        hasher.update(event.id.inner().as_bytes());
        feed_date(&mut hasher, event.order.date());
        hasher.update(event.order.sequence().to_be_bytes());
        // Содержимое, а не только идентичность: подменённая запись
        // в хранилище обязана поменять отпечаток.
        hasher.update(event.provenance.raw_hash().as_str().as_bytes());
    }
    StateHash(hasher.finalize().into())
}

fn feed_date(hasher: &mut Sha256, date: Date) {
    hasher.update(date.year().to_be_bytes());
    hasher.update(date.ordinal().to_be_bytes());
}
```

Добавьте зависимости в `crates/iaam-core/Cargo.toml` (изменение файла политики — потребуется метка `policy-change` на PR, см. `scripts/check-diff-lint.sh`):

```toml
# Каноническая сериализация состояния для отпечатка. CBOR, потому что
# карты состояния имеют составные ключи: JSON их не берёт.
ciborium = "0.2"
sha2 = "0.11"
```

- [ ] **Шаг 4: Инварианты**

`crates/iaam-core/src/projection/invariants.rs`:

```rust
//! Инварианты как исполняемый код (§15.2).
//!
//! Нарушение инварианта — **не** то же самое, что неполные данные.
//! Неполнота даёт нормальный результат плюс блок качества данных;
//! нарушение инварианта отменяет отчёт целиком: возвращать число
//! с предупреждением после доказанного нарушения тождества нельзя.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::lots::LotKey;
use super::state::LedgerState;
use crate::event::{Event, EventValidationError};
use crate::ids::EventId;
use crate::money::Money;
use crate::numeric::NumericError;

/// Проверенный инвариант. Отчёт показывает, что именно было проверено:
/// «инварианты выполнены» без перечисления неотличимо от «не проверялось».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckedInvariant {
    /// Структура каждого события соответствует его типу.
    EventStructure { events: usize },
    /// Сумма лотов равна позиции по каждому инструменту.
    LotsMatchPositions { pairs: usize },
    /// Приобретено = осталось + списано, в минимальных единицах, точно.
    BasisConserved { pairs: usize },
    /// Ни один внешний поток не имеет нулевой суммы.
    FlowsNonZero { flows: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvariantViolation {
    #[error("событие {event:?} не проходит структурную проверку: {source}")]
    EventStructure {
        event: EventId,
        #[source]
        source: EventValidationError,
    },
    #[error(
        "сумма лотов по {key:?} равна {lots}, позиция по ногам событий — {position}; \
         две независимые дороги к одному количеству разошлись"
    )]
    LotsDoNotMatchPosition {
        key: LotKey,
        lots: String,
        position: String,
    },
    #[error(
        "стоимость по {key:?} не сохраняется: приобретено {acquired}, \
         осталось {remaining}, списано {released}"
    )]
    BasisNotConserved {
        key: LotKey,
        acquired: i64,
        remaining: i64,
        released: i64,
    },
    #[error("внешний поток события {event:?} имеет нулевую сумму")]
    ZeroExternalFlow { event: EventId },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

impl InvariantViolation {
    /// Машиночитаемый код. Нарушение инварианта попадает в лог
    /// с идентификатором корреляции, а наружу уходит `not_computable` (§15.2).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EventStructure { .. } => "event_structure",
            Self::LotsDoNotMatchPosition { .. } => "lots_do_not_match_position",
            Self::BasisNotConserved { .. } => "basis_not_conserved",
            Self::ZeroExternalFlow { .. } => "zero_external_flow",
            Self::Numeric(_) => "numeric",
        }
    }
}

/// Отчёт о проверенных инвариантах.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantReport {
    checked: Vec<CheckedInvariant>,
}

impl InvariantReport {
    #[must_use]
    pub fn checked(&self) -> &[CheckedInvariant] {
        &self.checked
    }
}

/// Проверка всех инвариантов состояния.
///
/// Ядро не доверяет входу: события уже проверялись при записи, но
/// проекция строится и по данным, пришедшим из хранилища, а хранилище
/// могли наполнить в обход приёмки.
pub fn check(
    state: &LedgerState,
    events: &[&Event],
) -> Result<InvariantReport, InvariantViolation> {
    let mut checked = Vec::new();

    for event in events {
        event
            .validate_structure()
            .map_err(|source| InvariantViolation::EventStructure {
                event: event.id,
                source,
            })?;
    }
    checked.push(CheckedInvariant::EventStructure {
        events: events.len(),
    });

    let mut pairs = 0;
    let mut basis_pairs = 0;
    for (key, entry) in state.book().iter() {
        let lots = entry.quantity()?;
        let position = state.balances().quantity_of(key.account, key.instrument)?;
        if lots != position {
            return Err(InvariantViolation::LotsDoNotMatchPosition {
                key: *key,
                lots: format!("{:?}", lots.0.inner()),
                position: format!("{:?}", position.0.inner()),
            });
        }
        pairs += 1;

        if let Some(acquired) = entry.acquired_basis() {
            let remaining = entry
                .remaining_basis()
                .map_err(|_| InvariantViolation::BasisNotConserved {
                    key: *key,
                    acquired: acquired.amount().raw(),
                    remaining: 0,
                    released: 0,
                })?
                .unwrap_or_else(|| Money::zero(acquired.currency()));
            let released = entry
                .released_basis()
                .unwrap_or_else(|| Money::zero(acquired.currency()));
            let sum = remaining.amount().raw() + released.amount().raw();
            if sum != acquired.amount().raw() {
                return Err(InvariantViolation::BasisNotConserved {
                    key: *key,
                    acquired: acquired.amount().raw(),
                    remaining: remaining.amount().raw(),
                    released: released.amount().raw(),
                });
            }
            basis_pairs += 1;
        }
    }
    checked.push(CheckedInvariant::LotsMatchPositions { pairs });
    checked.push(CheckedInvariant::BasisConserved { pairs: basis_pairs });

    for flow in state.flows().external() {
        if flow.amount.is_zero() {
            return Err(InvariantViolation::ZeroExternalFlow { event: flow.event });
        }
    }
    checked.push(CheckedInvariant::FlowsNonZero {
        flows: state.flows().external().len(),
    });

    Ok(InvariantReport { checked })
}
```

- [ ] **Шаг 5: Тесты инвариантов**

Отчёт обязан перечислять, **что** проверено и с какими количествами: «инварианты выполнены» без чисел неотличимо от «не проверялось». Мутационный заслон это подтверждает — без такого теста счётчики `+=` можно заменить на `*=`, и отчёт будет показывать нули.

В конец `crates/iaam-core/src/projection/invariants.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourDefinition, ContourId, ContourVersion};
    use crate::event::kind::{EventKind, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::projection::{ProjectionContext, project};
    use crate::rules::{LotRuleVersion, RuleRegistry};
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    /// Отчёт обязан перечислять, ЧТО проверено, и с какими количествами:
    /// «инварианты выполнены» без чисел неотличимо от «не проверялось».
    #[test]
    fn the_report_names_what_was_checked_and_how_much() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = vec![
            event_with(
                account,
                date!(2025 - 01 - 01),
                1,
                EventKind::CashIn {
                    amount: rub(10_000_000),
                },
                vec![Leg::cash(account, rub(10_000_000))],
            ),
            event_with(
                account,
                date!(2025 - 02 - 01),
                2,
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(100),
                    gross: rub(900_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(account, rub(-900_000)),
                    Leg::security(account, CustodyId::new_random(), instrument, qty(100)),
                ],
            ),
        ];

        let projection = project(&events, &ctx).unwrap();
        let report = projection.invariants();
        assert!(!report.checked().is_empty());
        assert_eq!(
            report.checked(),
            &[
                CheckedInvariant::EventStructure { events: 2 },
                CheckedInvariant::LotsMatchPositions { pairs: 1 },
                CheckedInvariant::BasisConserved { pairs: 1 },
                CheckedInvariant::FlowsNonZero { flows: 1 },
            ]
        );
    }

    #[test]
    fn an_empty_journal_still_reports_what_it_checked() {
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let projection = project(
            &[],
            &ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            },
        )
        .unwrap();
        assert_eq!(
            projection.invariants().checked(),
            &[
                CheckedInvariant::EventStructure { events: 0 },
                CheckedInvariant::LotsMatchPositions { pairs: 0 },
                CheckedInvariant::BasisConserved { pairs: 0 },
                CheckedInvariant::FlowsNonZero { flows: 0 },
            ]
        );
    }

    #[test]
    fn every_violation_has_a_machine_readable_code() {
        let key = super::LotKey {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
        };
        assert_eq!(
            InvariantViolation::LotsDoNotMatchPosition {
                key,
                lots: "1".into(),
                position: "2".into(),
            }
            .code(),
            "lots_do_not_match_position"
        );
        assert_eq!(
            InvariantViolation::BasisNotConserved {
                key,
                acquired: 1,
                remaining: 0,
                released: 0,
            }
            .code(),
            "basis_not_conserved"
        );
        assert_eq!(
            InvariantViolation::ZeroExternalFlow {
                event: crate::ids::EventId::new_random(),
            }
            .code(),
            "zero_external_flow"
        );
        assert_eq!(
            InvariantViolation::Numeric(crate::numeric::NumericError::Overflow).code(),
            "numeric"
        );
    }
}
```

- [ ] **Шаг 6: Снимок, `project` и `advance`**

`crates/iaam-core/src/projection/mod.rs` (тесты из шага 1 остаются в конце файла):

```rust
//! Проекции журнала со снимками (§3.1).
//!
//! «Весь журнал в память» — умолчание, а не архитектурный инвариант.
//! Поэтому публичный интерфейс с самого начала знает про снимок:
//! [`project`] строит его с нуля, [`advance`] продвигает существующий,
//! и полный пересчёт остаётся эталоном для инкрементального.
//!
//! Снимки и кэш хранит **оболочка**: ядро остаётся без состояния.

pub mod balances;
pub mod flows;
pub mod invariants;
pub mod lots;
pub mod state;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::contour::{ContourDefinition, ContourId, ContourVersion};
use crate::dates::EffectiveOrder;
use crate::event::Event;
use crate::event::correction::{CorrectionError, resolve};
use crate::event::kind::EventKind;
use crate::rules::{LotRuleVersion, RuleRegistry};
use crate::valuation::InstrumentPrice;
use balances::BalanceError;
use flows::FlowError;
use invariants::{InvariantReport, InvariantViolation};
use lots::{LotBook, LotError};
use state::{LedgerState, StateHash};

/// Версия формата проекции. Снимок, построенный другой версией,
/// продвигать нельзя: смысл полей мог измениться.
pub const PROJECTION_VERSION: u32 = 1;

/// Неизменяемый вход проекции: границы контура и версии правил.
///
/// `Debug` не выводится: `RuleRegistry` хранит трейт-объекты стратегий,
/// у которых отладочного представления нет и быть не может.
#[derive(Clone, Copy)]
pub struct ProjectionContext<'a> {
    pub contour: &'a ContourDefinition,
    pub rules: &'a RuleRegistry,
    pub lot_rule: LotRuleVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectionError {
    #[error("снимок построен версией проекции {found}, текущая — {expected}")]
    SnapshotVersionMismatch { expected: u32, found: u32 },
    #[error("отпечаток снимка не совпадает с его состоянием: снимок собран мимо ядра")]
    SnapshotFingerprintMismatch,
    #[error(
        "снимок построен для контура {snapshot_contour:?} версии {snapshot_version:?}, \
         запрошен {requested_contour:?} версии {requested_version:?}"
    )]
    SnapshotContourMismatch {
        snapshot_contour: ContourId,
        snapshot_version: ContourVersion,
        requested_contour: ContourId,
        requested_version: ContourVersion,
    },
    #[error("снимок построен правилом списания {snapshot:?}, запрошено {requested:?}")]
    SnapshotRuleMismatch {
        snapshot: LotRuleVersion,
        requested: LotRuleVersion,
    },
    #[error(
        "действующий журнал до границы снимка изменился: снимок продвигать нельзя, \
         нужен полный пересчёт"
    )]
    PrefixChanged {
        expected: StateHash,
        found: StateHash,
    },
    #[error(transparent)]
    Correction(#[from] CorrectionError),
    #[error(transparent)]
    Balance(#[from] BalanceError),
    #[error(transparent)]
    Lot(#[from] LotError),
    #[error(transparent)]
    Flow(#[from] FlowError),
    #[error(transparent)]
    Invariant(#[from] InvariantViolation),
}

impl ProjectionError {
    /// Машиночитаемый код для API и логов.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SnapshotVersionMismatch { .. } => "snapshot_version_mismatch",
            Self::SnapshotFingerprintMismatch => "snapshot_fingerprint_mismatch",
            Self::SnapshotContourMismatch { .. } => "snapshot_contour_mismatch",
            Self::SnapshotRuleMismatch { .. } => "snapshot_rule_mismatch",
            Self::PrefixChanged { .. } => "prefix_changed",
            Self::Correction(_) => "correction",
            Self::Balance(_) => "balance",
            Self::Lot(_) => "lot",
            Self::Flow(_) => "flow",
            Self::Invariant(_) => "invariant",
        }
    }

    /// Отличает нарушение инварианта от неполноты входа (§15.2).
    #[must_use]
    pub const fn is_invariant_violation(&self) -> bool {
        matches!(self, Self::Invariant(_))
    }
}

/// Снимок состояния на границе `through`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    projection_version: u32,
    contour: ContourId,
    contour_version: ContourVersion,
    lot_rule: LotRuleVersion,
    through: Option<EffectiveOrder>,
    state: LedgerState,
    fingerprint: StateHash,
    /// Отпечаток действующего журнала, свёрнутого в этот снимок.
    prefix_digest: StateHash,
}

impl Snapshot {
    #[must_use]
    pub const fn projection_version(&self) -> u32 {
        self.projection_version
    }

    #[must_use]
    pub const fn contour(&self) -> ContourId {
        self.contour
    }

    #[must_use]
    pub const fn contour_version(&self) -> ContourVersion {
        self.contour_version
    }

    #[must_use]
    pub const fn lot_rule(&self) -> LotRuleVersion {
        self.lot_rule
    }

    #[must_use]
    pub const fn through(&self) -> Option<EffectiveOrder> {
        self.through
    }

    #[must_use]
    pub const fn state(&self) -> &LedgerState {
        &self.state
    }

    #[must_use]
    pub const fn fingerprint(&self) -> StateHash {
        self.fingerprint
    }

    /// Отпечаток свёрнутого префикса журнала. Позволяет отличить
    /// «журнал тот же» от «журнал изменился до границы снимка».
    #[must_use]
    pub const fn prefix_digest(&self) -> StateHash {
        self.prefix_digest
    }
}

/// Разобранный снимок.
///
/// Существует ради хранилища: снимок кладётся в базу по частям и
/// собирается обратно. Собранный таким образом снимок ядро проверяет
/// отпечатком — оболочка могла собрать его неверно или не полностью.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotParts {
    pub projection_version: u32,
    pub contour: ContourId,
    pub contour_version: ContourVersion,
    pub lot_rule: LotRuleVersion,
    pub through: Option<EffectiveOrder>,
    pub state: LedgerState,
    pub fingerprint: StateHash,
    pub prefix_digest: StateHash,
}

impl Snapshot {
    /// Сборка снимка из сохранённых частей. Отпечаток **не** пересчитывается:
    /// смысл проверки в `advance` именно в том, чтобы сравнить заявленный
    /// отпечаток с фактическим состоянием.
    #[must_use]
    pub fn restore(parts: SnapshotParts) -> Self {
        Self {
            projection_version: parts.projection_version,
            contour: parts.contour,
            contour_version: parts.contour_version,
            lot_rule: parts.lot_rule,
            through: parts.through,
            state: parts.state,
            fingerprint: parts.fingerprint,
            prefix_digest: parts.prefix_digest,
        }
    }

    #[must_use]
    pub fn into_parts(self) -> SnapshotParts {
        SnapshotParts {
            projection_version: self.projection_version,
            contour: self.contour,
            contour_version: self.contour_version,
            lot_rule: self.lot_rule,
            through: self.through,
            state: self.state,
            fingerprint: self.fingerprint,
            prefix_digest: self.prefix_digest,
        }
    }
}

/// Результат проекции: снимок плюс перечень проверенных инвариантов.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    snapshot: Snapshot,
    invariants: InvariantReport,
}

impl Projection {
    #[must_use]
    pub const fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    #[must_use]
    pub const fn state(&self) -> &LedgerState {
        self.snapshot.state()
    }

    #[must_use]
    pub const fn invariants(&self) -> &InvariantReport {
        &self.invariants
    }

    #[must_use]
    pub fn into_snapshot(self) -> Snapshot {
        self.snapshot
    }
}

/// Полный пересчёт с нуля. Эталон для [`advance`].
pub fn project(events: &[Event], ctx: &ProjectionContext) -> Result<Projection, ProjectionError> {
    let state = LedgerState::new(LotBook::new(ctx.lot_rule));
    let effective = resolve(events)?;
    fold(state, &[], &effective, ctx)
}

/// Продвижение снимка **полным срезом журнала**.
///
/// Принимает тот же срез, что и [`project`], а не «пачку новых событий».
/// Это не удобство, а требование корректности: событие, добавленное
/// задним числом до границы снимка, не меняет ни границу, ни состояние
/// снимка. Вызывающий, отбирающий «всё, что позже границы», молча
/// потеряет такое событие — и получит правдоподобные, но неверные
/// остатки, лоты и доходность. Проверено ревью: ровно этот дефект был
/// в первой редакции этого модуля.
///
/// Поэтому решение о применимости снимка принимает ядро: оно сворачивает
/// действующий набор, сравнивает отпечаток префикса и продвигает состояние
/// только тем, что за границей. Несовпадение префикса — не ошибка работы,
/// а сигнал «нужен полный пересчёт»; сторнирование события внутри снимка
/// проявляется именно так, потому что удаляет его из действующего набора.
pub fn advance(
    previous: &Snapshot,
    events: &[Event],
    ctx: &ProjectionContext,
) -> Result<Projection, ProjectionError> {
    if previous.projection_version != PROJECTION_VERSION {
        return Err(ProjectionError::SnapshotVersionMismatch {
            expected: PROJECTION_VERSION,
            found: previous.projection_version,
        });
    }
    if previous.contour != ctx.contour.id() || previous.contour_version != ctx.contour.version() {
        return Err(ProjectionError::SnapshotContourMismatch {
            snapshot_contour: previous.contour,
            snapshot_version: previous.contour_version,
            requested_contour: ctx.contour.id(),
            requested_version: ctx.contour.version(),
        });
    }
    if previous.lot_rule != ctx.lot_rule {
        return Err(ProjectionError::SnapshotRuleMismatch {
            snapshot: previous.lot_rule,
            requested: ctx.lot_rule,
        });
    }
    if previous.state.fingerprint() != previous.fingerprint {
        return Err(ProjectionError::SnapshotFingerprintMismatch);
    }

    let effective = resolve(events)?;
    let split = match previous.through {
        None => 0,
        Some(through) => effective.partition_point(|event| event.order <= through),
    };
    let (prefix, suffix) = effective.split_at(split);

    let found = state::prefix_digest(prefix);
    if found != previous.prefix_digest {
        return Err(ProjectionError::PrefixChanged {
            expected: previous.prefix_digest,
            found,
        });
    }

    fold(previous.state.clone(), prefix, suffix, ctx)
}

/// Применение действующего набора событий к состоянию.
///
/// Три независимых читателя журнала — остатки, лоты, потоки — вызываются
/// по очереди для каждого события. Общих вспомогательных функций у них
/// нет намеренно: инвариант «сумма лотов равна позиции» держится ровно
/// на этой независимости (§15.4).
fn fold(
    mut state: LedgerState,
    already_applied: &[&Event],
    effective: &[&Event],
    ctx: &ProjectionContext,
) -> Result<Projection, ProjectionError> {
    let mut through = already_applied.last().map(|event| event.order);
    for event in effective {
        {
            let (balances, book, flows) = state.parts_mut();
            balances.apply(event)?;
            book.apply(event, ctx.rules)?;
            flows.apply(event, ctx.contour)?;
        }
        if let EventKind::Valuation {
            instrument,
            price,
            currency,
            quality,
        } = &event.kind
        {
            if let Some(as_of) = event.dates.effective_date() {
                state.prices_mut().record(InstrumentPrice {
                    instrument: *instrument,
                    price: *price,
                    currency: *currency,
                    quality: *quality,
                    as_of,
                });
            }
        }
        state.observe(event);
        through = Some(event.order);
    }

    // Инварианты проверяются по всему действующему набору, а не только
    // по продвинутой части: состояние общее, и нарушение могло прийти
    // из снимка, которому ядро не обязано верить (§15.2).
    let all: Vec<&Event> = already_applied
        .iter()
        .chain(effective.iter())
        .copied()
        .collect();
    let invariants = invariants::check(&state, &all)?;
    let fingerprint = state.fingerprint();
    let prefix_digest = state::prefix_digest(&all);
    Ok(Projection {
        snapshot: Snapshot {
            projection_version: PROJECTION_VERSION,
            contour: ctx.contour.id(),
            contour_version: ctx.contour.version(),
            lot_rule: ctx.lot_rule,
            through,
            state,
            fingerprint,
            prefix_digest,
        },
        invariants,
    })
}
```

- [ ] **Шаг 7: Зелёная сборка**

```bash
nix develop -c cargo test -p iaam-core
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c ./scripts/check-architecture.sh
```

Ожидается: тесты проекции и инвариантов проходят, весь набор ядра зелёный.

- [ ] **Шаг 8: Коммит**

```bash
git add crates/iaam-core
git commit -m "feat(core): снимки проекции, project/advance и инварианты (iaam-1fk)"
```

---

## Задача 6: Численный решатель ставки

**Files:**
- Create: `crates/iaam-core/src/numeric/xirr.rs`
- Modify: `crates/iaam-core/src/numeric/mod.rs` — `pub mod xirr;`
- Modify: `scripts/check-architecture.sh` — заслоны 5 и 8

**Interfaces:**
- Consumes: `numeric::approx::{ApproxValue, SolverPolicy, dec_to_f64}`, `numeric::decimal::Dec`.
- Produces: `numeric::xirr::{DayCount, SolverFlow, SolverRefusal, RateOutcome, solve}`.

**Acceptance Criteria:**
- Решатель отказывается явно: меньше двух потоков, все потоки нулевые, отсутствие смены знака, корень не локализован, несколько интервалов со сменой знака, **недоказуемая единственность корня**, не сошлось, значение непредставимо, неверный диапазон.
- Единственность корня доказывается **правилом знаков**, а не подсчётом интервалов на сетке.
- Объявленная погрешность — половина ширины локализующего интервала, то есть доказанная граница.
- Допуски по невязке и по ставке разделены; невязка измеряется **относительно масштаба серии**, поэтому ставка масштабно-инвариантна.
- Нечисловые значения NPV дают отказ, а не мнимый корень.
- Каждый отказ имеет машиночитаемый код: внешний агент разбирает код, а не текст.
- Результат несёт политику решателя, базу начисления дней, оценку погрешности и число итераций.
- Двоичная плавающая точка не покидает файл: наружу уходят `Dec` на входе и `ApproxValue` на выходе.
- Заслон архитектуры пропускает `numeric/xirr.rs` и **по-прежнему ловит** `f64` в любом другом файле ядра.

> **Исправление при исполнении (2026-08-23).** Мутационный прогон дал одного выжившего: условие остановки `high - low <= rate_tolerance` можно было заменить на `high + low` незамеченно. Существующий тест `a_bracket_already_within_tolerance_needs_no_iterations` его не ловил — при диапазоне от −0,9999 сумма концов отрицательна и допуску тоже удовлетворяет. Добавлен тест `the_stopping_test_is_the_width_of_the_bracket_not_the_sum_of_its_ends` с корнем около +500 %: там разность концов мала, а сумма велика, и подмена видна.

**Почему нужно изменение заслона и почему это не ослабление.** Заслон 5 разрешал плавающую точку ровно в одном файле. Ставка требует возведения в дробную степень, которого `rust_decimal` не умеет, поэтому файлов становится два. Ослаблением это было бы, если бы исключение задавалось маской каталога: тогда третий файл появился бы незаметно. Список остаётся поимённым, а заслон 8 получает отдельный порог на каждый файл — 420 строк для решателя (порог считает и тесты, лежащие в том же файле) (сканирование диапазона и оценка погрешности объективно длиннее объявления политики). Изменение обязано сопровождаться меткой `policy-change` на PR и обоснованием в описании бида (§15.7).

**Как доказывается единственность корня.** Метод Ньютона находит корень, но никогда не отвечает на вопрос «сколько их». Первая редакция отвечала на него подсчётом интервалов со сменой знака на сетке — и это неверно: сетка из тысячи точек на диапазоне −99,99 %…+10 000 % имеет шаг около **0,1**, то есть примерно десять процентных пунктов ставки, и пропускает корни чётной кратности, пары корней внутри шага и близкие корни. Ревью показало, что при таком «доказательстве» система могла вернуть произвольно выбранную ставку вместо обязательного отказа.

Правильный ответ даёт правило знаков. Замена `x = 1/(1 + r)` превращает NPV в обобщённый многочлен `Σ aᵢ·x^tᵢ` с положительными показателями, для которого число положительных корней не превосходит числа перемен знака в упорядоченной по времени последовательности сумм. Одна перемена знака — корень не более одного; вместе с интервалом, на границах которого знаки различны, это ровно один корень. Больше одной перемены — единственность не доказуема, и возвращается отказ `UniquenessNotProven`. Сканирование остаётся, но только для **поиска интервала**, а не для подсчёта корней.

**Почему остановка по ширине интервала, а не по невязке, и почему допуск один.** Возле пологого корня невязка мала при большой ошибке ставки, а абсолютный допуск по невязке ещё и зависит от масштаба денег: та же серия, умноженная на тысячу, останавливалась бы в другой точке, хотя ставка обязана быть масштабно-инвариантной. Первая редакция пыталась держать два допуска сразу — и они конфликтовали: ослабление допуска по ставке давало отказ `NotConverged`, потому что проверка невязки продолжала требовать прежней точности. Допуск оставлен один, в единицах ставки: корень заключён в интервале по построению, значит половина ширины — доказанная граница, и вторая проверка ничего не добавляет.

**Почему метод Илинойса, а не Ньютон.** Ньютон с откатом на бисекцию был в первой редакции и вырождался в чистую бисекцию: шаг Ньютона возле корня почти не двигает дальний конец интервала, защита «не сократился вдвое — бисекция» срабатывала почти каждую итерацию, и на задаче с известным ответом уходило тридцать семь итераций вместо единиц. Проверено исполнением. Метод Илинойса — модифицированное ложное положение — никогда не теряет локализацию и сходится сверхлинейно, потому что застоявшийся конец «слабеет» вдвое и следующая секущая перескакивает на другую сторону. Производная перестала быть нужна вовсе: минус одна функция, минус одиннадцать выживших мутантов, минус класс ошибок.

- [ ] **Шаг 1: Разделить допуски в политике решателя**

В `crates/iaam-core/src/numeric/approx.rs` замените объявление политики и её умолчание:

```rust
pub struct SolverPolicy {
    /// Допустимая ширина интервала, локализующего корень, — в единицах
    /// **ставки**. Она же определяет объявленную погрешность результата.
    ///
    /// Допуск ровно один, и он в единицах ставки. Допуск по величине
    /// невязки здесь не нужен и вреден: возле пологого корня невязка
    /// мала при большой ошибке ставки, а абсолютный допуск по невязке
    /// ещё и зависел бы от масштаба денег — та же серия, умноженная
    /// на тысячу, останавливалась бы в другой точке, хотя ставка
    /// обязана быть масштабно-инвариантной. Корень заключён
    /// в интервале по построению, поэтому половина ширины — доказанная
    /// граница, а не оценка.
    pub rate_tolerance: f64,
    /// Максимум итераций до отказа.
    pub max_iterations: u32,
    /// Нижняя граница локализации корня.
    pub bracket_low: f64,
    /// Верхняя граница локализации корня.
    pub bracket_high: f64,
}
```

```rust
    /// Политика по умолчанию для расчёта ставок доходности.
    ///
    /// Локализация от −99,99 % до +10 000 % годовых покрывает любой
    /// реалистичный результат, включая полную потерю капитала.
    #[must_use]
    pub const fn returns_default() -> Self {
        Self {
            rate_tolerance: 1e-10,
            max_iterations: 200,
            bracket_low: -0.9999,
            bracket_high: 100.0,
        }
    }
```

- [ ] **Шаг 2: Написать падающие тесты**

Тесты решателя живут **отдельным файлом** `crates/iaam-core/tests/xirr_solver.rs`, а не внутри модуля: заслон архитектуры ограничивает размер файлов приближённого режима, чтобы в них не завёлся теневой расчётный слой, — а тестовый код, разрастаясь, съедал бы этот предел и вынуждал его поднимать.

```rust
//! Решатель ставки: отказы, единственность корня, масштабная инвариантность.
//!
//! Тесты вынесены из `src/numeric/xirr.rs` в отдельный файл намеренно:
//! заслон архитектуры ограничивает размер файлов приближённого режима,
//! чтобы в них не завёлся теневой расчётный слой, — а тестовый код,
//! разрастаясь, съедал бы этот предел и вынуждал его поднимать.

use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::numeric::xirr::{DayCount, SolverFlow, SolverRefusal, solve};
use rust_decimal::Decimal;

fn flow(day_offset: i64, amount: i64) -> SolverFlow {
    SolverFlow {
        day_offset,
        amount: Dec::new(Decimal::from(amount)),
    }
}

#[test]
fn a_single_year_of_ten_percent_is_ten_percent() {
    // Вложено 1000, через 365 дней получено 1100. Ставка известна
    // из условия задачи, а не из вывода программы (§15.5).
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!((outcome.rate().value() - 0.1).abs() < 1e-9);
    assert_eq!(outcome.day_count(), DayCount::Act365);
    // Погрешность — половина ширины локализующего интервала,
    // то есть доказанная граница, а не разность приближений.
    assert!(outcome.rate().error_bound() <= SolverPolicy::returns_default().rate_tolerance);
}

#[test]
fn the_rate_does_not_depend_on_the_scale_of_the_flows() {
    // Масштабная инвариантность (§15.3): абсолютный допуск по невязке
    // нарушал бы её, потому что зависел бы от размера сумм.
    let small = [flow(0, -1_000), flow(365, 1_100)];
    let large = [flow(0, -1_000_000_000), flow(365, 1_100_000_000)];
    let policy = SolverPolicy::returns_default();
    let left = solve(&small, policy, DayCount::Act365).unwrap();
    let right = solve(&large, policy, DayCount::Act365).unwrap();
    assert!((left.rate().value() - right.rate().value()).abs() < 1e-9);
}

#[test]
fn flows_of_one_sign_have_no_rate() {
    let flows = [flow(0, -1_000), flow(365, -1_100)];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::NoSignChange)
    );
}

#[test]
fn fewer_than_two_flows_have_no_rate() {
    let flows = [flow(0, -1_000)];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::TooFewFlows)
    );
}

#[test]
fn all_zero_flows_have_no_rate() {
    let flows = [flow(0, 0), flow(365, 0)];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::AllZero)
    );
}

#[test]
fn two_sign_changes_are_refused_even_when_the_grid_finds_one_bracket() {
    // Классический знакопеременный ряд. Сетка может найти один
    // интервал со сменой знака и «доказать» единственность — но она
    // пропускает корни чётной кратности и пары корней внутри шага.
    // Отказ обязателен, даже когда число выглядит правдоподобным.
    let flows = [flow(0, -1_000), flow(365, 2_500), flow(730, -1_540)];
    let refusal = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap_err();
    assert!(
        matches!(
            refusal,
            SolverRefusal::MultipleRoots { .. } | SolverRefusal::UniquenessNotProven { .. }
        ),
        "получено {refusal:?}"
    );
}

#[test]
fn a_coupon_series_with_one_sign_change_is_solved() {
    // Купоны между вложением и погашением знак не меняют: перемена
    // одна, корень один, отказа быть не должно.
    let flows = [
        flow(0, -98_000),
        flow(182, 4_500),
        flow(365, 4_500),
        flow(547, 4_500),
        flow(731, 104_500),
    ];
    let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!(outcome.rate().value() > 0.0);
}

#[test]
fn an_inverted_bracket_is_refused() {
    let policy = SolverPolicy {
        bracket_low: 1.0,
        bracket_high: -1.0,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::BadBracket)
    );
}

#[test]
fn a_bracket_reaching_minus_one_hundred_percent_is_refused() {
    // При ставке −100 % основание степени равно нулю: NPV не определён.
    let policy = SolverPolicy {
        bracket_low: -1.0,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::BadBracket)
    );
}

#[test]
fn a_root_outside_the_bracket_is_refused() {
    let policy = SolverPolicy {
        bracket_low: 0.0,
        bracket_high: 0.01,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::RootNotBracketed)
    );
}

#[test]
fn every_refusal_has_a_machine_readable_code() {
    assert_eq!(SolverRefusal::TooFewFlows.code(), "too_few_flows");
    assert_eq!(SolverRefusal::NoSignChange.code(), "no_sign_change");
    assert_eq!(SolverRefusal::RootNotBracketed.code(), "root_not_bracketed");
    assert_eq!(
        SolverRefusal::MultipleRoots { count: 2 }.code(),
        "multiple_roots"
    );
    assert_eq!(
        SolverRefusal::UniquenessNotProven { sign_changes: 3 }.code(),
        "uniqueness_not_proven"
    );
    assert_eq!(
        SolverRefusal::NotConverged { iterations: 1 }.code(),
        "not_converged"
    );
    assert_eq!(SolverRefusal::NotRepresentable.code(), "not_representable");
    assert_eq!(SolverRefusal::BadBracket.code(), "bad_bracket");
    assert_eq!(SolverRefusal::AllZero.code(), "all_zero");
}

#[test]
fn the_day_count_has_a_stable_code() {
    // Код уходит в отчёт и в снапшот: без него ставка невоспроизводима.
    assert_eq!(DayCount::Act365.code(), "act/365");
}

#[test]
fn the_solver_converges_superlinearly_not_by_halving() {
    // Метод Илинойса обязан сходиться заметно быстрее бисекции: на
    // интервале шириной около 0,1 до допуска 1e-10 чистой бисекции
    // нужно порядка тридцати шагов. Проверка числа итераций —
    // единственный способ заметить, что приём Илинойса сломан: ответ
    // при этом остаётся верным, просто добывается вдвое дольше.
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!(
        outcome.rate().iterations() <= 20,
        "итераций {}: метод вырождается в бисекцию",
        outcome.rate().iterations()
    );
}

#[test]
fn a_degenerate_bracket_is_refused() {
    let policy = SolverPolicy {
        bracket_low: 0.5,
        bracket_high: 0.5,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::BadBracket)
    );
}

#[test]
fn a_non_numeric_bracket_is_refused() {
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    for policy in [
        SolverPolicy {
            bracket_low: f64::NAN,
            ..SolverPolicy::returns_default()
        },
        SolverPolicy {
            bracket_high: f64::INFINITY,
            ..SolverPolicy::returns_default()
        },
    ] {
        assert_eq!(
            solve(&flows, policy, DayCount::Act365),
            Err(SolverRefusal::BadBracket)
        );
    }
}

#[test]
fn any_bracket_reaching_minus_one_hundred_percent_is_refused() {
    // Ставка −100 % обращает основание степени в ноль, ниже — делает его
    // отрицательным. Отказ обязан приходить от нижней границы независимо
    // от верхней: и когда диапазон пересекает −1, и когда он весь ниже.
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    for (low, high) in [(-1.0, 100.0), (-2.0, -1.0), (-3.0, -2.0)] {
        let policy = SolverPolicy {
            bracket_low: low,
            bracket_high: high,
            ..SolverPolicy::returns_default()
        };
        assert_eq!(
            solve(&flows, policy, DayCount::Act365),
            Err(SolverRefusal::BadBracket),
            "диапазон [{low}, {high}]"
        );
    }
}

#[test]
fn the_scan_step_covers_exactly_the_requested_range() {
    // Шаг — это (высокая − низкая) / точки. Симметричный диапазон ловит
    // подмену вычитания сложением: сумма границ там равна нулю, шаг
    // обращается в ноль, и сканирование перестаёт двигаться.
    let policy = SolverPolicy {
        bracket_low: -0.5,
        bracket_high: 0.5,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, policy, DayCount::Act365).unwrap();
    assert!((outcome.rate().value() - 0.1).abs() < 1e-9);
}

#[test]
fn a_bracket_already_within_tolerance_needs_no_iterations() {
    // Если найденный интервал уже уложился в допуск, уточнять нечего,
    // и объявленная погрешность равна ровно половине его ширины.
    //
    // Ширина здесь известна точно: это шаг сканирования, то есть
    // (100 − (−0,9999)) / 1000 ≈ 0,10100. Половина — около 0,05050.
    // Проверка привязана к числу точек сканирования намеренно: без
    // точного ожидания «половина ширины» неотличима от «ширина»
    // и от «две ширины».
    let policy = SolverPolicy {
        rate_tolerance: 1.0,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, policy, DayCount::Act365).unwrap();
    assert_eq!(outcome.rate().iterations(), 0);
    let bound = outcome.rate().error_bound();
    assert!(
        (0.0504..0.0506).contains(&bound),
        "граница погрешности {bound}: это не половина ширины интервала"
    );
}

#[test]
fn a_series_with_three_sign_changes_is_refused_by_the_sign_rule() {
    // Сетка находит здесь ровно один интервал со сменой знака — то есть
    // «доказала» бы единственность. Единственность отвергает правило
    // знаков, и только оно: без него система вернула бы одно из
    // возможных значений как ответ.
    let flows = [
        flow(0, -1_000),
        flow(365, 2_000),
        flow(730, -1_000),
        flow(1_095, 400),
    ];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::UniquenessNotProven { sign_changes: 3 })
    );
}

#[test]
fn the_error_bound_shrinks_with_the_requested_tolerance() {
    // Погрешность — половина ширины локализующего интервала, а не
    // произвольное число: ужесточение допуска обязано её уменьшать.
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let loose = solve(
        &flows,
        SolverPolicy {
            rate_tolerance: 1e-4,
            ..SolverPolicy::returns_default()
        },
        DayCount::Act365,
    )
    .unwrap();
    let tight = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!(loose.rate().error_bound() > tight.rate().error_bound());
    // Граница — ПОЛОВИНА ширины интервала, а не ширина: допуск задаёт
    // ширину, значит объявленная погрешность вдвое меньше него.
    assert!(loose.rate().error_bound() <= 1e-4 / 2.0 + f64::EPSILON);
}
```

- [ ] **Шаг 3: Убедиться, что тесты падают**

```bash
nix develop -c cargo test -p iaam-core --test xirr_solver
```

- [ ] **Шаг 4: Реализация**

```rust
//! Решатель ставки внутренней доходности (§6.1, §6.6).
//!
//! Второй и последний файл ядра, где разрешена двоичная плавающая точка:
//! ставка требует возведения в дробную степень, которого `rust_decimal`
//! не умеет. Результат решателя **никогда** не входит в денежное
//! тождество — он производная от сумм, а не их компонент (§6.6).
//!
//! **Уникальность корня доказывается правилом знаков, а не сканированием.**
//! Замена `x = 1/(1 + r)` превращает `NPV` в обобщённый многочлен
//! `Σ aᵢ·x^tᵢ` с положительными показателями, для которого число
//! положительных корней не превосходит числа перемен знака в
//! упорядоченной по времени последовательности сумм. Одна перемена
//! знака — корень не более одного; вместе с интервалом, на границах
//! которого знаки различны, это ровно один корень. Сканирование сетки
//! служит только поиску такого интервала: считать по нему корни нельзя —
//! оно пропускает корни чётной кратности и пары корней внутри шага.

use thiserror::Error;

use super::approx::{ApproxValue, SolverPolicy, dec_to_f64};
use super::decimal::Dec;

/// База начисления дней. Зафиксирована в результате: без неё ставка
/// не воспроизводима.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayCount {
    /// Фактические дни, год 365. Конвенция XIRR.
    Act365,
}

impl DayCount {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Act365 => "act/365",
        }
    }

    const fn year_length(self) -> f64 {
        match self {
            Self::Act365 => 365.0,
        }
    }
}

/// Поток для решателя: смещение в днях от первого потока и сумма
/// в валюте отчёта.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolverFlow {
    pub day_offset: i64,
    pub amount: Dec,
}

/// Отказ решателя. Отказ — результат, а не исключение: уравнение NPV
/// при чередующихся знаках потоков может не иметь корней, иметь
/// несколько или не позволять доказать единственность, и произвольно
/// выбранное число хуже честного отказа (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SolverRefusal {
    #[error("потоков меньше двух: ставка не определена")]
    TooFewFlows,
    #[error("все потоки одного знака: уравнение NPV корня не имеет")]
    NoSignChange,
    #[error("корень не локализован в заданном диапазоне ставок")]
    RootNotBracketed,
    #[error("в диапазоне ставок найдено интервалов со сменой знака: {count}; корень не один")]
    MultipleRoots { count: u32 },
    #[error(
        "знак потоков меняется {sign_changes} раз: единственность корня не доказуема, \
         и выбирать один из возможных нельзя"
    )]
    UniquenessNotProven { sign_changes: u32 },
    #[error("метод не сошёлся за {iterations} итераций")]
    NotConverged { iterations: u32 },
    #[error("сумма потока не переводится в приближённый режим или не является числом")]
    NotRepresentable,
    #[error("диапазон локализации задан неверно: нижняя граница не меньше верхней")]
    BadBracket,
    #[error("все потоки нулевые: ставка не определена")]
    AllZero,
}

impl SolverRefusal {
    /// Машиночитаемый код отказа. Нужен API: текст предназначен человеку,
    /// а внешний агент разбирает код (§13).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooFewFlows => "too_few_flows",
            Self::NoSignChange => "no_sign_change",
            Self::RootNotBracketed => "root_not_bracketed",
            Self::MultipleRoots { .. } => "multiple_roots",
            Self::UniquenessNotProven { .. } => "uniqueness_not_proven",
            Self::NotConverged { .. } => "not_converged",
            Self::NotRepresentable => "not_representable",
            Self::BadBracket => "bad_bracket",
            Self::AllZero => "all_zero",
        }
    }
}

/// Найденная ставка вместе с политикой, по которой она найдена.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateOutcome {
    rate: ApproxValue,
    policy: SolverPolicy,
    day_count: DayCount,
}

impl RateOutcome {
    #[must_use]
    pub const fn rate(&self) -> ApproxValue {
        self.rate
    }

    #[must_use]
    pub const fn policy(&self) -> SolverPolicy {
        self.policy
    }

    #[must_use]
    pub const fn day_count(&self) -> DayCount {
        self.day_count
    }
}

/// Число точек сканирования диапазона ставок.
///
/// При диапазоне по умолчанию (−99,99 %…+10 000 %) шаг составляет около
/// 0,1 — то есть примерно десять процентных пунктов ставки. Этого
/// достаточно, чтобы найти интервал со сменой знака у денежной серии
/// с одной переменой знака, и **недостаточно**, чтобы делать выводы
/// о числе корней: выводы делает правило знаков.
const SCAN_POINTS: u32 = 1_000;

/// Внутренняя денежная серия в приближённом режиме.
struct Series {
    /// Пары «доля года от первого потока, сумма», в порядке времени.
    terms: Vec<(f64, f64)>,
}

impl Series {
    fn build(flows: &[SolverFlow], day_count: DayCount) -> Result<Self, SolverRefusal> {
        if flows.len() < 2 {
            return Err(SolverRefusal::TooFewFlows);
        }
        let mut terms = Vec::with_capacity(flows.len());
        // Сумма модулей: нужна ровно для одного вывода — серия
        // из одних нулей ставки не имеет. В структуре не хранится,
        // потому что больше нигде не используется.
        let mut magnitude = 0.0_f64;
        for flow in flows {
            let amount = dec_to_f64(&flow.amount).ok_or(SolverRefusal::NotRepresentable)?;
            if !amount.is_finite() {
                return Err(SolverRefusal::NotRepresentable);
            }
            let years = flow.day_offset as f64 / day_count.year_length();
            if !years.is_finite() {
                return Err(SolverRefusal::NotRepresentable);
            }
            magnitude += amount.abs();
            terms.push((years, amount));
        }
        // Сумма модулей строго положительна для непустой ненулевой серии.
        // Проверяется именно так, а не «равна нулю»: отрицательное
        // значение означало бы ошибку в самом накоплении, и молчаливо
        // пропускать её нельзя.
        if !magnitude.is_finite() {
            return Err(SolverRefusal::NotRepresentable);
        }
        if magnitude <= 0.0 {
            return Err(SolverRefusal::AllZero);
        }
        Ok(Self { terms })
    }

    /// Число перемен знака в упорядоченной по времени последовательности
    /// сумм. Нулевые потоки пропускаются: ноль знака не имеет.
    fn sign_changes(&self) -> u32 {
        let mut changes = 0;
        let mut previous = 0.0_f64;
        for (_, amount) in &self.terms {
            if *amount == 0.0 {
                continue;
            }
            if previous != 0.0 && previous.signum() != amount.signum() {
                changes += 1;
            }
            previous = *amount;
        }
        changes
    }

    fn npv(&self, rate: f64) -> f64 {
        self.terms
            .iter()
            .map(|(years, amount)| amount / (1.0 + rate).powf(*years))
            .sum()
    }
}

/// Интервалы сканирования, на границах которых NPV меняет знак.
///
/// Нечисловые значения NPV прекращают поиск отказом: `NaN` не сравнивается
/// сам с собой, и наивная проверка знака превратила бы его в мнимый корень.
fn brackets(series: &Series, policy: SolverPolicy) -> Result<Vec<(f64, f64)>, SolverRefusal> {
    // Границы обязаны быть сравнимыми и упорядоченными: NaN в политике
    // означает, что диапазон задан неверно, а не «любой диапазон».
    if !policy.bracket_low.is_finite() || !policy.bracket_high.is_finite() {
        return Err(SolverRefusal::BadBracket);
    }
    if policy.bracket_low >= policy.bracket_high {
        return Err(SolverRefusal::BadBracket);
    }
    // Ставка −100 % обращает основание степени в ноль, ниже — делает его
    // отрицательным, а дробная степень отрицательного числа не определена.
    // Диапазон обязан начинаться строго выше: условие на верхнюю границу
    // тут не нужно и только создавало бы вторую, непроверяемую ветку.
    if policy.bracket_low <= -1.0 {
        return Err(SolverRefusal::BadBracket);
    }
    let step = (policy.bracket_high - policy.bracket_low) / f64::from(SCAN_POINTS);
    let mut found = Vec::new();
    let mut previous_rate = policy.bracket_low;
    let mut previous_value = series.npv(previous_rate);
    if !previous_value.is_finite() {
        return Err(SolverRefusal::NotRepresentable);
    }
    for i in 1..=SCAN_POINTS {
        let rate = policy.bracket_low + step * f64::from(i);
        let value = series.npv(rate);
        if !value.is_finite() {
            return Err(SolverRefusal::NotRepresentable);
        }
        if value == 0.0 {
            found.push((rate, rate));
        } else if previous_value != 0.0 && previous_value.signum() != value.signum() {
            found.push((previous_rate, rate));
        }
        previous_rate = rate;
        previous_value = value;
    }
    Ok(found)
}

/// Уточнение корня методом Илинойса (§6.1).
///
/// Это модифицированный метод ложного положения: он **никогда** не теряет
/// локализующий интервал — оба конца всегда дают значения разных знаков, —
/// и при этом сходится сверхлинейно, потому что при застревании одного
/// конца его значение вдвое уменьшается и следующая секущая перескакивает
/// на другую сторону.
///
/// Почему не Ньютон с откатом на бисекцию, как было в первой редакции:
/// шаг Ньютона, попадая близко к корню, почти не двигает дальний конец
/// интервала, а объявленная погрешность считается именно по интервалу.
/// Защита «не сократился вдвое — бисекция» срабатывала почти всегда,
/// и метод вырождался в чистую бисекцию: тридцать семь итераций там,
/// где достаточно единиц. Проверено исполнением.
///
/// Остановка — по **ширине интервала**, а не по величине невязки:
/// невязка возле пологого корня мала при большой ошибке ставки.
/// Отдельная проверка невязки не нужна: корень заключён в интервале
/// по построению, поэтому половина ширины — доказанная граница,
/// а не оценка.
fn refine(
    series: &Series,
    bracket: (f64, f64),
    policy: SolverPolicy,
) -> Result<ApproxValue, SolverRefusal> {
    // Концы интервала — точки сканирования, и их значения уже проверены
    // на численность в `brackets`: интервал возвращается только тогда,
    // когда оба значения конечны и разных знаков. Повторная проверка
    // здесь была бы мёртвой веткой, а мёртвая проверка создаёт ложное
    // впечатление, что случай обрабатывается.
    let (mut low, mut high) = bracket;
    let mut low_value = series.npv(low);
    let mut high_value = series.npv(high);
    if high - low <= policy.rate_tolerance {
        return Ok(finish(low, high, 0));
    }

    for iteration in 1..=policy.max_iterations {
        let denominator = high_value - low_value;
        let secant = high - high_value * (high - low) / denominator;
        let guess = if secant_is_inside(secant, low, high) {
            secant
        } else {
            (low + high) / 2.0
        };

        let value = series.npv(guess);
        if !value.is_finite() {
            return Err(SolverRefusal::NotRepresentable);
        }
        if value == 0.0 {
            return Ok(finish(guess, guess, iteration));
        }

        if value.signum() == high_value.signum() {
            high = guess;
            high_value = value;
            // Приём Илинойса: застоявшийся конец «слабеет», и следующая
            // секущая перепрыгивает на другую сторону корня.
            low_value /= 2.0;
        } else {
            low = high;
            low_value = high_value;
            high = guess;
            high_value = value;
        }

        let (left, right) = if low <= high {
            (low, high)
        } else {
            (high, low)
        };
        if right - left <= policy.rate_tolerance {
            return Ok(finish(left, right, iteration));
        }
    }
    Err(SolverRefusal::NotConverged {
        iterations: policy.max_iterations,
    })
}

/// Принимается ли секущая: строго внутри локализующего интервала.
///
/// Одно сравнение покрывает всё, что может пойти не так: `NaN` не больше
/// и не меньше ничего, бесконечность (нулевой знаменатель) не меньше
/// верхней границы, вышедшая за край секущая не проходит по определению.
///
/// Вынесено отдельной функцией не ради читаемости, а ради проверяемости:
/// внутри цикла эта ветка недостижима — для пары значений разных знаков
/// секущая математически лежит между концами, — и мутационный заслон
/// справедливо называл её условия эквивалентными. Отдельная функция
/// проверяется напрямую, и защита перестаёт быть непроверяемой.
const fn secant_is_inside(secant: f64, low: f64, high: f64) -> bool {
    secant > low && secant < high
}

/// Середина интервала как значение, половина ширины как доказанная
/// граница погрешности.
fn finish(low: f64, high: f64, iterations: u32) -> ApproxValue {
    ApproxValue::new((low + high) / 2.0, (high - low).abs() / 2.0, iterations)
}

/// Ставка, при которой приведённая стоимость потоков равна нулю.
pub fn solve(
    flows: &[SolverFlow],
    policy: SolverPolicy,
    day_count: DayCount,
) -> Result<RateOutcome, SolverRefusal> {
    let series = Series::build(flows, day_count)?;
    let sign_changes = series.sign_changes();
    if sign_changes == 0 {
        return Err(SolverRefusal::NoSignChange);
    }

    let found = brackets(&series, policy)?;
    let bracket = match found.len() {
        0 => return Err(SolverRefusal::RootNotBracketed),
        1 => found[0],
        n => {
            return Err(SolverRefusal::MultipleRoots {
                count: u32::try_from(n).unwrap_or(u32::MAX),
            });
        }
    };

    // Единственный найденный интервал доказывает единственность корня
    // только при одной перемене знака у потоков. При большем числе перемен
    // сетка могла пропустить корень чётной кратности или пару корней
    // внутри шага — и выдать одно из нескольких значений за ответ.
    if sign_changes > 1 {
        return Err(SolverRefusal::UniquenessNotProven { sign_changes });
    }

    let rate = refine(&series, bracket, policy)?;
    Ok(RateOutcome {
        rate,
        policy,
        day_count,
    })
}
```

И строка `pub mod xirr;` в `crates/iaam-core/src/numeric/mod.rs`.

- [ ] **Шаг 5: Проверить защиту секущей напрямую**

Внутри цикла эта ветка недостижима: для пары значений разных знаков секущая математически лежит между концами. Недостижимая проверка — это проверка, про которую неизвестно, работает ли она, и мутационный заслон называет её условия эквивалентными. Предикат вынесен отдельной функцией именно ради проверяемости; в конец `crates/iaam-core/src/numeric/xirr.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::secant_is_inside;

    /// Защита секущей проверяется напрямую: внутри цикла она недостижима,
    /// а недостижимая проверка — это проверка, про которую неизвестно,
    /// работает ли она.
    #[test]
    fn only_a_strictly_interior_secant_is_accepted() {
        assert!(secant_is_inside(0.5, 0.0, 1.0));
        // Границы не годятся: приняв конец интервала, метод перестал бы
        // его сокращать и зациклился бы.
        assert!(!secant_is_inside(0.0, 0.0, 1.0));
        assert!(!secant_is_inside(1.0, 0.0, 1.0));
        // Выход за интервал — потеря локализации, то есть потеря
        // доказанной границы погрешности.
        assert!(!secant_is_inside(-0.1, 0.0, 1.0));
        assert!(!secant_is_inside(1.1, 0.0, 1.0));
        // Нечисловое значение и бесконечности из нулевого знаменателя.
        assert!(!secant_is_inside(f64::NAN, 0.0, 1.0));
        assert!(!secant_is_inside(f64::INFINITY, 0.0, 1.0));
        assert!(!secant_is_inside(f64::NEG_INFINITY, 0.0, 1.0));
    }
}
```

- [ ] **Шаг 6: Убедиться, что заслон ловит новый файл**

```bash
nix develop -c ./scripts/check-architecture.sh
```

Ожидается **отказ**: «двоичная плавающая точка вне numeric/approx.rs». Это правильное поведение старого заслона — он и должен ловить новый файл, пока список не расширен.

- [ ] **Шаг 7: Расширить заслон поимённым списком**

В `scripts/check-architecture.sh` замените заслон 5:

```bash
# --- 5. Двоичная плавающая точка в ядре только в объявленных файлах ---
# Приближённый режим (§6.6) живёт в двух файлах и только в них: политика
# и результат с границей погрешности (approx.rs) и сам решатель ставки
# (xirr.rs). Список задан поимённо, а не маской каталога: маска позволила бы
# завести третий файл с плавающей точкой незаметно.
APPROX_FILES=(
  "numeric/approx.rs"
  "numeric/xirr.rs"
)
if [ -d "$CORE_SRC" ]; then
  hits=$(grep -rn '\bf64\b\|\bf32\b' "$CORE_SRC" --include='*.rs' || true)
  for allowed in "${APPROX_FILES[@]}"; do
    hits=$(printf '%s' "$hits" | { grep -v "^${CORE_SRC}/${allowed}:" || true; })
  done
  hits=$(printf '%s' "$hits" | strip_comments || true)
  if [ -n "$hits" ]; then
    err "двоичная плавающая точка вне приближённого режима (§6.6):"
    echo "$hits" >&2
  fi
fi
```

и заслон 8:

```bash
# --- 8. Приближённый режим не разрастается в теневой расчётный слой ---
# Исключение файла из заслона №5 опасно: в нём можно разместить денежную
# арифметику. Ограничение размера делает это заметным. Порог у каждого файла
# свой: решатель со сканированием диапазона и оценкой погрешности объективно
# длиннее объявления политики. Считаются ВСЕ строки файла, включая тесты, —
# так же, как считались для approx.rs; порог задан с учётом этого.
APPROX_LIMITS=(
  "numeric/approx.rs:200"
  "numeric/xirr.rs:420"
)
for entry in "${APPROX_LIMITS[@]}"; do
  file="$CORE_SRC/${entry%%:*}"
  limit="${entry##*:}"
  [ -f "$file" ] || continue
  lines=$(wc -l < "$file")
  if [ "$lines" -gt "$limit" ]; then
    err "$file разросся до $lines строк при пороге $limit."
    err "Приближённый режим должен оставаться тонким (§6.6)."
  fi
done
```

- [ ] **Шаг 8: Проверить, что заслон не сломан, а расширен**

Это обязательный шаг: расширенный заслон, который перестал ловить нарушения, хуже отсутствующего — он создаёт ложную уверенность.

```bash
nix develop -c ./scripts/check-architecture.sh          # ожидается: пройдены
printf '\nfn _guard_probe() -> f64 { 1.0 }\n' >> crates/iaam-core/src/money.rs
nix develop -c ./scripts/check-architecture.sh          # ожидается: ОТКАЗ на money.rs
git checkout crates/iaam-core/src/money.rs
```

Вывод второго запуска обязан содержать:

```
АРХИТЕКТУРА: двоичная плавающая точка вне приближённого режима (§6.6):
crates/iaam-core/src/money.rs:501:fn _guard_probe() -> f64 { 1.0 }
```

- [ ] **Шаг 9: Коммит**

Изменение файла политики — отдельный коммит с обоснованием, PR помечается меткой `policy-change`.

```bash
git add crates/iaam-core scripts/check-architecture.sh
git commit -m "feat(core): решатель ставки и поимённый список файлов приближённого режима (iaam-1fk)"
```

---

## Задача 7: Независимый эталон ставки и замороженные фикстуры

**Files:**
- Create: `scripts/gen-xirr-fixtures.py`
- Create: `tests/fixtures/xirr_cases.json`
- Create: `crates/iaam-core/tests/xirr_fixtures.rs`
- Modify: `tests/fixtures/MANIFEST.sha256`
- Modify: `crates/iaam-core/Cargo.toml` — `serde_json` в dev-зависимости

**Interfaces:**
- Consumes: `numeric::xirr::{solve, SolverFlow, DayCount}`, `numeric::approx::SolverPolicy`.
- Produces: замороженный корпус эталонных ставок, читаемый тестом ядра.

**Acceptance Criteria:**
- Эталон реализован **другим методом и на другом языке**: бисекция на десятичной арифметике 50 знаков против Ньютона с откатом на бисекцию в двоичной.
- Ни одной строки общего кода с проверяемой реализацией.
- Фикстура заморожена в `MANIFEST.sha256` и не правится ради зелёного теста.
- Тест падает, если фикстура пуста: пустой корпус не является пройденной проверкой.

**Почему эталон именно такой.** §15.4 отвергает пары, которые вырождаются в тавтологию. Второй решатель на Rust, делящий с продакшеном тип `Dec` и структуру серии, поймал бы опечатку, но не ошибку в самой модели дисконтирования. Эталон на Python с `decimal.getcontext().prec = 50`, чистой бисекцией и степенью через `exp(ln)` не делит с ядром ни арифметику, ни алгоритм, ни язык. Два случая корпуса проверяемы вручную без всякой программы: 1000 → 1100 за 365 дней даёт ровно 10 %, а 50 000 → 56 000 за 365 дней — ровно 12 %; они существуют именно для того, чтобы поймать ошибку в самом эталоне.

> **Исправление при исполнении (2026-08-23).** Команда шага 2 в первой редакции давала неразбираемый JSON: `shellHook` во `flake.nix` печатал приветствие в **stdout**, и оно попадало в перенаправленный вывод первой строкой. Починено в источнике, а не обходом в команде: приветствие ушло в stderr. Любой генератор, пишущий в stdout, ломался бы так же.

**Замечание о ловушке в эталоне.** При написании этого плана первая версия генератора сравнивала знаки через `Decimal.copy_sign`, который возвращает величину со знаком аргумента, то есть сравнивала модули. Бисекция «сходилась» к нижней границе, и все шесть значений получились равными `−0.9999`. Ошибку поймали два ручных случая. Функция `sign` в коде ниже написана явно именно поэтому — не заменяйте её обратно.

- [ ] **Шаг 1: Генератор эталонных значений**

`scripts/gen-xirr-fixtures.py`:

```python
#!/usr/bin/env python3
"""Независимый эталон XIRR (§15.4).

Реализация намеренно другая, чем в ядре: бисекция на 50 знаках десятичной
арифметики (`decimal`), без метода Ньютона и без двоичной плавающей точки.
Общего кода с продакшеном нет — он на другом языке.

Значения замораживаются в tests/fixtures/xirr_cases.json и после этого
не пересчитываются ради зелёного теста (§15.7).
"""

import datetime
import json
from decimal import Decimal, getcontext

getcontext().prec = 50

YEAR = Decimal(365)


def sign(value: Decimal) -> int:
    """Знак числа. Decimal.copy_sign сюда не годится: он возвращает величину
    со знаком аргумента, то есть сравнение copy_sign(1) сравнивало бы модули
    и всегда давало бы «знак не менялся»."""
    if value > 0:
        return 1
    if value < 0:
        return -1
    return 0


def npv(rate: Decimal, flows) -> Decimal:
    """Приведённая стоимость. Степень с дробным показателем считается
    через exp(ln), потому что Decimal не умеет возведения в дробную степень."""
    base = Decimal(1) + rate
    total = Decimal(0)
    d0 = flows[0][0]
    for day, amount in flows:
        years = (Decimal((day - d0).days)) / YEAR
        total += amount / (base ** years if years == int(years) else (years * base.ln()).exp())
    return total


def xirr(flows, low=Decimal("-0.9999"), high=Decimal(100)) -> Decimal:
    """Бисекция до 40 знаков. Никакого Ньютона — эталон обязан отличаться
    от проверяемой реализации не только числами, но и методом."""
    f_low = npv(low, flows)
    if sign(f_low) == sign(npv(high, flows)):
        raise ValueError("знак не меняется на границах диапазона")
    for _ in range(400):
        mid = (low + high) / 2
        f_mid = npv(mid, flows)
        if f_mid == 0:
            return mid
        if sign(f_mid) == sign(f_low):
            low, f_low = mid, f_mid
        else:
            high = mid
    return (low + high) / 2


def d(s: str) -> datetime.date:
    return datetime.date.fromisoformat(s)


CASES = [
    {
        "name": "один год, ровно десять процентов",
        "comment": "Вложено 1000, через 365 дней получено 1100.",
        "flows": [("2025-01-01", "-1000"), ("2026-01-01", "1100")],
    },
    {
        "name": "високосный год не даёт ровной ставки",
        "comment": "366 дней между потоками: ставка чуть ниже 10 %.",
        "flows": [("2024-01-01", "-1000"), ("2025-01-01", "1100")],
    },
    {
        "name": "два пополнения и вывод",
        "comment": "Типичный портфель: докупка через полгода.",
        "flows": [
            ("2024-03-01", "-100000"),
            ("2024-09-01", "-50000"),
            ("2026-03-01", "175000"),
        ],
    },
    {
        "name": "убыток",
        "comment": "Вложено 100000, изъято 80000 через два года.",
        "flows": [("2024-01-01", "-100000"), ("2026-01-01", "80000")],
    },
    {
        "name": "купоны между пополнением и выводом",
        "comment": "Облигационный поток: четыре купона и погашение.",
        "flows": [
            ("2024-01-15", "-98000"),
            ("2024-07-15", "4500"),
            ("2025-01-15", "4500"),
            ("2025-07-15", "4500"),
            ("2026-01-15", "104500"),
        ],
    },
    {
        "name": "внутридневная серия не схлопывается",
        "comment": "Два потока в один день плюс вывод через год.",
        "flows": [
            ("2025-02-10", "-30000"),
            ("2025-02-10", "-20000"),
            ("2026-02-10", "56000"),
        ],
    },
]


def main() -> None:
    out = {
        "source": "scripts/gen-xirr-fixtures.py, decimal.getcontext().prec = 50, бисекция 400 шагов",
        "day_count": "act/365",
        "cases": [],
    }
    for case in CASES:
        flows = [(d(day), Decimal(amount)) for day, amount in case["flows"]]
        rate = xirr(flows)
        out["cases"].append(
            {
                "name": case["name"],
                "comment": case["comment"],
                "flows": [{"date": day, "amount": amount} for day, amount in case["flows"]],
                "expected_rate": str(rate.quantize(Decimal("1.000000000000"))),
            }
        )
    print(json.dumps(out, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
```

- [ ] **Шаг 2: Породить и заморозить корпус**

```bash
nix develop -c python3 scripts/gen-xirr-fixtures.py > tests/fixtures/xirr_cases.json
nix develop -c sha256sum tests/fixtures/*.json > tests/fixtures/MANIFEST.sha256
nix develop -c ./scripts/check-fixtures.sh
```

Ожидаемые ставки (сверьте: расхождение означает, что генератор изменён):

| Случай | Ставка |
|---|---|
| один год, ровно десять процентов | `0.100000000000` |
| високосный год не даёт ровной ставки | `0.099713585934` |
| два пополнения и вывод | `0.087668842741` |
| убыток | `-0.105436283082` |
| купоны между пополнением и выводом | `0.103723345963` |
| внутридневная серия не схлопывается | `0.120000000000` |

Первый и последний случаи проверяются в уме: 1100/1000 за год — это 10 %, 56 000/50 000 за год — 12 %.

- [ ] **Шаг 3: Тест соответствия**

`crates/iaam-core/tests/xirr_fixtures.rs`:

```rust
//! Сверка решателя с независимым эталоном (§15.4).

use std::collections::BTreeMap;

use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::numeric::xirr::{DayCount, SolverFlow, solve};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::Date;
use time::macros::format_description;

#[derive(Debug, Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    flows: Vec<Flow>,
    expected_rate: String,
}

#[derive(Debug, Deserialize)]
struct Flow {
    date: String,
    amount: String,
}

fn parse_date(text: &str) -> Date {
    Date::parse(text, format_description!("[year]-[month]-[day]")).expect("дата фикстуры")
}

#[test]
fn solver_matches_independent_decimal_oracle() {
    let raw = include_str!("../../../tests/fixtures/xirr_cases.json");
    let fixture: Fixture = serde_json::from_str(raw).expect("разбор фикстуры");
    assert!(
        !fixture.cases.is_empty(),
        "пустая фикстура ничего не проверяет"
    );

    let mut worst = BTreeMap::new();
    for case in &fixture.cases {
        let first = parse_date(&case.flows[0].date);
        let flows: Vec<SolverFlow> = case
            .flows
            .iter()
            .map(|f| SolverFlow {
                day_offset: (parse_date(&f.date) - first).whole_days(),
                amount: Dec::new(f.amount.parse::<Decimal>().expect("сумма фикстуры")),
            })
            .collect();
        let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365)
            .unwrap_or_else(|e| panic!("{}: решатель отказал: {e}", case.name));
        let expected: f64 = case.expected_rate.parse().expect("ставка фикстуры");
        let delta = (outcome.rate().value() - expected).abs();
        worst.insert(case.name.clone(), delta);
        assert!(
            delta < 1e-7,
            "{}: ставка {} против эталонной {} (расхождение {delta})",
            case.name,
            outcome.rate().value(),
            expected
        );
    }
    println!("{worst:#?}");
}
```

Добавьте `serde_json = "1"` в `[dev-dependencies]` крейты `iaam-core`.

- [ ] **Шаг 4: Зелёная сборка**

```bash
nix develop -c cargo test -p iaam-core --test xirr_fixtures
nix develop -c ./scripts/check-fixtures.sh
```

Ожидается: расхождение решателя с эталоном не превышает 1e-7 (фактически наблюдалось около 1e-13).

- [ ] **Шаг 5: Коммит**

```bash
git add scripts/gen-xirr-fixtures.py tests/fixtures crates/iaam-core
git commit -m "test(core): независимый эталон XIRR на десятичной арифметике (iaam-1fk)"
```

---

## Задача 8: Отчёт о доходности

**Files:**
- Create: `crates/iaam-core/src/returns/mod.rs`
- Create: `crates/iaam-core/src/returns/xirr.rs`
- Modify: `crates/iaam-core/src/lib.rs` — `pub mod returns;`

**Interfaces:**
- Consumes: `projection::state::LedgerState`, `valuation::{FxTable, convert}`, `numeric::xirr::solve`.
- Produces: `returns::{Computed, NotComputable, DataQuality, DataQualityStatus, MaterialIssue, AppliedRules, ReturnsRequest, ReturnsReport, returns_report}`; `returns::xirr::{FlowSeries, flow_series, terminal_value, rate}`.

**Acceptance Criteria:**
- Отчёт отвечает на три вопроса этапа 1: внесено, выведено, XIRR **до налога**.
- Каждая величина — `Computed<T>`: либо значение, либо причина отказа с машиночитаемым кодом.
- Отчёт несёт применённые правила: контур и его версию, идентификатор правила списания, источник курса, базу начисления дней, политику решателя.
- Отчёт несёт блок качества данных и дату начала истории.
- Срез журнала, содержащий события позже даты отчёта, отвергается: это признак неверно собранного среза.
- Статус `Clean` на этапе 1 недостижим и не выставляется.

> **Исправление при исполнении (2026-08-23).** Мутационный прогон модуля отчёта дал семь выживших; четыре из них не закрылись и приёмочными тестами задачи 9. Добавлены три теста: табличный на коды `DataQualityStatus` и два на построение блока качества — с исполнимой ценой (единственная отметка — начало истории, статус `Mixed`) и с оценкой владельца (статус `Incomplete`). Без первого из них отрицание в двух условиях `data_quality` удалялось незамеченно, и статус `Incomplete` стоял бы всегда, перестав что-либо означать.

**Почему период — вся история, а не произвольный интервал.** XIRR за интервал требует стоимости на его начало как терминального потока со знаком минус. Оценка на этапе 1 существует только на дату отчёта: рыночных данных нет, а событий `Valuation` за прошлые даты может не быть ни одного. Подставить вместо начальной стоимости себестоимость означало бы выдать за доходность величину, которой не соответствует ни одна сделка. Интервальный XIRR появится вместе с рядом NAV в E4/E6.

**Знаковая конвенция.** Ряд для решателя — это отрицание движения денег по контуру: то, что для контура приход, для владельца расход. Внесение отрицательно, изъятие положительно, терминальная стоимость положительна. Одна строка `neg(converted)` вместо ветвления по направлению — потому что ветвление здесь и есть то место, где знак путают.

**Что НЕ входит в терминальную стоимость этапа 1.** Комиссии закрытия позиций и налог к уплате — их никто пока не считает. Поэтому величина названа терминальной стоимостью, а не `liquidation_value`: обещать ликвидационную оценку (§5.1) без издержек выхода нельзя.

- [ ] **Шаг 1: Написать падающие тесты**

Тесты этой задачи — приёмочные и живут в `crates/iaam-core/tests/acceptance_stage1.rs` (задача 9). Здесь достаточно двух модульных тестов отказа, в конец `crates/iaam-core/src/returns/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::xirr::SolverRefusal;

    #[test]
    fn every_refusal_has_a_machine_readable_code() {
        assert_eq!(NotComputable::NoExternalFlows.code(), "no_external_flows");
        assert_eq!(
            NotComputable::SolverRefused {
                refusal: SolverRefusal::NoSignChange
            }
            .code(),
            "solver_refused"
        );
        assert_eq!(
            NotComputable::MissingPrice {
                instrument: crate::ids::InstrumentId::new_random()
            }
            .code(),
            "missing_price"
        );
    }

    #[test]
    fn a_not_computable_value_carries_no_number() {
        // Тип не позволяет прочитать число там, где его нет:
        // «ноль с предупреждением» невозможно построить (§15.2).
        let value: Computed<Dec> = Computed::NotComputable {
            reason: NotComputable::NoExternalFlows,
        };
        assert!(value.value().is_none());
        assert_eq!(
            value.reason().map(NotComputable::code),
            Some("no_external_flows")
        );
    }
}
```

- [ ] **Шаг 2: Контракт отчёта**

`crates/iaam-core/src/returns/mod.rs`:

```rust
//! Отчёт о доходности (§6.1, §10.5, §16.3).
//!
//! Честная формулировка результата этапа 1: **XIRR до налога** для
//! простых long-only бумаг. Налоги появляются в E5, и до тех пор ни
//! одно поле этого отчёта не притворяется доходностью после налога.
//!
//! **Период отчёта — вся история счёта.** XIRR за произвольный интервал
//! требует оценки NAV на начало интервала как терминального потока,
//! а оценка на этапе 1 существует только на дату отчёта. Считать
//! интервал, подставив вместо начальной стоимости себестоимость,
//! означало бы выдать за доходность величину, которой не соответствует
//! ни одна сделка.

pub mod xirr;

use serde::{Deserialize, Serialize};
use time::Date;

use crate::contour::{ContourDefinition, ContourId, ContourVersion};
use crate::ids::{AccountId, InstrumentId};
use crate::money::CurrencyCode;
use crate::numeric::approx::SolverPolicy;
use crate::numeric::decimal::Dec;
use crate::numeric::xirr::{DayCount, RateOutcome, SolverRefusal};
use crate::projection::state::LedgerState;
use crate::rules::lot_disposal::RuleId;
use crate::valuation::{FxSource, FxTable, PriceQuality, ValuationError};

/// Величина, которую система может отказаться вычислить.
///
/// Отказ — часть контракта, а не исключительная ситуация: неизвестная
/// цена, отсутствующий курс и уравнение без единственного корня
/// встречаются в нормальной работе (§5.4, §6.1, §10.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Computed<T> {
    Value(T),
    NotComputable { reason: NotComputable },
}

impl<T> Computed<T> {
    #[must_use]
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Value(v) => Some(v),
            Self::NotComputable { .. } => None,
        }
    }

    #[must_use]
    pub const fn reason(&self) -> Option<&NotComputable> {
        match self {
            Self::Value(_) => None,
            Self::NotComputable { reason } => Some(reason),
        }
    }
}

/// Почему величина не вычислена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotComputable {
    /// Нет цены инструмента: стоимость позиции неизвестна.
    MissingPrice { instrument: InstrumentId },
    /// Нет курса на дату.
    MissingFxRate {
        from: CurrencyCode,
        to: CurrencyCode,
        date: Date,
    },
    /// Решатель отказался: корня нет, корней несколько, не сошлось.
    SolverRefused { refusal: SolverRefusal },
    /// Ни одного потока, пересекающего границу контура.
    NoExternalFlows,
    /// Срез журнала содержит события позже даты отчёта: он собран неверно.
    StateNewerThanReport { last_event: Date, as_of: Date },
    /// Арифметическая невозможность: переполнение, деление на ноль.
    Numeric { code: &'static str },
}

impl NotComputable {
    /// Машиночитаемый код для API (§13). Внешний агент разбирает код,
    /// а не текст: текст предназначен человеку.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingPrice { .. } => "missing_price",
            Self::MissingFxRate { .. } => "missing_fx_rate",
            Self::SolverRefused { .. } => "solver_refused",
            Self::NoExternalFlows => "no_external_flows",
            Self::StateNewerThanReport { .. } => "state_newer_than_report",
            Self::Numeric { .. } => "numeric",
        }
    }
}

impl From<ValuationError> for NotComputable {
    fn from(error: ValuationError) -> Self {
        match error {
            ValuationError::MissingPrice { instrument } => Self::MissingPrice { instrument },
            ValuationError::MissingFxRate { from, to, date } => {
                Self::MissingFxRate { from, to, date }
            }
            ValuationError::Numeric(_) => Self::Numeric { code: "numeric" },
        }
    }
}

/// Состояние качества данных (§10.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataQualityStatus {
    /// Все данные подтверждены. На этапе 1 недостижимо: сверки нет.
    Clean,
    /// Часть данных не подтверждена независимо.
    Mixed,
    /// Данных не хватает для полного ответа.
    Incomplete,
}

impl DataQualityStatus {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Mixed => "mixed",
            Self::Incomplete => "incomplete",
        }
    }
}

/// Материальная проблема качества данных. Показывается владельцу
/// только тогда, когда влияет на ответ (§10.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterialIssue {
    /// Позиция восстановлена без документированной стоимости (§10.7).
    RestoredWithoutBasis { account: AccountId },
    /// Цена устарела или является оценкой владельца.
    PriceNotExecutable {
        instrument: InstrumentId,
        quality: PriceQuality,
    },
    /// Отрицательный денежный остаток — обязательство в NAV (§15.9).
    NegativeCash {
        account: AccountId,
        currency: CurrencyCode,
    },
    /// Данных до этой даты нет; всё, что раньше, в расчёт не вошло.
    HistoryStartsAt { date: Date },
}

/// Блок качества данных.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataQuality {
    pub status: DataQualityStatus,
    /// Доля данных без независимого подтверждения.
    ///
    /// На этапе 1 равна единице **по определению, а не по подсчёту**:
    /// сверки не существует, подтверждать нечем. Считать её по полю
    /// `Confidence` было бы подменой: `Confidence` описывает уверенность
    /// в значении (§4.9), а не факт сверки (§10.3).
    pub unconfirmed_share: Dec,
    pub material_issues: Vec<MaterialIssue>,
}

/// Что именно применялось при расчёте. Без этого цифру не воспроизвести
/// (§3.2, §6.1).
///
/// `Eq` не выводится: политика решателя содержит допуск в двоичной
/// плавающей точке, а равенство таких величин не рефлексивно.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedRules {
    pub contour: ContourId,
    pub contour_version: ContourVersion,
    pub lot_rule: Option<RuleId>,
    pub fx_source: FxSource,
    pub day_count: DayCount,
    pub solver_policy: SolverPolicy,
}

/// Запрос отчёта.
#[derive(Debug, Clone, Copy)]
pub struct ReturnsRequest<'a> {
    pub contour: &'a ContourDefinition,
    pub as_of: Date,
    pub report_currency: CurrencyCode,
    pub fx: &'a FxTable,
    pub solver_policy: SolverPolicy,
}

/// Ответ на три вопроса этапа 1.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnsReport {
    pub as_of: Date,
    pub history_starts: Option<Date>,
    pub report_currency: CurrencyCode,
    /// Внесено в контур за всю историю.
    pub contributed: Computed<Dec>,
    /// Выведено из контура за всю историю.
    pub withdrawn: Computed<Dec>,
    /// Стоимость контура на дату отчёта: деньги плюс позиции по цене.
    pub terminal_value: Computed<Dec>,
    /// Внутренняя норма доходности **до налога**.
    pub xirr: Computed<RateOutcome>,
    pub applied_rules: AppliedRules,
    pub data_quality: DataQuality,
}

impl ReturnsReport {
    /// Ярлык результата. Существует, чтобы никакой потребитель API
    /// не назвал эту величину «доходностью» без оговорки (§16.3).
    pub const XIRR_LABEL: &'static str = "xirr_pre_tax";
}

/// Расчёт отчёта.
///
/// Ядро не ходит за данными: цены и курсы приходят готовыми, границы
/// контура заданы явно. Всё, чего не хватает, превращается в отказ
/// с указанием причины, а не в подставленное значение.
#[must_use]
pub fn returns_report(state: &LedgerState, request: &ReturnsRequest) -> ReturnsReport {
    let series = xirr::flow_series(state, request);
    let terminal = xirr::terminal_value(state, request);
    let (contributed, withdrawn) = match &series {
        Ok(series) => (
            Computed::Value(series.contributed),
            Computed::Value(series.withdrawn),
        ),
        Err(reason) => (
            Computed::NotComputable {
                reason: reason.clone(),
            },
            Computed::NotComputable {
                reason: reason.clone(),
            },
        ),
    };
    let terminal_value = match &terminal {
        Ok(value) => Computed::Value(*value),
        Err(reason) => Computed::NotComputable {
            reason: reason.clone(),
        },
    };
    let rate = xirr::rate(&series, &terminal, request);

    ReturnsReport {
        as_of: request.as_of,
        history_starts: state.coverage().first_event(),
        report_currency: request.report_currency,
        contributed,
        withdrawn,
        terminal_value,
        xirr: rate,
        applied_rules: AppliedRules {
            contour: request.contour.id(),
            contour_version: request.contour.version(),
            lot_rule: state.book().applied_rule().cloned(),
            fx_source: request.fx.source().clone(),
            day_count: DayCount::Act365,
            solver_policy: request.solver_policy,
        },
        data_quality: data_quality(state),
    }
}

/// Блок качества данных строится из состояния, а не из желания
/// показать зелёный статус: на этапе 1 подтверждать нечем, поэтому
/// `Clean` недостижим, и это записано прямо здесь.
fn data_quality(state: &LedgerState) -> DataQuality {
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
    // Начало истории сообщается всегда, но неполнотой не является:
    // «данных до 01.03.2024 нет» — это факт о периоде, а не дефект.
    // Статусом управляют остальные проблемы.
    let material = issues
        .iter()
        .any(|issue| !matches!(issue, MaterialIssue::HistoryStartsAt { .. }));
    DataQuality {
        // `Clean` на этапе 1 недостижим и не выставляется: подтверждать
        // данные нечем, пока нет сверки (E2).
        status: if material {
            DataQualityStatus::Incomplete
        } else {
            DataQualityStatus::Mixed
        },
        // Этап 1: независимого подтверждения нет ни у одного события,
        // потому что механизма подтверждения ещё не существует (E2).
        unconfirmed_share: Dec::one(),
        material_issues: issues,
    }
}
```

- [ ] **Шаг 3: Доменная обёртка решателя**

`crates/iaam-core/src/returns/xirr.rs`:

```rust
//! Доменная обёртка решателя ставки (§6.1).
//!
//! Здесь живёт то, что решатель знать не должен: границы контура, валюты,
//! курсы, цены и знаковая конвенция. Сам решатель работает с парами
//! «смещение в днях, сумма» и о портфеле ничего не знает.

use time::Date;

use super::{Computed, NotComputable, ReturnsRequest};
use crate::money::CurrencyCode;
use crate::numeric::decimal::Dec;
use crate::numeric::xirr::{DayCount, RateOutcome, SolverFlow, solve};
use crate::projection::flows::FlowDirection;
use crate::projection::state::LedgerState;
use crate::valuation::convert;

/// Ряд потоков в валюте отчёта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowSeries {
    /// Внесено за всю историю, положительная величина.
    pub contributed: Dec,
    /// Выведено за всю историю, положительная величина.
    pub withdrawn: Dec,
    /// Датированные суммы **в знаковой конвенции владельца**: внесение
    /// отрицательно, изъятие положительно. Это отрицание движения денег
    /// по контуру: то, что для контура приход, для владельца расход.
    pub flows: Vec<(Date, Dec)>,
}

/// Перевод внешних потоков в валюту отчёта.
pub fn flow_series(
    state: &LedgerState,
    request: &ReturnsRequest,
) -> Result<FlowSeries, NotComputable> {
    guard_state_not_newer(state, request.as_of)?;
    let mut contributed = Dec::zero();
    let mut withdrawn = Dec::zero();
    let mut flows = Vec::new();

    for flow in state.flows().external() {
        if flow.date > request.as_of {
            continue;
        }
        let converted = convert(flow.amount, request.report_currency, flow.date, request.fx)?;
        match flow.direction {
            FlowDirection::In => contributed = add(contributed, converted)?,
            FlowDirection::Out => withdrawn = sub(withdrawn, converted)?,
        }
        flows.push((flow.date, neg(converted)?));
    }
    flows.sort_by_key(|(date, _)| *date);
    Ok(FlowSeries {
        contributed,
        withdrawn,
        flows,
    })
}

/// Стоимость контура на дату отчёта: деньги плюс позиции по последней цене.
///
/// Это **ликвидационная** оценка в упрощённом виде (§5.1): комиссий
/// закрытия и налога к уплате в ней нет, потому что ни того, ни другого
/// этап 1 не считает. Разрыв с `contractual_hold_value` не вычисляется —
/// вклады и облигации целиком относятся к E3.
pub fn terminal_value(state: &LedgerState, request: &ReturnsRequest) -> Result<Dec, NotComputable> {
    guard_state_not_newer(state, request.as_of)?;
    let mut total = Dec::zero();

    for (account, money) in state.balances().iter_cash() {
        if !request.contour.contains(account) {
            continue;
        }
        total = add(
            total,
            convert(money, request.report_currency, request.as_of, request.fx)?,
        )?;
    }

    for (key, quantity) in state.balances().iter_positions() {
        if !request.contour.contains(key.account) {
            continue;
        }
        if quantity.0.is_zero() {
            continue;
        }
        let price = state
            .prices()
            .latest(key.instrument)
            .ok_or(NotComputable::MissingPrice {
                instrument: key.instrument,
            })?;
        let local = mul(quantity.0, price.price)?;
        total = add(total, in_report_currency(local, price.currency, request)?)?;
    }
    Ok(total)
}

/// Ставка по ряду потоков и терминальной стоимости.
pub fn rate(
    series: &Result<FlowSeries, NotComputable>,
    terminal: &Result<Dec, NotComputable>,
    request: &ReturnsRequest,
) -> Computed<RateOutcome> {
    let series = match series {
        Ok(series) => series,
        Err(reason) => {
            return Computed::NotComputable {
                reason: reason.clone(),
            };
        }
    };
    let terminal = match terminal {
        Ok(value) => *value,
        Err(reason) => {
            return Computed::NotComputable {
                reason: reason.clone(),
            };
        }
    };
    let Some((first_date, _)) = series.flows.first() else {
        return Computed::NotComputable {
            reason: NotComputable::NoExternalFlows,
        };
    };

    let mut solver_flows: Vec<SolverFlow> = series
        .flows
        .iter()
        .map(|(date, amount)| SolverFlow {
            day_offset: (*date - *first_date).whole_days(),
            amount: *amount,
        })
        .collect();
    solver_flows.push(SolverFlow {
        day_offset: (request.as_of - *first_date).whole_days(),
        amount: terminal,
    });

    match solve(&solver_flows, request.solver_policy, DayCount::Act365) {
        Ok(outcome) => Computed::Value(outcome),
        Err(refusal) => Computed::NotComputable {
            reason: NotComputable::SolverRefused { refusal },
        },
    }
}

/// Состояние обязано быть спроецировано **по дату отчёта**: фильтрацию
/// журнала делает оболочка при сборке среза, а не ядро. Событие позже
/// даты отчёта означает, что срез собран неверно, и молча посчитать
/// по нему — значит выдать отчёт на дату, которого на эту дату не было.
fn guard_state_not_newer(state: &LedgerState, as_of: Date) -> Result<(), NotComputable> {
    match state.coverage().last_event() {
        Some(last) if last > as_of => Err(NotComputable::StateNewerThanReport {
            last_event: last,
            as_of,
        }),
        _ => Ok(()),
    }
}

fn in_report_currency(
    amount: Dec,
    currency: CurrencyCode,
    request: &ReturnsRequest,
) -> Result<Dec, NotComputable> {
    let rate = request
        .fx
        .rate(currency, request.report_currency, request.as_of)
        .ok_or(NotComputable::MissingFxRate {
            from: currency,
            to: request.report_currency,
            date: request.as_of,
        })?;
    mul(amount, rate)
}

fn add(left: Dec, right: Dec) -> Result<Dec, NotComputable> {
    left.checked_add(right).map_err(numeric)
}

fn sub(left: Dec, right: Dec) -> Result<Dec, NotComputable> {
    left.checked_sub(right).map_err(numeric)
}

fn mul(left: Dec, right: Dec) -> Result<Dec, NotComputable> {
    left.checked_mul(right).map_err(numeric)
}

fn neg(value: Dec) -> Result<Dec, NotComputable> {
    value.checked_neg().map_err(numeric)
}

fn numeric(_: crate::numeric::NumericError) -> NotComputable {
    NotComputable::Numeric { code: "numeric" }
}
```

- [ ] **Шаг 4: Границы отчёта**

Ошибка в один день здесь не выглядит ошибкой: она даёт цифру, просто не ту. Обе границы — строгость сравнения дат — проверяются отдельным файлом `crates/iaam-core/tests/returns_boundaries.rs`:

```rust
//! Границы отчёта: дата отчёта включительно и запрет считать по срезу,
//! собранному не на ту дату.
//!
//! Обе проверки — про строгость сравнения дат. Ошибка в один день здесь
//! не выглядит ошибкой: она даёт цифру, просто не ту.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::{Projection, ProjectionContext, project};
use iaam_core::returns::xirr::{flow_series, terminal_value};
use iaam_core::returns::{NotComputable, ReturnsRequest};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn deposit(owner: OwnerId, account: AccountId, day: Date, sequence: u32, minor: i64) -> Event {
    let amount = rub(minor);
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind: EventKind::CashIn { amount },
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs: vec![Leg::cash(account, amount)],
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"8".repeat(64)).expect("хеш"),
            ParserVersion("boundary/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

struct Fixture {
    contour: ContourDefinition,
    projection: Projection,
}

fn project_days(days: &[(Date, i64)]) -> Fixture {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let events: Vec<Event> = days
        .iter()
        .enumerate()
        .map(|(i, (day, minor))| {
            deposit(
                owner,
                account,
                *day,
                u32::try_from(i).unwrap_or(u32::MAX) + 1,
                *minor,
            )
        })
        .collect();
    let projection = project(
        &events,
        &ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        },
    )
    .expect("проекция строится");
    Fixture {
        contour,
        projection,
    }
}

#[test]
fn a_flow_on_the_report_date_is_included() {
    // Дата отчёта включительна. Строгое «раньше» отрезало бы операцию
    // того же дня, и отчёт «на сегодня» не видел бы сегодняшнее
    // пополнение.
    let as_of = date!(2026 - 01 - 01);
    let fixture = project_days(&[(date!(2025 - 06 - 01), 10_000_000), (as_of, 5_000_000)]);
    let fx = FxTable::new(FxSource::OwnerSupplied);
    let request = ReturnsRequest {
        contour: &fixture.contour,
        as_of,
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
    };

    let series = flow_series(fixture.projection.state(), &request).expect("ряд потоков");
    assert_eq!(series.flows.len(), 2, "поток на дату отчёта обязан войти");
    assert_eq!(series.contributed, Dec::new(Decimal::from(150_000)));
}

#[test]
fn a_slice_containing_events_after_the_report_date_is_refused() {
    // Срез на дату собирает оболочка. Событие позже даты отчёта означает,
    // что срез собран неверно, и посчитать по нему — значит выдать отчёт
    // на дату, которого на эту дату не существовало.
    let as_of = date!(2026 - 01 - 01);
    let fixture = project_days(&[(as_of, 10_000_000), (date!(2026 - 02 - 01), 1_000_000)]);
    let fx = FxTable::new(FxSource::OwnerSupplied);
    let request = ReturnsRequest {
        contour: &fixture.contour,
        as_of,
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
    };

    assert!(matches!(
        flow_series(fixture.projection.state(), &request),
        Err(NotComputable::StateNewerThanReport { .. })
    ));
    assert!(matches!(
        terminal_value(fixture.projection.state(), &request),
        Err(NotComputable::StateNewerThanReport { .. })
    ));
}

#[test]
fn a_slice_ending_exactly_on_the_report_date_is_accepted() {
    // Граница на единицу: последнее событие ровно на дату отчёта —
    // это нормальный срез, а не сбор на будущее.
    let as_of = date!(2026 - 01 - 01);
    let fixture = project_days(&[(as_of, 10_000_000)]);
    let fx = FxTable::new(FxSource::OwnerSupplied);
    let request = ReturnsRequest {
        contour: &fixture.contour,
        as_of,
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
    };
    assert!(flow_series(fixture.projection.state(), &request).is_ok());
    assert!(terminal_value(fixture.projection.state(), &request).is_ok());
}
```

- [ ] **Шаг 5: Зелёная сборка**

```bash
nix develop -c cargo test -p iaam-core returns
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Шаг 6: Коммит**

```bash
git add crates/iaam-core
git commit -m "feat(core): отчёт о доходности с явными отказами и качеством данных (iaam-1fk)"
```

---

## Задача 9: Приёмка ядра и свойства проекций

**Files:**
- Create: `crates/iaam-core/tests/acceptance_stage1.rs`
- Modify: `crates/iaam-core/tests/properties.rs` — свойства проекций
- Modify: `scripts/check-mutants.sh` — новые критичные модули

**Acceptance Criteria:**
- Приёмочный критерий эпика выполняется на уровне ядра: по одному счёту с ручным вводом система отвечает, сколько внесено, сколько выведено и какова доходность до налога.
- Ожидаемая ставка приёмочного сценария получена независимым эталоном, а не выводом программы.
- Свойства §15.3, применимые к проекциям, проверяются `proptest` с указанием области.
- Новые доменные модули внесены в список мутационного заслона.

**Проверяемые свойства и их область применимости.**

| Свойство | Область |
|---|---|
| Перестановка порядка импорта не меняет проекцию | всегда (§4.8) |
| Повторный импорт того же события не меняет состояние | только при совпадающем `EventId` — дедупликация по ключам живёт в приёмке (§10.6) |
| Событие вместе со своим сторно оставляют состояние нетронутым | всегда |
| Перевод между счетами внутри контура не меняет ряд внешних потоков | всегда (§15.9) |
| `advance` совпадает с полным пересчётом | только для пачки строго после границы снимка |

**Свойств, которых здесь нет и не должно появиться:** склейка периодов для XIRR (IRR не цепляется), масштабирование сумм при налогах (их ещё нет, но и потом нельзя), сдвиг всех дат (налоговые годы и база начисления дней его ломают).

- [ ] **Шаг 1: Приёмочный тест**

`crates/iaam-core/tests/acceptance_stage1.rs`:

```rust
//! Приёмка этапа 1 (§16.3): по одному счёту с ручным вводом система
//! отвечает, сколько внесено, сколько выведено и какова доходность
//! до налога.
//!
//! Ожидаемая ставка получена независимым эталоном на Python
//! (`scripts/gen-xirr-fixtures.py`, арифметика `decimal`, 50 знаков),
//! а не выводом проверяемой программы (§15.5).

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, TradeDate};
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::returns::{ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable, PriceQuality};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

struct Fixture {
    owner: OwnerId,
    account: AccountId,
    custody: CustodyId,
    instrument: InstrumentId,
    source: SourceId,
    sequence: u32,
}

impl Fixture {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            custody: CustodyId::new_random(),
            instrument: InstrumentId::new_random(),
            source: SourceId::new_random(),
            sequence: 0,
        }
    }

    fn provenance(&self) -> Provenance {
        Provenance::new(
            self.source,
            RawHash::parse(&"a".repeat(64)).expect("хеш фикстуры"),
            ParserVersion("manual/1".into()),
        )
    }

    fn event(&mut self, day: Date, kind: EventKind, dates: EventDates, legs: Vec<Leg>) -> Event {
        self.sequence += 1;
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account: self.account,
            kind,
            dates,
            order: EffectiveOrder::new(day, self.sequence),
            legs,
            provenance: self.provenance(),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
}

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

/// Ручной ввод: пополнение, покупка, дивиденд, вывод, оценка.
fn journal(fixture: &mut Fixture) -> Vec<Event> {
    let deposit = rub(10_000_000);
    let gross = rub(9_000_000);
    // Комиссия сделки задаётся ПОЛОЖИТЕЛЬНОЙ величиной: `trade_settlement`
    // прибавляет её к телу сделки и уже потом меняет знак при покупке
    // (у продажи — вычитает из выручки). Отрицательная комиссия здесь
    // уменьшила бы стоимость покупки — проверено: `AmountMismatch`.
    let fee = rub(10_000);
    let dividend = rub(300_000);
    let withdrawal = rub(-1_000_000);

    vec![
        fixture.event(
            date!(2025 - 01 - 01),
            EventKind::CashIn { amount: deposit },
            EventDates::for_cash(CashPostedDate(date!(2025 - 01 - 01))),
            vec![Leg::cash(fixture.account, deposit)],
        ),
        {
            let settlement = rub(-(9_000_000 + 10_000));
            let account = fixture.account;
            let custody = fixture.custody;
            let instrument = fixture.instrument;
            fixture.event(
                date!(2025 - 01 - 15),
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(100),
                    gross,
                    fee: Some(fee),
                    accrued_interest: None,
                },
                EventDates::for_trade(TradeDate(date!(2025 - 01 - 15)), None),
                vec![
                    Leg::cash(account, settlement),
                    Leg::security(account, custody, instrument, qty(100)),
                ],
            )
        },
        {
            let account = fixture.account;
            let instrument = fixture.instrument;
            fixture.event(
                date!(2025 - 07 - 01),
                EventKind::Income {
                    instrument: Some(instrument),
                    gross: dividend,
                },
                EventDates::for_cash(CashPostedDate(date!(2025 - 07 - 01))),
                vec![Leg::cash(account, dividend)],
            )
        },
        {
            let account = fixture.account;
            fixture.event(
                date!(2025 - 09 - 01),
                EventKind::CashOut { amount: withdrawal },
                EventDates::for_cash(CashPostedDate(date!(2025 - 09 - 01))),
                vec![Leg::cash(account, withdrawal)],
            )
        },
        {
            let instrument = fixture.instrument;
            fixture.event(
                date!(2026 - 01 - 01),
                EventKind::Valuation {
                    instrument,
                    price: Dec::new(Decimal::from(1_000)),
                    currency: CurrencyCode::Rub,
                    quality: PriceQuality::PreviousClose,
                },
                EventDates::for_cash(CashPostedDate(date!(2026 - 01 - 01))),
                vec![],
            )
        },
    ]
}

#[test]
fn single_account_answers_the_three_questions_of_stage_one() {
    let mut fixture = Fixture::new();
    let events = journal(&mut fixture);
    let contour = ContourDefinition::new(
        ContourId::new_random(),
        ContourVersion(1),
        [fixture.account],
    );
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };

    let projection = project(&events, &ctx).expect("проекция строится");
    let state = projection.state();

    // Деньги: 100 000 − 90 100 + 3 000 − 10 000 = 2 900 рублей.
    assert_eq!(
        state.balances().cash(fixture.account, CurrencyCode::Rub),
        Some(rub(290_000))
    );
    // Позиция: 100 бумаг, стоимость приобретения 90 100 рублей.
    assert_eq!(
        state
            .balances()
            .quantity_of(fixture.account, fixture.instrument)
            .expect("количество"),
        qty(100)
    );

    // Инварианты проверены, а не просто «не упало»: отчёт перечисляет,
    // что именно проверялось.
    assert!(!projection.invariants().checked().is_empty());

    let fx = FxTable::new(FxSource::OwnerSupplied);
    let request = ReturnsRequest {
        contour: &contour,
        as_of: date!(2026 - 01 - 01),
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
    };
    let report = returns_report(state, &request);

    assert_eq!(
        report.contributed.value(),
        Some(&Dec::new(Decimal::from(100_000)))
    );
    assert_eq!(
        report.withdrawn.value(),
        Some(&Dec::new(Decimal::from(10_000)))
    );
    // 2 900 денег + 100 бумаг по 1 000 = 102 900.
    assert_eq!(
        report.terminal_value.value(),
        Some(&Dec::new(Decimal::from(102_900)))
    );

    let outcome = report.xirr.value().expect("ставка вычислена");
    let expected = 0.133_270_341_032_f64;
    assert!(
        (outcome.rate().value() - expected).abs() < 1e-7,
        "ставка {} против эталонной {expected}",
        outcome.rate().value()
    );
    // Дивиденд границу контура не пересекает: он не является вложением.
    assert_eq!(state.flows().external().len(), 2);
}
```

- [ ] **Шаг 2: Прогон**

```bash
nix develop -c cargo test -p iaam-core --test acceptance_stage1
```

Ожидаемая ставка `0.133270341032` получена тем же эталоном, что и корпус задачи 7:

```bash
nix develop -c python3 -c "
import importlib.util, datetime
from decimal import Decimal
spec = importlib.util.spec_from_file_location('g', 'scripts/gen-xirr-fixtures.py')
g = importlib.util.module_from_spec(spec); spec.loader.exec_module(g)
d = datetime.date.fromisoformat
flows = [(d('2025-01-01'), Decimal('-100000')),
         (d('2025-09-01'), Decimal('10000')),
         (d('2026-01-01'), Decimal('102900'))]
print(g.xirr(flows).quantize(Decimal('1.000000000000')))
"
```

- [ ] **Шаг 3: Свойства проекций**

В конец `crates/iaam-core/tests/properties.rs` (файл уже существует с шапкой, объясняющей отсутствующие свойства — не трогайте её):

```rust
mod projection_properties {
    use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
    use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::leg::Leg;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
    use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PostedMinor};
    use iaam_core::projection::{ProjectionContext, project};
    use iaam_core::rules::{LotRuleVersion, RuleRegistry};
    use proptest::prelude::*;
    use time::macros::date;

    fn deposit(account: AccountId, sequence: u32, minor: i64) -> Event {
        let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
        let day = date!(2025 - 01 - 01) + time::Duration::days(i64::from(sequence));
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"e".repeat(64)).unwrap(),
                ParserVersion("prop/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    proptest! {
        /// Область: всегда (§4.8). Порядок задаёт `EffectiveOrder`,
        /// а не порядок загрузки файлов.
        #[test]
        fn import_order_never_changes_the_projection(
            amounts in prop::collection::vec(1_i64..1_000_000, 1..12),
            rotation in 0_usize..12,
        ) {
            let account = AccountId::new_random();
            let contour = ContourDefinition::new(
                ContourId::new_random(),
                ContourVersion(1),
                [account],
            );
            let rules = RuleRegistry::with_defaults();
            let ctx = ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            };

            let events: Vec<Event> = amounts
                .iter()
                .enumerate()
                .map(|(i, minor)| {
                    let index = u32::try_from(i).unwrap_or(u32::MAX);
                    deposit(account, index + 1, *minor)
                })
                .collect();

            let mut rotated = events.clone();
            let shift = rotation % events.len().max(1);
            rotated.rotate_left(shift);

            prop_assert_eq!(
                project(&events, &ctx).unwrap().snapshot().fingerprint(),
                project(&rotated, &ctx).unwrap().snapshot().fingerprint()
            );
        }

        /// Область: всегда. Сторно вместе с исходным событием не оставляют
        /// следа ни в остатках, ни в потоках.
        #[test]
        fn an_event_and_its_reversal_leave_no_trace(minor in 1_i64..1_000_000) {
            let account = AccountId::new_random();
            let contour = ContourDefinition::new(
                ContourId::new_random(),
                ContourVersion(1),
                [account],
            );
            let rules = RuleRegistry::with_defaults();
            let ctx = ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            };

            let original = deposit(account, 1, minor);
            let mut reversal = deposit(account, 2, minor);
            reversal.relation = Relation::Reversal { target: original.id };

            let projection = project(&[original, reversal], &ctx).unwrap();
            prop_assert!(projection.state().flows().external().is_empty());
            prop_assert_eq!(
                projection.state().balances().cash(account, CurrencyCode::Rub),
                None
            );
        }
    }
}
```

- [ ] **Шаг 4: Метаморфные тесты (§15.6)**

Метаморфный тест проверяет не значение, а **преобразование**: что должно измениться и что обязано остаться прежним. Три безопасных преобразования спеки, применимых на этапе 1, — счёт вне контура, разделение инструмента на два одинаковых, масштабирование всех потоков. Четвёртое (перестановка независимых счетов) покрыто свойством «порядок импорта не меняет проекцию» из шага 3.

`crates/iaam-core/tests/metamorphic.rs`:

```rust
//! Метаморфные тесты (§15.6).
//!
//! Проверяют не значение, а **преобразование**: что должно измениться
//! и что обязано остаться прежним. Область применимости у каждого своя
//! и указана явно — метаморфное свойство без оговорки так же опасно,
//! как обычное (§15.3).

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::returns::{Computed, ReturnsReport, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable, PriceQuality};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

struct Ledger {
    owner: OwnerId,
    source: SourceId,
    sequence: u32,
    events: Vec<Event>,
}

impl Ledger {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            source: SourceId::new_random(),
            sequence: 0,
            events: Vec::new(),
        }
    }

    fn push(&mut self, account: AccountId, day: Date, kind: EventKind, legs: Vec<Leg>) {
        self.sequence += 1;
        self.events.push(Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, self.sequence),
            legs,
            provenance: Provenance::new(
                self.source,
                RawHash::parse(&"5".repeat(64)).expect("хеш"),
                ParserVersion("meta/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        });
    }

    fn deposit(&mut self, account: AccountId, day: Date, minor: i64) {
        let amount = rub(minor);
        self.push(
            account,
            day,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
    }

    fn withdraw(&mut self, account: AccountId, day: Date, minor: i64) {
        let amount = rub(-minor);
        self.push(
            account,
            day,
            EventKind::CashOut { amount },
            vec![Leg::cash(account, amount)],
        );
    }

    fn buy(
        &mut self,
        account: AccountId,
        day: Date,
        instrument: InstrumentId,
        units: i64,
        gross: i64,
    ) {
        let custody = CustodyId::new_random();
        self.push(
            account,
            day,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(units),
                gross: rub(gross),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, rub(-gross)),
                Leg::security(account, custody, instrument, qty(units)),
            ],
        );
    }

    fn valuation(&mut self, account: AccountId, day: Date, instrument: InstrumentId, price: i64) {
        self.push(
            account,
            day,
            EventKind::Valuation {
                instrument,
                price: Dec::new(Decimal::from(price)),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::PreviousClose,
            },
            vec![],
        );
    }
}

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

fn report(events: &[Event], accounts: &[AccountId], as_of: Date) -> ReturnsReport {
    let contour = ContourDefinition::new(
        ContourId::new_random(),
        ContourVersion(1),
        accounts.to_vec(),
    );
    let rules = RuleRegistry::with_defaults();
    let projection = project(
        events,
        &ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        },
    )
    .expect("проекция строится");
    let fx = FxTable::new(FxSource::OwnerSupplied);
    returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &contour,
            as_of,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
        },
    )
}

fn rate_of(report: &ReturnsReport) -> f64 {
    match &report.xirr {
        Computed::Value(outcome) => outcome.rate().value(),
        Computed::NotComputable { reason } => {
            panic!("ставка не вычислена: {}", reason.code())
        }
    }
}

/// Область: всегда (§4.10). Счёт вне контура на доходность контура
/// не влияет — именно из-за нарушения этого правила чужие сервисы
/// показывают доходность, в которую попали чужие деньги.
#[test]
fn an_account_outside_the_contour_does_not_change_the_rate() {
    let inside = AccountId::new_random();
    let outside = AccountId::new_random();
    let instrument = InstrumentId::new_random();

    let mut ledger = Ledger::new();
    ledger.deposit(inside, date!(2025 - 01 - 01), 10_000_000);
    ledger.buy(inside, date!(2025 - 02 - 01), instrument, 100, 9_000_000);
    ledger.valuation(inside, date!(2026 - 01 - 01), instrument, 1_000);
    let base = report(&ledger.events, &[inside], date!(2026 - 01 - 01));

    // Тот же журнал плюс бурная деятельность на счёте вне контура.
    ledger.deposit(outside, date!(2025 - 03 - 01), 50_000_000);
    ledger.withdraw(outside, date!(2025 - 04 - 01), 20_000_000);
    let widened = report(&ledger.events, &[inside], date!(2026 - 01 - 01));

    assert!(
        (rate_of(&base) - rate_of(&widened)).abs() < 1e-12,
        "счёт вне контура изменил ставку"
    );
    assert_eq!(base.contributed, widened.contributed);
    assert_eq!(base.terminal_value, widened.terminal_value);
}

/// Область: инструменты без корпоративных действий и без правил,
/// зависящих от количества (минимальная комиссия, лот). На этапе 1
/// таких правил нет; при их появлении свойство перестанет выполняться,
/// и его придётся сузить, а не «починить».
#[test]
fn splitting_one_instrument_into_two_identical_halves_keeps_the_aggregates() {
    let account = AccountId::new_random();
    let single = InstrumentId::new_random();
    let first = InstrumentId::new_random();
    let second = InstrumentId::new_random();
    let as_of = date!(2026 - 01 - 01);

    let mut whole = Ledger::new();
    whole.deposit(account, date!(2025 - 01 - 01), 10_000_000);
    whole.buy(account, date!(2025 - 02 - 01), single, 100, 9_000_000);
    whole.valuation(account, as_of, single, 1_000);

    let mut halves = Ledger::new();
    halves.deposit(account, date!(2025 - 01 - 01), 10_000_000);
    halves.buy(account, date!(2025 - 02 - 01), first, 50, 4_500_000);
    halves.buy(account, date!(2025 - 02 - 01), second, 50, 4_500_000);
    halves.valuation(account, as_of, first, 1_000);
    halves.valuation(account, as_of, second, 1_000);

    let left = report(&whole.events, &[account], as_of);
    let right = report(&halves.events, &[account], as_of);

    assert_eq!(left.terminal_value, right.terminal_value);
    assert_eq!(left.contributed, right.contributed);
    assert!((rate_of(&left) - rate_of(&right)).abs() < 1e-12);
}

/// Область: **масштабная инвариантность ставки** при отключённых налогах,
/// порогах и минимальных комиссиях (§15.3). Линейные величины умножаются
/// на `k`, ставка не меняется. При появлении прогрессивной шкалы
/// свойство становится неверным.
#[test]
fn scaling_every_flow_scales_the_amounts_and_leaves_the_rate() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let as_of = date!(2026 - 01 - 01);
    let factor = 7;

    let mut plain = Ledger::new();
    plain.deposit(account, date!(2025 - 01 - 01), 10_000_000);
    plain.buy(account, date!(2025 - 02 - 01), instrument, 100, 9_000_000);
    plain.withdraw(account, date!(2025 - 08 - 01), 500_000);
    plain.valuation(account, as_of, instrument, 1_000);

    let mut scaled = Ledger::new();
    scaled.deposit(account, date!(2025 - 01 - 01), 10_000_000 * factor);
    scaled.buy(
        account,
        date!(2025 - 02 - 01),
        instrument,
        100 * factor,
        9_000_000 * factor,
    );
    scaled.withdraw(account, date!(2025 - 08 - 01), 500_000 * factor);
    // Цена за единицу не масштабируется: масштабируется количество.
    scaled.valuation(account, as_of, instrument, 1_000);

    let left = report(&plain.events, &[account], as_of);
    let right = report(&scaled.events, &[account], as_of);

    assert!(
        (rate_of(&left) - rate_of(&right)).abs() < 1e-9,
        "ставка изменилась при масштабировании: {} против {}",
        rate_of(&left),
        rate_of(&right)
    );
    let scaled_contribution = left
        .contributed
        .value()
        .expect("внесено")
        .checked_mul(Dec::new(Decimal::from(factor)))
        .expect("умножение");
    assert_eq!(right.contributed.value(), Some(&scaled_contribution));
}
```

```bash
nix develop -c cargo test -p iaam-core --test metamorphic
```

- [ ] **Шаг 5: Мутационный заслон на новых модулях**

В `scripts/check-mutants.sh`, в массив `MODULES`, добавьте:

```bash
  "crates/iaam-core/src/numeric/decimal.rs"
  "crates/iaam-core/src/projection/balances.rs"
  "crates/iaam-core/src/projection/lots.rs"
  "crates/iaam-core/src/projection/flows.rs"
  "crates/iaam-core/src/projection/invariants.rs"
  "crates/iaam-core/src/projection/state.rs"
  "crates/iaam-core/src/projection/mod.rs"
  "crates/iaam-core/src/numeric/xirr.rs"
  "crates/iaam-core/src/returns/xirr.rs"
  "crates/iaam-core/src/returns/mod.rs"
  "crates/iaam-core/src/valuation.rs"
```

> **Исправление при исполнении (2026-08-23).** Первая редакция добавляла семь модулей и пропускала четыре: `numeric/decimal.rs` — числовое основание всех денежных расчётов; `projection/state.rs` и `projection/mod.rs` — состояние, отпечатки и `advance`; `returns/mod.rs` — контракт отчёта, то есть ровно то место, где решается, доверять ли цифре. Пропуск не был случайным следствием: в каждом из четырёх при исполнении нашлись выжившие мутанты (бид iaam-1fk.22).

```bash
nix develop -c ./scripts/check-mutants.sh
```

Прогон долгий: около получаса на все шестнадцать модулей. **Ожидается ноль выживших** — именно это состояние достигнуто при написании плана, и снижать планку не нужно.

Выживший мутант означает, что какой-то тест ничего не проверяет. Порядок действий:

1. **Сначала ищите тест.** При написании плана заслон дал 86 выживших, и почти все закрылись обычными тестами: пути отказа проверок, значения `code()`, значения аксессоров, счётчики проверенных инвариантов, границы сравнений.
2. **Если проверка недостижима изнутри — вынесите её туда, где достижима.** Последние три мутанта сидели на защите, до которой цикл не доходит по построению; вынос предиката в отдельную функцию сделал её проверяемой напрямую. «Недостижимо» — почти всегда повод изменить код, а не признать мутанта эквивалентным.
3. **Объявление мутанта эквивалентным — последнее средство**, и только письменно, в описании бида (§15.7). Известные слепые зоны инструмента описаны в `docs/irreversible-core.md`: имя `new`, `is_zero`, замыкания в `.map(...).sum()`, тела `else`.

Два настоящих дефекта решателя были найдены именно так — тестами, написанными ради убийства мутантов, а не ревью и не сборкой.

- [ ] **Шаг 6: Коммит**

```bash
git add crates/iaam-core scripts/check-mutants.sh
git commit -m "test(core): приёмка этапа 1, свойства и метаморфные тесты (iaam-1fk)"
```

---

# Часть B — оболочка

## Задача 10: `iaam-store` — журнал фактов

**Files:**
- Create: `crates/iaam-store/Cargo.toml`
- Create: `crates/iaam-store/migrations/0001_initial.sql`
- Create: `crates/iaam-store/src/lib.rs`, `src/schema.rs`, `src/events.rs`
- Create: `crates/iaam-store/tests/journal.rs`
- Modify: `Cargo.toml` — крейта в `members`

**Interfaces:**
- Produces: `iaam_store::{SqliteStore, StoreError}`, `iaam_store::schema::{migrate, SCHEMA_VERSION}`, `iaam_store::events::Appended`; методы `SqliteStore::{open, open_in_memory, append_event, append_event_in_order, load_events, load_events_through}`.

**Acceptance Criteria:**
- `UPDATE` и `DELETE` по таблице событий отклоняются **базой**, а не кодом.
- Повторная запись по ключу идемпотентности возвращает первое событие, а не создаёт второе.
- Порядковый номер внутри дня назначается **в той же транзакции**, что и вставка, а уникальный индекс `(owner, effective_date, sequence)` делает гонку ошибкой, а не тихой перестановкой.
- Две одинаковые операции в один день записываются обе.
- Срез по дату не включает более поздние события.
- Миграции идемпотентны; база более новой версии — отказ, а не попытка работать.

> **Исправление при исполнении (2026-08-23).** Два упущения плана. Первое: `lib.rs` объявляет `pub mod bundle;`, а заглушку под него план велит создать только для `reference`, `snapshots` и `tokens` — без `src/bundle.rs` крейта не собирается. Второе: Global Constraint требует, чтобы новая крейта приходила вместе со строкой в `scripts/check-architecture.sh`, но эта задача такой строки не добавляет. Добавлен заслон 2a: `iaam-store` не зависит от вышележащих слоёв. Проверен исполнением на срабатывание.

**Почему запрет живёт в триггерах, а не в коде.** Дисциплина «журнал append-only» не переживает первый же скрипт починки данных, написанный в три часа ночи. Триггер переживает: он отклоняет `UPDATE` независимо от того, кто его выполняет — сервис, `sqlite3` из консоли или чужой инструмент.

**Почему событие хранится одним JSON, а не разложено по таблицам.** Журнал неизменяем: разложение ничего не даёт для записи, но добавляет способ потерять поле при добавлении варианта события. Индексируемые поля вынесены в колонки, остальное — в текстовом поле, а round-trip проверяется тестом ядра (задача 19). Обратная сторона решения честная: запрос «все сделки по инструменту X» потребует чтения журнала целиком, и когда это станет узким местом, добавится проекция, а не изменится схема журнала.

- [ ] **Шаг 1: Манифест крейты**

```toml
[package]
name = "iaam-store"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
# Версия рядом с путём обязательна: `cargo deny` считает path-зависимость
# без версии wildcard-зависимостью, а wildcards = "deny" в deny.toml.
iaam-core = { path = "../iaam-core", version = "0.1.0" }
rusqlite = { version = "0.40", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
# Снимок проекции содержит карты с составными ключами, которые JSON
# представить не может («key must be a string»). CBOR может.
ciborium = "0.2"
thiserror = "2"
time = { version = "0.3", default-features = false, features = ["std", "macros", "parsing", "formatting"] }
uuid = { version = "1", features = ["serde", "v4"] }

[lints]
workspace = true
```

Добавьте `"crates/iaam-store"` в `members` корневого `Cargo.toml` (изменение файла политики — метка `policy-change`).

- [ ] **Шаг 2: Написать падающие тесты**

`crates/iaam-store/tests/journal.rs`:

```rust
//! Журнал append-only и идемпотентность.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_store::SqliteStore;
use iaam_store::events::Appended;
use time::macros::date;

struct Ctx {
    owner: OwnerId,
    account: AccountId,
    source: SourceId,
}

impl Ctx {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            source: SourceId::new_random(),
        }
    }

    fn deposit(&self, sequence: u32, minor: i64) -> Event {
        let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
        let day = date!(2026 - 02 - 01);
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account: self.account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: vec![Leg::cash(self.account, amount)],
            provenance: Provenance::new(
                self.source,
                RawHash::parse(&"1".repeat(64)).unwrap(),
                ParserVersion("manual/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
}

#[test]
fn an_event_survives_a_write_and_a_read() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let event = ctx.deposit(1, 100_000);
    assert_eq!(
        store.append_event(&event).unwrap(),
        Appended::Inserted { id: event.id }
    );
    let loaded = store.load_events(ctx.owner).unwrap();
    assert_eq!(loaded, vec![event]);
}

#[test]
fn the_journal_is_append_only_at_the_database_level() {
    // Дисциплина кода не переживает первый же скрипт починки данных,
    // поэтому запрет живёт в базе (§4.8).
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let event = ctx.deposit(1, 100_000);
    store.append_event(&event).unwrap();

    let update = store
        .connection()
        .execute("UPDATE events SET kind = 'cash_out'", []);
    assert!(update.is_err(), "UPDATE обязан быть отклонён базой");

    let delete = store.connection().execute("DELETE FROM events", []);
    assert!(delete.is_err(), "DELETE обязан быть отклонён базой");

    assert_eq!(store.load_events(ctx.owner).unwrap().len(), 1);
}

#[test]
fn the_same_idempotency_key_returns_the_first_event() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let mut first = ctx.deposit(1, 100_000);
    first.idempotency_key = Some("import-42".into());
    let mut second = ctx.deposit(2, 555_000);
    second.idempotency_key = Some("import-42".into());

    store.append_event(&first).unwrap();
    assert_eq!(
        store.append_event(&second).unwrap(),
        Appended::Duplicate { existing: first.id }
    );
    assert_eq!(store.load_events(ctx.owner).unwrap().len(), 1);
}

#[test]
fn the_same_source_operation_is_not_recorded_twice() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let mut first = ctx.deposit(1, 100_000);
    first.provenance = Provenance::new(
        ctx.source,
        RawHash::parse(&"2".repeat(64)).unwrap(),
        ParserVersion("broker/1".into()),
    )
    .with_source_operation_id("OP-7");
    let mut second = ctx.deposit(2, 100_000);
    second.provenance = first.provenance.clone();

    store.append_event(&first).unwrap();
    assert_eq!(
        store.append_event(&second).unwrap(),
        Appended::Duplicate { existing: first.id }
    );
}

#[test]
fn two_identical_purchases_on_the_same_day_are_both_recorded() {
    // Естественный ключ «счёт + дата + сумма» слишком слаб: две одинаковые
    // операции в один день — законная ситуация (§10.6, §15.9).
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    store.append_event(&ctx.deposit(1, 100_000)).unwrap();
    store.append_event(&ctx.deposit(2, 100_000)).unwrap();
    assert_eq!(store.load_events(ctx.owner).unwrap().len(), 2);
}

#[test]
fn a_slice_through_a_date_excludes_later_events() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let early = ctx.deposit(1, 100_000);
    let mut late = ctx.deposit(2, 200_000);
    late.order = EffectiveOrder::new(date!(2026 - 03 - 01), 2);
    store.append_event(&early).unwrap();
    store.append_event(&late).unwrap();

    let slice = store
        .load_events_through(ctx.owner, date!(2026 - 02 - 15))
        .unwrap();
    assert_eq!(slice, vec![early]);
}

#[test]
fn migrations_are_idempotent() {
    let store = SqliteStore::open_in_memory().unwrap();
    iaam_store::schema::migrate(store.connection()).unwrap();
    let version: u32 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);
}
```

- [ ] **Шаг 3: Убедиться, что тесты падают**

```bash
nix develop -c cargo test -p iaam-store
```

- [ ] **Шаг 4: Схема**

`crates/iaam-store/migrations/0001_initial.sql`:

```sql
-- Схема журнала фактов (§4.1, §16.1).
--
-- Событие хранится целиком в JSON, а индексируемые поля вынесены
-- в колонки. Причина: журнал неизменяем, и разложение его по таблицам
-- ничего не даёт для записи, но добавляет способ потерять поле при
-- добавлении варианта события. Round-trip JSON проверен тестом ядра.

CREATE TABLE events (
    id                  TEXT PRIMARY KEY,
    schema_version      INTEGER NOT NULL,
    owner               TEXT NOT NULL,
    account             TEXT NOT NULL,
    kind                TEXT NOT NULL,
    effective_date      TEXT NOT NULL,
    sequence            INTEGER NOT NULL,
    relation_kind       TEXT NOT NULL,
    relation_target     TEXT,
    source              TEXT NOT NULL,
    source_operation_id TEXT,
    idempotency_key     TEXT,
    raw_hash            TEXT NOT NULL,
    payload             TEXT NOT NULL,
    recorded_at         TEXT NOT NULL
) STRICT;

-- Порядок проекции: дата, затем sequence, затем идентификатор.
-- Уникальность (owner, дата, sequence) обязательна: без неё два
-- одновременных запроса получают один и тот же номер, и порядок внутри
-- дня начинает определяться случайным UUID, а не объявленной семантикой
-- effectiveOrder (§4.8).
CREATE UNIQUE INDEX events_by_order ON events (owner, effective_date, sequence);

-- Идемпотентность (§10.6). Ключи разной силы — разные индексы:
-- клиентский ключ и идентификатор операции источника не заменяют друг друга.
CREATE UNIQUE INDEX events_idempotency_key
    ON events (owner, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE UNIQUE INDEX events_source_operation
    ON events (owner, source, source_operation_id)
    WHERE source_operation_id IS NOT NULL;

-- Append-only не как договорённость, а как поведение базы (§4.8).
-- Дисциплина кода не переживает первый же скрипт починки данных.
CREATE TRIGGER events_are_immutable
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'журнал фактов append-only: исправление — новое событие');
END;

CREATE TRIGGER events_are_not_deletable
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'журнал фактов append-only: удаление запрещено');
END;

-- Справочники. Меняются, поэтому обычные таблицы без триггеров.
CREATE TABLE accounts (
    id           TEXT PRIMARY KEY,
    owner        TEXT NOT NULL,
    title        TEXT NOT NULL,
    institution  TEXT,
    created_at   TEXT NOT NULL
) STRICT;

-- Владелец входит в уникальный ключ: без этого счёт нельзя сослать
-- внешним ключом из состава контура так, чтобы чужой счёт в него
-- не попал.
CREATE UNIQUE INDEX accounts_by_owner ON accounts (owner, id);

CREATE TABLE instruments (
    id       TEXT PRIMARY KEY,
    symbol   TEXT NOT NULL,
    title    TEXT NOT NULL,
    currency TEXT NOT NULL
) STRICT;

-- Контур версионирован: состав на версии неизменяем, новая версия —
-- новая строка (§4.10). Иначе изменение состава задним числом молча
-- переписало бы историческую доходность.
CREATE TABLE contour_versions (
    owner    TEXT NOT NULL,
    contour  TEXT NOT NULL,
    version  INTEGER NOT NULL,
    title    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (owner, contour, version)
) STRICT;

CREATE TABLE contour_accounts (
    owner   TEXT NOT NULL,
    contour TEXT NOT NULL,
    version INTEGER NOT NULL,
    account TEXT NOT NULL,
    PRIMARY KEY (owner, contour, version, account),
    FOREIGN KEY (owner, contour, version)
        REFERENCES contour_versions (owner, contour, version),
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;

CREATE TRIGGER contour_versions_are_immutable
BEFORE UPDATE ON contour_versions
BEGIN
    SELECT RAISE(ABORT, 'состав контура версионирован: заведите новую версию');
END;

CREATE TRIGGER contour_accounts_are_immutable
BEFORE UPDATE ON contour_accounts
BEGIN
    SELECT RAISE(ABORT, 'состав контура версионирован: заведите новую версию');
END;

-- Удаление запрещено наравне с изменением. Запрет только на UPDATE
-- ловит правку строки, но пропускает DELETE + INSERT, а это тот же
-- результат: исторический состав версии изменён, и все посчитанные
-- по ней цифры молча стали другими (§4.10).
CREATE TRIGGER contour_versions_are_not_deletable
BEFORE DELETE ON contour_versions
BEGIN
    SELECT RAISE(ABORT, 'состав контура версионирован: удаление запрещено');
END;

CREATE TRIGGER contour_accounts_are_not_deletable
BEFORE DELETE ON contour_accounts
BEGIN
    SELECT RAISE(ABORT, 'состав контура версионирован: удаление запрещено');
END;

-- Снимки проекций — кэш. Потеря снимка не является потерей данных:
-- он всегда восстановим полным пересчётом журнала.
CREATE TABLE snapshots (
    owner              TEXT NOT NULL,
    contour            TEXT NOT NULL,
    contour_version    INTEGER NOT NULL,
    lot_rule           INTEGER NOT NULL,
    projection_version INTEGER NOT NULL,
    through_date       TEXT,
    through_sequence   INTEGER,
    fingerprint        TEXT NOT NULL,
    body               BLOB NOT NULL,
    created_at         TEXT NOT NULL,
    PRIMARY KEY (owner, contour, contour_version, lot_rule)
) STRICT;

-- Агентские токены: хранится хеш, не сам токен (§14).
CREATE TABLE api_tokens (
    id          TEXT PRIMARY KEY,
    owner       TEXT NOT NULL,
    label       TEXT NOT NULL,
    token_hash  TEXT NOT NULL UNIQUE,
    scope       TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    revoked_at  TEXT
) STRICT;

CREATE TABLE token_usage (
    token   TEXT NOT NULL,
    used_at TEXT NOT NULL,
    route   TEXT NOT NULL,
    outcome TEXT NOT NULL
) STRICT;

CREATE INDEX token_usage_by_token ON token_usage (token, used_at);
```

- [ ] **Шаг 5: Подключение и миграции**

`crates/iaam-store/src/lib.rs`:

```rust
//! Хранилище: SQLite как полное рабочее состояние (§3.3).
//!
//! Крейта синхронная и блокирующая. Асинхронность живёт в `iaam-app`,
//! которая зовёт хранилище через выделенный блокирующий исполнитель:
//! `rusqlite` блокирует поток, и вызов его прямо из обработчика axum
//! останавливает исполнитель (§3.2).

pub mod bundle;
pub mod events;
pub mod reference;
pub mod schema;
pub mod snapshots;
pub mod tokens;

use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("ошибка SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("не удалось разобрать сохранённое событие {id}: {source}")]
    EventDecode {
        id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("не удалось сериализовать событие: {0}")]
    EventEncode(#[source] serde_json::Error),
    #[error("не удалось разобрать снимок: {0}")]
    SnapshotDecode(String),
    #[error("не удалось сериализовать снимок: {0}")]
    SnapshotEncode(String),
    #[error("схема базы версии {found} новее поддерживаемой {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("в базе нет записи {what} {id}")]
    NotFound { what: &'static str, id: String },
    #[error("архивный бандл повреждён: {detail}")]
    BundleCorrupted { detail: String },
}

/// Подключение к базе.
///
/// Владеет соединением монопольно: `rusqlite::Connection` не `Sync`,
/// и пул на этапе 1 не нужен — писатель один, а чтение идёт под тем же
/// блокирующим исполнителем.
pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    /// Открытие файла базы с применением миграций.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::prepare(conn)
    }

    /// База в памяти. Нужна тестам: файловая база в тесте оставляет
    /// мусор и делает тесты зависимыми друг от друга.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::prepare(conn)
    }

    fn prepare(conn: Connection) -> Result<Self, StoreError> {
        // foreign_keys выключены в SQLite по умолчанию: без этой строки
        // объявленные внешние ключи не проверяются вообще.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // WAL: читатель не блокирует писателя. Для одного пользователя
        // это не про нагрузку, а про то, чтобы отчёт не падал во время записи.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let store = Self { conn };
        schema::migrate(&store.conn)?;
        Ok(store)
    }

    #[must_use]
    pub const fn connection(&self) -> &Connection {
        &self.conn
    }

    #[must_use]
    pub const fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}
```

`crates/iaam-store/src/schema.rs`:

```rust
//! Миграции.
//!
//! Миграции нумерованы и применяются по одной в транзакции. Файл схемы
//! встроен в двоичный файл: база, открытая версией программы, обязана
//! соответствовать этой версии, а не тому, что лежит рядом на диске.

use rusqlite::Connection;

use crate::StoreError;

/// Версия схемы, которую понимает эта сборка.
pub const SCHEMA_VERSION: u32 = 1;

const MIGRATIONS: [(u32, &str); 1] = [(1, include_str!("../migrations/0001_initial.sql"))];

/// Применение недостающих миграций.
///
/// База новее программы — отказ, а не попытка работать: неизвестная
/// колонка молча читается как отсутствующая, и это худший вид ошибки.
pub fn migrate(conn: &Connection) -> Result<(), StoreError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    for (version, sql) in MIGRATIONS {
        if version <= current {
            continue;
        }
        conn.execute_batch(&format!(
            "BEGIN; {sql} PRAGMA user_version = {version}; COMMIT;"
        ))?;
    }
    Ok(())
}
```

Пустые заглушки `src/reference.rs`, `src/snapshots.rs`, `src/tokens.rs`, `src/bundle.rs` создаются в этой задаче с одной строкой `//! Заполняется задачей 11.` — модули объявлены в `lib.rs`, и без файлов крейта не собирается.

- [ ] **Шаг 6: Запись и чтение журнала**

`crates/iaam-store/src/events.rs`:

```rust
//! Журнал фактов: запись и чтение.

use iaam_core::dates::EffectiveOrder;
use iaam_core::event::{Event, Relation};
use iaam_core::ids::{EventId, OwnerId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{SqliteStore, StoreError};

/// Что произошло при попытке записи.
///
/// Повтор — не ошибка: повторный вызов с тем же ключом обязан вернуть
/// тот же результат, иначе клиент, не получивший ответа, не может
/// безопасно повторить запрос (§10.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Appended {
    Inserted { id: EventId },
    Duplicate { existing: EventId },
}

impl SqliteStore {
    /// Запись события с уже назначенным порядком.
    ///
    /// Применяется там, где порядок задан извне и менять его нельзя:
    /// импорт архивного бандла и восстановление из архива.
    pub fn append_event(&self, event: &Event) -> Result<Appended, StoreError> {
        if let Some(existing) = find_duplicate(&self.conn, event)? {
            return Ok(Appended::Duplicate { existing });
        }
        insert_event(&self.conn, event)?;
        Ok(Appended::Inserted { id: event.id })
    }

    /// Запись события с назначением порядкового номера **в той же
    /// транзакции**.
    ///
    /// Раздельные «узнать `MAX(sequence) + 1`» и «вставить» — гонка:
    /// два одновременных запроса получают один и тот же номер, и порядок
    /// внутри дня начинает определяться случайным идентификатором вместо
    /// объявленной семантики (§4.8). Транзакция с немедленным захватом
    /// записи закрывает гонку и между процессами, а уникальный индекс
    /// `(owner, effective_date, sequence)` превращает оставшийся зазор
    /// в ошибку вместо тихой перестановки.
    pub fn append_event_in_order(&mut self, event: &Event) -> Result<Appended, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_duplicate(&transaction, event)? {
            return Ok(Appended::Duplicate { existing });
        }
        let day = event.order.date();
        let used: Option<u32> = transaction.query_row(
            "SELECT MAX(sequence) FROM events WHERE owner = ?1 AND effective_date = ?2",
            params![event.owner.inner().to_string(), day.to_string()],
            |row| row.get(0),
        )?;
        let stamped = Event {
            order: EffectiveOrder::new(day, used.map_or(1, |value| value.saturating_add(1))),
            ..event.clone()
        };
        insert_event(&transaction, &stamped)?;
        transaction.commit()?;
        Ok(Appended::Inserted { id: stamped.id })
    }

    /// Весь журнал владельца в порядке `EffectiveOrder`.
    ///
    /// Порядок задаётся базой, но проекция всё равно сортирует срез сама:
    /// ядро не обязано верить порядку, пришедшему снаружи (§4.8).
    pub fn load_events(&self, owner: OwnerId) -> Result<Vec<Event>, StoreError> {
        self.query_events(
            "SELECT id, payload FROM events
             WHERE owner = ?1
             ORDER BY effective_date, sequence, id",
            params![owner.inner().to_string()],
        )
    }

    /// Журнал владельца по дату включительно. Срез для отчёта на дату
    /// собирает оболочка: ядро событий по датам не фильтрует (§6.1).
    pub fn load_events_through(
        &self,
        owner: OwnerId,
        through: time::Date,
    ) -> Result<Vec<Event>, StoreError> {
        self.query_events(
            "SELECT id, payload FROM events
             WHERE owner = ?1 AND effective_date <= ?2
             ORDER BY effective_date, sequence, id",
            params![owner.inner().to_string(), through.to_string()],
        )
    }

    fn query_events(
        &self,
        sql: &str,
        parameters: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<Event>, StoreError> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(parameters, |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (id, payload) = row?;
            let event: Event = serde_json::from_str(&payload)
                .map_err(|source| StoreError::EventDecode { id, source })?;
            events.push(event);
        }
        Ok(events)
    }
}

/// Вставка события. Тело вынесено из публичных методов: оба пути записи
/// обязаны класть в базу одно и то же, и второй экземпляр этого SQL
/// когда-нибудь разошёлся бы с первым.
pub(crate) fn insert_event(conn: &Connection, event: &Event) -> Result<(), StoreError> {
    let payload = serde_json::to_string(event).map_err(StoreError::EventEncode)?;
    let (relation_kind, relation_target) = match event.relation {
        Relation::None => ("none", None),
        Relation::Reversal { target } => ("reversal", Some(target.inner().to_string())),
        Relation::Replacement { target } => ("replacement", Some(target.inner().to_string())),
    };
    let recorded_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"));

    conn.execute(
        "INSERT INTO events (
             id, schema_version, owner, account, kind, effective_date, sequence,
             relation_kind, relation_target, source, source_operation_id,
             idempotency_key, raw_hash, payload, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            event.id.inner().to_string(),
            event.schema_version,
            event.owner.inner().to_string(),
            event.account.inner().to_string(),
            event.kind.discriminant(),
            event.order.date().to_string(),
            event.order.sequence(),
            relation_kind,
            relation_target,
            event.provenance.source().inner().to_string(),
            event.provenance.source_operation_id(),
            event.idempotency_key.as_deref(),
            event.provenance.raw_hash().as_str(),
            payload,
            recorded_at,
        ],
    )?;
    Ok(())
}

/// Поиск дубликата по ключам от сильного к слабому (§10.6).
///
/// Естественный ключ «счёт + дата + сумма» здесь намеренно отсутствует:
/// две одинаковые покупки в один день — законная ситуация, и склеивать
/// их значит терять факт.
pub(crate) fn find_duplicate(
    conn: &Connection,
    event: &Event,
) -> Result<Option<EventId>, StoreError> {
    if let Some(operation) = event.provenance.source_operation_id() {
        let found = lookup(
            conn,
            "SELECT id FROM events WHERE owner = ?1 AND source = ?2 AND source_operation_id = ?3",
            params![
                event.owner.inner().to_string(),
                event.provenance.source().inner().to_string(),
                operation
            ],
        )?;
        if found.is_some() {
            return Ok(found);
        }
    }
    if let Some(key) = event.idempotency_key.as_deref() {
        let found = lookup(
            conn,
            "SELECT id FROM events WHERE owner = ?1 AND idempotency_key = ?2",
            params![event.owner.inner().to_string(), key],
        )?;
        if found.is_some() {
            return Ok(found);
        }
    }
    lookup(
        conn,
        "SELECT id FROM events WHERE id = ?1",
        params![event.id.inner().to_string()],
    )
}

fn lookup(
    conn: &Connection,
    sql: &str,
    parameters: &[&dyn rusqlite::ToSql],
) -> Result<Option<EventId>, StoreError> {
    let found: Option<String> = conn
        .query_row(sql, parameters, |row| row.get(0))
        .optional()?;
    Ok(found
        .and_then(|id| uuid::Uuid::parse_str(&id).ok())
        .map(EventId))
}
```

- [ ] **Шаг 7: Зелёная сборка**

```bash
nix develop -c cargo test -p iaam-store
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Ожидается: семь тестов проходят. Первая сборка долгая — `rusqlite` с feature `bundled` компилирует SQLite из исходников.

- [ ] **Шаг 8: Коммит**

```bash
git add crates/iaam-store Cargo.toml
git commit -m "feat(store): журнал фактов append-only на SQLite (iaam-1fk)"
```

---

## Задача 11: `iaam-store` — снимки, справочники, токены

**Files:**
- Create: `crates/iaam-store/src/snapshots.rs`, `src/reference.rs`, `src/tokens.rs`
- Create: `crates/iaam-store/tests/snapshots_and_reference.rs`

**Interfaces:**
- Produces: `SqliteStore::{save_snapshot, load_snapshot, drop_snapshot}`; `iaam_store::reference::{AccountRecord, InstrumentRecord}` и методы `upsert_account`, `list_accounts`, `upsert_instrument`, `insert_contour_version`, `load_contour`, `latest_contour_version`; `iaam_store::tokens::{TokenRecord, TokenScope}` и методы `insert_token`, `find_token`, `revoke_token`, `record_token_use`.

**Acceptance Criteria:**
- Снимок переживает запись и чтение полностью, включая карты с составными ключами.
- Повторное сохранение снимка заменяет предыдущий, а не создаёт второй.
- Состав контура нельзя изменить ни `UPDATE`, ни `DELETE`: оба отклоняются базой.
- Контур не принимает счёт другого владельца — отказывает внешний ключ.
- Контур, справочник и снимок другого владельца не находятся: владелец входит в запрос, а не проверяется после.
- Отозванный токен не находится.
- В базе лежит хеш токена; сам токен хранилищу неизвестен.

> **Исправление при исполнении (2026-08-23).** Мутационный прогон крейты дал десять выживших, и `iaam-store` вообще не входил в мутационный заслон — план его туда не вносит ни в одной задаче. Это ровно тот модуль, где живут граница владельца и append-only журнал, то есть свойства безопасности. Четыре файла крейты внесены в `MODULES`, десять выживших закрыты пятью тестами: назначение порядкового номера хранилищем, а не клиентом; удаление снимка действительно удаляет; запись инструмента доходит до таблицы и не задваивается; каждое использование токена, включая отклонённое, попадает в журнал; область действия токена переживает круг через свой код.
>
> Попутно устранено дублирование: функция `now()` существовала копией в `reference.rs` и в `tokens.rs`. Две копии одного форматирования расходятся молча — колонки `created_at` разных таблиц начали бы отличаться форматом. Одна `pub(crate) fn now()` в `lib.rs` с тестом на разбор обратно.

**Почему CBOR, а не JSON.** Проверено исполнением: `serde_json` отказывается сериализовать состояние проекции с ошибкой «key must be a string» — карты в состоянии имеют составные ключи (счёт + валюта, счёт + место хранения + инструмент). Это не повод менять состояние: ключ-кортеж здесь правильная модель. Снимок — кэш, и формат для него выбирается по критерию «переживёт ли он состав состояния».

**Почему `drop_snapshot` — единственное удаление в хранилище.** Кэш выбрасывать можно, факты — нет.

- [ ] **Шаг 1: Написать падающие тесты**

`crates/iaam-store/tests/snapshots_and_reference.rs`:

```rust
//! Снимки, справочники и версии контуров.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_store::SqliteStore;
use iaam_store::reference::AccountRecord;
use iaam_store::tokens::{TokenRecord, TokenScope};
use time::macros::date;
use uuid::Uuid;

fn deposit(owner: OwnerId, account: AccountId, sequence: u32, minor: i64) -> Event {
    let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
    let day = date!(2026 - 02 - 01);
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind: EventKind::CashIn { amount },
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs: vec![Leg::cash(account, amount)],
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"3".repeat(64)).unwrap(),
            ParserVersion("manual/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

#[test]
fn a_snapshot_survives_a_write_and_a_read() {
    // Состояние содержит карты с составными ключами: JSON их не берёт,
    // поэтому снимок хранится в CBOR. Тест ловит возврат к JSON.
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let events = vec![
        deposit(owner, account, 1, 100_000),
        deposit(owner, account, 2, 50_000),
    ];
    let snapshot = project(&events, &ctx).unwrap().into_snapshot();

    store.save_snapshot(owner, &snapshot).unwrap();
    let loaded = store
        .load_snapshot(owner, contour.id(), contour.version(), LotRuleVersion(1))
        .unwrap()
        .expect("снимок найден");

    assert_eq!(loaded.fingerprint(), snapshot.fingerprint());
    assert_eq!(
        loaded.state().balances().cash(account, CurrencyCode::Rub),
        snapshot.state().balances().cash(account, CurrencyCode::Rub)
    );
    assert_eq!(loaded, snapshot);
}

#[test]
fn saving_a_snapshot_twice_replaces_it() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let first = project(&[deposit(owner, account, 1, 100_000)], &ctx)
        .unwrap()
        .into_snapshot();
    let second = project(
        &[
            deposit(owner, account, 1, 100_000),
            deposit(owner, account, 2, 1),
        ],
        &ctx,
    )
    .unwrap()
    .into_snapshot();

    store.save_snapshot(owner, &first).unwrap();
    store.save_snapshot(owner, &second).unwrap();
    let loaded = store
        .load_snapshot(owner, contour.id(), contour.version(), LotRuleVersion(1))
        .unwrap()
        .unwrap();
    assert_eq!(loaded.fingerprint(), second.fingerprint());
}

#[test]
fn a_contour_version_cannot_be_edited_in_place() {
    // Изменение состава контура задним числом молча переписало бы
    // историческую доходность (§4.10).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Брокерский".into(),
            institution: None,
        })
        .unwrap();
    store
        .insert_contour_version(owner, &contour, "Мой портфель", &[account])
        .unwrap();

    let update = store
        .connection()
        .execute("UPDATE contour_accounts SET account = 'подмена'", []);
    assert!(
        update.is_err(),
        "UPDATE состава контура обязан быть отклонён"
    );

    let loaded = store
        .load_contour(owner, contour.id(), ContourVersion(1))
        .unwrap()
        .unwrap();
    assert!(loaded.contains(account));
    assert_eq!(
        store.latest_contour_version(owner, contour.id()).unwrap(),
        Some(ContourVersion(1))
    );
}

#[test]
fn accounts_round_trip() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = AccountRecord {
        id: AccountId::new_random(),
        owner,
        title: "Брокерский".into(),
        institution: Some("Т-Банк".into()),
    };
    store.upsert_account(&record).unwrap();
    assert_eq!(store.list_accounts(owner).unwrap(), vec![record]);
}

#[test]
fn a_revoked_token_is_not_found() {
    let store = SqliteStore::open_in_memory().unwrap();
    let record = TokenRecord {
        id: Uuid::new_v4(),
        owner: OwnerId::new_random(),
        label: "агент".into(),
        scope: TokenScope::Agent,
        revoked: false,
    };
    store.insert_token(&record, "хеш-токена").unwrap();
    assert_eq!(
        store.find_token("хеш-токена").unwrap(),
        Some(record.clone())
    );

    store.revoke_token(record.id).unwrap();
    assert_eq!(store.find_token("хеш-токена").unwrap(), None);
}

#[test]
fn an_agent_token_may_submit_but_not_administer() {
    assert!(TokenScope::Agent.may_submit());
    assert!(!TokenScope::Agent.may_administer());
    assert!(!TokenScope::ReadOnly.may_submit());
    assert!(TokenScope::Owner.may_administer());
}

#[test]
fn a_contour_cannot_include_an_account_of_another_owner() {
    // Контур из чужих счетов — это доступ к чужим деньгам, а не ошибка
    // ввода. Отказывает база по внешнему ключу (owner, account) (§14).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let foreign_account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: foreign_account,
            owner: stranger,
            title: "Чужой".into(),
            institution: None,
        })
        .unwrap();

    let contour = ContourId::new_random();
    let attempt = store.insert_contour_version(
        owner,
        &ContourDefinition::new(contour, ContourVersion(1), [foreign_account]),
        "Чужие деньги",
        &[foreign_account],
    );
    assert!(
        attempt.is_err(),
        "чужой счёт в контуре обязан быть отклонён"
    );
}

#[test]
fn a_contour_of_another_owner_is_not_found() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Свой".into(),
            institution: None,
        })
        .unwrap();
    let contour = ContourId::new_random();
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(contour, ContourVersion(1), [account]),
            "Мой",
            &[account],
        )
        .unwrap();

    // Знание идентификатора не даёт доступа.
    assert_eq!(
        store
            .load_contour(stranger, contour, ContourVersion(1))
            .unwrap(),
        None
    );
    assert_eq!(
        store.latest_contour_version(stranger, contour).unwrap(),
        None
    );
}

#[test]
fn an_account_of_another_owner_is_not_overwritten() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let id = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id,
            owner,
            title: "Мой счёт".into(),
            institution: None,
        })
        .unwrap();
    // Тот же идентификатор, другой владелец: строка не должна измениться.
    let attempt = store.upsert_account(&AccountRecord {
        id,
        owner: stranger,
        title: "Захвачено".into(),
        institution: None,
    });
    assert!(attempt.is_ok(), "конфликт не должен быть ошибкой записи");
    assert_eq!(store.list_accounts(owner).unwrap()[0].title, "Мой счёт");
    assert!(store.list_accounts(stranger).unwrap().is_empty());
}

#[test]
fn a_snapshot_of_another_owner_is_not_found() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let snapshot = project(&[deposit(owner, account, 1, 1_000)], &ctx)
        .unwrap()
        .into_snapshot();
    store.save_snapshot(owner, &snapshot).unwrap();

    assert!(
        store
            .load_snapshot(stranger, contour.id(), contour.version(), LotRuleVersion(1))
            .unwrap()
            .is_none()
    );
}
```

- [ ] **Шаг 2: Убедиться, что тесты падают**

```bash
nix develop -c cargo test -p iaam-store --test snapshots_and_reference
```

- [ ] **Шаг 3: Снимки**

```rust
//! Снимки проекций.
//!
//! Снимок — **кэш**: его потеря не является потерей данных, потому что
//! он восстановим полным пересчётом журнала. Поэтому формат выбран
//! из соображений «переживёт ли он состав состояния», а не долговечности.
//!
//! Формат — CBOR, а не JSON. Состояние содержит карты с составными
//! ключами (счёт + валюта, счёт + место хранения + инструмент), которые
//! JSON представить не может: `serde_json` отказывает с «key must be
//! a string». Проверено исполнением.

use iaam_core::contour::{ContourId, ContourVersion};
use iaam_core::ids::OwnerId;
use iaam_core::projection::Snapshot;
use iaam_core::rules::LotRuleVersion;
use rusqlite::{OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{SqliteStore, StoreError};

impl SqliteStore {
    /// Сохранение снимка. Ключ — контур, его версия и версия правила
    /// списания: снимок, построенный другими правилами, не является
    /// снимком того же расчёта.
    pub fn save_snapshot(&self, owner: OwnerId, snapshot: &Snapshot) -> Result<(), StoreError> {
        let mut body = Vec::new();
        ciborium::into_writer(snapshot, &mut body)
            .map_err(|error| StoreError::SnapshotEncode(error.to_string()))?;
        let created_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"));

        self.conn.execute(
            "INSERT INTO snapshots (
                 owner, contour, contour_version, lot_rule, projection_version,
                 through_date, through_sequence, fingerprint, body, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (owner, contour, contour_version, lot_rule) DO UPDATE SET
                 projection_version = excluded.projection_version,
                 through_date       = excluded.through_date,
                 through_sequence   = excluded.through_sequence,
                 fingerprint        = excluded.fingerprint,
                 body               = excluded.body,
                 created_at         = excluded.created_at",
            params![
                owner.inner().to_string(),
                snapshot.contour().0.to_string(),
                snapshot.contour_version().0,
                snapshot.lot_rule().0,
                snapshot.projection_version(),
                snapshot.through().map(|order| order.date().to_string()),
                snapshot.through().map(|order| order.sequence()),
                snapshot.fingerprint().to_string(),
                body,
                created_at,
            ],
        )?;
        Ok(())
    }

    /// Чтение снимка.
    ///
    /// Снимок, который не читается, — не ошибка работы: формат мог
    /// измениться вместе с версией проекции. Вызывающий получает `None`
    /// и пересчитывает журнал с нуля.
    pub fn load_snapshot(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
        lot_rule: LotRuleVersion,
    ) -> Result<Option<Snapshot>, StoreError> {
        let body: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT body FROM snapshots
                 WHERE owner = ?1 AND contour = ?2 AND contour_version = ?3 AND lot_rule = ?4",
                params![
                    owner.inner().to_string(),
                    contour.0.to_string(),
                    version.0,
                    lot_rule.0
                ],
                |row| row.get(0),
            )
            .optional()?;
        let Some(body) = body else {
            return Ok(None);
        };
        Ok(ciborium::from_reader(body.as_slice()).ok())
    }

    /// Удаление снимка. Единственная операция удаления в хранилище:
    /// кэш выбрасывать можно, факты — нет.
    pub fn drop_snapshot(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
        lot_rule: LotRuleVersion,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM snapshots
             WHERE owner = ?1 AND contour = ?2 AND contour_version = ?3 AND lot_rule = ?4",
            params![
                owner.inner().to_string(),
                contour.0.to_string(),
                version.0,
                lot_rule.0
            ],
        )?;
        Ok(())
    }
}
```

- [ ] **Шаг 4: Справочники**

```rust
//! Справочники: счета, инструменты, версии контуров.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, InstrumentId, OwnerId};
use rusqlite::params;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{SqliteStore, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub id: AccountId,
    pub owner: OwnerId,
    pub title: String,
    pub institution: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentRecord {
    pub id: InstrumentId,
    pub symbol: String,
    pub title: String,
    pub currency: String,
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

impl SqliteStore {
    /// Создание или обновление счёта.
    ///
    /// Условие `WHERE accounts.owner = excluded.owner` обязательно:
    /// без него запрос с чужим (или угаданным) идентификатором
    /// переписывал бы название счёта другого владельца (§14).
    pub fn upsert_account(&self, account: &AccountRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO accounts (id, owner, title, institution, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 title = excluded.title,
                 institution = excluded.institution
             WHERE accounts.owner = excluded.owner",
            params![
                account.id.inner().to_string(),
                account.owner.inner().to_string(),
                account.title,
                account.institution,
                now(),
            ],
        )?;
        Ok(())
    }

    pub fn list_accounts(&self, owner: OwnerId) -> Result<Vec<AccountRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, title, institution FROM accounts WHERE owner = ?1 ORDER BY title, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut accounts = Vec::new();
        for row in rows {
            let (id, title, institution) = row?;
            accounts.push(AccountRecord {
                id: AccountId(parse_uuid(&id, "account")?),
                owner,
                title,
                institution,
            });
        }
        Ok(accounts)
    }

    pub fn upsert_instrument(&self, instrument: &InstrumentRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO instruments (id, symbol, title, currency)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET
                 symbol = excluded.symbol,
                 title = excluded.title,
                 currency = excluded.currency",
            params![
                instrument.id.inner().to_string(),
                instrument.symbol,
                instrument.title,
                instrument.currency,
            ],
        )?;
        Ok(())
    }

    /// Новая версия состава контура.
    ///
    /// Версия неизменяема: изменение состава — новая строка, а не UPDATE.
    /// Это обеспечено триггером в схеме, а не только этим методом.
    pub fn insert_contour_version(
        &mut self,
        owner: OwnerId,
        definition: &ContourDefinition,
        title: &str,
        accounts: &[AccountId],
    ) -> Result<(), StoreError> {
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "INSERT INTO contour_versions (owner, contour, version, title, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                owner.inner().to_string(),
                definition.id().0.to_string(),
                definition.version().0,
                title,
                now(),
            ],
        )?;
        for account in accounts {
            // Внешний ключ на (owner, account) отклонит чужой счёт:
            // контур из чужих счетов — это доступ к чужим деньгам,
            // а не ошибка ввода.
            transaction.execute(
                "INSERT INTO contour_accounts (owner, contour, version, account)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    owner.inner().to_string(),
                    definition.id().0.to_string(),
                    definition.version().0,
                    account.inner().to_string(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Состав контура на версии **для указанного владельца**.
    ///
    /// Владелец входит в запрос, а не проверяется после: идентификатор
    /// контура — это UUID, а UUID не является правом доступа (§14).
    pub fn load_contour(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
    ) -> Result<Option<ContourDefinition>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT account FROM contour_accounts
             WHERE owner = ?1 AND contour = ?2 AND version = ?3",
        )?;
        let rows = statement.query_map(
            params![owner.inner().to_string(), contour.0.to_string(), version.0],
            |row| row.get::<_, String>(0),
        )?;
        let mut accounts = Vec::new();
        for row in rows {
            accounts.push(AccountId(parse_uuid(&row?, "contour_account")?));
        }
        if accounts.is_empty() {
            return Ok(None);
        }
        Ok(Some(ContourDefinition::new(contour, version, accounts)))
    }

    /// Наибольшая версия контура. Отчёт без явно указанной версии
    /// считается по последней — и пишет её в применённые правила.
    pub fn latest_contour_version(
        &self,
        owner: OwnerId,
        contour: ContourId,
    ) -> Result<Option<ContourVersion>, StoreError> {
        let version: Option<u32> = self.conn.query_row(
            "SELECT MAX(version) FROM contour_versions WHERE owner = ?1 AND contour = ?2",
            params![owner.inner().to_string(), contour.0.to_string()],
            |row| row.get(0),
        )?;
        Ok(version.map(ContourVersion))
    }
}

fn parse_uuid(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}
```

- [ ] **Шаг 5: Токены**

```rust
//! Агентские токены (§14).
//!
//! Хранится **хеш** токена, а не токен: утечка файла базы не должна
//! давать доступ к API. Сам хеш считает транспортный слой — хранилище
//! не знает, чем именно, и потому не может ослабить алгоритм.

use iaam_core::ids::OwnerId;
use rusqlite::{OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::{SqliteStore, StoreError};

/// Права токена. Исчерпаемый `enum`, а не строка в базе: добавление
/// права обязано сломать сборку везде, где его не обработали (§15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    /// Полный доступ владельца.
    Owner,
    /// Внешний агент: чтение отчётов и отправка операций в приёмку.
    /// Прямой записи в журнал у него нет — она результат прохождения
    /// приёмки, а не отдельное разрешённое действие (§13).
    Agent,
    /// Только чтение отчётов.
    ReadOnly,
}

impl TokenScope {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Agent => "agent",
            Self::ReadOnly => "read_only",
        }
    }

    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "owner" => Some(Self::Owner),
            "agent" => Some(Self::Agent),
            "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }

    /// Может ли токен отправлять операции в приёмку.
    #[must_use]
    pub const fn may_submit(self) -> bool {
        match self {
            Self::Owner | Self::Agent => true,
            Self::ReadOnly => false,
        }
    }

    /// Может ли токен управлять другими токенами и справочниками.
    #[must_use]
    pub const fn may_administer(self) -> bool {
        match self {
            Self::Owner => true,
            Self::Agent | Self::ReadOnly => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    pub id: Uuid,
    pub owner: OwnerId,
    pub label: String,
    pub scope: TokenScope,
    pub revoked: bool,
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

impl SqliteStore {
    /// Регистрация токена по его хешу.
    pub fn insert_token(&self, record: &TokenRecord, token_hash: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO api_tokens (id, owner, label, token_hash, scope, created_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                record.id.to_string(),
                record.owner.inner().to_string(),
                record.label,
                token_hash,
                record.scope.code(),
                now(),
            ],
        )?;
        Ok(())
    }

    /// Поиск действующего токена по хешу. Отозванный не находится.
    pub fn find_token(&self, token_hash: &str) -> Result<Option<TokenRecord>, StoreError> {
        let row: Option<(String, String, String, String)> = self
            .conn
            .query_row(
                "SELECT id, owner, label, scope FROM api_tokens
                 WHERE token_hash = ?1 AND revoked_at IS NULL",
                params![token_hash],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((id, owner, label, scope)) = row else {
            return Ok(None);
        };
        let scope = TokenScope::parse(&scope).ok_or(StoreError::NotFound {
            what: "token_scope",
            id: scope.clone(),
        })?;
        Ok(Some(TokenRecord {
            id: Uuid::parse_str(&id).map_err(|_| StoreError::NotFound {
                what: "token",
                id: id.clone(),
            })?,
            owner: OwnerId(Uuid::parse_str(&owner).map_err(|_| StoreError::NotFound {
                what: "owner",
                id: owner.clone(),
            })?),
            label,
            scope,
            revoked: false,
        }))
    }

    pub fn revoke_token(&self, id: Uuid) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE api_tokens SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![id.to_string(), now()],
        )?;
        Ok(())
    }

    /// Журнал использования токена (§14). Пишется на каждый запрос,
    /// включая отклонённый: попытки с отозванным токеном — то, ради
    /// чего журнал и нужен.
    pub fn record_token_use(
        &self,
        token_hash: &str,
        route: &str,
        outcome: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO token_usage (token, used_at, route, outcome) VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, now(), route, outcome],
        )?;
        Ok(())
    }
}
```

- [ ] **Шаг 6: Зелёная сборка и коммит**

```bash
nix develop -c cargo test -p iaam-store
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/iaam-store
git commit -m "feat(store): снимки, справочники и агентские токены (iaam-1fk)"
```

---

## Задача 12: `iaam-ingest` — нормализация операций

**Files:**
- Create: `crates/iaam-ingest/Cargo.toml`, `src/lib.rs`, `src/verdict.rs`, `src/operation.rs`
- Create: `crates/iaam-ingest/tests/normalization.rs`
- Modify: корневой `Cargo.toml`

**Interfaces:**
- Produces: `iaam_ingest::{SubmittedOperation, OperationKind, OperationDates, Normalized, normalize, Verdict, Rejection}`, `iaam_ingest::operation::{NormalizationContext, PARSER_VERSION, to_minor_units}`.

**Acceptance Criteria:**
- Каждый вид операции даёт событие, проходящее `validate_structure` ядра.
- Клиент присылает **положительные** величины; знаки и ноги строит приёмка.
- Отказ несёт поле, ожидаемое и полученное значение — тело ответа `422`.
- Операция без единой даты отклоняется, а не получает подставленную дату.
- Перевод на тот же счёт отклоняется до построения ног.
- Отпечаток нормализованной записи попадает в provenance.

> **Исправление при исполнении (2026-08-23).** `lib.rs` объявляет `pub mod csv_source;`, а файл под него создаёт только задача 13 — без заглушки крейта не собирается. Крейта, как и `iaam-store`, не входила в мутационный заслон: три её файла внесены в `MODULES`. Прогон дал десять выживших, закрытых четырьмя тестами: даты строки попадают и в дату сделки, и в дату денег; коды и признак «записано» у всех пяти вердиктов; ноль отклоняется наравне с отрицательной величиной.
>
> Последний выживший оказался тем же случаем, что и в задаче 6: проверка `!name.is_empty()` изнутри разбора недостижима — csv отдаёт для пустой ячейки `None`, а не `Some("")`. Разрешение места хранения вынесено функцией `resolve_custody` **ради проверяемости, а не читаемости**, и проверено напрямую на всех трёх состояниях имени.

**Почему знаки строит приёмка.** Знаковая конвенция — часть модели журнала, а не публичного контракта. Если её обязан знать клиент, то её обязан знать и внешний агент, которому арифметика запрещена (§13); а первая же ошибка в знаке даёт доходность, в которой вывод средств выглядит доходом.

**Проверено исполнением:** комиссия сделки задаётся **положительной** величиной. `trade_settlement` ядра прибавляет её к телу сделки и уже потом меняет знак при покупке. Отрицательная комиссия уменьшает стоимость покупки и даёт `AmountMismatch` при структурной проверке.

- [ ] **Шаг 1: Манифест**

```toml
[package]
name = "iaam-ingest"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
iaam-core = { path = "../iaam-core", version = "0.1.0" }
csv = "1"
rust_decimal = { version = "1", default-features = false, features = ["std", "serde"] }
serde = { version = "1", features = ["derive"] }
sha2 = "0.11"
thiserror = "2"
time = { version = "0.3", default-features = false, features = ["std", "macros", "parsing", "formatting"] }
uuid = { version = "1", features = ["serde", "v4"] }

[lints]
workspace = true
```

- [ ] **Шаг 2: Написать падающие тесты**

`crates/iaam-ingest/tests/normalization.rs`:

```rust
//! Нормализованная операция обязана давать событие, проходящее
//! структурную проверку ядра. Это шов, на котором ломается всё:
//! приёмка строит ноги, а форму этих ног задаёт ядро.

use iaam_core::event::kind::{EventKind, FeeOrigin, TradeSide};
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{OperationDates, OperationKind, SubmittedOperation, normalize};
use rust_decimal::Decimal;
use time::macros::date;

fn context() -> NormalizationContext {
    NormalizationContext {
        owner: OwnerId::new_random(),
        source: SourceId::new_random(),
    }
}

fn submit(kind: OperationKind) -> SubmittedOperation {
    SubmittedOperation {
        account: AccountId::new_random(),
        kind,
        dates: OperationDates {
            cash_posted: Some(date!(2026 - 04 - 01)),
            trade: Some(date!(2026 - 04 - 01)),
            ..OperationDates::default()
        },
        idempotency_key: None,
        source_operation_id: None,
    }
}

fn all_kinds() -> Vec<OperationKind> {
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let quantity = Dec::new(Decimal::from(10));
    vec![
        OperationKind::Deposit {
            amount_minor: 100_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Withdrawal {
            amount_minor: 25_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Transfer {
            to: AccountId::new_random(),
            amount_minor: 40_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Buy {
            instrument,
            custody,
            quantity,
            gross_minor: 900_000,
            fee_minor: Some(1_500),
            accrued_interest_minor: Some(700),
            currency: CurrencyCode::Rub,
        },
        OperationKind::Sell {
            instrument,
            custody,
            quantity,
            gross_minor: 950_000,
            fee_minor: Some(1_500),
            accrued_interest_minor: Some(300),
            currency: CurrencyCode::Rub,
        },
        OperationKind::Income {
            instrument: Some(instrument),
            gross_minor: 12_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Fee {
            amount_minor: 900,
            currency: CurrencyCode::Rub,
            origin: FeeOrigin::Depositary,
        },
        OperationKind::OpeningCash {
            amount_minor: -5_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::OpeningPosition {
            instrument,
            custody,
            quantity,
            cost_basis_minor: None,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Valuation {
            instrument,
            price: Dec::new(Decimal::new(1_005, 1)),
            currency: CurrencyCode::Rub,
            quality: PriceQuality::OwnerEstimate,
        },
    ]
}

#[test]
fn every_operation_kind_produces_a_structurally_valid_event() {
    for kind in all_kinds() {
        let operation = submit(kind.clone());
        let normalized = normalize(&operation, context())
            .unwrap_or_else(|rejection| panic!("{kind:?} отклонена: {rejection:?}"));
        normalized
            .event
            .validate_structure()
            .unwrap_or_else(|error| panic!("{kind:?} даёт неверную форму: {error}"));
    }
}

#[test]
fn a_purchase_settles_for_body_plus_accrued_plus_fee() {
    // 9 000,00 тела + 7,00 НКД + 15,00 комиссии = списание 9 022,00.
    let operation = submit(OperationKind::Buy {
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
        quantity: Dec::new(Decimal::from(10)),
        gross_minor: 900_000,
        fee_minor: Some(1_500),
        accrued_interest_minor: Some(700),
        currency: CurrencyCode::Rub,
    });
    let event = normalize(&operation, context()).unwrap().event;
    let cash = event
        .cash_effect(CurrencyCode::Rub)
        .expect("денежный эффект");
    assert_eq!(cash.amount(), PostedMinor::new(-902_200));
}

#[test]
fn a_sale_settles_for_body_plus_accrued_minus_fee() {
    // 9 500,00 тела + 3,00 НКД − 15,00 комиссии = приход 9 488,00.
    let operation = submit(OperationKind::Sell {
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
        quantity: Dec::new(Decimal::from(10)),
        gross_minor: 950_000,
        fee_minor: Some(1_500),
        accrued_interest_minor: Some(300),
        currency: CurrencyCode::Rub,
    });
    let event = normalize(&operation, context()).unwrap().event;
    let cash = event
        .cash_effect(CurrencyCode::Rub)
        .expect("денежный эффект");
    assert_eq!(cash.amount(), PostedMinor::new(948_800));
    match event.kind {
        EventKind::Trade { side, .. } => assert_eq!(side, TradeSide::Sell),
        other => panic!("ожидалась сделка, получено {other:?}"),
    }
}

#[test]
fn a_negative_amount_is_rejected_with_field_expected_actual() {
    // Знак задаёт вид операции, а не клиент: отрицательное пополнение
    // не «исправляется» в вывод средств (§13, ответ 422).
    let operation = submit(OperationKind::Deposit {
        amount_minor: -1,
        currency: CurrencyCode::Rub,
    });
    let rejection = normalize(&operation, context()).unwrap_err();
    assert_eq!(rejection.field, "amount_minor");
    assert_eq!(rejection.actual, "-1");
}

#[test]
fn an_operation_without_any_date_is_rejected() {
    let mut operation = submit(OperationKind::Deposit {
        amount_minor: 1_000,
        currency: CurrencyCode::Rub,
    });
    operation.dates = OperationDates::default();
    let rejection = normalize(&operation, context()).unwrap_err();
    assert_eq!(rejection.field, "dates");
}

#[test]
fn a_transfer_to_the_same_account_is_rejected_before_the_legs_are_built() {
    let account = AccountId::new_random();
    let mut operation = submit(OperationKind::Transfer {
        to: account,
        amount_minor: 1_000,
        currency: CurrencyCode::Rub,
    });
    operation.account = account;
    let rejection = normalize(&operation, context()).unwrap_err();
    assert_eq!(rejection.field, "to");
}
```

- [ ] **Шаг 3: Вердикты**

```rust
//! Вердикты приёмки (§10.4).

use iaam_core::ids::EventId;
use serde::{Deserialize, Serialize};

/// Почему строка отклонена. Поле, ожидаемое и полученное — требование
/// §13 к ответам `422`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

/// Вердикт по одной строке.
///
/// Отдельного шага подтверждения в нормальном пути нет: есть отправка
/// и вердикт (§10.4). Вариант `Accepted` на этапе 1 недостижим —
/// подтверждать нечем, пока нет сверки (E2), и это записано в типе,
/// а не в комментарии к документации.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Записано, независимого подтверждения пока нет.
    Provisional { event: EventId },
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
            Self::Provisional { .. } => "provisional",
            Self::Duplicate { .. } => "duplicate",
            Self::NeedsClassification { .. } => "needs_classification",
            Self::Unsupported { .. } => "unsupported",
            Self::Rejected { .. } => "rejected",
        }
    }

    /// Была ли строка записана в журнал.
    #[must_use]
    pub const fn is_recorded(&self) -> bool {
        match self {
            Self::Provisional { .. } | Self::Duplicate { .. } => true,
            Self::NeedsClassification { .. } | Self::Unsupported { .. } | Self::Rejected { .. } => {
                false
            }
        }
    }
}
```

- [ ] **Шаг 4: Нормализация**

```rust
//! Нормализованная операция и её превращение в событие журнала.

use iaam_core::dates::{
    CashPostedDate, EffectiveOrder, EventDates, PaidDate, SettledDate, TradeDate,
};
use iaam_core::event::kind::{EventKind, FeeOrigin, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::verdict::Rejection;

/// Версия разбора. Пишется в provenance: без неё нельзя отличить ошибку
/// источника от ошибки разбора, исправленной позже (§4.1).
pub const PARSER_VERSION: &str = "ingest/manual/1";

/// Даты операции. Все необязательны, кроме той, что делает операцию
/// датированной: событие без единой даты не попадает ни в один период.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OperationDates {
    pub trade: Option<time::Date>,
    pub settled: Option<time::Date>,
    pub cash_posted: Option<time::Date>,
    pub paid: Option<time::Date>,
}

impl OperationDates {
    fn to_event_dates(self) -> EventDates {
        EventDates {
            trade: self.trade.map(TradeDate),
            settled: self.settled.map(SettledDate),
            cash_posted: self.cash_posted.map(CashPostedDate),
            entitlement: None,
            paid: self.paid.map(PaidDate),
            tax_period_override: None,
        }
    }
}

/// Что произошло. Величины **положительные**: знак определяет вид
/// операции, а не клиент.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OperationKind {
    Deposit {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    Withdrawal {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    Transfer {
        to: AccountId,
        amount_minor: i64,
        currency: CurrencyCode,
    },
    Buy {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Dec,
        gross_minor: i64,
        fee_minor: Option<i64>,
        accrued_interest_minor: Option<i64>,
        currency: CurrencyCode,
    },
    Sell {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Dec,
        gross_minor: i64,
        fee_minor: Option<i64>,
        accrued_interest_minor: Option<i64>,
        currency: CurrencyCode,
    },
    Income {
        instrument: Option<InstrumentId>,
        gross_minor: i64,
        currency: CurrencyCode,
    },
    Fee {
        amount_minor: i64,
        currency: CurrencyCode,
        origin: FeeOrigin,
    },
    OpeningCash {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    OpeningPosition {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Dec,
        cost_basis_minor: Option<i64>,
        currency: CurrencyCode,
    },
    Valuation {
        instrument: InstrumentId,
        price: Dec,
        currency: CurrencyCode,
        quality: PriceQuality,
    },
}

/// Операция, пришедшая через API или из файла.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmittedOperation {
    pub account: AccountId,
    pub kind: OperationKind,
    pub dates: OperationDates,
    /// Ключ идемпотентности клиента (§10.6).
    pub idempotency_key: Option<String>,
    /// Идентификатор операции в источнике, если он есть.
    pub source_operation_id: Option<String>,
}

/// Готовое к записи событие плюс отпечаток сырой записи.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    pub event: Event,
}

/// Контекст нормализации: кто владелец и из какого источника пришло.
///
/// Порядкового номера здесь нет намеренно: его назначает хранилище
/// в той же транзакции, что и вставку. Приёмка ставит номер `1`
/// как заведомо временный — хранилище его перезапишет (§4.8).
#[derive(Debug, Clone, Copy)]
pub struct NormalizationContext {
    pub owner: OwnerId,
    pub source: SourceId,
}

/// Превращение операции в событие журнала.
///
/// Возвращает отказ, а не паникует и не подставляет умолчания: строка
/// с непонятой операцией получает вердикт, а документ продолжает
/// разбираться (§10.1).
pub fn normalize(
    operation: &SubmittedOperation,
    context: NormalizationContext,
) -> Result<Normalized, Rejection> {
    let dates = operation.dates.to_event_dates();
    let day = dates.effective_date().ok_or_else(|| Rejection {
        field: "dates".into(),
        expected: "хотя бы одна дата: trade, settled, cash_posted или paid".into(),
        actual: "ни одной".into(),
    })?;

    let (kind, legs) = build(operation, &operation.kind)?;
    let raw_hash = fingerprint(operation);

    Ok(Normalized {
        event: Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: context.owner,
            account: operation.account,
            kind,
            dates,
            // Временный номер: окончательный ставит хранилище.
            order: EffectiveOrder::new(day, 1),
            legs,
            provenance: {
                let base = Provenance::new(
                    context.source,
                    raw_hash,
                    ParserVersion(PARSER_VERSION.to_owned()),
                );
                match operation.source_operation_id.as_deref() {
                    Some(id) => base.with_source_operation_id(id),
                    None => base,
                }
            },
            relation: Relation::None,
            // `Confidence` описывает **значение**, а не сверку (§4.9):
            // владелец, вводящий пополнение вручную, знает его сумму.
            // Отсутствие независимого подтверждения — это утверждение
            // о счёте и интервале (§10.3), оно появится в E2 отдельной
            // сущностью и полем события не является.
            confidence: Confidence::Known,
            idempotency_key: operation.idempotency_key.clone(),
        },
    })
}

/// Отпечаток нормализованной записи (§10.6, ключ третьей силы).
fn fingerprint(operation: &SubmittedOperation) -> RawHash {
    let mut hasher = Sha256::new();
    hasher.update(operation.account.inner().as_bytes());
    hasher.update(format!("{:?}", operation.kind).as_bytes());
    hasher.update(format!("{:?}", operation.dates).as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    // Длина и алфавит гарантированы SHA-256, поэтому разбор не может
    // отказать; но подставлять заглушку в случае отказа нельзя —
    // provenance без хеша не должно существовать.
    RawHash::parse(&hex).unwrap_or_else(|| {
        unreachable_hash();
    })
}

/// Отдельная функция вместо `unwrap`: `unwrap` на `Option` в этом месте
/// читался бы как «а вдруг», хотя вариант невозможен по построению.
fn unreachable_hash() -> ! {
    panic!("SHA-256 всегда даёт 64 шестнадцатеричных знака")
}

/// Перевод десятичной суммы в минимальные единицы **без округления**.
///
/// Сумма с большей точностью, чем минимальная единица валюты, — это
/// не «почти правильная» сумма, а неверные входные данные: округлив её,
/// система запишет факт, которого не было (§3.4).
pub fn to_minor_units(
    value: rust_decimal::Decimal,
    currency: CurrencyCode,
    field: &str,
) -> Result<i64, Rejection> {
    let scale = currency.minor_units();
    if value.scale() > scale {
        return Err(Rejection {
            field: field.to_owned(),
            expected: format!(
                "не более {scale} знаков после запятой для {}",
                currency.code()
            ),
            actual: value.to_string(),
        });
    }
    let factor = rust_decimal::Decimal::from(10_i64.pow(scale));
    let scaled = value
        .checked_mul(factor)
        .ok_or_else(|| Rejection {
            field: field.to_owned(),
            expected: "представимая сумма".into(),
            actual: value.to_string(),
        })?
        .normalize();
    i64::try_from(scaled.mantissa())
        .ok()
        .filter(|_| scaled.scale() == 0)
        .ok_or_else(|| Rejection {
            field: field.to_owned(),
            expected: "целое число минимальных единиц".into(),
            actual: scaled.to_string(),
        })
}

fn money(minor: i64, currency: CurrencyCode) -> Money {
    Money::new(PostedMinor::new(minor), currency)
}

fn positive(value: i64, field: &str) -> Result<i64, Rejection> {
    if value > 0 {
        Ok(value)
    } else {
        Err(Rejection {
            field: field.to_owned(),
            expected: "положительная величина в минимальных единицах".into(),
            actual: value.to_string(),
        })
    }
}

/// Построение типа события и ног.
///
/// Диспетчер исчерпывающий: новый вид операции обязан сломать сборку.
fn build(
    operation: &SubmittedOperation,
    kind: &OperationKind,
) -> Result<(EventKind, Vec<Leg>), Rejection> {
    let account = operation.account;
    match kind {
        OperationKind::Deposit {
            amount_minor,
            currency,
        } => {
            let amount = money(positive(*amount_minor, "amount_minor")?, *currency);
            Ok((
                EventKind::CashIn { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::Withdrawal {
            amount_minor,
            currency,
        } => {
            let amount = money(-positive(*amount_minor, "amount_minor")?, *currency);
            Ok((
                EventKind::CashOut { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::Transfer {
            to,
            amount_minor,
            currency,
        } => {
            if *to == account {
                return Err(Rejection {
                    field: "to".into(),
                    expected: "счёт, отличный от счёта операции".into(),
                    actual: to.inner().to_string(),
                });
            }
            let amount = money(positive(*amount_minor, "amount_minor")?, *currency);
            let outgoing = amount.checked_negate().map_err(|error| Rejection {
                field: "amount_minor".into(),
                expected: "представимая сумма".into(),
                actual: error.to_string(),
            })?;
            Ok((
                EventKind::CashTransfer {
                    transfer_id: iaam_core::ids::TransferId::new_random(),
                    from: account,
                    to: *to,
                    amount,
                },
                vec![Leg::cash(account, outgoing), Leg::cash(*to, amount)],
            ))
        }
        OperationKind::Buy {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        } => {
            let gross = money(positive(*gross_minor, "gross_minor")?, *currency);
            let fee = fee_money(*fee_minor, *currency)?;
            let accrued = fee_money(*accrued_interest_minor, *currency)?;
            let mut settlement = gross.amount().raw();
            settlement += accrued.map_or(0, |value| value.amount().raw());
            settlement += fee.map_or(0, |value| value.amount().raw());
            Ok((
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument: *instrument,
                    quantity: Quantity(*quantity),
                    gross,
                    fee,
                    accrued_interest: accrued,
                },
                vec![
                    Leg::cash(account, money(-settlement, *currency)),
                    Leg::security(account, *custody, *instrument, Quantity(*quantity)),
                ],
            ))
        }
        OperationKind::Sell {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        } => {
            let gross = money(positive(*gross_minor, "gross_minor")?, *currency);
            let fee = fee_money(*fee_minor, *currency)?;
            let accrued = fee_money(*accrued_interest_minor, *currency)?;
            let mut settlement = gross.amount().raw();
            settlement += accrued.map_or(0, |value| value.amount().raw());
            settlement -= fee.map_or(0, |value| value.amount().raw());
            let sold = quantity.checked_neg().map_err(|error| Rejection {
                field: "quantity".into(),
                expected: "представимое количество".into(),
                actual: error.to_string(),
            })?;
            Ok((
                EventKind::Trade {
                    side: TradeSide::Sell,
                    instrument: *instrument,
                    quantity: Quantity(*quantity),
                    gross,
                    fee,
                    accrued_interest: accrued,
                },
                vec![
                    Leg::cash(account, money(settlement, *currency)),
                    Leg::security(account, *custody, *instrument, Quantity(sold)),
                ],
            ))
        }
        OperationKind::Income {
            instrument,
            gross_minor,
            currency,
        } => {
            let gross = money(positive(*gross_minor, "gross_minor")?, *currency);
            Ok((
                EventKind::Income {
                    instrument: *instrument,
                    gross,
                },
                vec![Leg::cash(account, gross)],
            ))
        }
        OperationKind::Fee {
            amount_minor,
            currency,
            origin,
        } => {
            let amount = money(-positive(*amount_minor, "amount_minor")?, *currency);
            Ok((
                EventKind::Fee {
                    amount,
                    origin: *origin,
                },
                vec![Leg::fee(account, amount)],
            ))
        }
        OperationKind::OpeningCash {
            amount_minor,
            currency,
        } => {
            // Восстановленный остаток может быть отрицательным (§15.9),
            // поэтому нуля здесь не требуется, а знак берётся как есть.
            let amount = money(*amount_minor, *currency);
            Ok((
                EventKind::OpeningCash { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::OpeningPosition {
            instrument,
            custody,
            quantity,
            cost_basis_minor,
            currency,
        } => {
            let cost_basis = match cost_basis_minor {
                Some(value) => Some(money(positive(*value, "cost_basis_minor")?, *currency)),
                None => None,
            };
            Ok((
                EventKind::OpeningPosition {
                    instrument: *instrument,
                    quantity: Quantity(*quantity),
                    cost_basis,
                },
                vec![Leg::security(
                    account,
                    *custody,
                    *instrument,
                    Quantity(*quantity),
                )],
            ))
        }
        OperationKind::Valuation {
            instrument,
            price,
            currency,
            quality,
        } => Ok((
            EventKind::Valuation {
                instrument: *instrument,
                price: *price,
                currency: *currency,
                quality: *quality,
            },
            vec![],
        )),
    }
}

/// Комиссия и НКД приходят положительными: знак задаёт `trade_settlement`
/// ядра, и дублировать это решение в приёмке нельзя.
fn fee_money(value: Option<i64>, currency: CurrencyCode) -> Result<Option<Money>, Rejection> {
    match value {
        None => Ok(None),
        Some(minor) => Ok(Some(money(positive(minor, "fee_minor")?, currency))),
    }
}
```

- [ ] **Шаг 5: Корень крейты**

```rust
//! Приёмка (§10).
//!
//! Единый вход: и ручной ввод, и CSV, и внешний агент попадают сюда.
//! Разбор построчный — документ целиком не отклоняется из-за одной
//! непонятой строки (§10.1), и каждая строка получает вердикт (§10.4).
//!
//! **Знаки и ноги строит приёмка, а не клиент.** Клиент присылает
//! положительную величину и вид операции; превращение её в ноги события
//! с правильными знаками — работа этого крейта. Иначе знаковая
//! конвенция становится частью публичного контракта, и её обязан
//! знать внешний агент, которому арифметика запрещена (§13).

pub mod csv_source;
pub mod operation;
pub mod verdict;

pub use operation::{Normalized, OperationDates, OperationKind, SubmittedOperation, normalize};
pub use verdict::{Rejection, Verdict};
```

Файл `src/csv_source.rs` создаётся заглушкой `//! Заполняется задачей 13.` — модуль объявлен в `lib.rs`.

- [ ] **Шаг 6: Зелёная сборка и коммит**

```bash
nix develop -c cargo test -p iaam-ingest
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/iaam-ingest Cargo.toml
git commit -m "feat(ingest): нормализация операций в события журнала (iaam-1fk)"
```

---

## Задача 13: `iaam-ingest` — разбор CSV

**Files:**
- Create: `crates/iaam-ingest/src/csv_source.rs`
- Create: `crates/iaam-ingest/tests/csv_rows.rs`

**Interfaces:**
- Produces: `iaam_ingest::csv_source::{Directory, Row, ParsedRow, parse}`.

**Acceptance Criteria:**
- Разбор построчный: одна непонятая строка не отменяет документ.
- Счёт и инструмент указываются человеческими именами и разрешаются справочником.
- Сумма с точностью выше минимальной единицы валюты **отклоняется**, а не округляется.
- Неизвестный вид операции, неизвестное имя и неверная дата дают отказ с именем поля.

**Почему имена, а не идентификаторы.** UUID в файле, который заполняет человек, — способ гарантировать ошибки. Справочник заполняет оболочка из таблицы счетов; неизвестное имя даёт отказ с полем `account`, а не молчаливое создание счёта.

**Почему округление запрещено.** Округлив входную сумму, система запишет факт, которого не было. Источник, публикующий суммы с большей точностью, — это повод разобраться с источником, а не тихо потерять копейки.

- [ ] **Шаг 1: Написать падающие тесты**

`crates/iaam-ingest/tests/csv_rows.rs`:

```rust
//! Построчный разбор CSV: одна непонятая строка не отменяет документ.

use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_ingest::csv_source::{Directory, ParsedRow, parse};
use iaam_ingest::operation::OperationKind;

fn directory() -> (Directory, AccountId, InstrumentId) {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut dir = Directory {
        default_custody: Some(CustodyId::new_random()),
        ..Directory::default()
    };
    dir.accounts.insert("Брокерский".into(), account);
    dir.instruments.insert("SBER".into(), instrument);
    (dir, account, instrument)
}

const HEADER: &str = "date,type,account,counterparty_account,instrument,custody,quantity,amount,fee,accrued_interest,currency,idempotency_key";

#[test]
fn a_good_document_parses_into_operations() {
    let (dir, account, instrument) = directory();
    let document = format!(
        "{HEADER}\n\
         2026-01-10,deposit,Брокерский,,,,,100000.00,,,RUB,dep-1\n\
         2026-01-15,buy,Брокерский,,SBER,,100,29050.50,35.00,,RUB,buy-1\n"
    );
    let rows = parse(&document, &dir);
    assert_eq!(rows.len(), 2);

    match &rows[0] {
        ParsedRow::Operation(operation) => {
            assert_eq!(operation.account, account);
            assert_eq!(operation.idempotency_key.as_deref(), Some("dep-1"));
            assert_eq!(
                operation.kind,
                OperationKind::Deposit {
                    amount_minor: 10_000_000,
                    currency: iaam_core::money::CurrencyCode::Rub,
                }
            );
        }
        other => panic!("ожидалась операция, получено {other:?}"),
    }

    match &rows[1] {
        ParsedRow::Operation(operation) => match &operation.kind {
            OperationKind::Buy {
                instrument: parsed,
                gross_minor,
                fee_minor,
                ..
            } => {
                assert_eq!(*parsed, instrument);
                // 29 050,50 рубля = 2 905 050 копеек; комиссия 35,00 = 3 500.
                assert_eq!(*gross_minor, 2_905_050);
                assert_eq!(*fee_minor, Some(3_500));
            }
            other => panic!("ожидалась покупка, получено {other:?}"),
        },
        other => panic!("ожидалась операция, получено {other:?}"),
    }
}

#[test]
fn one_bad_row_does_not_cancel_the_document() {
    let (dir, _, _) = directory();
    let document = format!(
        "{HEADER}\n\
         2026-01-10,deposit,Брокерский,,,,,100000.00,,,RUB,\n\
         не-дата,deposit,Брокерский,,,,,1000.00,,,RUB,\n\
         2026-01-12,deposit,Неизвестный счёт,,,,,1000.00,,,RUB,\n\
         2026-01-13,летающая операция,Брокерский,,,,,1000.00,,,RUB,\n"
    );
    let rows = parse(&document, &dir);
    assert_eq!(rows.len(), 4);
    assert!(matches!(rows[0], ParsedRow::Operation(_)));

    let fields: Vec<&str> = rows[1..]
        .iter()
        .map(|row| match row {
            ParsedRow::Rejected(rejection) => rejection.field.as_str(),
            ParsedRow::Operation(_) => "операция",
        })
        .collect();
    assert_eq!(fields, vec!["date", "account", "type"]);
}

#[test]
fn an_amount_more_precise_than_the_currency_is_rejected_not_rounded() {
    // Округлив входную сумму, система запишет факт, которого не было.
    let (dir, _, _) = directory();
    let document = format!("{HEADER}\n2026-01-10,deposit,Брокерский,,,,,100.005,,,RUB,\n");
    let rows = parse(&document, &dir);
    match &rows[0] {
        ParsedRow::Rejected(rejection) => {
            assert_eq!(rejection.field, "amount");
            assert_eq!(rejection.actual, "100.005");
        }
        other => panic!("ожидался отказ, получено {other:?}"),
    }
}
```

- [ ] **Шаг 2: Убедиться, что тесты падают**

```bash
nix develop -c cargo test -p iaam-ingest --test csv_rows
```

- [ ] **Шаг 3: Реализация**

```rust
//! Разбор CSV (§10.1).
//!
//! Строка — единица разбора: непонятая строка получает вердикт, а
//! документ продолжает разбираться. Счёт и инструмент указываются
//! человеческими именами и разрешаются справочником: идентификаторы
//! UUID в файле, который заполняет человек, — способ гарантировать
//! ошибки.
//!
//! Суммы записываются как десятичные числа. Число с большей точностью,
//! чем минимальная единица валюты, **отклоняется**, а не округляется:
//! округление входных данных — это тихое изменение факта.

use std::collections::BTreeMap;

use iaam_core::event::kind::FeeOrigin;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use rust_decimal::Decimal;
use serde::Deserialize;
use time::Date;
use time::macros::format_description;

use crate::operation::{OperationDates, OperationKind, SubmittedOperation, to_minor_units};
use crate::verdict::Rejection;

/// Справочник имён. Заполняется оболочкой из таблиц счетов и инструментов.
#[derive(Debug, Clone, Default)]
pub struct Directory {
    pub accounts: BTreeMap<String, AccountId>,
    pub custodies: BTreeMap<String, CustodyId>,
    pub instruments: BTreeMap<String, InstrumentId>,
    /// Место хранения по умолчанию для счёта без указанного депозитария.
    pub default_custody: Option<CustodyId>,
}

/// Одна строка файла в сыром виде.
#[derive(Debug, Clone, Deserialize)]
pub struct Row {
    pub date: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub account: String,
    #[serde(default)]
    pub counterparty_account: Option<String>,
    #[serde(default)]
    pub instrument: Option<String>,
    #[serde(default)]
    pub custody: Option<String>,
    #[serde(default)]
    pub quantity: Option<String>,
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(default)]
    pub fee: Option<String>,
    #[serde(default)]
    pub accrued_interest: Option<String>,
    pub currency: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// Результат разбора одной строки.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedRow {
    Operation(Box<SubmittedOperation>),
    Rejected(Rejection),
}

/// Разбор всего документа. Возвращает по элементу на строку, включая
/// отклонённые: номер строки — это индекс в результате плюс единица.
#[must_use]
pub fn parse(content: &str, directory: &Directory) -> Vec<ParsedRow> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());
    let mut parsed = Vec::new();
    for record in reader.deserialize::<Row>() {
        parsed.push(match record {
            Ok(row) => match row_to_operation(&row, directory) {
                Ok(operation) => ParsedRow::Operation(Box::new(operation)),
                Err(rejection) => ParsedRow::Rejected(rejection),
            },
            Err(error) => ParsedRow::Rejected(Rejection {
                field: "row".into(),
                expected: "строка в формате заголовка файла".into(),
                actual: error.to_string(),
            }),
        });
    }
    parsed
}

fn row_to_operation(row: &Row, directory: &Directory) -> Result<SubmittedOperation, Rejection> {
    let date = parse_date(&row.date)?;
    let currency = parse_currency(&row.currency)?;
    let account = lookup(&directory.accounts, &row.account, "account")?;
    let kind = build_kind(row, directory, currency)?;

    Ok(SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: Some(date),
            cash_posted: Some(date),
            ..OperationDates::default()
        },
        idempotency_key: row.idempotency_key.clone(),
        source_operation_id: None,
    })
}

fn build_kind(
    row: &Row,
    directory: &Directory,
    currency: CurrencyCode,
) -> Result<OperationKind, Rejection> {
    match row.kind.as_str() {
        "deposit" => Ok(OperationKind::Deposit {
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "withdrawal" => Ok(OperationKind::Withdrawal {
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "transfer" => Ok(OperationKind::Transfer {
            to: lookup(
                &directory.accounts,
                row.counterparty_account.as_deref().unwrap_or_default(),
                "counterparty_account",
            )?,
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "buy" | "sell" => build_trade(row, directory, currency),
        "income" => Ok(OperationKind::Income {
            instrument: match row.instrument.as_deref() {
                None | Some("") => None,
                Some(symbol) => Some(lookup(&directory.instruments, symbol, "instrument")?),
            },
            gross_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "fee" => Ok(OperationKind::Fee {
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
            origin: FeeOrigin::Other,
        }),
        "valuation" => Ok(OperationKind::Valuation {
            instrument: lookup(
                &directory.instruments,
                row.instrument.as_deref().unwrap_or_default(),
                "instrument",
            )?,
            price: Dec::new(decimal(row.amount.as_deref(), "amount")?),
            currency,
            // Цена, названная владельцем, не является исполнимой (§5.4).
            quality: PriceQuality::OwnerEstimate,
        }),
        other => Err(Rejection {
            field: "type".into(),
            expected: "deposit, withdrawal, transfer, buy, sell, income, fee или valuation".into(),
            actual: other.to_owned(),
        }),
    }
}

fn build_trade(
    row: &Row,
    directory: &Directory,
    currency: CurrencyCode,
) -> Result<OperationKind, Rejection> {
    let instrument = lookup(
        &directory.instruments,
        row.instrument.as_deref().unwrap_or_default(),
        "instrument",
    )?;
    let custody = match row.custody.as_deref() {
        Some(name) if !name.is_empty() => lookup(&directory.custodies, name, "custody")?,
        _ => directory.default_custody.ok_or_else(|| Rejection {
            field: "custody".into(),
            expected: "известное место хранения или значение по умолчанию".into(),
            actual: "не указано".into(),
        })?,
    };
    let quantity = Dec::new(decimal(row.quantity.as_deref(), "quantity")?);
    let gross_minor = minor(row.amount.as_deref(), "amount", currency)?;
    let fee_minor = optional_minor(row.fee.as_deref(), "fee", currency)?;
    let accrued_interest_minor = optional_minor(
        row.accrued_interest.as_deref(),
        "accrued_interest",
        currency,
    )?;

    if row.kind == "buy" {
        Ok(OperationKind::Buy {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        })
    } else {
        Ok(OperationKind::Sell {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        })
    }
}

fn lookup<T: Copy>(
    table: &BTreeMap<String, T>,
    name: &str,
    field: &'static str,
) -> Result<T, Rejection> {
    table.get(name).copied().ok_or_else(|| Rejection {
        field: field.to_owned(),
        expected: "имя из справочника".into(),
        actual: name.to_owned(),
    })
}

fn parse_date(value: &str) -> Result<Date, Rejection> {
    Date::parse(value, format_description!("[year]-[month]-[day]")).map_err(|_| Rejection {
        field: "date".into(),
        expected: "дата в формате ГГГГ-ММ-ДД".into(),
        actual: value.to_owned(),
    })
}

fn parse_currency(value: &str) -> Result<CurrencyCode, Rejection> {
    match value {
        "RUB" => Ok(CurrencyCode::Rub),
        "USD" => Ok(CurrencyCode::Usd),
        "EUR" => Ok(CurrencyCode::Eur),
        "CNY" => Ok(CurrencyCode::Cny),
        "XAU" => Ok(CurrencyCode::Xau),
        other => Err(Rejection {
            field: "currency".into(),
            expected: "RUB, USD, EUR, CNY или XAU".into(),
            actual: other.to_owned(),
        }),
    }
}

fn decimal(value: Option<&str>, field: &'static str) -> Result<Decimal, Rejection> {
    let raw = value.unwrap_or_default();
    raw.parse::<Decimal>().map_err(|_| Rejection {
        field: field.to_owned(),
        expected: "десятичное число".into(),
        actual: raw.to_owned(),
    })
}

fn minor(
    value: Option<&str>,
    field: &'static str,
    currency: CurrencyCode,
) -> Result<i64, Rejection> {
    to_minor_units(decimal(value, field)?, currency, field)
}

fn optional_minor(
    value: Option<&str>,
    field: &'static str,
    currency: CurrencyCode,
) -> Result<Option<i64>, Rejection> {
    match value {
        None | Some("") => Ok(None),
        Some(raw) => minor(Some(raw), field, currency).map(Some),
    }
}
```

- [ ] **Шаг 4: Зелёная сборка и коммит**

```bash
nix develop -c cargo test -p iaam-ingest
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/iaam-ingest
git commit -m "feat(ingest): построчный разбор CSV с вердиктами (iaam-1fk)"
```

---

## Задача 14: `iaam-app` — порты, адаптер и сценарии

**Files:**
- Create: `crates/iaam-app/Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/ports.rs`
- Create: `crates/iaam-app/src/adapters/mod.rs`, `src/adapters/sqlite.rs`
- Create: `crates/iaam-app/src/scenarios/mod.rs`, `src/scenarios/ingest.rs`, `src/scenarios/reports.rs`
- Modify: корневой `Cargo.toml`

**Interfaces:**
- Produces: `iaam_app::{AppServices, ingest}`, `iaam_app::ports::{Store, Clock, SystemClock, Principal, Scope, Recorded, AccountView}`, `iaam_app::error::AppError`, `iaam_app::adapters::sqlite::SqliteAdapter`, `iaam_app::scenarios::ingest::submit_operations`, `iaam_app::scenarios::reports::{ReturnsQuery, returns}`.

**Acceptance Criteria:**
- Объектобезопасные порты существуют **только** здесь; механизм асинхронных трейтов один — `async_trait`.
- Каждый вызов блокирующего хранилища уходит в `spawn_blocking`.
- Нарушение инварианта отличается от прочих ошибок, несёт идентификатор корреляции и **не приводит к пересчёту**: полный пересчёт дал бы то же самое.
- Владелец передаётся в каждый порт, работающий со справочниками, контурами и снимками.
- Снимок продвигается, когда это возможно, и пересчитывается целиком, когда нет.
- Снимок сохраняется только для отчёта на сегодня.
- Арифметики над деньгами в крейте нет ни одной строки.

**Почему `async_trait`, а не RPITIT.** Спека требует выбрать **один** механизм и закрепить его (§3.2). Порты обязаны быть объектобезопасными (`Arc<dyn Store>` в `AppServices`), а RPITIT объектобезопасности не даёт. Цена — по одному боксу на вызов порта; на фоне обращения к SQLite это не измеримо.

**Почему адаптер живёт здесь, а не в `iaam-bootstrap`.** Граница async/blocking — это политика приложения, а не деталь сборки: именно приложение решает, что `rusqlite` вызывается через `spawn_blocking`. Транспорт про адаптер по-прежнему не знает — это проверяет заслон.

**Почему отравленный мьютекс восстанавливается, а не приводит к панике.** Паника в одном запросе не должна выводить из строя весь сервис, а состояние `SqliteStore` — это соединение, которое паника предыдущего вызова не повреждает.

**Почему снимок сохраняется только на сегодня.** Ключ снимка — контур, его версия и версия правила. Снимок, построенный по срезу на прошлую дату, лёг бы под тем же ключом и молча подменил бы состояние следующему запросу.

- [ ] **Шаг 1: Манифест**

```toml
[package]
name = "iaam-app"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
iaam-core = { path = "../iaam-core", version = "0.1.0" }
iaam-store = { path = "../iaam-store", version = "0.1.0" }
iaam-ingest = { path = "../iaam-ingest", version = "0.1.0" }
# Один и только один механизм асинхронных трейтов (§3.2). Выбран async-trait:
# порты обязаны быть объектобезопасными (`Arc<dyn Store>`), а RPITIT
# объектобезопасности не даёт. Смешение механизмов запрещено заслоном.
async-trait = "0.1"
thiserror = "2"
time = { version = "0.3", default-features = false, features = ["std", "macros"] }
tokio = { version = "1", features = ["rt", "macros", "sync"] }
tracing = "0.1"
uuid = { version = "1", features = ["serde", "v4"] }

[dev-dependencies]
tokio = { version = "1", features = ["rt", "macros", "rt-multi-thread"] }

[lints]
workspace = true
```

- [ ] **Шаг 2: Ошибки**

```rust
//! Ошибки сценариев.
//!
//! Разделение по §15.2: неполнота данных ошибкой не является и уходит
//! в отчёт блоком качества; нарушение инварианта отменяет отчёт и уходит
//! в лог с идентификатором корреляции.

use iaam_core::projection::ProjectionError;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("хранилище недоступно: {0}")]
    Store(String),
    #[error("не найдено: {what} {id}")]
    NotFound { what: &'static str, id: String },
    #[error("запрос некорректен: поле {field}, ожидалось {expected}, получено {actual}")]
    Invalid {
        field: String,
        expected: String,
        actual: String,
    },
    #[error("нарушен внутренний инвариант, идентификатор корреляции {correlation}")]
    Invariant {
        correlation: Uuid,
        #[source]
        source: ProjectionError,
    },
    #[error("проекция не построена: {0}")]
    Projection(#[source] ProjectionError),
}

impl AppError {
    /// Проекция превращается в ошибку приложения так, чтобы нарушение
    /// инварианта нельзя было спутать с обычным отказом: у первого
    /// появляется идентификатор корреляции для логов (§15.2).
    #[must_use]
    pub fn from_projection(error: ProjectionError) -> Self {
        if error.is_invariant_violation() {
            Self::Invariant {
                correlation: Uuid::new_v4(),
                source: error,
            }
        } else {
            Self::Projection(error)
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Store(_) => "store_unavailable",
            Self::NotFound { .. } => "not_found",
            Self::Invalid { .. } => "invalid_request",
            Self::Invariant { .. } => "invariant_violated",
            Self::Projection(_) => "projection_failed",
        }
    }
}
```

- [ ] **Шаг 3: Порты**

```rust
//! Объектобезопасные порты. Единственное место, где они существуют (§3.2).

use async_trait::async_trait;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::projection::Snapshot;
use iaam_core::rules::LotRuleVersion;
use time::Date;
use uuid::Uuid;

use crate::error::AppError;

/// Результат записи события. Тип принадлежит порту, а не хранилищу:
/// иначе транспорт узнал бы про SQLite через возвращаемое значение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    Inserted { id: iaam_core::ids::EventId },
    Duplicate { existing: iaam_core::ids::EventId },
}

/// Права токена на уровне приложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Owner,
    Agent,
    ReadOnly,
}

impl Scope {
    #[must_use]
    pub const fn may_submit(self) -> bool {
        match self {
            Self::Owner | Self::Agent => true,
            Self::ReadOnly => false,
        }
    }

    #[must_use]
    pub const fn may_administer(self) -> bool {
        match self {
            Self::Owner => true,
            Self::Agent | Self::ReadOnly => false,
        }
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Agent => "agent",
            Self::ReadOnly => "read_only",
        }
    }
}

/// Опознанный носитель токена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub token_id: Uuid,
    pub owner: OwnerId,
    pub scope: Scope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountView {
    pub id: AccountId,
    pub title: String,
    pub institution: Option<String>,
}

/// Хранилище фактов и справочников.
#[async_trait]
pub trait Store: Send + Sync {
    /// Запись событий с назначением порядка внутри дня.
    ///
    /// Порядок назначает хранилище в той же транзакции, что и вставку:
    /// раздельные «узнать следующий номер» и «вставить» — гонка (§4.8).
    async fn append_events(&self, events: Vec<Event>) -> Result<Vec<Recorded>, AppError>;
    async fn load_events(&self, owner: OwnerId) -> Result<Vec<Event>, AppError>;
    async fn load_events_through(
        &self,
        owner: OwnerId,
        through: Date,
    ) -> Result<Vec<Event>, AppError>;

    /// Владелец входит в каждый запрос справочников и контуров.
    /// Идентификатор контура — это UUID, а UUID не является правом
    /// доступа: без владельца в запросе любой знающий идентификатор
    /// читает чужой портфель (§14).
    async fn load_contour(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
    ) -> Result<Option<ContourDefinition>, AppError>;
    async fn latest_contour_version(
        &self,
        owner: OwnerId,
        contour: ContourId,
    ) -> Result<Option<ContourVersion>, AppError>;
    async fn insert_contour_version(
        &self,
        owner: OwnerId,
        definition: ContourDefinition,
        title: String,
        accounts: Vec<AccountId>,
    ) -> Result<(), AppError>;

    async fn upsert_account(&self, owner: OwnerId, account: AccountView) -> Result<(), AppError>;
    async fn list_accounts(&self, owner: OwnerId) -> Result<Vec<AccountView>, AppError>;

    async fn save_snapshot(&self, owner: OwnerId, snapshot: Snapshot) -> Result<(), AppError>;
    async fn load_snapshot(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
        lot_rule: LotRuleVersion,
    ) -> Result<Option<Snapshot>, AppError>;

    async fn find_principal(&self, token_hash: String) -> Result<Option<Principal>, AppError>;
    async fn record_token_use(
        &self,
        token_hash: String,
        route: String,
        outcome: String,
    ) -> Result<(), AppError>;
}

/// Часы. Порт, а не `OffsetDateTime::now_utc()` внутри сценария:
/// отчёт «на сегодня» иначе невоспроизводим в тесте.
pub trait Clock: Send + Sync {
    fn today(&self) -> Date;
}

/// Системные часы.
pub struct SystemClock;

impl Clock for SystemClock {
    fn today(&self) -> Date {
        time::OffsetDateTime::now_utc().date()
    }
}
```

- [ ] **Шаг 4: Адаптер хранилища**

`crates/iaam-app/src/adapters/mod.rs`:

```rust
//! Адаптеры портов.

pub mod sqlite;
```

```rust
//! Порт хранилища поверх `iaam-store`.
//!
//! Здесь и только здесь пересекается граница async/blocking (§3.2).
//! `rusqlite` блокирует поток; вызов его прямо из обработчика `axum`
//! останавливает исполнитель, поэтому каждая операция уходит в
//! `spawn_blocking`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::projection::Snapshot;
use iaam_core::rules::LotRuleVersion;
use iaam_store::SqliteStore;
use iaam_store::events::Appended;
use iaam_store::reference::AccountRecord;
use iaam_store::tokens::TokenScope;
use time::Date;

use crate::error::AppError;
use crate::ports::{AccountView, Principal, Recorded, Scope, Store};

/// Соединение под мьютексом: `rusqlite::Connection` не `Sync`, а писатель
/// у однопользовательской базы один. Пул появится тогда, когда появится
/// второй писатель, а не раньше.
pub struct SqliteAdapter {
    store: Arc<Mutex<SqliteStore>>,
}

impl SqliteAdapter {
    #[must_use]
    pub fn new(store: SqliteStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
        }
    }

    /// Выполнение блокирующей операции.
    ///
    /// Отравленный мьютекс восстанавливается, а не приводит к панике:
    /// паника в одном запросе не должна выводить из строя весь сервис,
    /// а состояние `SqliteStore` — это соединение, которое паника
    /// предыдущего вызова не повреждает.
    async fn blocking<T, F>(&self, work: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(&mut SqliteStore) -> Result<T, AppError> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let mut guard = match store.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            work(&mut guard)
        })
        .await
        .map_err(|error| AppError::Store(format!("блокирующая задача не выполнена: {error}")))?
    }
}

fn store_error(error: iaam_store::StoreError) -> AppError {
    AppError::Store(error.to_string())
}

#[async_trait]
impl Store for SqliteAdapter {
    async fn append_events(&self, events: Vec<Event>) -> Result<Vec<Recorded>, AppError> {
        self.blocking(move |store| {
            let mut recorded = Vec::with_capacity(events.len());
            for event in &events {
                let outcome = store.append_event_in_order(event).map_err(store_error)?;
                recorded.push(match outcome {
                    Appended::Inserted { id } => Recorded::Inserted { id },
                    Appended::Duplicate { existing } => Recorded::Duplicate { existing },
                });
            }
            Ok(recorded)
        })
        .await
    }

    async fn load_events(&self, owner: OwnerId) -> Result<Vec<Event>, AppError> {
        self.blocking(move |store| store.load_events(owner).map_err(store_error))
            .await
    }

    async fn load_events_through(
        &self,
        owner: OwnerId,
        through: Date,
    ) -> Result<Vec<Event>, AppError> {
        self.blocking(move |store| {
            store
                .load_events_through(owner, through)
                .map_err(store_error)
        })
        .await
    }

    async fn load_contour(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
    ) -> Result<Option<ContourDefinition>, AppError> {
        self.blocking(move |store| {
            store
                .load_contour(owner, contour, version)
                .map_err(store_error)
        })
        .await
    }

    async fn latest_contour_version(
        &self,
        owner: OwnerId,
        contour: ContourId,
    ) -> Result<Option<ContourVersion>, AppError> {
        self.blocking(move |store| {
            store
                .latest_contour_version(owner, contour)
                .map_err(store_error)
        })
        .await
    }

    async fn insert_contour_version(
        &self,
        owner: OwnerId,
        definition: ContourDefinition,
        title: String,
        accounts: Vec<AccountId>,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .insert_contour_version(owner, &definition, &title, &accounts)
                .map_err(store_error)
        })
        .await
    }

    async fn upsert_account(&self, owner: OwnerId, account: AccountView) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .upsert_account(&AccountRecord {
                    id: account.id,
                    owner,
                    title: account.title,
                    institution: account.institution,
                })
                .map_err(store_error)
        })
        .await
    }

    async fn list_accounts(&self, owner: OwnerId) -> Result<Vec<AccountView>, AppError> {
        self.blocking(move |store| {
            let accounts = store.list_accounts(owner).map_err(store_error)?;
            Ok(accounts
                .into_iter()
                .map(|record| AccountView {
                    id: record.id,
                    title: record.title,
                    institution: record.institution,
                })
                .collect())
        })
        .await
    }

    async fn save_snapshot(&self, owner: OwnerId, snapshot: Snapshot) -> Result<(), AppError> {
        self.blocking(move |store| store.save_snapshot(owner, &snapshot).map_err(store_error))
            .await
    }

    async fn load_snapshot(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
        lot_rule: LotRuleVersion,
    ) -> Result<Option<Snapshot>, AppError> {
        self.blocking(move |store| {
            store
                .load_snapshot(owner, contour, version, lot_rule)
                .map_err(store_error)
        })
        .await
    }

    async fn find_principal(&self, token_hash: String) -> Result<Option<Principal>, AppError> {
        self.blocking(move |store| {
            let found = store.find_token(&token_hash).map_err(store_error)?;
            Ok(found.map(|record| Principal {
                token_id: record.id,
                owner: record.owner,
                scope: match record.scope {
                    TokenScope::Owner => Scope::Owner,
                    TokenScope::Agent => Scope::Agent,
                    TokenScope::ReadOnly => Scope::ReadOnly,
                },
            }))
        })
        .await
    }

    async fn record_token_use(
        &self,
        token_hash: String,
        route: String,
        outcome: String,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .record_token_use(&token_hash, &route, &outcome)
                .map_err(store_error)
        })
        .await
    }
}
```

- [ ] **Шаг 5: Сценарии**

`crates/iaam-app/src/scenarios/mod.rs`:

```rust
//! Сценарии: собрать срез, позвать ядро, сохранить результат.
//!
//! Арифметики над деньгами здесь нет ни одной строки. Любое число,
//! попадающее в ответ API, приходит из `iaam-core` (§3.1, §13).

pub mod ingest;
pub mod reports;
```

```rust
//! Приёмка операций.

use iaam_core::ids::SourceId;
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{SubmittedOperation, Verdict, normalize};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{Principal, Recorded};

/// Отправка пачки операций.
///
/// Вердикт выдаётся **на каждую строку**: одна непонятая операция
/// не отменяет остальные (§10.1). Порядковые номера выдаются
/// хранилищем по дате, поэтому две операции одного дня не сливаются
/// в одну позицию порядка.
pub async fn submit_operations(
    services: &AppServices,
    principal: &Principal,
    source: SourceId,
    operations: &[SubmittedOperation],
) -> Result<Vec<Verdict>, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".into(),
            expected: "право отправки операций".into(),
            actual: principal.scope.code().to_owned(),
        });
    }

    let mut verdicts = Vec::with_capacity(operations.len());
    for operation in operations {
        // Порядковый номер внутри дня назначает хранилище в той же
        // транзакции, что и вставку: «узнать следующий» отдельным
        // вызовом — гонка, дающая двум событиям один номер (§4.8).
        let normalized = match normalize(
            operation,
            NormalizationContext {
                owner: principal.owner,
                source,
            },
        ) {
            Ok(normalized) => normalized,
            Err(rejection) => {
                verdicts.push(Verdict::Rejected { rejection });
                continue;
            }
        };

        // Структурная проверка ядра до записи: журнал append-only,
        // и неверное по форме событие из него уже не убрать (§4.8).
        if let Err(error) = normalized.event.validate_structure() {
            verdicts.push(Verdict::Rejected {
                rejection: iaam_ingest::Rejection {
                    field: "operation".into(),
                    expected: "форма события, соответствующая его типу".into(),
                    actual: error.to_string(),
                },
            });
            continue;
        }

        let recorded = services.store.append_events(vec![normalized.event]).await?;
        verdicts.push(match recorded.first() {
            Some(Recorded::Inserted { id }) => Verdict::Provisional { event: *id },
            Some(Recorded::Duplicate { existing }) => Verdict::Duplicate {
                existing: *existing,
            },
            None => Verdict::Rejected {
                rejection: iaam_ingest::Rejection {
                    field: "storage".into(),
                    expected: "запись события".into(),
                    actual: "хранилище не вернуло результата".into(),
                },
            },
        });
    }
    Ok(verdicts)
}
```

```rust
//! Отчёты.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::projection::{Projection, ProjectionContext, advance, project};
use iaam_core::returns::{ReturnsReport, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::FxTable;
use time::Date;

use crate::AppServices;
use crate::error::AppError;
use crate::ports::Principal;

/// Запрос отчёта о доходности.
#[derive(Debug, Clone)]
pub struct ReturnsQuery {
    pub contour: ContourId,
    pub contour_version: Option<ContourVersion>,
    pub as_of: Option<Date>,
    pub report_currency: CurrencyCode,
    pub fx: FxTable,
    pub lot_rule: LotRuleVersion,
}

/// Отчёт по контуру.
pub async fn returns(
    services: &AppServices,
    principal: &Principal,
    query: &ReturnsQuery,
) -> Result<ReturnsReport, AppError> {
    let version = match query.contour_version {
        Some(version) => version,
        None => services
            .store
            .latest_contour_version(principal.owner, query.contour)
            .await?
            .ok_or_else(|| AppError::NotFound {
                what: "contour",
                id: query.contour.0.to_string(),
            })?,
    };
    // Контур загружается ВМЕСТЕ с владельцем: чужой контур не находится,
    // а не находится и отклоняется потом (§14).
    let definition = services
        .store
        .load_contour(principal.owner, query.contour, version)
        .await?
        .ok_or_else(|| AppError::NotFound {
            what: "contour_version",
            id: format!("{}/{}", query.contour.0, version.0),
        })?;

    let today = services.clock.today();
    let as_of = query.as_of.unwrap_or(today);
    let events = services
        .store
        .load_events_through(principal.owner, as_of)
        .await?;

    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &definition,
        rules: &rules,
        lot_rule: query.lot_rule,
    };

    let projection = build_projection(
        services,
        principal.owner,
        query,
        &definition,
        &events,
        &context,
    )
    .await?;

    // Снимок сохраняется только для отчёта на сегодня: снимок, построенный
    // по срезу на прошлую дату, лежал бы под тем же ключом и молча
    // подменял бы состояние следующему запросу.
    if as_of == today {
        services
            .store
            .save_snapshot(principal.owner, projection.snapshot().clone())
            .await?;
    }

    Ok(returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &definition,
            as_of,
            report_currency: query.report_currency,
            fx: &query.fx,
            solver_policy: SolverPolicy::returns_default(),
        },
    ))
}

/// Построение проекции: продвижение снимка, если оно применимо,
/// иначе полный пересчёт.
///
/// Срез передаётся в `advance` **целиком**: решение о том, что уже
/// свёрнуто, принимает ядро. Оболочка не имеет права отбирать «только
/// новое» — событие, пришедшее задним числом до границы снимка, при
/// таком отборе исчезло бы из расчёта молча.
///
/// Любой отказ `advance` — законный повод пересчитать журнал целиком:
/// снимок является кэшем, и его непригодность не является ошибкой
/// работы. Нарушение инварианта при этом никуда не денется — оно
/// проявится и при полном пересчёте.
async fn build_projection(
    services: &AppServices,
    owner: iaam_core::ids::OwnerId,
    query: &ReturnsQuery,
    definition: &ContourDefinition,
    events: &[iaam_core::event::Event],
    context: &ProjectionContext<'_>,
) -> Result<Projection, AppError> {
    let snapshot = services
        .store
        .load_snapshot(owner, definition.id(), definition.version(), query.lot_rule)
        .await?;

    if let Some(snapshot) = snapshot {
        match advance(&snapshot, events, context) {
            Ok(projection) => return Ok(projection),
            Err(error) if error.is_invariant_violation() => {
                // Нарушение инварианта — не повод пересчитывать: полный
                // пересчёт даст то же самое. Отдаём его наверх, чтобы
                // оно попало в лог с идентификатором корреляции (§15.2).
                return Err(AppError::from_projection(error));
            }
            Err(error) => tracing::info!(
                contour = %definition.id().0,
                reason = error.code(),
                "снимок непригоден, пересчитываем журнал целиком"
            ),
        }
    }

    project(events, context).map_err(AppError::from_projection)
}
```

- [ ] **Шаг 6: Корень крейты**

```rust
//! Сценарии и порты (§3.1, §3.2).
//!
//! Оболочка собирает срез, зовёт ядро и сохраняет результат. Арифметики
//! над деньгами здесь нет и быть не может: любое число в ответе API
//! приходит из `iaam-core`.

pub mod adapters;
/// Типы приёмки, доступные транспорту.
///
/// `iaam-server` не зависит от `iaam-ingest` напрямую — это запрещено
/// заслоном архитектуры (§3.2). Приложение переэкспортирует ровно то,
/// что нужно транспорту для преобразования DTO в доменные типы.
pub use iaam_ingest as ingest;

pub mod error;
pub mod ports;
pub mod scenarios;

use std::sync::Arc;

use ports::{Clock, Store};

/// Собранные зависимости. Точка сборки создаёт один экземпляр,
/// обработчики получают `Arc<AppServices>` (§3.2).
pub struct AppServices {
    pub store: Arc<dyn Store>,
    pub clock: Arc<dyn Clock>,
}

impl AppServices {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }
}
```

- [ ] **Шаг 7: Зелёная сборка и коммит**

```bash
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c cargo test --workspace
git add crates/iaam-app Cargo.toml
git commit -m "feat(app): порты, адаптер SQLite и сценарии этапа 1 (iaam-1fk)"
```

Тестов у этой задачи нет намеренно: сценарии проверяются контрактными тестами задачи 17 через поднятый сервер. Модульный тест сценария с поддельным портом проверял бы поддельный порт.

---

## Задача 15: `iaam-server` — DTO, маршруты и OpenAPI

**Files:**
- Create: `crates/iaam-server/Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/dto.rs`, `src/routes.rs`, `src/openapi.rs`
- Create: `crates/iaam-server/src/rate_limit.rs`, `src/auth.rs` (задача 16 наполняет их тестами)
- Modify: корневой `Cargo.toml`

**Interfaces:**
- Produces: `iaam_server::{ServerState, build}`, `iaam_server::dto::*`, `iaam_server::error::{ApiError, ApiFailure}`, `iaam_server::openapi::ApiDoc`.

**Acceptance Criteria:**
- API отдаёт готовые отчёты, а не сырые данные.
- Ошибка валидации — `422` с полем, ожидаемым и полученным значением.
- Нарушение инварианта — `500` с идентификатором корреляции и **без** числа.
- Ставка печатается округлённой до восьми знаков: последние знаки двоичной плавающей точки — шум, меняющийся между платформами.
- Спека порождается из типов обработчиков и объявляет схему аутентификации.
- Поле ставки называется `xirr_pre_tax`: оговорка «до налога» встроена в контракт.
- `iaam-server` не зависит от `iaam-store`, `iaam-market` и `iaam-ingest`.

**Почему суммы передаются строками.** JSON-число `0.1` в двоичной плавающей точке не равно одной десятой. Денежная сумма, прошедшая через JSON-число, перестаёт быть фактом — а журнал состоит из фактов.

**Почему даты требуют явного формата.** Проверено исполнением: штатная сериализация `time::Date` не является строкой «ГГГГ-ММ-ДД», и без объявления формата разбор тела падает с «invalid type: string "2025-01-01", expected a `Date`». Формат объявлен один раз макросом `time::serde::format_description!`, а параметр `as_of` разбирается явной функцией с отказом `422`: молчаливое умолчание «сегодня» вместо непонятой даты выдало бы отчёт не на ту дату.

**Почему `ApiFailure` держит тело в `Box`.** `Result<T, ApiFailure>` возвращают все обработчики, и `clippy::result_large_err` справедливо возражает против варианта ошибки размером в полтораста байт на каждом успешном пути.

**Почему `iaam-server` не зависит от `iaam-ingest`.** Заслон запрещает транспорту знать адаптеры (§3.2) — проверено исполнением: заслон отклонил сборку с этой зависимостью. Типы приёмки транспорт получает переэкспортом через `iaam_app::ingest`.

- [ ] **Шаг 1: Манифест**

```toml
[package]
name = "iaam-server"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
# Транспорт знает про приложение и про ядро (доменные типы в DTO
# преобразуются на границе), но НЕ про адаптеры: их собирает
# iaam-bootstrap. Проверяется заслоном архитектуры (§3.2).
iaam-app = { path = "../iaam-app", version = "0.1.0" }
iaam-core = { path = "../iaam-core", version = "0.1.0" }
axum = "0.8"
rust_decimal = { version = "1", default-features = false, features = ["std", "serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.11"
thiserror = "2"
time = { version = "0.3", default-features = false, features = ["std", "macros", "parsing", "formatting", "serde"] }
tokio = { version = "1", features = ["rt", "macros", "net", "signal"] }
tower = "0.5"
tower-http = { version = "0.7", features = ["trace", "limit"] }
tracing = "0.1"
utoipa = { version = "5", features = ["axum_extras", "time", "uuid"] }
utoipa-axum = "0.2"
uuid = { version = "1", features = ["serde", "v4"] }

[dev-dependencies]
tokio = { version = "1", features = ["rt", "macros", "rt-multi-thread"] }
http-body-util = "0.1"
# Снапшоты отчётов (§15.8): ловят непреднамеренное изменение формы ответа,
# которое поштучные проверки полей пропускают.
insta = { version = "1", features = ["json", "redactions"] }
iaam-store = { path = "../iaam-store", version = "0.1.0" }
tower = "0.5"

[lints]
workspace = true
```

- [ ] **Шаг 2: Ответы об ошибках**

```rust
//! Ответы об ошибках.
//!
//! Ошибка валидации — `422` с указанием поля, ожидаемого и полученного
//! значения (§13). Нарушение инварианта наружу уходит как `500`
//! с идентификатором корреляции и **без** числа: выдать результат
//! после доказанного нарушения тождества нельзя (§15.2).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use iaam_app::error::AppError;
use serde::Serialize;
use utoipa::ToSchema;

/// Тело ответа об ошибке.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiError {
    /// Машиночитаемый код. Агент разбирает его, а не текст.
    pub code: String,
    /// Пояснение для человека.
    pub message: String,
    /// Поле запроса, вызвавшее отказ.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// Идентификатор корреляции: по нему нарушение инварианта ищется
    /// в логах. Наружу не уходит ничего, кроме него.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl ApiError {
    #[must_use]
    pub fn simple(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            field: None,
            expected: None,
            actual: None,
            correlation_id: None,
        }
    }
}

/// Ошибка обработчика.
///
/// Тело в `Box`: `Result<T, ApiFailure>` возвращают все обработчики,
/// и `clippy::result_large_err` справедливо возражает против варианта
/// ошибки размером в полтораста байт на каждом успешном пути.
#[derive(Debug)]
pub struct ApiFailure {
    pub status: StatusCode,
    pub body: Box<ApiError>,
}

impl ApiFailure {
    #[must_use]
    pub fn new(status: StatusCode, body: ApiError) -> Self {
        Self {
            status,
            body: Box::new(body),
        }
    }

    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ApiError::simple("unauthorized", "требуется действующий токен"),
        )
    }

    #[must_use]
    pub fn forbidden(scope: &str) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ApiError::simple(
                "forbidden",
                format!("права токена ({scope}) не позволяют эту операцию"),
            ),
        )
    }

    #[must_use]
    pub fn too_many_requests() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ApiError::simple("rate_limited", "слишком много запросов"),
        )
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        (self.status, Json(*self.body)).into_response()
    }
}

impl From<AppError> for ApiFailure {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Invalid {
                ref field,
                ref expected,
                ref actual,
            } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError {
                    code: error.code().to_owned(),
                    message: error.to_string(),
                    field: Some(field.clone()),
                    expected: Some(expected.clone()),
                    actual: Some(actual.clone()),
                    correlation_id: None,
                },
            ),
            AppError::NotFound { what, ref id } => Self::new(
                StatusCode::NOT_FOUND,
                ApiError::simple("not_found", format!("не найдено: {what} {id}")),
            ),
            AppError::Invariant { correlation, .. } => {
                // Подробности остаются в логе: наружу уходит только код
                // и идентификатор корреляции.
                tracing::error!(%correlation, error = %error, "нарушен инвариант проекции");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError {
                        code: "invariant_violated".into(),
                        message: "результат не может быть выдан: нарушен внутренний инвариант"
                            .into(),
                        field: None,
                        expected: None,
                        actual: None,
                        correlation_id: Some(correlation.to_string()),
                    },
                )
            }
            AppError::Store(_) | AppError::Projection(_) => {
                tracing::error!(error = %error, "сценарий не выполнен");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError::simple(error.code(), error.to_string()),
                )
            }
        }
    }
}
```

- [ ] **Шаг 3: DTO**

```rust
//! Транспортные представления (§3.2).
//!
//! DTO живут здесь и никогда не переезжают в общий крейт: общий крейт
//! типов быстро превращается в свалку, и формально независимое ядро
//! оказывается зависимым от слоя, который знает обо всём.
//!
//! **Суммы передаются десятичными строками**, а не числами с плавающей
//! точкой: JSON-число `0.1` в двоичной плавающей точке не равно одной
//! десятой, и денежная сумма, прошедшая через него, перестаёт быть фактом.

use iaam_app::ingest::operation::{OperationDates, OperationKind, SubmittedOperation};
use iaam_app::ingest::{Rejection, Verdict};
use iaam_core::event::kind::FeeOrigin;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_core::returns::{Computed, DataQuality, MaterialIssue, NotComputable, ReturnsReport};
use iaam_core::valuation::PriceQuality;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use utoipa::ToSchema;
use uuid::Uuid;

// Собственный формат дат: штатная сериализация `time::Date` не является
// строкой «ГГГГ-ММ-ДД», и без этой строки API принимал бы даты
// в непредсказуемом виде. Проверено исполнением: без неё разбор тела
// падает с «invalid type: string "2025-01-01", expected a `Date`».
time::serde::format_description!(iso_date, Date, "[year]-[month]-[day]");

/// Код валюты в транспорте. Отдельный тип, потому что `CurrencyCode`
/// ядра не знает про OpenAPI и знать не должен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum CurrencyDto {
    Rub,
    Usd,
    Eur,
    Cny,
    Xau,
}

impl CurrencyDto {
    #[must_use]
    pub const fn to_domain(self) -> CurrencyCode {
        match self {
            Self::Rub => CurrencyCode::Rub,
            Self::Usd => CurrencyCode::Usd,
            Self::Eur => CurrencyCode::Eur,
            Self::Cny => CurrencyCode::Cny,
            Self::Xau => CurrencyCode::Xau,
        }
    }

    #[must_use]
    pub const fn from_domain(currency: CurrencyCode) -> Self {
        match currency {
            CurrencyCode::Rub => Self::Rub,
            CurrencyCode::Usd => Self::Usd,
            CurrencyCode::Eur => Self::Eur,
            CurrencyCode::Cny => Self::Cny,
            CurrencyCode::Xau => Self::Xau,
        }
    }
}

/// Качество цены в транспорте.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceQualityDto {
    Executable,
    PreviousClose,
    CarriedForward,
    Stale,
    OwnerEstimate,
}

impl PriceQualityDto {
    #[must_use]
    pub const fn to_domain(self) -> PriceQuality {
        match self {
            Self::Executable => PriceQuality::Executable,
            Self::PreviousClose => PriceQuality::PreviousClose,
            Self::CarriedForward => PriceQuality::CarriedForward,
            Self::Stale => PriceQuality::Stale,
            Self::OwnerEstimate => PriceQuality::OwnerEstimate,
        }
    }
}

/// Происхождение комиссии в транспорте.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeeOriginDto {
    Brokerage,
    Depositary,
    AccountMaintenance,
    MarginInterest,
    Other,
}

impl FeeOriginDto {
    #[must_use]
    pub const fn to_domain(self) -> FeeOrigin {
        match self {
            Self::Brokerage => FeeOrigin::Brokerage,
            Self::Depositary => FeeOrigin::Depositary,
            Self::AccountMaintenance => FeeOrigin::AccountMaintenance,
            Self::MarginInterest => FeeOrigin::MarginInterest,
            Self::Other => FeeOrigin::Other,
        }
    }
}

/// Даты операции.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct OperationDatesDto {
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date, example = "2026-01-15")]
    pub trade: Option<Date>,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub settled: Option<Date>,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub cash_posted: Option<Date>,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub paid: Option<Date>,
}

/// Вид операции. Величины **положительные**: знак задаёт вид, а не клиент.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationKindDto {
    Deposit {
        amount: String,
        currency: CurrencyDto,
    },
    Withdrawal {
        amount: String,
        currency: CurrencyDto,
    },
    Transfer {
        to_account: Uuid,
        amount: String,
        currency: CurrencyDto,
    },
    Buy {
        instrument: Uuid,
        custody: Uuid,
        quantity: String,
        amount: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fee: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accrued_interest: Option<String>,
        currency: CurrencyDto,
    },
    Sell {
        instrument: Uuid,
        custody: Uuid,
        quantity: String,
        amount: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fee: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accrued_interest: Option<String>,
        currency: CurrencyDto,
    },
    Income {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instrument: Option<Uuid>,
        amount: String,
        currency: CurrencyDto,
    },
    Fee {
        amount: String,
        currency: CurrencyDto,
        origin: FeeOriginDto,
    },
    OpeningCash {
        amount: String,
        currency: CurrencyDto,
    },
    OpeningPosition {
        instrument: Uuid,
        custody: Uuid,
        quantity: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_basis: Option<String>,
        currency: CurrencyDto,
    },
    Valuation {
        instrument: Uuid,
        price: String,
        currency: CurrencyDto,
        quality: PriceQualityDto,
    },
}

/// Операция целиком.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OperationDto {
    pub account: Uuid,
    #[serde(flatten)]
    pub kind: OperationKindDto,
    #[serde(default)]
    pub dates: OperationDatesDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_operation_id: Option<String>,
}

fn decimal(value: &str, field: &str) -> Result<Decimal, Rejection> {
    value.parse::<Decimal>().map_err(|_| Rejection {
        field: field.to_owned(),
        expected: "десятичное число в виде строки".into(),
        actual: value.to_owned(),
    })
}

fn minor(value: &str, currency: CurrencyDto, field: &str) -> Result<i64, Rejection> {
    iaam_app::ingest::operation::to_minor_units(decimal(value, field)?, currency.to_domain(), field)
}

fn optional_minor(
    value: Option<&String>,
    currency: CurrencyDto,
    field: &str,
) -> Result<Option<i64>, Rejection> {
    match value {
        None => Ok(None),
        Some(raw) => minor(raw, currency, field).map(Some),
    }
}

impl OperationDto {
    /// Преобразование в доменную операцию.
    ///
    /// Единственное место, где транспорт встречается с доменом. Отказ
    /// возвращается с полем, ожидаемым и полученным — это тело ответа
    /// `422` (§13).
    pub fn to_domain(&self) -> Result<SubmittedOperation, Rejection> {
        let kind = self.kind_to_domain()?;
        Ok(SubmittedOperation {
            account: AccountId(self.account),
            kind,
            dates: OperationDates {
                trade: self.dates.trade,
                settled: self.dates.settled,
                cash_posted: self.dates.cash_posted,
                paid: self.dates.paid,
            },
            idempotency_key: self.idempotency_key.clone(),
            source_operation_id: self.source_operation_id.clone(),
        })
    }

    fn kind_to_domain(&self) -> Result<OperationKind, Rejection> {
        Ok(match &self.kind {
            OperationKindDto::Deposit { amount, currency } => OperationKind::Deposit {
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Withdrawal { amount, currency } => OperationKind::Withdrawal {
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Transfer {
                to_account,
                amount,
                currency,
            } => OperationKind::Transfer {
                to: AccountId(*to_account),
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Buy {
                instrument,
                custody,
                quantity,
                amount,
                fee,
                accrued_interest,
                currency,
            } => OperationKind::Buy {
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Dec::new(decimal(quantity, "quantity")?),
                gross_minor: minor(amount, *currency, "amount")?,
                fee_minor: optional_minor(fee.as_ref(), *currency, "fee")?,
                accrued_interest_minor: optional_minor(
                    accrued_interest.as_ref(),
                    *currency,
                    "accrued_interest",
                )?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Sell {
                instrument,
                custody,
                quantity,
                amount,
                fee,
                accrued_interest,
                currency,
            } => OperationKind::Sell {
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Dec::new(decimal(quantity, "quantity")?),
                gross_minor: minor(amount, *currency, "amount")?,
                fee_minor: optional_minor(fee.as_ref(), *currency, "fee")?,
                accrued_interest_minor: optional_minor(
                    accrued_interest.as_ref(),
                    *currency,
                    "accrued_interest",
                )?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Income {
                instrument,
                amount,
                currency,
            } => OperationKind::Income {
                instrument: instrument.map(InstrumentId),
                gross_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Fee {
                amount,
                currency,
                origin,
            } => OperationKind::Fee {
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
                origin: origin.to_domain(),
            },
            OperationKindDto::OpeningCash { amount, currency } => OperationKind::OpeningCash {
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::OpeningPosition {
                instrument,
                custody,
                quantity,
                cost_basis,
                currency,
            } => OperationKind::OpeningPosition {
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Dec::new(decimal(quantity, "quantity")?),
                cost_basis_minor: optional_minor(cost_basis.as_ref(), *currency, "cost_basis")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Valuation {
                instrument,
                price,
                currency,
                quality,
            } => OperationKind::Valuation {
                instrument: InstrumentId(*instrument),
                price: Dec::new(decimal(price, "price")?),
                currency: currency.to_domain(),
                quality: quality.to_domain(),
            },
        })
    }
}

/// Запрос приёмки.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubmitOperationsRequest {
    /// Метка источника: ручной ввод, конкретный агент, конкретный файл.
    pub source_label: String,
    pub operations: Vec<OperationDto>,
}

/// Вердикт по одной операции.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VerdictDto {
    /// Номер операции во входной пачке, с единицы.
    pub row: usize,
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl VerdictDto {
    #[must_use]
    pub fn from_domain(row: usize, verdict: &Verdict) -> Self {
        let base = Self {
            row,
            verdict: verdict.code().to_owned(),
            event_id: None,
            field: None,
            expected: None,
            actual: None,
            detail: None,
        };
        match verdict {
            Verdict::Provisional { event } => Self {
                event_id: Some(event.inner()),
                ..base
            },
            Verdict::Duplicate { existing } => Self {
                event_id: Some(existing.inner()),
                ..base
            },
            Verdict::NeedsClassification { question } => Self {
                detail: Some(question.clone()),
                ..base
            },
            Verdict::Unsupported { reason } => Self {
                detail: Some(reason.clone()),
                ..base
            },
            Verdict::Rejected { rejection } => Self {
                field: Some(rejection.field.clone()),
                expected: Some(rejection.expected.clone()),
                actual: Some(rejection.actual.clone()),
                ..base
            },
        }
    }
}

/// Величина, которую система могла отказаться вычислить.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ComputedDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_computable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ComputedDto {
    fn from_dec(value: &Computed<Dec>) -> Self {
        match value {
            Computed::Value(amount) => Self {
                value: Some(amount.inner().to_string()),
                not_computable: None,
                detail: None,
            },
            Computed::NotComputable { reason } => Self {
                value: None,
                not_computable: Some(reason.code().to_owned()),
                detail: Some(describe(reason)),
            },
        }
    }
}

fn describe(reason: &NotComputable) -> String {
    match reason {
        NotComputable::MissingPrice { instrument } => {
            format!("нет цены инструмента {}", instrument.inner())
        }
        NotComputable::MissingFxRate { from, to, date } => {
            format!("нет курса {}→{} на {date}", from.code(), to.code())
        }
        NotComputable::SolverRefused { refusal } => refusal.to_string(),
        NotComputable::NoExternalFlows => "нет потоков, пересекающих границу контура".into(),
        NotComputable::StateNewerThanReport { last_event, as_of } => {
            format!("срез содержит события до {last_event}, отчёт на {as_of}")
        }
        NotComputable::Numeric { code } => format!("арифметический отказ: {code}"),
    }
}

/// Печать приближённой величины.
///
/// Печатать `f64` как есть нельзя: последние знаки двоичной плавающей
/// точки — шум, а не результат, и они меняются между платформами.
/// Восемь знаков — на четыре порядка точнее допуска решателя (1e-9
/// по невязке NPV) и ровно настолько, насколько ставка вообще имеет
/// смысл: 0,00000001 — это одна миллионная процента годовых.
fn format_rate(value: f64) -> String {
    let scaled = (value * 1e8).round();
    // −0 и 0 — одно и то же число, но печатаются по-разному.
    let normalized = if scaled == 0.0 { 0.0 } else { scaled / 1e8 };
    format!("{normalized:.8}")
}

/// Ставка доходности вместе с политикой решателя.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RateDto {
    /// Ставка в долях единицы. Приближённая величина: в денежные
    /// тождества не входит (§6.6).
    pub value: String,
    pub error_bound: String,
    pub iterations: u32,
    pub day_count: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_computable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Блок качества данных (§10.5).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DataQualityDto {
    pub status: String,
    pub unconfirmed_share: String,
    pub material_issues: Vec<String>,
}

impl DataQualityDto {
    fn from_domain(quality: &DataQuality) -> Self {
        Self {
            status: quality.status.code().to_owned(),
            unconfirmed_share: quality.unconfirmed_share.inner().to_string(),
            material_issues: quality.material_issues.iter().map(issue).collect(),
        }
    }
}

fn issue(value: &MaterialIssue) -> String {
    match value {
        MaterialIssue::RestoredWithoutBasis { account } => format!(
            "счёт {} восстановлен без документированной стоимости",
            account.inner()
        ),
        MaterialIssue::PriceNotExecutable {
            instrument,
            quality,
        } => format!(
            "цена инструмента {} не исполнима: {}",
            instrument.inner(),
            quality.code()
        ),
        MaterialIssue::NegativeCash { account, currency } => format!(
            "отрицательный остаток на счёте {} в {}",
            account.inner(),
            currency.code()
        ),
        MaterialIssue::HistoryStartsAt { date } => format!("история начинается {date}"),
    }
}

/// Отчёт о доходности.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReturnsReportDto {
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub as_of: Date,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub history_starts: Option<Date>,
    pub report_currency: CurrencyDto,
    pub contributed: ComputedDto,
    pub withdrawn: ComputedDto,
    pub terminal_value: ComputedDto,
    /// **Доходность до налога.** Имя поля содержит оговорку намеренно:
    /// налоги появляются в E5, и до тех пор называть эту величину
    /// «доходностью» без уточнения нельзя (§16.3).
    pub xirr_pre_tax: RateDto,
    pub applied_rules: AppliedRulesDto,
    pub data_quality: DataQualityDto,
}

/// Применённые правила (§3.2, §6.1).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppliedRulesDto {
    pub contour: Uuid,
    pub contour_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lot_rule: Option<String>,
    pub fx_source: String,
    pub day_count: String,
    /// Допустимая ширина интервала по ставке — она же определяет
    /// объявленную погрешность результата.
    pub solver_rate_tolerance: String,
    pub solver_max_iterations: u32,
}

impl ReturnsReportDto {
    #[must_use]
    pub fn from_domain(report: &ReturnsReport) -> Self {
        let rate = match &report.xirr {
            Computed::Value(outcome) => RateDto {
                value: format_rate(outcome.rate().value()),
                error_bound: format_rate(outcome.rate().error_bound()),
                iterations: outcome.rate().iterations(),
                day_count: outcome.day_count().code().to_owned(),
                not_computable: None,
                detail: None,
            },
            Computed::NotComputable { reason } => RateDto {
                value: String::new(),
                error_bound: String::new(),
                iterations: 0,
                day_count: report.applied_rules.day_count.code().to_owned(),
                not_computable: Some(reason.code().to_owned()),
                detail: Some(describe(reason)),
            },
        };
        Self {
            as_of: report.as_of,
            history_starts: report.history_starts,
            report_currency: CurrencyDto::from_domain(report.report_currency),
            contributed: ComputedDto::from_dec(&report.contributed),
            withdrawn: ComputedDto::from_dec(&report.withdrawn),
            terminal_value: ComputedDto::from_dec(&report.terminal_value),
            xirr_pre_tax: rate,
            applied_rules: AppliedRulesDto {
                contour: report.applied_rules.contour.0,
                contour_version: report.applied_rules.contour_version.0,
                lot_rule: report
                    .applied_rules
                    .lot_rule
                    .as_ref()
                    .map(|id| id.0.clone()),
                fx_source: report.applied_rules.fx_source.code().to_owned(),
                day_count: report.applied_rules.day_count.code().to_owned(),
                solver_rate_tolerance: report
                    .applied_rules
                    .solver_policy
                    .rate_tolerance
                    .to_string(),
                solver_max_iterations: report.applied_rules.solver_policy.max_iterations,
            },
            data_quality: DataQualityDto::from_domain(&report.data_quality),
        }
    }
}

/// Счёт.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountDto {
    pub id: Uuid,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
}

/// Создание счёта.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateAccountRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
}

/// Новая версия состава контура.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateContourVersionRequest {
    /// Идентификатор контура. Отсутствует — заводится новый.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contour: Option<Uuid>,
    pub title: String,
    pub accounts: Vec<Uuid>,
}

/// Ответ о версии контура.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContourVersionDto {
    pub contour: Uuid,
    pub version: u32,
    pub accounts: Vec<Uuid>,
}

/// Курс валюты на дату, названный владельцем (§6.1).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FxRateDto {
    pub from: CurrencyDto,
    pub to: CurrencyDto,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub date: Date,
    pub rate: String,
}

/// Состояние сервиса.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthDto {
    pub status: String,
    pub schema_version: u32,
    pub projection_version: u32,
}
```

- [ ] **Шаг 4: Маршруты**

```rust
//! Маршруты.
//!
//! Обработчик делает три вещи: разбирает DTO, зовёт сценарий, сериализует
//! результат. Ни одной арифметической операции над деньгами здесь нет —
//! это проверяется заслоном архитектуры (§3.1, §13).

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use iaam_app::AppServices;
use iaam_app::ingest::csv_source::{Directory, ParsedRow, parse};
use iaam_app::ingest::{SubmittedOperation, Verdict};
use iaam_app::ports::{AccountView, Principal};
use iaam_app::scenarios::ingest::submit_operations;
use iaam_app::scenarios::reports::{ReturnsQuery, returns};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, SourceId};
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::PROJECTION_VERSION;
use iaam_core::rules::LotRuleVersion;
use iaam_core::valuation::{FxSource, FxTable};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::Date;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::ServerState;
use crate::dto::{
    AccountDto, ContourVersionDto, CreateAccountRequest, CreateContourVersionRequest, CurrencyDto,
    FxRateDto, HealthDto, ReturnsReportDto, SubmitOperationsRequest, VerdictDto,
};
use crate::error::{ApiError, ApiFailure};

/// Состояние сервиса.
#[utoipa::path(
    get,
    path = "/v1/health",
    responses((status = 200, description = "Сервис отвечает", body = HealthDto))
)]
pub async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        status: "ok".into(),
        schema_version: iaam_core::event::SCHEMA_VERSION,
        projection_version: PROJECTION_VERSION,
    })
}

/// Список счетов.
#[utoipa::path(
    get,
    path = "/v1/accounts",
    responses((status = 200, description = "Счета владельца", body = Vec<AccountDto>)),
    security(("bearer" = []))
)]
pub async fn list_accounts(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<AccountDto>>, ApiFailure> {
    let accounts = state.services.store.list_accounts(principal.owner).await?;
    Ok(Json(
        accounts
            .into_iter()
            .map(|account| AccountDto {
                id: account.id.inner(),
                title: account.title,
                institution: account.institution,
            })
            .collect(),
    ))
}

/// Создание счёта.
#[utoipa::path(
    post,
    path = "/v1/accounts",
    request_body = CreateAccountRequest,
    responses(
        (status = 201, description = "Счёт создан", body = AccountDto),
        (status = 403, description = "Недостаточно прав", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_account(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<CreateAccountRequest>,
) -> Result<(StatusCode, Json<AccountDto>), ApiFailure> {
    require_admin(&principal)?;
    let account = AccountView {
        id: AccountId::new_random(),
        title: request.title,
        institution: request.institution,
    };
    state
        .services
        .store
        .upsert_account(principal.owner, account.clone())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(AccountDto {
            id: account.id.inner(),
            title: account.title,
            institution: account.institution,
        }),
    ))
}

/// Новая версия состава контура.
#[utoipa::path(
    post,
    path = "/v1/contours",
    request_body = CreateContourVersionRequest,
    responses(
        (status = 201, description = "Версия создана", body = ContourVersionDto),
        (status = 403, description = "Недостаточно прав", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_contour_version(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<CreateContourVersionRequest>,
) -> Result<(StatusCode, Json<ContourVersionDto>), ApiFailure> {
    require_admin(&principal)?;
    if request.accounts.is_empty() {
        return Err(ApiFailure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError {
                code: "invalid_request".into(),
                message: "контур без счетов не имеет границы".into(),
                field: Some("accounts".into()),
                expected: Some("хотя бы один счёт".into()),
                actual: Some("пустой список".into()),
                correlation_id: None,
            },
        ));
    }
    let contour = ContourId(request.contour.unwrap_or_else(Uuid::new_v4));
    let previous = state
        .services
        .store
        .latest_contour_version(principal.owner, contour)
        .await?;
    let version = ContourVersion(previous.map_or(1, |value| value.0.saturating_add(1)));
    let accounts: Vec<AccountId> = request.accounts.iter().copied().map(AccountId).collect();
    let definition = ContourDefinition::new(contour, version, accounts.clone());

    state
        .services
        .store
        .insert_contour_version(principal.owner, definition, request.title, accounts.clone())
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ContourVersionDto {
            contour: contour.0,
            version: version.0,
            accounts: accounts.iter().map(|id| id.inner()).collect(),
        }),
    ))
}

/// Приёмка операций.
#[utoipa::path(
    post,
    path = "/v1/ingest/operations",
    request_body = SubmitOperationsRequest,
    responses(
        (status = 200, description = "Вердикт по каждой операции", body = Vec<VerdictDto>),
        (status = 403, description = "Недостаточно прав", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn ingest_operations(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<SubmitOperationsRequest>,
) -> Result<Json<Vec<VerdictDto>>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let source = SourceId::new_random();

    // Разбор DTO даёт вердикт на строку: одна непонятая операция
    // не отменяет остальные (§10.1).
    let mut verdicts: Vec<VerdictDto> = Vec::with_capacity(request.operations.len());
    let mut accepted: Vec<(usize, SubmittedOperation)> = Vec::new();
    for (index, operation) in request.operations.iter().enumerate() {
        match operation.to_domain() {
            Ok(domain) => accepted.push((index + 1, domain)),
            Err(rejection) => verdicts.push(VerdictDto::from_domain(
                index + 1,
                &Verdict::Rejected { rejection },
            )),
        }
    }

    let domain: Vec<SubmittedOperation> = accepted
        .iter()
        .map(|(_, operation)| operation.clone())
        .collect();
    let outcomes = submit_operations(&state.services, &principal, source, &domain).await?;
    for ((row, _), verdict) in accepted.iter().zip(outcomes.iter()) {
        verdicts.push(VerdictDto::from_domain(*row, verdict));
    }
    verdicts.sort_by_key(|verdict| verdict.row);
    Ok(Json(verdicts))
}

/// Приёмка CSV.
#[utoipa::path(
    post,
    path = "/v1/ingest/csv",
    request_body(content = String, description = "Документ CSV", content_type = "text/csv"),
    responses(
        (status = 200, description = "Вердикт по каждой строке", body = Vec<VerdictDto>),
        (status = 403, description = "Недостаточно прав", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn ingest_csv(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    body: String,
) -> Result<Json<Vec<VerdictDto>>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let directory = build_directory(&state.services, &principal).await?;
    let rows = parse(&body, &directory);

    let mut verdicts = Vec::with_capacity(rows.len());
    let mut accepted: Vec<(usize, SubmittedOperation)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match row {
            ParsedRow::Operation(operation) => {
                accepted.push((index + 1, (**operation).clone()));
            }
            ParsedRow::Rejected(rejection) => verdicts.push(VerdictDto::from_domain(
                index + 1,
                &Verdict::Rejected {
                    rejection: rejection.clone(),
                },
            )),
        }
    }

    let source = SourceId::new_random();
    let domain: Vec<SubmittedOperation> = accepted
        .iter()
        .map(|(_, operation)| operation.clone())
        .collect();
    let outcomes = submit_operations(&state.services, &principal, source, &domain).await?;
    for ((row, _), verdict) in accepted.iter().zip(outcomes.iter()) {
        verdicts.push(VerdictDto::from_domain(*row, verdict));
    }
    verdicts.sort_by_key(|verdict| verdict.row);
    Ok(Json(verdicts))
}

/// Параметры отчёта о доходности.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ReturnsParams {
    /// Идентификатор контура.
    pub contour: Uuid,
    /// Версия состава контура. По умолчанию — последняя.
    #[serde(default)]
    pub contour_version: Option<u32>,
    /// Дата отчёта в формате ГГГГ-ММ-ДД. По умолчанию — сегодня.
    #[serde(default)]
    #[param(value_type = Option<String>, format = Date, example = "2026-01-01")]
    pub as_of: Option<String>,
    /// Валюта отчёта.
    pub currency: CurrencyDto,
}

/// Отчёт о доходности **до налога**.
#[utoipa::path(
    get,
    path = "/v1/reports/returns",
    params(ReturnsParams),
    responses(
        (status = 200, description = "Отчёт", body = ReturnsReportDto),
        (status = 404, description = "Контур не найден", body = ApiError),
        (status = 500, description = "Нарушен инвариант", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn returns_report(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ReturnsParams>,
) -> Result<Json<ReturnsReportDto>, ApiFailure> {
    let query = ReturnsQuery {
        contour: ContourId(params.contour),
        contour_version: params.contour_version.map(ContourVersion),
        as_of: parse_as_of(params.as_of.as_deref())?,
        report_currency: params.currency.to_domain(),
        // Курсы на этапе 1 называет владелец: рыночные данные — E3.
        // Источник записывается в отчёт, поэтому подмены не происходит.
        fx: FxTable::new(FxSource::OwnerSupplied),
        lot_rule: LotRuleVersion(1),
    };
    let report = returns(&state.services, &principal, &query).await?;
    Ok(Json(ReturnsReportDto::from_domain(&report)))
}

/// Курсы, переданные вместе с запросом отчёта.
///
/// Отдельный обработчик, а не поле запроса `GET`: таблица курсов —
/// это тело, а тело у `GET` бывает, но им никто не пользуется.
#[utoipa::path(
    post,
    path = "/v1/reports/returns",
    params(ReturnsParams),
    request_body = Vec<FxRateDto>,
    responses(
        (status = 200, description = "Отчёт с указанными курсами", body = ReturnsReportDto),
        (status = 422, description = "Некорректный курс", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn returns_report_with_rates(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ReturnsParams>,
    Json(rates): Json<Vec<FxRateDto>>,
) -> Result<Json<ReturnsReportDto>, ApiFailure> {
    let mut fx = FxTable::new(FxSource::OwnerSupplied);
    for rate in &rates {
        let parsed = rate.rate.parse::<Decimal>().map_err(|_| {
            ApiFailure::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError {
                    code: "invalid_request".into(),
                    message: "курс должен быть десятичным числом".into(),
                    field: Some("rate".into()),
                    expected: Some("десятичное число в виде строки".into()),
                    actual: Some(rate.rate.clone()),
                    correlation_id: None,
                },
            )
        })?;
        fx = fx.with_rate(
            rate.from.to_domain(),
            rate.to.to_domain(),
            rate.date,
            Dec::new(parsed),
        );
    }

    let query = ReturnsQuery {
        contour: ContourId(params.contour),
        contour_version: params.contour_version.map(ContourVersion),
        as_of: parse_as_of(params.as_of.as_deref())?,
        report_currency: params.currency.to_domain(),
        fx,
        lot_rule: LotRuleVersion(1),
    };
    let report = returns(&state.services, &principal, &query).await?;
    Ok(Json(ReturnsReportDto::from_domain(&report)))
}

/// Разбор даты отчёта.
///
/// Отдельная функция с явным отказом `422`: `serde` для `time::Date`
/// не принимает строку «ГГГГ-ММ-ДД» без указания формата, и молчаливое
/// умолчание «сегодня» вместо непонятой даты выдало бы отчёт не на ту дату.
fn parse_as_of(value: Option<&str>) -> Result<Option<Date>, ApiFailure> {
    let Some(raw) = value else {
        return Ok(None);
    };
    Date::parse(
        raw,
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map(Some)
    .map_err(|_| {
        ApiFailure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError {
                code: "invalid_request".into(),
                message: "дата отчёта должна быть в формате ГГГГ-ММ-ДД".into(),
                field: Some("as_of".into()),
                expected: Some("ГГГГ-ММ-ДД".into()),
                actual: Some(raw.to_owned()),
                correlation_id: None,
            },
        )
    })
}

fn require_admin(principal: &Principal) -> Result<(), ApiFailure> {
    if principal.scope.may_administer() {
        Ok(())
    } else {
        Err(ApiFailure::forbidden(principal.scope.code()))
    }
}

/// Справочник имён для разбора CSV.
///
/// Инструменты и места хранения на этапе 1 приходят из того же
/// справочника счетов: отдельная таблица инструментов заполняется
/// в E3 вместе с рыночными данными, а до тех пор CSV со сделками
/// требует явных идентификаторов через API операций.
async fn build_directory(
    services: &Arc<AppServices>,
    principal: &Principal,
) -> Result<Directory, ApiFailure> {
    let accounts = services.store.list_accounts(principal.owner).await?;
    let mut directory = Directory::default();
    for account in accounts {
        directory.accounts.insert(account.title, account.id);
    }
    Ok(directory)
}
```

- [ ] **Шаг 5: Спека**

```rust
//! Спека OpenAPI, порождённая из типов обработчиков (§17.1).
//!
//! Порождение устраняет расхождение **схемы данных**, но не поведения:
//! коды ответов в рантайме, требования аутентификации и фактическая
//! сериализация собственных типов остаются вне генерации. Поэтому
//! существуют чёрноящичные контрактные тесты (задача 15).

use utoipa::Modify;
use utoipa::OpenApi;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};

use crate::dto::{
    AccountDto, ComputedDto, ContourVersionDto, CreateAccountRequest, CreateContourVersionRequest,
    CurrencyDto, DataQualityDto, FeeOriginDto, FxRateDto, HealthDto, OperationDatesDto,
    OperationDto, OperationKindDto, PriceQualityDto, RateDto, ReturnsReportDto,
    SubmitOperationsRequest, VerdictDto,
};
use crate::error::ApiError;

/// Схема аутентификации. Объявляется отдельно: `utoipa` порождает её
/// из типов, а требование `Bearer` типом не выражается.
pub struct BearerSecurity;

impl Modify for BearerSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("opaque")
                        .description(Some(
                            "Агентский токен. Выдаётся владельцем, отзывается им же (§14).",
                        ))
                        .build(),
                ),
            );
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "IAAM",
        version = "1.0.0",
        description = "Учёт инвестиций. Этап 1: денежные потоки и XIRR до налога."
    ),
    modifiers(&BearerSecurity),
    components(schemas(
        AccountDto,
        ApiError,
        ComputedDto,
        ContourVersionDto,
        CreateAccountRequest,
        CreateContourVersionRequest,
        CurrencyDto,
        DataQualityDto,
        FeeOriginDto,
        FxRateDto,
        HealthDto,
        OperationDatesDto,
        OperationDto,
        OperationKindDto,
        PriceQualityDto,
        RateDto,
        ReturnsReportDto,
        SubmitOperationsRequest,
        VerdictDto,
    ))
)]
pub struct ApiDoc;
```

- [ ] **Шаг 6: Сборка приложения**

```rust
//! REST-транспорт (§13).
//!
//! API отдаёт **готовые отчёты**, а не сырые данные: числа считает ядро,
//! транспорт их сериализует. Агенту запрещена собственная арифметика,
//! и требование проверяемо — число в ответе агента, отсутствующее
//! в ответах API, является ошибкой.

pub mod auth;
pub mod dto;
pub mod error;
pub mod openapi;
pub mod rate_limit;
pub mod routes;

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router, middleware};
use iaam_app::AppServices;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::openapi::ApiDoc;
use crate::rate_limit::RateLimiter;

/// Состояние сервера.
#[derive(Clone)]
pub struct ServerState {
    pub services: Arc<AppServices>,
    pub limiter: Arc<RateLimiter>,
}

impl ServerState {
    #[must_use]
    pub fn new(services: Arc<AppServices>, limiter: Arc<RateLimiter>) -> Self {
        Self { services, limiter }
    }
}

/// Сборка приложения axum вместе с порождённой спекой.
///
/// Публичным остаётся только `/v1/health` и сама спека: аутентификация
/// с первого дня, и отложенной она не станет никогда (§14).
pub fn build(state: ServerState) -> (Router, utoipa::openapi::OpenApi) {
    let protected = OpenApiRouter::new()
        .routes(routes!(routes::list_accounts, routes::create_account))
        .routes(routes!(routes::create_contour_version))
        .routes(routes!(routes::ingest_operations))
        .routes(routes!(routes::ingest_csv))
        .routes(routes!(
            routes::returns_report,
            routes::returns_report_with_rates
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ));

    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(routes::health))
        .merge(protected)
        .split_for_parts();

    let spec = api.clone();
    let router = router
        .route(
            "/v1/openapi.json",
            get(move || {
                let spec = spec.clone();
                async move { Json(spec) }
            }),
        )
        .with_state(state);
    (router, api)
}
```

- [ ] **Шаг 7: Зелёная сборка**

```bash
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c ./scripts/check-architecture.sh
```

Проверьте отдельно, что заслон ловит запрещённую зависимость: добавьте `iaam-ingest` в зависимости `iaam-server`, запустите заслон — он обязан отказать словами «iaam-server зависит от адаптеров», — и уберите зависимость обратно.

- [ ] **Шаг 8: Коммит**

```bash
git add crates/iaam-server Cargo.toml
git commit -m "feat(server): REST-контракт этапа 1 с порождённой спекой (iaam-1fk)"
```

---

## Задача 16: `iaam-server` — аутентификация и ограничение частоты

**Files:**
- Create (наполнение заглушек задачи 15): `crates/iaam-server/src/auth.rs`, `src/rate_limit.rs`

**Interfaces:**
- Produces: `iaam_server::auth::{hash_token, bearer, authenticate, principal}`, `iaam_server::rate_limit::{RateLimiter}`.

**Acceptance Criteria:**
- Ни один защищённый маршрут не отвечает без действующего токена.
- В базе лежит хеш; сам токен нигде не сохраняется и в лог не попадает.
- Никакого «постоянного по времени сравнения»: поиск идёт запросом к базе, и функция, не участвующая в аутентификации, обещала бы защиту, которой нет.
- Журнал использования токена пишется на каждый запрос, включая отклонённый.
- Превышение частоты даёт `429`, а не молчаливую задержку.
- Число ключей ограничителя ограничено, протухшие окна освобождают место: поток случайных токенов не растит память.
- Неизвестный токен **не пишется** в журнал использования: журнал ведётся по токену, а токена в этом случае нет.

**Почему SHA-256, а не argon2.** Токен — это 256 случайных бит из системного источника, а не пароль: перебирать его нечем. Медленный пароль-хеш на каждом запросе стоит дороже, чем даёт. Для паролей владельца (если они когда-нибудь появятся) вывод обратный.

**Почему нет сравнения за постоянное время.** Оно было — отдельной функцией на `subtle`, которую вызывал только тест: аутентификация сравнивала хеши запросом `WHERE token_hash = ?`, то есть силами SQLite. Ревью справедливо назвало это фиктивной мерой. Функция и зависимость удалены, а обоснование записано в коде: подбирать по времени пришлось бы образ SHA-256 от 256-битного случайного значения, а не сам токен.

**Почему ограничитель написан на месте.** Правило простое — фиксированное окно на токен, — а лишняя зависимость в слое, отвечающем за безопасность, стоит дороже сорока строк кода. Проверка вынесена в `allow_at` с явным моментом времени: тест обязан уметь двигать время, не засыпая на длину окна.

**Что этот ограничитель НЕ делает:** он не защищает от распределённой нагрузки, не делится состоянием между процессами и не заменяет ограничение на уровне реверс-прокси. Это защита от зациклившегося агента, а не от злоумышленника; так и записано в `docs/deployment.md` (задача 20).

- [ ] **Шаг 1: Написать падающие тесты**

В конец `crates/iaam-server/src/rate_limit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_within_the_limit_are_allowed_and_the_next_one_is_not() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("токен", now));
        assert!(limiter.allow_at("токен", now));
        assert!(!limiter.allow_at("токен", now));
    }

    #[test]
    fn a_new_window_resets_the_counter() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("токен", now));
        assert!(!limiter.allow_at("токен", now));
        assert!(limiter.allow_at("токен", now + Duration::from_secs(61)));
    }

    #[test]
    fn different_tokens_do_not_share_a_counter() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("первый", now));
        assert!(limiter.allow_at("второй", now));
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn the_map_does_not_grow_without_bound() {
        // Поток случайных токенов не должен превращаться в рост памяти.
        let limiter = RateLimiter::with_capacity(10, Duration::from_secs(60), 4);
        let now = Instant::now();
        for i in 0..100 {
            let _ = limiter.allow_at(&format!("токен-{i}"), now);
        }
        assert!(limiter.tracked_keys() <= 4);
    }

    #[test]
    fn an_expired_window_frees_its_slot() {
        let limiter = RateLimiter::with_capacity(10, Duration::from_secs(60), 2);
        let now = Instant::now();
        assert!(limiter.allow_at("первый", now));
        assert!(limiter.allow_at("второй", now));
        // Пока окна живы, третий ключ не помещается.
        assert!(!limiter.allow_at("третий", now));
        // После истечения окна место освобождается.
        assert!(limiter.allow_at("третий", now + Duration::from_secs(61)));
    }
}
```

В конец `crates/iaam-server/src/auth.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_stable_and_does_not_contain_the_token() {
        let hash = hash_token("секрет");
        assert_eq!(hash, hash_token("секрет"));
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains("секрет"));
        assert_ne!(hash, hash_token("секрет "));
    }

    #[test]
    fn different_tokens_hash_differently() {
        assert_ne!(hash_token("a"), hash_token("b"));
        assert_ne!(hash_token(""), hash_token(" "));
    }
}
```

- [ ] **Шаг 2: Ограничитель частоты**

```rust
//! Ограничение частоты запросов (§14).
//!
//! Реализовано на месте, а не внешней крейтой: правило простое —
//! фиксированное окно на один токен, — а лишняя зависимость в слое,
//! отвечающем за безопасность, стоит дороже сорока строк кода.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Максимум различных ключей в памяти.
///
/// Ограничитель считает по ключу, а ключом является хеш **предъявленного**
/// токена — то есть чего угодно. Без предела поток случайных токенов
/// растит карту неограниченно: отказ в обслуживании ценой одного curl.
const DEFAULT_CAPACITY: usize = 10_000;

/// Ограничитель с фиксированным окном.
pub struct RateLimiter {
    window: Duration,
    limit: u32,
    capacity: usize,
    counters: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(limit: u32, window: Duration) -> Self {
        Self::with_capacity(limit, window, DEFAULT_CAPACITY)
    }

    #[must_use]
    pub fn with_capacity(limit: u32, window: Duration, capacity: usize) -> Self {
        Self {
            window,
            limit,
            capacity,
            counters: Mutex::new(HashMap::new()),
        }
    }

    /// Разрешён ли запрос. Тело вынесено из конструктора, потому что
    /// именно оно должно проверяться мутационным заслоном.
    #[must_use]
    pub fn allow(&self, key: &str) -> bool {
        self.allow_at(key, Instant::now())
    }

    /// Число ключей под наблюдением. Нужно тесту предела памяти:
    /// утверждение «карта не растёт» иначе непроверяемо.
    #[must_use]
    pub fn tracked_keys(&self) -> usize {
        match self.counters.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Проверка с явным моментом времени: тест обязан уметь двигать
    /// время, не засыпая на длину окна.
    #[must_use]
    pub fn allow_at(&self, key: &str, now: Instant) -> bool {
        let mut counters = match self.counters.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Протухшие окна удаляются только при подходе к пределу: чистка
        // на каждом запросе стоила бы обхода всей карты ради ничего.
        if counters.len() >= self.capacity {
            counters.retain(|_, (started, _)| now.duration_since(*started) < self.window);
        }
        // Карта заполнена действующими окнами — незнакомый ключ
        // не принимается. Это отказ для нового токена, а не для всех:
        // уже известные ключи продолжают обслуживаться. Выбор осознанный,
        // неограниченный рост памяти отказал бы вообще всем (§14).
        if counters.len() >= self.capacity && !counters.contains_key(key) {
            return false;
        }
        let entry = counters.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}
```

- [ ] **Шаг 3: Аутентификация**

```rust
//! Аутентификация (§14).
//!
//! Аутентификация с первого дня: отложенная не добавляется никогда.
//! В базе лежит **хеш** токена; сравнение — за постоянное время, чтобы
//! время ответа не выдавало правильный префикс.

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use iaam_app::ports::Principal;
use sha2::{Digest, Sha256};

use crate::ServerState;
use crate::error::ApiFailure;

/// Хеш токена.
///
/// SHA-256, а не пароль-хеш: токен — это 256 случайных бит из системного
/// источника, перебирать его нечем, и argon2 на каждом запросе стоит
/// дороже, чем даёт. Для паролей владельца — если они когда-нибудь
/// появятся — вывод обратный.
///
/// **Сравнения за постоянное время здесь нет, и это осознанно.** Поиск
/// идёт запросом `WHERE token_hash = ?`, то есть сравнение выполняет
/// SQLite, и оно не является постоянным по времени. Утечка времени
/// сравнения даёт атакующему возможность подбирать хеш по префиксу —
/// но подбирать нужно образ SHA-256 от 256-битного случайного значения,
/// а не сам токен. Функция «постоянное сравнение», не используемая
/// на пути аутентификации, обещала бы защиту, которой нет: такая
/// функция здесь была и удалена.
#[must_use]
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Извлечение токена из заголовка `Authorization: Bearer …`.
#[must_use]
pub fn bearer(request: &Request) -> Option<String> {
    let value = request.headers().get(AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    if token.is_empty() {
        return None;
    }
    Some(token.to_owned())
}

/// Слой аутентификации и ограничения частоты.
///
/// Журнал использования токена пишется на **каждый** запрос, включая
/// отклонённый: попытки с отозванным токеном — это то, ради чего
/// журнал и нужен (§14).
pub async fn authenticate(
    State(state): State<ServerState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiFailure> {
    let route = request.uri().path().to_owned();
    let Some(token) = bearer(&request) else {
        return Err(ApiFailure::unauthorized());
    };
    let hash = hash_token(&token);

    if !state.limiter.allow(&hash) {
        tracing::warn!(%route, "превышена частота запросов");
        return Err(ApiFailure::too_many_requests());
    }

    let principal = state
        .services
        .store
        .find_principal(hash.clone())
        .await
        .map_err(ApiFailure::from)?;

    let Some(principal) = principal else {
        // Неизвестный токен НЕ пишется в журнал использования: журнал
        // ведётся по токену, а токена здесь нет. Запись на каждую
        // попытку превращала бы поток случайных строк в неограниченный
        // рост базы через единственный незащищённый путь (§14).
        tracing::warn!(%route, "предъявлен неизвестный токен");
        return Err(ApiFailure::unauthorized());
    };

    let _ = state
        .services
        .store
        .record_token_use(hash, route, "accepted".into())
        .await;

    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

/// Извлечение опознанного носителя токена в обработчике.
pub fn principal(request: &Request) -> Result<Principal, ApiFailure> {
    request
        .extensions()
        .get::<Principal>()
        .cloned()
        .ok_or_else(ApiFailure::unauthorized)
}
```

- [ ] **Шаг 4: Зелёная сборка и коммит**

```bash
nix develop -c cargo test -p iaam-server
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/iaam-server
git commit -m "feat(server): аутентификация токенами и ограничение частоты (iaam-1fk)"
```

---

## Задача 17: `iaam-bootstrap` и контрактные тесты

**Files:**
- Create: `crates/iaam-bootstrap/Cargo.toml`, `src/main.rs`, `src/config.rs`
- Create: `crates/iaam-server/tests/contract.rs`
- Modify: корневой `Cargo.toml`, `deny.toml`

**Interfaces:**
- Produces: двоичный файл `iaam`.

**Acceptance Criteria:**
- Точка сборки — единственное место, знающее одновременно про транспорт и про адаптеры.
- Токен владельца выдаётся криптографическим источником случайности и печатается один раз.
- Каждый маршрут, описанный в спеке, существует и отвечает не `404` и не `405`.
- Приёмочный критерий эпика проходит через API целиком.
- `cargo deny check` проходит.

**Почему контрактные тесты нужны при порождённой спеке.** `utoipa` порождает спеку из типов и потому устраняет расхождение схемы данных. Поведение — коды ответов в рантайме, требования аутентификации, ошибки middleware, фактическая сериализация собственных типов — остаётся вне генерации. Для контракта, которым пользуется внешний агент, синтаксически верная, но поведенчески неверная спека означает, что агент будет чиниться по неверной подсказке.

**Почему `SysRng`, а не `rand::rng()`.** Токен — это ключ от чужих денег. Потоковый генератор общего назначения здесь дешевле ровно настолько, насколько дороже обходится его слабость.

- [ ] **Шаг 1: Манифест и конфигурация**

```toml
[package]
name = "iaam-bootstrap"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[[bin]]
name = "iaam"
path = "src/main.rs"

[dependencies]
# Единственная крейта, которой позволено знать про всё: собрать конкретные
# адаптеры где-то нужно, но это не повод давать транспорту знать про SQLite.
iaam-app = { path = "../iaam-app", version = "0.1.0" }
iaam-core = { path = "../iaam-core", version = "0.1.0" }
iaam-server = { path = "../iaam-server", version = "0.1.0" }
iaam-store = { path = "../iaam-store", version = "0.1.0" }
axum = "0.8"
rand = { version = "0.10", features = ["sys_rng"] }
thiserror = "2"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1", features = ["v4"] }

[lints]
workspace = true
```

```rust
//! Конфигурация из окружения.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("переменная {name} задана неверно: {value}")]
    Invalid { name: &'static str, value: String },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database: PathBuf,
    pub listen: SocketAddr,
    pub rate_limit: u32,
    pub rate_window: Duration,
}

impl Config {
    /// Чтение конфигурации.
    ///
    /// Умолчания есть у всего, кроме пути к базе: база в неожиданном
    /// месте — худший вид умолчания.
    pub fn from_env() -> Result<Self, ConfigError> {
        let database = std::env::var("IAAM_DATABASE").map_err(|_| ConfigError::Invalid {
            name: "IAAM_DATABASE",
            value: "не задана".into(),
        })?;
        let listen = std::env::var("IAAM_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into());
        let listen = listen.parse().map_err(|_| ConfigError::Invalid {
            name: "IAAM_LISTEN",
            value: listen.clone(),
        })?;
        let rate_limit = parse_u32("IAAM_RATE_LIMIT", 120)?;
        let rate_window =
            Duration::from_secs(u64::from(parse_u32("IAAM_RATE_WINDOW_SECONDS", 60)?));

        Ok(Self {
            database: PathBuf::from(database),
            listen,
            rate_limit,
            rate_window,
        })
    }
}

fn parse_u32(name: &'static str, default: u32) -> Result<u32, ConfigError> {
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(value) => value
            .parse()
            .map_err(|_| ConfigError::Invalid { name, value }),
    }
}
```

- [ ] **Шаг 2: Точка сборки**

```rust
//! Точка сборки (§3.2).
//!
//! Единственное место, знающее одновременно про транспорт и про адаптеры.
//! Заслон архитектуры проверяет, что это остаётся правдой.

mod config;

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::SystemClock;
use iaam_core::ids::OwnerId;
use iaam_server::auth::hash_token;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use iaam_store::tokens::{TokenRecord, TokenScope};
use rand::TryRng;
use uuid::Uuid;

use crate::config::Config;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Логирование обязательно: без него отладка приёмки невозможна.
    // Чувствительные поля не логируются никогда — сам токен в лог
    // не попадает, только его хеш (§14).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let store = SqliteStore::open(&config.database)?;

    // Разовая выдача токена владельца: без него в систему не войти,
    // а откладывать аутентификацию нельзя (§14).
    if let Ok(label) = std::env::var("IAAM_ISSUE_OWNER_TOKEN") {
        let token = issue_owner_token(&store, &label)?;
        println!("{token}");
        return Ok(());
    }

    let services = Arc::new(AppServices::new(
        Arc::new(SqliteAdapter::new(store)),
        Arc::new(SystemClock),
    ));
    let limiter = Arc::new(RateLimiter::new(config.rate_limit, config.rate_window));
    let (router, _api) = build(ServerState::new(services, limiter));

    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(address = %config.listen, "сервер запущен");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Выдача токена владельца. Сам токен печатается **один раз** и нигде
/// не сохраняется: в базе лежит только его хеш.
fn issue_owner_token(
    store: &SqliteStore,
    label: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // Криптографический источник, а не `rand::rng()`: токен — это ключ
    // от чужих денег, и слабый генератор здесь дороже всего остального
    // в этом файле.
    let mut bytes = [0_u8; 32];
    rand::rngs::SysRng.try_fill_bytes(&mut bytes)?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    store.insert_token(
        &TokenRecord {
            id: Uuid::new_v4(),
            owner: OwnerId::new_random(),
            label: label.to_owned(),
            scope: TokenScope::Owner,
            revoked: false,
        },
        &hash_token(&token),
    )?;
    Ok(token)
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("получен сигнал остановки");
}
```

- [ ] **Шаг 3: Контрактные тесты**

`crates/iaam-server/tests/contract.rs`:

```rust
//! Контрактные тесты против порождённой спеки (§17.1).
//!
//! `utoipa` порождает спеку из типов и потому устраняет расхождение
//! **схемы данных**. Поведение — коды ответов, требования аутентификации,
//! фактическая сериализация — остаётся вне генерации, и проверяется
//! только вызовом поднятого сервера. Для контракта, которым пользуется
//! внешний агент, синтаксически верная, но поведенчески неверная спека
//! означает, что агент будет чиниться по неверной подсказке.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::Clock;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId};
use iaam_server::auth::hash_token;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use iaam_store::reference::AccountRecord;
use iaam_store::tokens::{TokenRecord, TokenScope};
use serde_json::{Value, json};
use std::time::Duration;
use time::Date;
use time::macros::date;
use tower::ServiceExt;
use uuid::Uuid;

/// Часы с зафиксированной датой: отчёт «на сегодня» иначе
/// невоспроизводим в тесте.
struct FixedClock(Date);

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
    }
}

struct Harness {
    router: Router,
    api: utoipa::openapi::OpenApi,
    owner_token: String,
    readonly_token: String,
    account: AccountId,
    instrument: InstrumentId,
    custody: CustodyId,
}

fn harness() -> Harness {
    let store = SqliteStore::open_in_memory().expect("база в памяти");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Брокерский".into(),
            institution: None,
        })
        .expect("счёт");

    let owner_token = "owner-secret-token";
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: "владелец".into(),
                scope: TokenScope::Owner,
                revoked: false,
            },
            &hash_token(owner_token),
        )
        .expect("токен владельца");

    let readonly_token = "read-only-token";
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: "чтение".into(),
                scope: TokenScope::ReadOnly,
                revoked: false,
            },
            &hash_token(readonly_token),
        )
        .expect("токен чтения");

    let services = Arc::new(AppServices::new(
        Arc::new(SqliteAdapter::new(store)),
        Arc::new(FixedClock(date!(2026 - 01 - 01))),
    ));
    let state = ServerState::new(
        services,
        Arc::new(RateLimiter::new(1_000, Duration::from_secs(60))),
    );
    let (router, api) = build(state);

    Harness {
        router,
        api,
        owner_token: owner_token.to_owned(),
        readonly_token: readonly_token.to_owned(),
        account,
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
    }
}

async fn call(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("обработчик ответил");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("тело ответа")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn get(path: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(path).method("GET");
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("запрос")
}

fn post(path: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("POST")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("запрос")
}

#[tokio::test]
async fn health_is_public_and_reports_versions() {
    let harness = harness();
    let (status, body) = call(&harness.router, get("/v1/health", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    // Версия 2: вариант события Valuation добавлен после заморозки v1,
    // и одна версия не может обозначать две схемы (§4.1).
    assert_eq!(body["schema_version"], 2);
    assert_eq!(body["projection_version"], 1);
}

#[tokio::test]
async fn every_documented_path_answers_something_other_than_404() {
    // Спека, описывающая несуществующий маршрут, — это инструкция
    // внешнему агенту чинить себя по неверной подсказке.
    let harness = harness();
    for (path, item) in harness.api.paths.paths.clone() {
        // `PathItem` в utoipa 5 хранит операции отдельными полями,
        // а не картой: перечисляем ровно те методы, которые использует API.
        let methods = [
            ("GET", item.get.is_some()),
            ("POST", item.post.is_some()),
            ("PUT", item.put.is_some()),
            ("DELETE", item.delete.is_some()),
            ("PATCH", item.patch.is_some()),
        ];
        for (verb, present) in methods {
            if !present {
                continue;
            }
            let request = Request::builder()
                .uri(path.replace("{id}", &Uuid::new_v4().to_string()))
                .method(verb)
                .header("Authorization", format!("Bearer {}", harness.owner_token))
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .expect("запрос");
            let (status, _) = call(&harness.router, request).await;
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "маршрут {path} {verb} описан в спеке, но не существует"
            );
            assert_ne!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "метод {verb} для {path} описан в спеке, но не поддерживается"
            );
        }
    }
}

#[tokio::test]
async fn a_request_without_a_token_is_rejected() {
    // Аутентификация с первого дня (§14).
    let harness = harness();
    let (status, body) = call(&harness.router, get("/v1/accounts", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");
}

#[tokio::test]
async fn an_unknown_token_is_rejected() {
    let harness = harness();
    let (status, _) = call(&harness.router, get("/v1/accounts", Some("чужой"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_read_only_token_may_not_submit_operations() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.readonly_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(response["code"], "forbidden");
}

#[tokio::test]
async fn an_invalid_amount_is_reported_as_422_with_field_expected_actual() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.005",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    // Вердикт на строку, а не отказ всего документа (§10.1).
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response[0]["verdict"], "rejected");
    assert_eq!(response[0]["field"], "amount");
    assert_eq!(response[0]["actual"], "1000.005");
}

#[tokio::test]
async fn the_stage_one_question_is_answered_end_to_end() {
    // Приёмочный критерий эпика через API: сколько внесено, сколько
    // выведено, какова доходность до налога.
    let harness = harness();

    let contour = json!({
        "title": "Мой портфель",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("контур")
        .to_owned();

    let operations = json!({
        "source_label": "ручной ввод",
        "operations": [
            {
                "account": harness.account.inner(),
                "type": "deposit",
                "amount": "100000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-01-01" },
                "idempotency_key": "dep-1"
            },
            {
                "account": harness.account.inner(),
                "type": "buy",
                "instrument": harness.instrument.inner(),
                "custody": harness.custody.inner(),
                "quantity": "100",
                "amount": "90000.00",
                "fee": "100.00",
                "currency": "RUB",
                "dates": { "trade": "2025-01-15", "cash_posted": "2025-01-15" }
            },
            {
                "account": harness.account.inner(),
                "type": "income",
                "instrument": harness.instrument.inner(),
                "amount": "3000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-07-01" }
            },
            {
                "account": harness.account.inner(),
                "type": "withdrawal",
                "amount": "10000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-09-01" }
            },
            {
                "account": harness.account.inner(),
                "type": "valuation",
                "instrument": harness.instrument.inner(),
                "price": "1000",
                "currency": "RUB",
                "quality": "previous_close",
                "dates": { "cash_posted": "2026-01-01" }
            }
        ]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    for verdict in verdicts.as_array().expect("массив вердиктов") {
        assert_eq!(verdict["verdict"], "provisional", "{verdict}");
    }

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-01"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    // Масштаб сохраняется: рубль имеет две минимальные единицы, и сумма,
    // переведённая из проведённой в расчётную, остаётся с двумя знаками.
    assert_eq!(report["contributed"]["value"], "100000.00");
    assert_eq!(report["withdrawn"]["value"], "10000.00");
    // 2 900,00 рубля денег плюс 100 бумаг по 1 000 = 102 900,00.
    assert_eq!(report["terminal_value"]["value"], "102900.00");
    assert_eq!(report["history_starts"], "2025-01-01");
    assert_eq!(report["applied_rules"]["fx_source"], "owner_supplied");
    assert_eq!(report["applied_rules"]["day_count"], "act/365");

    // Ставка получена независимым эталоном (scripts/gen-xirr-fixtures.py),
    // а не выводом проверяемой программы (§15.5).
    let rate: f64 = report["xirr_pre_tax"]["value"]
        .as_str()
        .expect("ставка")
        .parse()
        .expect("число");
    assert!(
        (rate - 0.133_270_341_032).abs() < 1e-7,
        "ставка {rate} не совпадает с эталонной"
    );
    assert_eq!(report["data_quality"]["unconfirmed_share"], "1");
}

#[tokio::test]
async fn repeating_an_idempotent_operation_returns_the_same_event() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" },
            "idempotency_key": "one"
        }]
    });
    let (_, first) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    let (_, second) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;

    assert_eq!(first[0]["verdict"], "provisional");
    assert_eq!(second[0]["verdict"], "duplicate");
    assert_eq!(first[0]["event_id"], second[0]["event_id"]);
}

#[tokio::test]
async fn the_openapi_document_declares_bearer_security() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        spec["components"]["securitySchemes"]["bearer"].is_object(),
        "спека обязана описывать схему аутентификации"
    );
}

#[tokio::test]
async fn the_report_shape_is_frozen_by_a_snapshot() {
    // Поштучные проверки полей ловят неверное значение, но не ловят
    // исчезнувшее поле и не ловят появление лишнего. Снапшот ловит
    // форму целиком (§15.8).
    let harness = harness();
    let contour = json!({
        "title": "Снапшот",
        "accounts": [harness.account.inner()],
    });
    let (_, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("контур")
        .to_owned();

    let operations = json!({
        "source_label": "снапшот",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "50000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" }
        }]
    });
    call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-01"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    insta::assert_json_snapshot!(report, {
        ".applied_rules.contour" => "[contour]",
    });
}
```

Дополните `[dev-dependencies]` крейты `iaam-server`:

```toml
[dev-dependencies]
tokio = { version = "1", features = ["rt", "macros", "rt-multi-thread"] }
http-body-util = "0.1"
# Хранилище — dev-зависимость: контрактный тест обязан работать против
# настоящей базы, а не против поддельного порта. Заслон архитектуры
# проверяет обычные зависимости, dev-зависимости он не ограничивает.
iaam-store = { path = "../iaam-store", version = "0.1.0" }
tower = "0.5"
```

- [ ] **Шаг 4: Породить снапшот формы отчёта**

Поштучные проверки полей ловят неверное значение, но не ловят исчезнувшее поле и не ловят появление лишнего. Снапшот ловит форму целиком (§15.8).

```bash
nix develop -c env INSTA_UPDATE=always cargo test -p iaam-server --test contract the_report_shape
git add crates/iaam-server/tests/snapshots/
```

Прочитайте порождённый файл целиком: он и есть публичный контракт ответа. Ожидаемое содержимое:

```json
{
  "applied_rules": {
    "contour": "[contour]",
    "contour_version": 1,
    "day_count": "act/365",
    "fx_source": "owner_supplied",
    "solver_max_iterations": 200,
    "solver_rate_tolerance": "0.0000000001"
  },
  "as_of": "2026-01-01",
  "contributed": {
    "value": "50000.00"
  },
  "data_quality": {
    "material_issues": [
      "история начинается 2025-01-01"
    ],
    "status": "mixed",
    "unconfirmed_share": "1"
  },
  "history_starts": "2025-01-01",
  "report_currency": "RUB",
  "terminal_value": {
    "value": "50000.00"
  },
  "withdrawn": {
    "value": "0"
  },
  "xirr_pre_tax": {
    "day_count": "act/365",
    "error_bound": "0.00000000",
    "iterations": 7,
    "value": "0.00000000"
  }
}
```

Ставка печатается с восемью знаками не для красоты: последние знаки двоичной плавающей точки — шум, а не результат, и без округления снапшот менялся бы между платформами. Без нормализации нуля здесь стояло бы `-0.00000000` — проверено исполнением.

- [ ] **Шаг 5: Заслоны на оболочку**

Global Constraints этого плана обещают, что оболочка не считает деньги и что механизм асинхронных трейтов один. Обещание без механической проверки — это комментарий. Добавьте в `scripts/check-architecture.sh` перед итоговой проверкой `fail`:

```bash
# --- 9. Оболочка не считает деньги ---
# Требование §3.1 и §13: любое число в ответе API приходит из ядра.
# Заслон ищет денежную арифметику там, где её быть не может: в сценариях
# приложения и в транспорте. Приёмка (iaam-ingest) сюда не входит
# намеренно — она СОБИРАЕТ факт из полей источника, а не вычисляет
# результат, и запрет сложения сделал бы её нереализуемой.
SHELL_DIRS=("crates/iaam-app/src" "crates/iaam-server/src")
for dir in "${SHELL_DIRS[@]}"; do
  [ -d "$dir" ] || continue
  hits=$(grep -rnE '\.(try_add|try_sub|checked_add|checked_sub|checked_mul|checked_negate)\(' \
    "$dir" --include='*.rs' | strip_comments || true)
  if [ -n "$hits" ]; then
    err "денежная арифметика в оболочке ($dir): число в ответе обязано приходить из ядра (§3.1, §13)"
    echo "$hits" >&2
  fi
done

# --- 10. Один механизм асинхронных трейтов ---
# §3.2 требует выбрать один и закрепить. Выбран async_trait, и он живёт
# только в iaam-app: объектобезопасные порты существуют только там.
# Смешение механизмов — это два способа писать одно и то же и вечный
# спор о том, какой применять здесь.
for crate_dir in crates/*/src; do
  case "$crate_dir" in
    crates/iaam-app/src) continue ;;
  esac
  [ -d "$crate_dir" ] || continue
  hits=$(grep -rn 'async_trait' "$crate_dir" --include='*.rs' | strip_comments || true)
  if [ -n "$hits" ]; then
    err "async_trait вне iaam-app ($crate_dir): порты живут только в приложении (§3.2)"
    echo "$hits" >&2
  fi
done
```

Проверьте, что заслон **ловит**, а не просто проходит:

```bash
nix develop -c ./scripts/check-architecture.sh    # ожидается: пройдены
printf '\nfn _probe(a: iaam_core::money::Money, b: iaam_core::money::Money) { let _ = a.try_add(b); }\n' \
  >> crates/iaam-server/src/rate_limit.rs
nix develop -c ./scripts/check-architecture.sh    # ожидается: ОТКАЗ на rate_limit.rs
git checkout crates/iaam-server/src/rate_limit.rs
```

Приёмка (`iaam-ingest`) в список намеренно не входит: она **собирает** факт из полей источника — складывает тело сделки, НКД и комиссию в расчётную сумму, — а не вычисляет результат. Запрет сложения сделал бы её нереализуемой, а заслон, который нельзя выполнить, снимают целиком.

- [ ] **Шаг 6: Политика зависимостей**

`cargo deny check` отказывает на `RUSTSEC-2024-0436` (`paste` не поддерживается автором; приходит транзитивно из `utoipa-axum`). Добавьте в `deny.toml` **точечное** исключение:

```toml
[advisories]
yanked = "deny"
# RUSTSEC-2024-0436: крейта `paste` объявлена автором неподдерживаемой.
# Приходит транзитивно из utoipa-axum, является процедурным макросом
# времени сборки и в собранный двоичный файл не попадает. Уязвимости
# в advisory нет — только отсутствие сопровождения. Исключение
# ограничено конкретным идентификатором: «unmaintained = none» отключило
# бы весь класс проверок, а не этот случай.
ignore = ["RUSTSEC-2024-0436"]
```

Если к моменту исполнения плана `utoipa-axum` уже не тянет `paste`, исключение не добавляется: проверьте `cargo tree -i paste`.

- [ ] **Шаг 7: Полный прогон**

```bash
nix develop -c cargo fmt --all
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c cargo test --workspace
nix develop -c ./scripts/check-architecture.sh
nix develop -c cargo deny check
```

Ожидается: десять контрактных тестов зелёные, заслоны пройдены, `advisories ok, bans ok, licenses ok, sources ok`.

- [ ] **Шаг 8: Ручная проверка живого сервера**

```bash
export IAAM_DATABASE=/tmp/iaam-check.db
TOKEN=$(nix develop -c env IAAM_ISSUE_OWNER_TOKEN="проверка" cargo run -p iaam-bootstrap --quiet)
nix develop -c cargo run -p iaam-bootstrap --quiet &
sleep 2
curl -s localhost:8080/v1/health
curl -s -H "Authorization: Bearer $TOKEN" localhost:8080/v1/accounts
curl -s localhost:8080/v1/accounts    # ожидается 401
kill %1
```

- [ ] **Шаг 9: Коммит**

```bash
git add crates/iaam-bootstrap crates/iaam-server Cargo.toml deny.toml
git commit -m "feat(bootstrap): точка сборки и контрактные тесты против спеки (iaam-1fk)"
```

---

# Часть C — сдача

## Задача 18: Архивный бандл

**Files:**
- Create: `crates/iaam-store/src/bundle.rs`
- Create: `crates/iaam-store/tests/bundle.rs`
- Modify: `crates/iaam-store/src/lib.rs`, `Cargo.toml` крейты (`sha2`)

**Interfaces:**
- Produces: `iaam_store::bundle::{Bundle, BUNDLE_VERSION, ImportOutcome, AccountSection, ContourSection}`; методы `SqliteStore::{export_bundle, import_bundle}`.

**Acceptance Criteria:**
- Бандл восстанавливает полное рабочее состояние, а не только события.
- Повторный импорт ничего не меняет.
- Бандл с повреждённым содержимым отклоняется.
- Бандл более новой версии формата отклоняется.
- Бандл переживает JSON.

**Почему не «копия файла базы».** Копия файла — это снимок SQLite, привязанный к версии схемы и к платформе. Бандл — переносимый архив: он читается другой сборкой, другой версией схемы (в пределах поддерживаемых) и просто человеком. Спека требует обоих механизмов (§14), и это разные механизмы, а не два имени одного.

**Чего в бандле этапа 1 ещё нет.** Рыночных данных и курсов (E3), налогового контекста (E5), правил классификации (E2). Каждая секция добавится вместе со своим эпиком — версия формата на это и существует. Пропуск назван в шапке модуля, а не подразумевается.

- [ ] **Шаг 1: Написать падающие тесты**

`crates/iaam-store/tests/bundle.rs`:

```rust
//! Архивный бандл: экспорт, импорт, повреждение.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_store::SqliteStore;
use iaam_store::bundle::ImportOutcome;
use iaam_store::reference::AccountRecord;
use time::macros::date;

fn deposit(owner: OwnerId, account: AccountId, sequence: u32, minor: i64) -> Event {
    let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
    let day = date!(2026 - 05 - 05);
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind: EventKind::CashIn { amount },
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs: vec![Leg::cash(account, amount)],
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"7".repeat(64)).unwrap(),
            ParserVersion("manual/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn populated() -> (SqliteStore, OwnerId, AccountId, ContourId) {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Брокерский".into(),
            institution: Some("Т-Банк".into()),
        })
        .unwrap();
    let contour = ContourId::new_random();
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(contour, ContourVersion(1), [account]),
            "Мой портфель",
            &[account],
        )
        .unwrap();
    store
        .append_event(&deposit(owner, account, 1, 100_000))
        .unwrap();
    store
        .append_event(&deposit(owner, account, 2, 250_000))
        .unwrap();
    (store, owner, account, contour)
}

#[test]
fn a_bundle_restores_a_complete_working_state() {
    // Экспорт одних событий не является бэкапом: из него получатся
    // другие проекции, потому что состав контуров останется снаружи.
    let (source, owner, account, contour) = populated();
    let bundle = source.export_bundle(owner).unwrap();
    assert_eq!(bundle.events.len(), 2);
    assert_eq!(bundle.accounts.len(), 1);
    assert_eq!(bundle.contours.len(), 1);
    assert_eq!(bundle.contours[0].accounts, vec![account.inner()]);

    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert_eq!(
        restored.import_bundle(&bundle).unwrap(),
        ImportOutcome::Applied {
            inserted: 2,
            duplicates: 0
        }
    );
    assert_eq!(
        restored.load_events(owner).unwrap(),
        source.load_events(owner).unwrap()
    );
    assert_eq!(restored.list_accounts(owner).unwrap().len(), 1);
    assert!(
        restored
            .load_contour(owner, contour, ContourVersion(1))
            .unwrap()
            .unwrap()
            .contains(account)
    );
}

#[test]
fn importing_the_same_bundle_twice_changes_nothing() {
    let (source, owner, _, _) = populated();
    let bundle = source.export_bundle(owner).unwrap();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored.import_bundle(&bundle).unwrap();
    assert_eq!(
        restored.import_bundle(&bundle).unwrap(),
        ImportOutcome::Applied {
            inserted: 0,
            duplicates: 2
        }
    );
    assert_eq!(restored.load_events(owner).unwrap().len(), 2);
}

#[test]
fn a_tampered_bundle_is_refused() {
    // Повреждённый архив хуже отсутствующего: он выглядит как целый.
    let (source, owner, _, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events.truncate(1);
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(restored.import_bundle(&bundle).is_err());
}

#[test]
fn a_changed_amount_breaks_the_checksum() {
    // Первая редакция суммы хешировала только идентификаторы событий:
    // подменённая денежная величина проходила проверку, и архив
    // с неверными суммами выглядел целым.
    let (source, owner, account, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events[0] = Event {
        kind: EventKind::CashIn {
            amount: Money::new(PostedMinor::new(999_999_999), CurrencyCode::Rub),
        },
        legs: vec![Leg::cash(
            account,
            Money::new(PostedMinor::new(999_999_999), CurrencyCode::Rub),
        )],
        ..bundle.events[0].clone()
    };
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(
        restored.import_bundle(&bundle).is_err(),
        "подменённая сумма обязана ломать контрольную сумму"
    );
}

#[test]
fn a_bundle_carrying_a_foreign_event_is_refused() {
    let (source, owner, account, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events[0] = Event {
        owner: OwnerId::new_random(),
        ..bundle.events[0].clone()
    };
    bundle.checksum = bundle.compute_checksum();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(restored.import_bundle(&bundle).is_err());
    // Ничего не записалось: импорт идёт одной транзакцией.
    assert!(restored.load_events(owner).unwrap().is_empty());
    assert!(restored.list_accounts(owner).unwrap().is_empty());
    let _ = account;
}

#[test]
fn a_bundle_written_by_a_newer_schema_is_refused() {
    let (source, owner, _, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.schema_version = iaam_store::schema::SCHEMA_VERSION + 1;
    bundle.checksum = bundle.compute_checksum();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(restored.import_bundle(&bundle).is_err());
}

#[test]
fn a_bundle_of_a_newer_format_is_refused() {
    let (source, owner, _, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.bundle_version = iaam_store::bundle::BUNDLE_VERSION + 1;
    bundle.checksum = bundle.compute_checksum();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(restored.import_bundle(&bundle).is_err());
}

#[test]
fn a_bundle_survives_json() {
    // Бандл — переносимый архив: он обязан пережить текстовый формат.
    let (source, owner, _, _) = populated();
    let bundle = source.export_bundle(owner).unwrap();
    let json = serde_json::to_string(&bundle).unwrap();
    let back: iaam_store::bundle::Bundle = serde_json::from_str(&json).unwrap();
    assert_eq!(back, bundle);
}
```

- [ ] **Шаг 2: Реализация**

```rust
//! Архивный бандл (§14).
//!
//! **Копия файла базы не является полноценным бэкапом**, а экспорт одних
//! событий — тем более: из него получатся другие проекции, потому что
//! состав контуров и справочники останутся снаружи. Бандл везёт всё,
//! что нужно, чтобы повторить расчёт: события, счета, версии контуров
//! и версию схемы, под которой всё это записано.
//!
//! Чего в бандле этапа 1 ещё нет и почему: рыночных данных и курсов
//! (появятся в E3), налогового контекста (E5), правил классификации
//! (E2). Каждая из этих секций добавится в бандл вместе со своим эпиком,
//! и версия формата на это и существует.

use iaam_core::event::Event;
use iaam_core::ids::OwnerId;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::events::{find_duplicate, insert_event};
use crate::{SqliteStore, StoreError};

/// Версия формата бандла. Бандл более новой версии не читается:
/// молча пропустить неизвестную секцию значит потерять данные.
pub const BUNDLE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContourSection {
    pub contour: uuid::Uuid,
    pub version: u32,
    pub title: String,
    pub accounts: Vec<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSection {
    pub id: uuid::Uuid,
    pub title: String,
    pub institution: Option<String>,
}

/// Бандл целиком.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub bundle_version: u32,
    pub schema_version: u32,
    pub exported_at: String,
    pub owner: OwnerId,
    pub events: Vec<Event>,
    pub accounts: Vec<AccountSection>,
    pub contours: Vec<ContourSection>,
    /// Контрольная сумма содержимого. Считается по каноническому
    /// представлению всех секций, кроме самой суммы.
    pub checksum: String,
}

/// Содержимое бандла без служебных полей. Существует ради контрольной
/// суммы: считать её нужно по всему, что бандл переносит, и только по нему.
#[derive(Debug, Serialize)]
struct BundleContent<'a> {
    bundle_version: u32,
    schema_version: u32,
    owner: OwnerId,
    events: &'a [Event],
    accounts: &'a [AccountSection],
    contours: &'a [ContourSection],
}

impl Bundle {
    /// Контрольная сумма содержимого.
    ///
    /// Считается по **канонической сериализации всего содержимого**.
    /// Первая редакция хешировала только идентификаторы событий и хеши
    /// сырья — при такой сумме подменённая денежная величина проходила
    /// проверку, а повреждённый архив выглядел целым. Это ровно тот
    /// отказ, ради предотвращения которого сумма и существует (§14).
    ///
    /// Дата выгрузки в сумму не входит: она описывает выгрузку,
    /// а не переносимые факты, и её изменение ничего не портит.
    #[must_use]
    pub fn compute_checksum(&self) -> String {
        let content = BundleContent {
            bundle_version: self.bundle_version,
            schema_version: self.schema_version,
            owner: self.owner,
            events: &self.events,
            accounts: &self.accounts,
            contours: &self.contours,
        };
        let mut body = Vec::new();
        ciborium::into_writer(&content, &mut body)
            .unwrap_or_else(|error| panic!("бандл не сериализуется: {error}"));
        let digest = Sha256::digest(&body);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Сколько событий добавлено и сколько уже было.
    Applied { inserted: usize, duplicates: usize },
}

impl SqliteStore {
    /// Экспорт бандла.
    pub fn export_bundle(&self, owner: OwnerId) -> Result<Bundle, StoreError> {
        let events = self.load_events(owner)?;
        let accounts = self
            .list_accounts(owner)?
            .into_iter()
            .map(|record| AccountSection {
                id: record.id.inner(),
                title: record.title,
                institution: record.institution,
            })
            .collect();

        let mut statement = self.conn.prepare(
            "SELECT v.contour, v.version, v.title, a.account
             FROM contour_versions v
             LEFT JOIN contour_accounts a
               ON a.owner = v.owner AND a.contour = v.contour AND a.version = v.version
             WHERE v.owner = ?1
             ORDER BY v.contour, v.version, a.account",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut contours: Vec<ContourSection> = Vec::new();
        for row in rows {
            let (contour, version, title, account) = row?;
            let contour = parse(&contour, "contour")?;
            let account = account
                .map(|value| parse(&value, "contour_account"))
                .transpose()?;
            match contours
                .last_mut()
                .filter(|section| section.contour == contour && section.version == version)
            {
                Some(section) => section.accounts.extend(account),
                None => contours.push(ContourSection {
                    contour,
                    version,
                    title,
                    accounts: account.into_iter().collect(),
                }),
            }
        }

        let mut bundle = Bundle {
            bundle_version: BUNDLE_VERSION,
            schema_version: crate::schema::SCHEMA_VERSION,
            exported_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z")),
            owner,
            events,
            accounts,
            contours,
            checksum: String::new(),
        };
        bundle.checksum = bundle.compute_checksum();
        Ok(bundle)
    }

    /// Импорт бандла.
    ///
    /// Идемпотентен: события с известными ключами не создаются повторно.
    /// Выполняется **одной транзакцией**: частично импортированный архив
    /// — это состояние, которого никогда не существовало, и разбираться
    /// с ним хуже, чем с неудавшимся импортом.
    ///
    /// Отклоняется бандл, который: новее поддерживаемого формата; записан
    /// схемой новее поддерживаемой; не сходится с контрольной суммой;
    /// содержит события чужого владельца.
    pub fn import_bundle(&mut self, bundle: &Bundle) -> Result<ImportOutcome, StoreError> {
        if bundle.bundle_version > BUNDLE_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: bundle.bundle_version,
                supported: BUNDLE_VERSION,
            });
        }
        if bundle.schema_version > crate::schema::SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: bundle.schema_version,
                supported: crate::schema::SCHEMA_VERSION,
            });
        }
        if bundle.checksum != bundle.compute_checksum() {
            return Err(StoreError::BundleCorrupted {
                detail: "контрольная сумма не совпадает с содержимым".into(),
            });
        }
        // Владелец в бандле один. Событие чужого владельца означает либо
        // склейку двух архивов, либо подмену: и то и другое сделало бы
        // границу владельца фикцией (§14).
        if let Some(foreign) = bundle
            .events
            .iter()
            .find(|event| event.owner != bundle.owner)
        {
            return Err(StoreError::BundleCorrupted {
                detail: format!(
                    "событие {} принадлежит другому владельцу",
                    foreign.id.inner()
                ),
            });
        }

        let owner = bundle.owner;
        let created_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"));
        let transaction = self.conn.transaction()?;

        for account in &bundle.accounts {
            transaction.execute(
                "INSERT INTO accounts (id, owner, title, institution, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (id) DO UPDATE SET
                     title = excluded.title,
                     institution = excluded.institution
                 WHERE accounts.owner = excluded.owner",
                params![
                    account.id.to_string(),
                    owner.inner().to_string(),
                    account.title,
                    account.institution,
                    created_at,
                ],
            )?;
        }

        for contour in &bundle.contours {
            // Версия контура неизменяема: уже существующая пропускается,
            // а не переписывается.
            let known: Option<u32> = transaction
                .query_row(
                    "SELECT version FROM contour_versions
                     WHERE owner = ?1 AND contour = ?2 AND version = ?3",
                    params![
                        owner.inner().to_string(),
                        contour.contour.to_string(),
                        contour.version
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                continue;
            }
            transaction.execute(
                "INSERT INTO contour_versions (owner, contour, version, title, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    owner.inner().to_string(),
                    contour.contour.to_string(),
                    contour.version,
                    contour.title,
                    created_at,
                ],
            )?;
            for account in &contour.accounts {
                transaction.execute(
                    "INSERT INTO contour_accounts (owner, contour, version, account)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        owner.inner().to_string(),
                        contour.contour.to_string(),
                        contour.version,
                        account.to_string(),
                    ],
                )?;
            }
        }

        let mut inserted = 0;
        let mut duplicates = 0;
        for event in &bundle.events {
            if find_duplicate(&transaction, event)?.is_some() {
                duplicates += 1;
                continue;
            }
            insert_event(&transaction, event)?;
            inserted += 1;
        }

        transaction.commit()?;
        Ok(ImportOutcome::Applied {
            inserted,
            duplicates,
        })
    }
}

fn parse(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}
```

Добавьте `pub mod bundle;` в `crates/iaam-store/src/lib.rs` и `sha2 = "0.11"` в зависимости крейты.

- [ ] **Шаг 3: Зелёная сборка и коммит**

```bash
nix develop -c cargo test -p iaam-store
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/iaam-store
git commit -m "feat(store): архивный бандл с экспортом и импортом (iaam-1fk)"
```

---

## Задача 19: Золотые сценарии и незакрытые вопросы первого плана

**Files:**
- Create: `crates/iaam-core/tests/golden_stage1.rs`
- Create: `crates/iaam-core/tests/serde_roundtrip.rs`
- Create: `crates/iaam-core/tests/ui.rs`, `tests/ui/*.rs`
- Modify: `crates/iaam-core/Cargo.toml` — `trybuild` в dev-зависимости
- Modify: `docs/irreversible-core.md` — раздел «Чего проверки не гарантируют»

**Acceptance Criteria:**
- Применимая к этапу 1 часть золотого набора §15.9 покрыта тестами; неприменимая **названа поимённо** с указанием эпика.
- Round-trip журнала через JSON проверен для **каждого** варианта события.
- Несовместимость типов идентификаторов, дат и денежных величин проверяется постоянным заслоном, а не комментарием.
- Раздел «Чего проверки не гарантируют» обновлён.

**Три незакрытых вопроса первого плана и что с ними стало.**

| Вопрос | Состояние |
|---|---|
| Round-trip через JSON не проверяется | **Закрыт**: `tests/serde_roundtrip.rs` проверяет все варианты `EventKind` и сохранение масштаба `Decimal` |
| Несовместимость типов идентификаторов проверена только вручную | **Закрыт**: `trybuild` в `tests/ui/` — четыре случая, включая запрет сложения денег |
| Мутационный заслон почти слеп на исчерпывающих `match` | **Остаётся**. Гарантию по-прежнему даёт табличный тест на все сочетания, и это записано как граница доверия, а не как решённая задача |

**Почему `trybuild`, а не комментарий.** Закомментированная строка в обычном тесте не проверяет ничего: её никто не собирает. `trybuild` собирает — и падает, если запрет исчез. Цена: тексты диагностик меняются между версиями тулчейна, и `.stderr` придётся обновлять командой `TRYBUILD=overwrite`. Обновлять их надо, **читая диффы**: «ошибки больше нет» означает, что защита исчезла, а не что тест устарел.

- [ ] **Шаг 1: Золотые сценарии**

`crates/iaam-core/tests/golden_stage1.rs`:

```rust
//! Золотые сценарии этапа 1 (§15.9).
//!
//! Из обязательного набора спеки здесь те сценарии, которые этап 1
//! обязан отработать. Остальные — амортизация, ЛДВ, замещающие
//! облигации, налог прошлого периода — относятся к E3 и E5 и появятся
//! там вместе с механикой, которую проверяют. Пропуск не тихий:
//! каждый отсутствующий сценарий назван в конце файла.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId, TransferId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::lots::LotKey;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::returns::{MaterialIssue, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable, PriceQuality};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

struct World {
    owner: OwnerId,
    account: AccountId,
    other: AccountId,
    custody: CustodyId,
    instrument: InstrumentId,
    source: SourceId,
    sequence: u32,
}

impl World {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            other: AccountId::new_random(),
            custody: CustodyId::new_random(),
            instrument: InstrumentId::new_random(),
            source: SourceId::new_random(),
            sequence: 0,
        }
    }

    fn event(&mut self, day: Date, kind: EventKind, legs: Vec<Leg>) -> Event {
        self.sequence += 1;
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account: self.account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, self.sequence),
            legs,
            provenance: Provenance::new(
                self.source,
                RawHash::parse(&"9".repeat(64)).expect("хеш"),
                ParserVersion("golden/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn deposit(&mut self, day: Date, minor: i64) -> Event {
        let amount = rub(minor);
        let account = self.account;
        self.event(
            day,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        )
    }

    fn buy(&mut self, day: Date, units: i64, gross_minor: i64) -> Event {
        let (account, custody, instrument) = (self.account, self.custody, self.instrument);
        let gross = rub(gross_minor);
        self.event(
            day,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(units),
                gross,
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, rub(-gross_minor)),
                Leg::security(account, custody, instrument, qty(units)),
            ],
        )
    }

    fn sell(&mut self, day: Date, units: i64, gross_minor: i64) -> Event {
        let (account, custody, instrument) = (self.account, self.custody, self.instrument);
        let gross = rub(gross_minor);
        self.event(
            day,
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(units),
                gross,
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, gross),
                Leg::security(account, custody, instrument, qty(-units)),
            ],
        )
    }

    fn valuation(&mut self, day: Date, price: i64) -> Event {
        let instrument = self.instrument;
        self.event(
            day,
            EventKind::Valuation {
                instrument,
                price: Dec::new(Decimal::from(price)),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::PreviousClose,
            },
            vec![],
        )
    }

    fn transfer_inside(&mut self, day: Date, minor: i64) -> Event {
        let (from, to) = (self.account, self.other);
        let amount = rub(minor);
        self.event(
            day,
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount,
            },
            vec![Leg::cash(from, rub(-minor)), Leg::cash(to, amount)],
        )
    }

    fn opening_position(&mut self, day: Date, units: i64) -> Event {
        let (account, custody, instrument) = (self.account, self.custody, self.instrument);
        self.event(
            day,
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(units),
                cost_basis: None,
            },
            vec![Leg::security(account, custody, instrument, qty(units))],
        )
    }
}

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

fn dec(value: i64) -> Dec {
    Dec::new(Decimal::from(value))
}

fn report_of(world: &World, events: &[Event], both_accounts: bool, as_of: Date) -> ReportPair {
    let accounts: Vec<AccountId> = if both_accounts {
        vec![world.account, world.other]
    } else {
        vec![world.account]
    };
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), accounts);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let projection = project(events, &ctx).expect("проекция строится");
    let fx = FxTable::new(FxSource::OwnerSupplied);
    let report = returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &contour,
            as_of,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
        },
    );
    ReportPair {
        report,
        projection: Box::new(projection),
    }
}

struct ReportPair {
    report: iaam_core::returns::ReturnsReport,
    projection: Box<iaam_core::projection::Projection>,
}

/// §15.9: перевод между счетами внутри контура — XIRR не меняется.
#[test]
fn a_transfer_inside_the_contour_does_not_change_the_rate() {
    let mut world = World::new();
    let base = vec![
        world.deposit(date!(2025 - 01 - 01), 10_000_000),
        world.valuation(date!(2026 - 01 - 01), 1),
    ];
    let mut with_transfer = base.clone();
    with_transfer.push(world.transfer_inside(date!(2025 - 06 - 01), 3_000_000));

    let without = report_of(&world, &base, true, date!(2026 - 01 - 01));
    let with = report_of(&world, &with_transfer, true, date!(2026 - 01 - 01));

    let left = without.report.xirr.value().expect("ставка без перевода");
    let right = with.report.xirr.value().expect("ставка с переводом");
    assert!(
        (left.rate().value() - right.rate().value()).abs() < 1e-12,
        "перевод внутри контура изменил ставку: {} против {}",
        left.rate().value(),
        right.rate().value()
    );
    assert_eq!(with.projection.state().flows().internal(), 1);
    assert_eq!(with.report.contributed.value(), Some(&dec(100_000)));
}

/// §15.9: продажа части позиции — списание стоимости и перенос
/// нереализованного результата в реализованный.
#[test]
fn a_partial_sale_releases_basis_and_realizes_result() {
    let mut world = World::new();
    let events = vec![
        world.deposit(date!(2025 - 01 - 01), 10_000_000),
        // 100 бумаг за 9 000 рублей: по 90 рублей за бумагу.
        world.buy(date!(2025 - 02 - 01), 100, 900_000),
        // 40 бумаг проданы за 4 000 рублей: списанная стоимость
        // 9 000 × 40 / 100 = 3 600, реализовано 4 000 − 3 600 = 400.
        world.sell(date!(2025 - 09 - 01), 40, 400_000),
        world.valuation(date!(2026 - 01 - 01), 100),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));
    let key = LotKey {
        account: world.account,
        instrument: world.instrument,
    };
    let entry = pair
        .projection
        .state()
        .book()
        .entry(&key)
        .expect("книга лотов");

    assert_eq!(entry.quantity().unwrap(), qty(60));
    assert_eq!(entry.released_basis(), Some(rub(360_000)));
    assert_eq!(entry.realized(), Some(rub(40_000)));
    assert_eq!(entry.remaining_basis().unwrap(), Some(rub(540_000)));
    // Деньги: 100 000 − 9 000 + 4 000 = 95 000; бумаги: 60 × 100 = 6 000.
    assert_eq!(pair.report.terminal_value.value(), Some(&dec(101_000)));
}

/// §15.9: отрицательный денежный остаток — обязательство в стоимости,
/// а не исчезнувшая величина.
#[test]
fn negative_cash_lowers_the_terminal_value_and_is_reported() {
    let mut world = World::new();
    let events = vec![
        world.deposit(date!(2025 - 01 - 01), 1_000_000),
        world.buy(date!(2025 - 02 - 01), 100, 1_200_000),
        world.valuation(date!(2026 - 01 - 01), 100),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));

    assert_eq!(
        pair.projection
            .state()
            .balances()
            .cash(world.account, CurrencyCode::Rub),
        Some(rub(-200_000))
    );
    // −2 000 рублей денег плюс 100 бумаг по 100 = 8 000.
    assert_eq!(pair.report.terminal_value.value(), Some(&dec(8_000)));
    assert!(
        pair.report
            .data_quality
            .material_issues
            .iter()
            .any(|issue| matches!(issue, MaterialIssue::NegativeCash { .. })),
        "отрицательный остаток обязан попасть в блок качества данных"
    );
}

/// §15.9: частичная история без налоговой стоимости — `not_computable`
/// вместо выдуманной цифры.
#[test]
fn a_restored_position_without_basis_makes_the_realized_result_not_computable() {
    let mut world = World::new();
    let events = vec![
        world.opening_position(date!(2024 - 01 - 01), 50),
        world.deposit(date!(2025 - 01 - 01), 1_000_000),
        world.sell(date!(2025 - 06 - 01), 20, 300_000),
        world.valuation(date!(2026 - 01 - 01), 150),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));
    let key = LotKey {
        account: world.account,
        instrument: world.instrument,
    };
    let entry = pair
        .projection
        .state()
        .book()
        .entry(&key)
        .expect("книга лотов");

    assert_eq!(entry.unpriced(), qty(30));
    assert_eq!(
        entry.realized(),
        None,
        "прибыль от продажи бумаги неизвестной стоимости не вычисляется"
    );
    // Стоимость позиции при этом известна: 30 бумаг по 150 = 4 500,
    // плюс 10 000 + 3 000 денег.
    assert_eq!(pair.report.terminal_value.value(), Some(&dec(17_500)));
    assert!(
        pair.report
            .data_quality
            .material_issues
            .iter()
            .any(|issue| matches!(issue, MaterialIssue::RestoredWithoutBasis { .. }))
    );
}

/// §15.9: две одинаковые сделки в один день — обе учтены.
#[test]
fn two_identical_purchases_on_the_same_day_are_both_projected() {
    let mut world = World::new();
    let day = date!(2025 - 03 - 03);
    let events = vec![
        world.deposit(date!(2025 - 01 - 01), 10_000_000),
        world.buy(day, 10, 100_000),
        world.buy(day, 10, 100_000),
        world.valuation(date!(2026 - 01 - 01), 100),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));
    let key = LotKey {
        account: world.account,
        instrument: world.instrument,
    };
    let entry = pair.projection.state().book().entry(&key).expect("книга");
    assert_eq!(entry.lots().len(), 2, "две сделки — две партии");
    assert_eq!(entry.quantity().unwrap(), qty(20));
}

/// §15.9: цена без оценки — отчёт отказывается называть стоимость.
#[test]
fn a_position_without_a_price_makes_the_terminal_value_not_computable() {
    let mut world = World::new();
    let events = vec![
        world.deposit(date!(2025 - 01 - 01), 10_000_000),
        world.buy(date!(2025 - 02 - 01), 100, 900_000),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));
    assert_eq!(
        pair.report.terminal_value.reason().map(|r| r.code()),
        Some("missing_price")
    );
    assert_eq!(
        pair.report.xirr.reason().map(|r| r.code()),
        Some("missing_price"),
        "ставка без стоимости не определена и не подменяется нулём"
    );
}

// Сценарии §15.9, НЕ реализованные на этапе 1, и их адрес:
//   облигация с амортизацией ............................ E3
//   вклад с капитализацией и досрочным расторжением ..... E3
//   лот, доживший до ЛДВ ................................ E5
//   перевод бумаг между брокерами ....................... E3
//   доудержанный в январе налог за прошлый год .......... E5
//   возврат излишне удержанного налога .................. E5
//   дивиденд в валюте с разложением FX .................. E4
//   замещающая облигация ................................ E3
//   оферта без предъявления ............................. E3
//   сплит с дробным остатком ............................ E3
//   делистинг ........................................... E3
//   компенсирующая ошибка парсера ....................... E2
```

- [ ] **Шаг 2: Round-trip журнала**

`crates/iaam-core/tests/serde_roundtrip.rs`:

```rust
//! Round-trip журнала через JSON (незакрытый вопрос первого плана).
//!
//! `docs/irreversible-core.md` фиксировал: корректность `Serialize`/
//! `Deserialize` держится на том, что derive компилируется. Журнал фактов,
//! не переживающий сериализацию, бесполезен — хранилище кладёт событие
//! в текстовое поле и читает обратно.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, TradeDate};
use iaam_core::event::kind::{EventKind, FeeOrigin, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash, RowLocator};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId, TransferId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use rust_decimal::Decimal;
use time::macros::date;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

fn envelope(kind: EventKind, legs: Vec<Leg>) -> Event {
    let account = AccountId::new_random();
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner: OwnerId::new_random(),
        account,
        kind,
        dates: EventDates {
            trade: Some(TradeDate(date!(2025 - 12 - 30))),
            cash_posted: Some(CashPostedDate(date!(2026 - 01 - 03))),
            ..EventDates::empty()
        },
        order: EffectiveOrder::new(date!(2026 - 01 - 03), 7),
        legs,
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"f".repeat(64)).expect("хеш"),
            ParserVersion("manual/1".into()),
        )
        .with_source_operation_id("op-42")
        .with_row(RowLocator {
            document: "отчёт.xlsx".into(),
            sheet: Some("Сделки".into()),
            row: 17,
        }),
        relation: Relation::Replacement {
            target: EventId::new_random(),
        },
        confidence: Confidence::Estimated,
        idempotency_key: Some("key-1".into()),
    }
}

/// Каждый вариант `EventKind`, чтобы новый вариант ломал этот тест
/// вместе со сборкой, а не молча оставался непроверенным.
fn every_kind() -> Vec<Event> {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    vec![
        envelope(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(10),
                gross: rub(100_000),
                fee: Some(rub(500)),
                accrued_interest: Some(rub(1_234)),
            },
            vec![
                Leg::cash(account, rub(99_500)),
                Leg::security(account, custody, instrument, qty(-10)),
            ],
        ),
        envelope(
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(account, rub(1))],
        ),
        envelope(
            EventKind::CashOut { amount: rub(-1) },
            vec![Leg::cash(account, rub(-1))],
        ),
        envelope(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: account,
                to: AccountId::new_random(),
                amount: rub(5_000),
            },
            vec![Leg::cash(account, rub(-5_000))],
        ),
        envelope(
            EventKind::Income {
                instrument: Some(instrument),
                gross: rub(700),
            },
            vec![Leg::cash(account, rub(700))],
        ),
        envelope(
            EventKind::Fee {
                amount: rub(-99),
                origin: FeeOrigin::MarginInterest,
            },
            vec![Leg::fee(account, rub(-99))],
        ),
        envelope(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(3),
                cost_basis: None,
            },
            vec![Leg::security(account, custody, instrument, qty(3))],
        ),
        envelope(
            EventKind::OpeningCash { amount: rub(-42) },
            vec![Leg::cash(account, rub(-42))],
        ),
        envelope(
            EventKind::Valuation {
                instrument,
                price: Dec::new(Decimal::new(1_234_567, 4)),
                currency: CurrencyCode::Usd,
                quality: PriceQuality::Stale,
            },
            vec![],
        ),
    ]
}

#[test]
fn every_event_kind_survives_a_json_round_trip() {
    for event in every_kind() {
        let json = serde_json::to_string(&event).expect("сериализация");
        let back: Event = serde_json::from_str(&json).expect("разбор");
        assert_eq!(back, event, "round-trip изменил событие: {json}");
    }
}

#[test]
fn a_decimal_keeps_its_scale_through_json() {
    // Масштаб — часть значения: 1.2340 и 1.234 различаются точностью
    // источника, и потеря масштаба меняет смысл цены (§3.4).
    let price = Dec::new(Decimal::new(12_340, 4));
    let json = serde_json::to_string(&price).expect("сериализация");
    let back: Dec = serde_json::from_str(&json).expect("разбор");
    assert_eq!(back.inner().scale(), 4, "масштаб потерян: {json}");
    assert_eq!(back, price);
}
```

- [ ] **Шаг 3: Заслон на непредставимые ошибки**

`crates/iaam-core/tests/ui.rs`:

```rust
//! Проверки, которые обязаны **не компилироваться** (§15.1).
//!
//! Первый слой проверки — типы, делающие ошибку непредставимой. Без
//! этого теста слой держится на честном слове: закомментированная строка
//! в обычном тесте не проверяет ничего, потому что её никто не собирает.
//!
//! Ожидаемый вывод компилятора лежит рядом в `.stderr`. При смене версии
//! тулчейна тексты диагностик меняются; обновлять их надо командой
//! `TRYBUILD=overwrite cargo test -p iaam-core --test ui` и **читать
//! диффы**: изменение вида «ошибки больше нет» означает, что защита
//! исчезла, а не что тест устарел.

#[test]
fn errors_that_must_not_compile() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
```

`crates/iaam-core/tests/ui/identity_mismatch.rs`:

```rust
//! Идентификаторы разных сущностей не взаимозаменяемы (§4.5).

use iaam_core::ids::{AccountId, OwnerId};

fn main() {
    let _: AccountId = OwnerId::new_random();
}
```

`crates/iaam-core/tests/ui/money_addition.rs`:

```rust
//! Деньги нельзя сложить в обход проверки валюты (§15.1).

use iaam_core::money::{CurrencyCode, Money, PostedMinor};

fn main() {
    let rubles = Money::new(PostedMinor::new(100), CurrencyCode::Rub);
    let dollars = Money::new(PostedMinor::new(100), CurrencyCode::Usd);
    let _ = rubles + dollars;
}
```

`crates/iaam-core/tests/ui/date_mismatch.rs`:

```rust
//! Шесть семантических дат — шесть разных типов (§4.2).

use iaam_core::dates::{SettledDate, TradeDate};
use time::macros::date;

fn main() {
    let _: SettledDate = TradeDate(date!(2026 - 01 - 01));
}
```

`crates/iaam-core/tests/ui/posted_vs_calculated.rs`:

```rust
//! Проведённые суммы и расчётные величины не смешиваются (§3.4).

use iaam_core::money::PostedMinor;
use iaam_core::numeric::decimal::Dec;

fn main() {
    let _: Dec = PostedMinor::new(100);
}
```

Добавьте `trybuild = "1"` в `[dev-dependencies]` крейты `iaam-core`, породите эталоны диагностик и **прочитайте их**:

```bash
nix develop -c env TRYBUILD=overwrite cargo test -p iaam-core --test ui
git diff crates/iaam-core/tests/ui/
nix develop -c cargo test -p iaam-core --test ui
```

Каждый `.stderr` обязан содержать ошибку. Пустой файл означает, что код скомпилировался, — то есть защиты нет.

- [ ] **Шаг 4: Обновить документ необратимого ядра**

В `docs/irreversible-core.md`, раздел «Чего проверки не гарантируют», удалите пункты про round-trip и про идентификаторы, а оставшиеся приведите к виду:

```markdown
- **Мутационный заслон почти слеп на `contour::classify`
  и `EventKind::flow_endpoints`.** Исчерпывающий `match`, возвращающий
  `enum` без `Default`, даёт единственный нежизнеспособный мутант.
  Гарантию даёт табличный тест на все шестнадцать сочетаний.
- **Свойство сохранения стоимости не ловит неверное разнесение.**
  Невыбывшую часть считает то же значение, которое вернуло разнесение.
  Величину ловит только детерминированный тест с посчитанным вручную
  ожиданием.
- **Тексты диагностик в `tests/ui/*.stderr` привязаны к версии
  тулчейна.** Обновление тулчейна требует перегенерации и чтения диффов:
  исчезнувшая ошибка означает исчезнувшую защиту.
- `cargo-mutants` не порождает мутантов для функций с именем `new`,
  для `is_zero()`, для замыканий в `.map(...).sum()` и для тел
  `else`-ветвей.
```

- [ ] **Шаг 5: Зелёная сборка и коммит**

```bash
nix develop -c cargo test --workspace
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c ./scripts/check-fixtures.sh
git add crates/iaam-core docs/irreversible-core.md
git commit -m "test: золотые сценарии этапа 1 и закрытие двух незакрытых вопросов (iaam-1fk)"
```

---

## Задача 20: Скилл агента, документация и закрытие эпика

**Files:**
- Create: `docs/agent-skill/SKILL.md`
- Create: `docs/deployment.md`
- Modify: `README.md`

**Acceptance Criteria:**
- Скилл описывает семантику, а не только эндпоинты: контуры, приёмку, вердикты, ключи идемпотентности, запрет на вычисления, трактовку `dataQuality` и `not_computable`.
- Инструкция по развёртыванию описывает HTTPS через реверс-прокси, выдачу токена, бэкап и **границы** ограничителя частоты.
- `README.md` честно называет, на что система отвечает, а на что — нет.
- Эпик `iaam-1fk` закрыт, следующие эпики разблокированы.

**Почему скилл описывает семантику, а не эндпоинты.** Список эндпоинтов у агента уже есть — это `/v1/openapi.json`. Чего в спеке нет и быть не может: что перевод внутри контура не является доходом, что отказ вычислить величину нельзя заменять собственной оценкой и что `xirr_pre_tax` называется так не для красоты. Именно на этом агент ошибается.

- [ ] **Шаг 1: Скилл внешнего агента**

Создайте `docs/agent-skill/SKILL.md` со следующим содержимым:

````markdown
---
name: iaam
description: Учёт инвестиций IAAM. Отвечает, сколько внесено, сколько выведено и какова доходность до налога по контуру счетов. Используйте, когда спрашивают про портфель, доходность, пополнения или стоимость позиций.
---

# IAAM — учёт инвестиций

## Главное правило

**Собственная арифметика запрещена.** Любое число в вашем ответе обязано
присутствовать в ответе API дословно. Не складывайте суммы, не считайте
проценты, не переводите валюты, не оценивайте доходность «примерно».
Число, которого нет в ответе API, является ошибкой — даже если оно верное.

Если API отказался вычислить величину, ответ так и звучит: система
не может её вычислить, и вот почему. Замена отказа собственной оценкой —
самая дорогая ошибка, которую здесь можно сделать.

## Что такое контур

Контур — это набор счетов, который владелец считает «своим портфелем».
Граница проводится владельцем, а не учреждением. Перевод денег между
двумя счетами **внутри** контура доходность не меняет: это перекладывание
из кармана в карман. Перевод со счёта вне контура на счёт внутри —
внесение средств.

У контура есть **версия**. Отчёт всегда возвращает версию, по которой
считал. Две цифры, посчитанные по разным версиям контура, сравнивать
нельзя.

## Как записать операцию

Запись в журнал — результат прохождения приёмки, а не отдельное действие.
Отправляйте операции в `POST /v1/ingest/operations`; на каждую придёт
вердикт:

| Вердикт | Значение | Что делать |
|---|---|---|
| `provisional` | записано; независимого подтверждения нет | ничего |
| `duplicate` | уже было записано по этому ключу | ничего, это нормальный ответ на повтор |
| `needs_classification` | классификация неоднозначна | задать владельцу вопрос из поля `detail` |
| `unsupported` | операция вне периметра | сообщить владельцу |
| `rejected` | строка не разобрана | показать `field`, `expected`, `actual` |

**Величины всегда положительные.** Знак задаёт вид операции: `deposit`
и `withdrawal` — разные виды, а не разные знаки одной суммы. Суммы
передаются строками (`"1000.50"`), а не числами: JSON-число теряет
точность, а сумма в журнале — это факт.

**Точность суммы не должна превышать минимальную единицу валюты.**
`"100.005"` для рубля будет отклонено, а не округлено.

## Ключи идемпотентности

Всегда передавайте `idempotency_key`, если можете его построить. Повтор
запроса с тем же ключом вернёт `duplicate` и идентификатор первого
события — это правильный ответ, а не ошибка. Без ключа повторная отправка
создаст второе событие: две одинаковые покупки в один день — законная
ситуация, и система не имеет права их склеивать.

## Как прочитать отчёт

`GET /v1/reports/returns?contour=…&currency=RUB` возвращает:

- `contributed` — сколько внесено в контур за всю историю;
- `withdrawn` — сколько выведено;
- `terminal_value` — стоимость контура на дату отчёта;
- `xirr_pre_tax` — доходность **до налога**;
- `applied_rules` — по каким правилам считалось;
- `data_quality` — чего в данных не хватает.

**Период отчёта — вся история.** Доходность за произвольный интервал
на этом этапе не считается: для неё нужна стоимость на начало интервала,
а она известна только на дату отчёта.

**Называйте `xirr_pre_tax` доходностью до налога.** Не «доходностью»,
не «сколько заработано». Налоги в системе ещё не считаются, и разница
может достигать 13–15 % от результата.

## Как читать `not_computable`

Любая величина может прийти без значения и с полем `not_computable`:

| Код | Значение | Что сказать владельцу |
|---|---|---|
| `missing_price` | нет цены инструмента | «нужна оценка бумаги на дату отчёта» |
| `missing_fx_rate` | нет курса на дату | «нужен курс валюты» |
| `solver_refused` | уравнение доходности не имеет единственного корня | «доходность не определена для такой последовательности потоков» |
| `no_external_flows` | не было ни одного вложения | «вложений в контур не было» |
| `state_newer_than_report` | срез собран неверно | сообщить как ошибку системы |

Отказ — это ответ. Пересчитывать его самостоятельно нельзя.

## Как читать `data_quality`

- `status: incomplete` — есть материальные проблемы, они перечислены
  в `material_issues`;
- `unconfirmed_share` — доля событий без независимого подтверждения;
  на текущем этапе это всегда `1`, потому что сверка ещё не реализована.

Упоминайте качество данных, когда оно меняет ответ, а не в каждом
сообщении: постоянное предупреждение перестают замечать.

## Аутентификация

Токен передаётся заголовком `Authorization: Bearer …`. Токен агента
не может создавать счета и контуры и не пишет в журнал напрямую: запись
всегда проходит приёмку. При `429` — уменьшите частоту запросов,
а не повторяйте сразу.

## Чего система не делает

Не считает налоги, не знает рыночных цен (цену сообщаете вы или
владелец), не работает с шортами, маржой и производными, не сверяет
данные с отчётами брокера. Всё это появится позже; сейчас об этом
надо честно сказать, а не заполнить пробел догадкой.
````

- [ ] **Шаг 2: Инструкция по развёртыванию**

Создайте `docs/deployment.md`:

````markdown
# Развёртывание

## Запуск

```bash
export IAAM_DATABASE=/var/lib/iaam/iaam.db
export IAAM_LISTEN=127.0.0.1:8080
cargo run -p iaam-bootstrap --release
```

| Переменная | Умолчание | Смысл |
|---|---|---|
| `IAAM_DATABASE` | нет, обязательна | путь к файлу базы |
| `IAAM_LISTEN` | `127.0.0.1:8080` | адрес прослушивания |
| `IAAM_RATE_LIMIT` | `120` | запросов на токен в окне |
| `IAAM_RATE_WINDOW_SECONDS` | `60` | длина окна |
| `RUST_LOG` | `info` | уровень логирования |

Умолчание адреса — петлевой интерфейс намеренно: сервис не предназначен
смотреть в интернет напрямую.

## Первый токен

```bash
IAAM_ISSUE_OWNER_TOKEN="ноутбук" cargo run -p iaam-bootstrap --release
```

Токен печатается **один раз**: в базе хранится только его хеш.
Потерянный токен не восстанавливается — выпускается новый, старый
отзывается.

## HTTPS

Сервис слушает HTTP на петлевом интерфейсе. TLS терминирует реверс-прокси
(nginx, Caddy). Пример для Caddy:

```
iaam.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

Без прокси токен уходит по сети открытым текстом. Это единственная
причина, по которой прокси обязателен.

## Ограничение частоты

Встроенный ограничитель — фиксированное окно на токен внутри процесса.
Он защищает от зациклившегося агента, **но не от распределённой нагрузки
и не от злоумышленника**: состояние не делится между процессами и
сбрасывается при перезапуске. Ограничение на уровне прокси остаётся
обязательным.

## Бэкап

Копия файла базы не является полноценным бэкапом: она привязана к версии
схемы и к платформе. Регулярно выгружайте архивный бандл — он переносим
и проверяется контрольной суммой при импорте.

```bash
sqlite3 "$IAAM_DATABASE" ".backup /var/backups/iaam-$(date +%F).db"
```

Файл базы содержит весь журнал фактов. Обращайтесь с ним как с выпиской
из банка.
````

- [ ] **Шаг 3: README**

Замените описание проекта в `README.md`:

```markdown
## Что система умеет сейчас

По контуру счетов с ручным вводом и CSV отвечает на три вопроса:
сколько внесено, сколько выведено и какова доходность **до налога**
(XIRR). Цены на инструменты сообщаются событием оценки: рыночных
данных система пока не получает.

## Чего она пока не умеет

Налоги, сверку с отчётами брокера, облигационную и депозитную механику,
TWR, ряд NAV, веб-интерфейс. Порядок появления — в `.internal/specs`.

## Запуск

См. `docs/deployment.md`. Описание API для внешнего агента —
`docs/agent-skill/SKILL.md`, машиночитаемая спека — `/v1/openapi.json`
поднятого сервиса.
```

- [ ] **Шаг 4: Полный прогон перед закрытием**

```bash
nix develop -c cargo fmt --all -- --check
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c cargo test --workspace
nix develop -c cargo test --workspace --doc
nix develop -c ./scripts/check-architecture.sh
nix develop -c ./scripts/check-fixtures.sh
nix develop -c cargo deny check
nix develop -c ./scripts/check-mutants.sh
```

Мутационный прогон долгий. Выживший мутант означает тест, который ничего
не проверяет: добавляйте тест, а не исключение.

- [ ] **Шаг 5: Биды на перенесённую работу**

```bash
bd create "iaam-cli: локальные команды поверх iaam-app" -t task -p 3
bd create "Интервальный XIRR: требует ряда NAV на начало интервала" -t task -p 2
bd create "Справочник инструментов и мест хранения для CSV" -t task -p 2
bd create "Мутационный заслон слеп на исчерпывающих match" -t task -p 3
```

- [ ] **Шаг 6: Коммит и закрытие эпика**

```bash
git add docs README.md
git commit -m "docs: скилл агента, развёртывание и честный README (iaam-1fk)"
bd close iaam-1fk.1 --reason "Второй план E1 выполнен"
bd close iaam-1fk --reason "Этап 1: внесено, выведено, XIRR до налога через API; инварианты исполняются; контрактные тесты зелёные"
bd ready
```

---

## Приёмка эпика E1

Эпик закрывается, когда выполняется каждый пункт:

| Критерий | Где проверяется |
|---|---|
| По одному счёту с ручным вводом система отвечает: сколько внесено, сколько выведено, XIRR до налога | `crates/iaam-server/tests/contract.rs::the_stage_one_question_is_answered_end_to_end` |
| То же на уровне ядра, с ожиданием от независимого эталона | `crates/iaam-core/tests/acceptance_stage1.rs` |
| Инварианты §15.2 исполняются как код и отменяют отчёт при нарушении | `crates/iaam-core/src/projection/invariants.rs`, тест `lots_disagreeing_with_positions_abort_the_projection` |
| Контрактные тесты API против OpenAPI зелёные | `crates/iaam-server/tests/contract.rs::every_documented_path_answers_something_other_than_404` |
| Аутентификация работает с первого дня | `contract.rs::a_request_without_a_token_is_rejected` |
| Журнал append-only на уровне базы | `crates/iaam-store/tests/journal.rs::the_journal_is_append_only_at_the_database_level` |
| Данные восстановимы из переносимого архива, а повреждённый архив отклоняется | `crates/iaam-store/tests/bundle.rs::a_changed_amount_breaks_the_checksum` |
| Событие, добавленное задним числом, не теряется снимком | `crates/iaam-core/src/projection/mod.rs::an_event_inserted_before_the_snapshot_boundary_forces_a_full_recompute` |
| Ставка возвращается только там, где единственность корня доказана | `crates/iaam-core/tests/xirr_solver.rs::two_sign_changes_are_refused_even_when_the_grid_finds_one_bracket` |
| Чужой контур, счёт и снимок недоступны по идентификатору | `crates/iaam-store/tests/snapshots_and_reference.rs::a_contour_of_another_owner_is_not_found` |
| Противоречивое событие не попадает в журнал | `crates/iaam-core/src/projection/mod.rs::a_leg_contradicting_the_event_never_reaches_the_projection` |
| Золотые сценарии этапа 1 покрыты, остальные названы поимённо | `crates/iaam-core/tests/golden_stage1.rs` |
| Все заслоны зелёные | CI: `.github/workflows/ci.yml`, `mutants.yml` |
