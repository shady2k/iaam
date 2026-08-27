# E3.4 часть 2 — график выплат облигаций: план реализации

> **Для агентов-исполнителей:** ОБЯЗАТЕЛЬНАЯ ПОДСКИЛЛА — `beads-superpowers:subagent-driven-development`
> (рекомендуется) либо `beads-superpowers:executing-plans`. Каждая задача становится бидом
> (`bd create -t task --parent <epic-id>`). Шаги внутри задач — чекбоксы `- [ ]` для человека.

**Цель:** график выплат облигации приходит из MOEX ISS в базу как битемпоральный снимок целиком,
с доказанной полнотой и без единой строки, истолкованной разборщиком.

**Архитектура.** `iaam-market` описывает запрос и разбирает ответ в доменные типы, не толкуя коды
источника. `iaam-store` хранит снимок графика целиком, дедуплицируя по хэшу содержимого, и держит
словарь кодов источника. `iaam-app` оркеструет пагинацию, переводит коды через словарь, проверяет
структурные инварианты профиля источника и записывает снимок. Строка графика собственной оси знания
не имеет — ось знания принадлежит снимку.

**Технологии:** Rust (workspace), `rusqlite` + SQLite STRICT, `serde_json`, `time`, `rust_decimal`,
`sha2`, `cargo nextest`, `cargo-mutants`.

Спека: `.internal/specs/2026-08-26-e3-4-bonds-design.md` §2 (переписан 2026-08-27).
Брейншторм: бид `iaam-mps9`. Родительский эпик: `iaam-d8b` (E3).

## Что этот план намеренно не делает

Это **объявленный срез, а не умолчание**. Каждый пункт получает бид.

- **Не строит спецификацию формулы купона (§2.7) и валютную формулу (§2.8).** Обе — справочные
  записи с вводом владельца, и их единственный потребитель — сценарии ставки E3.4.5. Отдельный
  план, часть 3.
- **Не заводит ряд RUONIA (§2.9).** Транспорт тот же, что у ключевой ставки, но без формулы
  флоатера ряд ни на что не влияет. Часть 3.
- **Не реализует `ConflictingSourceFields` из §2.3.** Проверка согласованности суммы и ставки
  требует известной базы начисления дней, а MOEX её не даёт вовсе (§2.11). Вариант, который никто
  не может построить, — мёртвый код; правило вступит в силу вместе с первым источником,
  сообщающим базу. Бид заводится в задаче 1.
- **Не решает, как совмещаются несколько источников графика одного выпуска** (§9 спеки). Пока
  источник один, вопрос не имеет наблюдаемых последствий; построчное слияние запрещено уже сейчас
  тем, что единица хранения — снимок.
- **Не строит `ScheduleDiff` из §2.2.** Разбор изменений между снимками нужен для объяснимости,
  и расчёт от него не зависит — снимок берётся целиком. Потребителя у него сейчас нет: показать
  разницу некому, пока нет ни отчёта об изменениях графика, ни экрана аудита. Бид заводится
  в задаче 4.
- **Не заводит ряд опубликованных фиксингов (§2.6).** Опубликованный фиксинг — наблюдение, но
  единственный, кто его читает, — проекция ставки флоатера, а она в части 3. Ряд без читателя
  дал бы таблицу, которую нечем проверить на осмысленность. Часть 3.
- **Не вычисляет `UsableForCalculation` из §2.10.** Третье утверждение зависит от метрики: НКД
  довольствуется текущим периодом, `W_T` требует графика до погашения. Считать его в отрыве
  от метрики значит выдумать метрику. Приходит вместе с первым потребителем — НКД в E3.4.4.
  Два первых утверждения (`FetchExhausted`, `StructurallyValidated`) хранятся уже здесь.
- **Не выводит окончательность возврата номинала построчно (§2.1).** Правило — накопленная сумма
  долей — реализовано в инварианте полноты (задача 8), но признак «этот возврат погашает выпуск»
  на строке не выставляется: его потребитель, проекция лота, приходит с НКД в E3.4.4. Бид
  заводится в задаче 8.

## Глобальные ограничения

- `cargo build --workspace` и `make check` зелёные **на каждом коммите**. Красное дерево между
  задачами не допускается.
- Крейта `iaam-market` не знает HTTP: транспорт живёт в `iaam-http`, это охраняется
  `scripts/check-architecture.sh`. Крейта описывает запрос (возвращает `HttpRequest`) и разбирает
  байты ответа.
- Крейта `iaam-store` не знает форматов источников: принимает строковые поля и отдаёт строковые
  поля. Преобразование доменных типов в строки — на границе `iaam-app`.
- Наблюдения append-only: исправление — новая строка, `UPDATE` и `DELETE` запрещены триггером,
  как в `0006_market_observations.sql`.
- Ось знания `observed_at` назначается системой, а не берётся из ответа источника.
- Отсутствие значения — `unknown`, никогда не ноль и никогда не значение по умолчанию.
- Неизвестный код источника — явный отказ, а не `Other`, тихо выпадающий из расчёта.
- Правки `tests/fixtures`, `scripts` и `Cargo.toml` — файлы политики
  (`scripts/check-diff-lint.sh:80`). Такие правки идут **отдельным коммитом** с
  `POLICY_CHANGE_APPROVED=1` и меткой PR `policy-change`.
- Следующая свободная миграция — `0010`. `SCHEMA_VERSION` в
  `crates/iaam-store/src/schema.rs:12` поднимается вместе с ней, в том же коммите.
- Русские доккомментарии, объясняющие **почему так, а не иначе**, — принятый стиль репозитория.

---

### Задача 1: доменные типы графика

**Файлы:**
- Создать: `crates/iaam-market/src/schedule/mod.rs`
- Изменить: `crates/iaam-market/src/lib.rs`

**Интерфейсы:**
- Отдаёт: `Knowledge<T>`, `CouponPeriod`, `PrincipalRepayment`, `OfferWindow`, `CouponAmount`,
  `ScheduleSnapshot` — на них опираются задачи 2, 4, 5, 8, 10.

**Критерии приёмки:**
- Строка графика не несёт собственного `observed_at`: ось знания принадлежит снимку.
- `CouponPeriod` различает конец начисления и дату платежа отдельными полями.
- `PrincipalRepayment` несёт долю первоначального номинала, а не сумму.
- Код вида, пришедший от источника, хранится как есть и здесь не толкуется.
- Неизвестное значение выражается `Knowledge::Unknown`, а не значением по умолчанию.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-market/src/schedule/mod.rs` пока только с блоком тестов:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use time::macros::date;

    #[test]
    fn a_coupon_period_keeps_accrual_end_and_payment_date_apart() {
        // Перенос выплаты с выходного двигает дату платежа, но не конец
        // начисления. Одно поле на оба смысла теряет перенос молча, а НКД
        // считается по концу начисления.
        let period = CouponPeriod {
            period_start: date!(2026 - 02 - 15),
            accrual_end: date!(2026 - 08 - 15),
            payment_date: date!(2026 - 08 - 17),
            record_date: Knowledge::Unknown,
            amount: CouponAmount::Undetermined,
            source_entry_id: None,
        };
        assert_ne!(period.accrual_end, period.payment_date);
    }

    #[test]
    fn a_repayment_carries_a_share_not_an_amount() {
        // Сумма зависит от остатка номинала, а остаток выводится из
        // первоначального и ряда возвратов. Хранить сумму значило бы
        // завести второй источник истины рядом с выводом.
        let repayment = PrincipalRepayment {
            repayment_date: date!(2034 - 08 - 09),
            share_percent: Dec::new(Decimal::from(25)),
            source_kind: "amortization".to_owned(),
            source_entry_id: None,
        };
        assert_eq!(repayment.share_percent, Dec::new(Decimal::from(25)));
    }

    #[test]
    fn an_offer_window_without_dates_is_unknown_not_absent() {
        // Источник массово отдаёт окна без дат подачи и без цены.
        // Пустое окно — незнание условий, а не заявление, что окна нет.
        let window = OfferWindow {
            execution_date: date!(2027 - 08 - 26),
            submission_start: Knowledge::Unknown,
            submission_end: Knowledge::Unknown,
            price_percent: Knowledge::Unknown,
            agent: Knowledge::Unknown,
            source_kind: "Оферта".to_owned(),
            source_entry_id: None,
        };
        assert!(matches!(window.price_percent, Knowledge::Unknown));
    }
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-market schedule 2>&1 | head -20`
Expected: FAIL — `cannot find type CouponPeriod in this scope` (модуль ещё не объявлен в `lib.rs`,
типов нет).

- [ ] **Шаг 3: написать типы**

В начало `crates/iaam-market/src/schedule/mod.rs`, перед блоком тестов:

```rust
//! Доменные типы графика выплат (§2.1 спеки E3.4).
//!
//! Разрез идёт по роли строки в расчёте, а не по колонкам источника.
//! `CouponPeriod` даёт поток и базу не двигает; `PrincipalRepayment` даёт
//! поток и уменьшает непогашенный номинал; `OfferWindow` потока не даёт
//! вовсе — он даёт опцию. Общая таблица с видом строки заставила бы
//! каждого потребителя ветвиться по виду, то есть вернула бы тот `match`,
//! который вынесен из разборщика в словарь базы (миграция 0009).
//!
//! Ни один тип здесь не толкует коды источника: вид возврата номинала и
//! вид права по оферте хранятся так, как их назвал источник, и переводятся
//! словарём на границе приложения (§2.5).

use iaam_core::ids::InstrumentId;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use serde::{Deserialize, Serialize};
use time::Date;

use crate::observation::ObservedAt;

/// Знание об атрибуте: известен или неизвестен.
///
/// Отдельный тип, а не `Option`, намеренно: `Option` соблазняет на
/// `unwrap_or_default`, а подставленная по умолчанию база начисления дней
/// даёт правдоподобно неверный НКД, которого не покажет ни один тест на
/// бумаге с целым числом периодов.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Knowledge<T> {
    Known(T),
    Unknown,
}

impl<T> Knowledge<T> {
    /// Известное значение, если оно есть.
    ///
    /// Существует ради чтения; значения по умолчанию тут нет и не будет.
    pub const fn known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown => None,
        }
    }
}

/// Что известно о выплате за купонный период (§2.3).
///
/// Ноль — присутствующее числовое значение, отсутствие — его отрицание.
/// Подмена одного другим занижает и полученный поток, и YTM, и делает это
/// правдоподобно. Статус **не выводится из даты**: у проверенного флоатера
/// купон 2020 года пришёл без суммы и без ставки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CouponAmount {
    /// Сумма на единицу первоначального номинала и её валюта известны.
    AmountFixed { per_unit: Dec, currency: CurrencyCode },
    /// Ставка известна, сумма ещё нет.
    RateFixedAmountUndetermined { rate_percent: Dec },
    /// Ни того, ни другого.
    Undetermined,
}

/// Начисление дохода за купонный период.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CouponPeriod {
    /// Начало периода. Эмитент его не двигает — в отличие от даты платежа.
    pub period_start: Date,
    /// Конец начисления. НКД считается по нему.
    pub accrual_end: Date,
    /// Дата платежа. Двигается переносом с выходного и правкой эмитента.
    pub payment_date: Date,
    /// Дата фиксации права. Источник её сообщает не всегда.
    pub record_date: Knowledge<Date>,
    pub amount: CouponAmount,
    /// Собственный идентификатор записи у источника.
    ///
    /// `Option`, потому что у MOEX его нет вовсе (§2.11). Отсутствие —
    /// нормальное состояние, а не пустое обязательное поле.
    pub source_entry_id: Option<String>,
}

/// Возврат части номинала на дату.
///
/// Окончательность возврата здесь **не хранится**: она выводится из
/// накопленной суммы долей (§2.1). Кода окончательности у источника может
/// не быть вовсе, а вывод, записанный наблюдением, запрещён ADR-0002.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalRepayment {
    pub repayment_date: Date,
    /// Доля **первоначального** номинала, в процентах.
    pub share_percent: Dec,
    /// Как вид назвал источник. Здесь не толкуется.
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Право предъявления к выкупу в окне.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferWindow {
    pub execution_date: Date,
    pub submission_start: Knowledge<Date>,
    pub submission_end: Knowledge<Date>,
    /// Цена выкупа в процентах номинала.
    pub price_percent: Knowledge<Dec>,
    pub agent: Knowledge<String>,
    /// Как вид права назвал источник. У MOEX это свободный русский текст.
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Снимок графика выпуска целиком — единица наблюдения (§2.2).
///
/// Единицей служит снимок, а не строка, потому что построчная модель не
/// умеет выразить **исчезновение** строки: отсутствие новой версии по
/// старой координате неотличимо от «источник не присылал обновлений», и
/// отменённая амортизация остаётся рядом с новым графиком. Стабильного
/// идентификатора, которым эту беду обычно чинят, источник не даёт.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleSnapshot {
    pub instrument: InstrumentId,
    pub observed_at: ObservedAt,
    pub coupon_periods: Vec<CouponPeriod>,
    pub principal_repayments: Vec<PrincipalRepayment>,
    pub offer_windows: Vec<OfferWindow>,
}
```

В `crates/iaam-market/src/lib.rs` добавить объявление модуля и реэкспорт:

```rust
pub mod schedule;
```

```rust
pub use schedule::{
    CouponAmount, CouponPeriod, Knowledge, OfferWindow, PrincipalRepayment, ScheduleSnapshot,
};
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-market schedule`
Expected: PASS, 3 теста.

- [ ] **Шаг 5: завести бид на отложенное правило §2.3**

```bash
bd create "E3.4: проверка согласованности суммы и ставки купона (§2.3)" -t task -p 2 \
  -d "Спека §2.3 требует отказа ConflictingSourceFields при расхождении суммы и ставки. Проверка требует известной базы начисления дней, а MOEX её не даёт вовсе (§2.11), поэтому вариант никто не может построить и он не заведён. Правило вступает в силу вместе с первым источником, сообщающим базу начисления дней.

## Acceptance Criteria

- Строка с расходящимися суммой и ставкой при известной базе начисления не попадает в расчёт.
- Отказ называет обе величины и базу, по которой они сверялись."
```

- [ ] **Шаг 6: коммит**

```bash
git add crates/iaam-market/src/schedule/mod.rs crates/iaam-market/src/lib.rs
git commit -m "feat(market): доменные типы графика выплат (iaam-d8b)"
```

---

### Задача 2: параметры выпуска — две оси времени и знание по атрибуту

**Файлы:**
- Создать: `crates/iaam-market/src/schedule/terms.rs`
- Изменить: `crates/iaam-market/src/schedule/mod.rs`, `crates/iaam-market/src/lib.rs`

**Интерфейсы:**
- Потребляет: `Knowledge<T>` из задачи 1.
- Отдаёт: `IssueTerms`, `DefaultFlags` — на них опираются задачи 3, 9, 10.

**Критерии приёмки:**
- `effective_from` — отдельное поле от `observed_at`, и оно может быть неизвестно.
- База начисления дней и календарь представимы как `Unknown` и значения по умолчанию не имеют.
- Текущий номинал в параметрах отсутствует: он выводится из первоначального и ряда возвратов.
- Признак объявленного дефолта входит в снимок условий.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-market/src/schedule/terms.rs` с блоком тестов:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    fn minimal() -> IssueTerms {
        IssueTerms {
            instrument: InstrumentId::new_random(),
            observed_at: ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
            effective_from: Knowledge::Unknown,
            maturity_date: Knowledge::Known(date!(2036 - 02 - 06)),
            initial_face_value: Knowledge::Known(Dec::new(Decimal::from(1000))),
            face_currency_code: Knowledge::Known("SUR".to_owned()),
            coupon_periods_per_year: Knowledge::Known(2),
            day_count: Knowledge::Unknown,
            calendar: Knowledge::Unknown,
            default_flags: DefaultFlags {
                declared: false,
                technical: false,
            },
        }
    }

    #[test]
    fn effective_from_is_a_separate_axis_from_observed_at() {
        // Правка эмитента, вступающая в силу с будущей даты, при одной оси
        // либо применяется ко всей истории, либо игнорируется на as_of.
        // Подставить observed_at вместо неизвестной даты вступления в силу
        // значит выдать догадку за факт.
        let terms = minimal();
        assert!(matches!(terms.effective_from, Knowledge::Unknown));
        assert!(terms.applies_at(date!(2026 - 08 - 27)));
        assert!(!terms.applies_at(date!(2026 - 08 - 26)));
    }

    #[test]
    fn day_count_and_calendar_have_no_default() {
        // MOEX не даёт ни того, ни другого — ни в графике, ни в описании
        // выпуска. Подставленный day-count даёт правдоподобно неверный НКД.
        let terms = minimal();
        assert!(terms.day_count.known().is_none());
        assert!(terms.calendar.known().is_none());
    }
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-market terms 2>&1 | head -20`
Expected: FAIL — `cannot find type IssueTerms in this scope`.

- [ ] **Шаг 3: написать типы**

В начало `crates/iaam-market/src/schedule/terms.rs`:

```rust
//! Параметры выпуска: две оси времени и знание по каждому атрибуту (§2.4).
//!
//! Ось `observed_at` отвечает на вопрос «когда мы узнали», ось
//! `effective_from` — «с какой даты условия действуют». Одна ось на оба
//! вопроса заставляет отчёт воспроизвести условия, которых на выбранную
//! дату не существовало.

use iaam_core::ids::InstrumentId;
use iaam_core::numeric::decimal::Dec;
use serde::{Deserialize, Serialize};
use time::Date;

use crate::observation::ObservedAt;
use crate::schedule::Knowledge;

/// Объявленный дефолт по выпуску.
///
/// Метрика по бумаге в дефолте, посчитанная так, будто выплаты
/// состоятся, — правдоподобная ложь. Признак обязан доходить до отказа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultFlags {
    pub declared: bool,
    pub technical: bool,
}

/// Снимок условий выпуска — набор утверждений **одного** источника
/// на **один** `observed_at`.
///
/// Собрать одну спецификацию из полей разных наблюдений нельзя: получится
/// выпуск, которого не существовало ни в один момент времени.
///
/// Текущего номинала здесь нет намеренно: он выводится из первоначального
/// и ряда возвратов. Хранить оба значило бы завести два источника истины.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueTerms {
    pub instrument: InstrumentId,
    pub observed_at: ObservedAt,
    /// С какой даты условия действуют. MOEX её не сообщает.
    pub effective_from: Knowledge<Date>,
    pub maturity_date: Knowledge<Date>,
    pub initial_face_value: Knowledge<Dec>,
    /// Код валюты **как его назвал источник**. Перевод — словарём (§2.5).
    pub face_currency_code: Knowledge<String>,
    pub coupon_periods_per_year: Knowledge<u32>,
    /// База начисления дней. У MOEX всегда `Unknown` (§2.11).
    pub day_count: Knowledge<String>,
    /// Календарь. У MOEX всегда `Unknown` (§2.11).
    pub calendar: Knowledge<String>,
    pub default_flags: DefaultFlags,
}

impl IssueTerms {
    /// Действуют ли эти условия на дату `as_of`.
    ///
    /// При неизвестной `effective_from` снимок описывает условия на момент
    /// наблюдения и к более ранним датам не применяется: там действует
    /// предыдущий снимок либо `unknown`. Это отказ вместо угадывания.
    #[must_use]
    pub fn applies_at(&self, as_of: Date) -> bool {
        match &self.effective_from {
            Knowledge::Known(from) => as_of >= *from,
            Knowledge::Unknown => as_of >= self.observed_at.0.date(),
        }
    }
}
```

В `crates/iaam-market/src/schedule/mod.rs` добавить подмодуль сразу после доккомментария модуля:

```rust
pub mod terms;
```

В `crates/iaam-market/src/lib.rs` расширить реэкспорт:

```rust
pub use schedule::terms::{DefaultFlags, IssueTerms};
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-market terms`
Expected: PASS, 2 теста.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-market/src/schedule/terms.rs crates/iaam-market/src/schedule/mod.rs crates/iaam-market/src/lib.rs
git commit -m "feat(market): условия выпуска с двумя осями времени (iaam-d8b)"
```

---

### Задача 3: миграция 0010 — снимки графика, условия выпуска, словарь кодов

**Файлы:**
- Создать: `crates/iaam-store/migrations/0010_bond_schedule.sql`
- Изменить: `crates/iaam-store/src/schema.rs:12` и массив `MIGRATIONS`
- Тест: `crates/iaam-store/tests/migration_0010.rs`

**Интерфейсы:**
- Отдаёт: таблицы `schedule_snapshots`, `schedule_coupon_periods`,
  `schedule_principal_repayments`, `schedule_offer_windows`, `issue_terms`,
  `market_source_codes`, `schedule_completeness` — на них опираются задачи 4, 6, 8.

**Критерии приёмки:**
- База версии 9 мигрирует до 10 без потери данных.
- Снимок append-only: `UPDATE` и `DELETE` по нему запрещены триггером.
- Строка графика ссылается на снимок и собственного `observed_at` не имеет.
- `SCHEMA_VERSION` поднят до 10 в том же коммите, что и миграция.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-store/tests/migration_0010.rs`:

```rust
//! Миграция 0010: снимки графика выплат облигаций.

use rusqlite::Connection;

// База версии 9 собирается применением прежних миграций к пустому
// соединению — тот же приём, что в `migration_0008.rs`. Публичного
// конструктора из готового `Connection` у `SqliteStore` нет, и заводить
// его ради теста значило бы расширять API под тест.
fn database_at_version_nine() -> Connection {
    let conn = Connection::open_in_memory().expect("база в памяти");
    conn.execute_batch(
        "CREATE TABLE instruments (
             id       TEXT PRIMARY KEY,
             symbol   TEXT NOT NULL,
             title    TEXT NOT NULL,
             currency TEXT NOT NULL
         ) STRICT;
         INSERT INTO instruments (id, symbol, title, currency)
         VALUES ('instrument-1', 'SU46020RMFS2', 'ОФЗ 46020', 'RUB');
         PRAGMA user_version = 9;",
    )
    .expect("схема версии 9");
    conn
}

#[test]
fn a_snapshot_row_cannot_be_rewritten() {
    // Снимок — наблюдение: исправление источника ложится новым снимком,
    // а не правкой старого. Иначе воспроизводимость отчёта на прошлую
    // координату знания теряется безвозвратно.
    let conn = database_at_version_nine();
    iaam_store::schema::migrate(&conn).expect("миграция до 10");
    conn.execute_batch(
        "INSERT INTO schedule_snapshots
             (id, instrument_id, source_id, observed_at, content_hash, recorded_at)
         VALUES ('snap-1', 'instrument-1', 'moex-iss',
                 '2026-08-27T12:00:00Z', 'hash-1', '2026-08-27T12:00:00Z');",
    )
    .expect("снимок записан");

    let rewritten = conn.execute(
        "UPDATE schedule_snapshots SET content_hash = 'hash-2' WHERE id = 'snap-1'",
        [],
    );
    assert!(rewritten.is_err(), "правка снимка обязана быть запрещена");

    let deleted = conn.execute("DELETE FROM schedule_snapshots WHERE id = 'snap-1'", []);
    assert!(deleted.is_err(), "удаление снимка обязано быть запрещено");
}

#[test]
fn a_coupon_row_belongs_to_a_snapshot_and_has_no_own_knowledge_axis() {
    // Ось знания принадлежит снимку. Колонка observed_at в строке вернула
    // бы построчную модель, которая не умеет выразить исчезновение строки.
    let conn = database_at_version_nine();
    iaam_store::schema::migrate(&conn).expect("миграция до 10");
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_info('schedule_coupon_periods')")
        .expect("описание таблицы");
    let columns: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .expect("чтение колонок")
        .collect::<Result<_, _>>()
        .expect("колонки");
    assert!(columns.iter().any(|c| c == "snapshot_id"));
    assert!(
        !columns.iter().any(|c| c == "observed_at"),
        "у строки графика своей оси знания быть не должно: {columns:?}"
    );
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-store --test migration_0010 2>&1 | head -20`
Expected: FAIL — `no such table: schedule_snapshots` (миграции 0010 нет).

- [ ] **Шаг 3: написать миграцию**

Создать `crates/iaam-store/migrations/0010_bond_schedule.sql`:

```sql
-- 0010: график выплат облигаций снимками (спека E3.4 §2.2).
--
-- Единица наблюдения — снимок графика выпуска ЦЕЛИКОМ, а не строка.
-- Построчная модель не умеет выразить исчезновение строки: отсутствие
-- новой версии по старой координате неотличимо от «источник не присылал
-- обновлений», и отменённая эмитентом амортизация остаётся рядом с новым
-- графиком, удваивая выплату. Стабильного идентификатора записи, которым
-- эту беду обычно чинят, источник не даёт вовсе.
CREATE TABLE schedule_snapshots (
    id            TEXT PRIMARY KEY,
    instrument_id TEXT NOT NULL REFERENCES instruments(id),
    source_id     TEXT NOT NULL,
    observed_at   TEXT NOT NULL,
    -- Хэш содержимого снимка. Снимок с неизменным содержимым не пишется:
    -- иначе ежедневная синхронизация писала бы неизменный график каждый
    -- день и раздувала ряд в сотни раз.
    content_hash  TEXT NOT NULL,
    recorded_at   TEXT NOT NULL,
    UNIQUE (instrument_id, source_id, observed_at)
) STRICT;

CREATE INDEX schedule_snapshots_by_series
    ON schedule_snapshots (instrument_id, source_id, observed_at);

CREATE TRIGGER schedule_snapshots_are_immutable
BEFORE UPDATE ON schedule_snapshots
BEGIN
    SELECT RAISE(ABORT, 'снимок графика append-only: исправление — новый снимок');
END;

CREATE TRIGGER schedule_snapshots_are_not_deletable
BEFORE DELETE ON schedule_snapshots
BEGIN
    SELECT RAISE(ABORT, 'снимок графика append-only: удаление запрещено');
END;

-- Строки графика. Своей оси знания у них НЕТ намеренно: она принадлежит
-- снимку. Колонка observed_at здесь вернула бы построчную модель.
CREATE TABLE schedule_coupon_periods (
    snapshot_id     TEXT NOT NULL REFERENCES schedule_snapshots(id),
    period_start    TEXT NOT NULL,
    accrual_end     TEXT NOT NULL,
    -- Дата платежа отдельно от конца начисления: перенос с выходного
    -- двигает первую, но не второй, а НКД считается по второму.
    payment_date    TEXT NOT NULL,
    record_date     TEXT,
    -- Статус определённости выплаты. Список закрыт (§2.3).
    amount_status   TEXT NOT NULL,
    -- Сумма на единицу ПЕРВОНАЧАЛЬНОГО номинала. NULL — неизвестно,
    -- и это не ноль: ноль — присутствующее значение.
    amount_per_unit TEXT,
    amount_currency TEXT,
    rate_percent    TEXT,
    source_entry_id TEXT,
    PRIMARY KEY (snapshot_id, period_start),
    CHECK (amount_status IN (
        'amount_fixed', 'rate_fixed_amount_undetermined', 'undetermined'
    )),
    -- Статус и наличие полей обязаны сходиться: строка со статусом
    -- «сумма известна» и пустой суммой — молчаливый ноль в потоке.
    CHECK (
        (amount_status = 'amount_fixed'
             AND amount_per_unit IS NOT NULL AND amount_currency IS NOT NULL)
        OR (amount_status = 'rate_fixed_amount_undetermined'
             AND rate_percent IS NOT NULL AND amount_per_unit IS NULL)
        OR (amount_status = 'undetermined'
             AND amount_per_unit IS NULL AND rate_percent IS NULL)
    ),
    CHECK (accrual_end >= period_start),
    CHECK (payment_date >= accrual_end)
) STRICT;

-- Доля первоначального номинала, а не сумма: сумма зависит от остатка,
-- а остаток выводится. Окончательность возврата здесь не хранится —
-- она свойство проекции, и кода окончательности источник даёт не всегда.
CREATE TABLE schedule_principal_repayments (
    snapshot_id     TEXT NOT NULL REFERENCES schedule_snapshots(id),
    repayment_date  TEXT NOT NULL,
    share_percent   TEXT NOT NULL,
    -- Как вид назвал источник. Множество открыто и источнику принадлежит,
    -- поэтому текст без CHECK: толкование — в словаре market_source_codes.
    source_kind     TEXT NOT NULL,
    source_entry_id TEXT,
    PRIMARY KEY (snapshot_id, repayment_date)
) STRICT;

-- Окно оферты. Пустые условия — незнание, а не заявление об их отсутствии:
-- источник массово отдаёт окна без дат подачи, без цены и без агента.
CREATE TABLE schedule_offer_windows (
    snapshot_id      TEXT NOT NULL REFERENCES schedule_snapshots(id),
    execution_date   TEXT NOT NULL,
    submission_start TEXT,
    submission_end   TEXT,
    price_percent    TEXT,
    agent            TEXT,
    source_kind      TEXT NOT NULL,
    source_entry_id  TEXT,
    PRIMARY KEY (snapshot_id, execution_date)
) STRICT;

-- Условия выпуска: две оси времени. effective_from NULL означает, что
-- источник даты вступления в силу не сообщил, — и это НЕ повод подставить
-- observed_at: догадка, выданная за факт, воспроизводит условия, которых
-- на выбранную дату не существовало.
CREATE TABLE issue_terms (
    instrument_id            TEXT NOT NULL REFERENCES instruments(id),
    source_id                TEXT NOT NULL,
    observed_at              TEXT NOT NULL,
    effective_from           TEXT,
    maturity_date            TEXT,
    initial_face_value       TEXT,
    -- Код валюты как его назвал источник. Перевод — словарём.
    face_currency_code       TEXT,
    coupon_periods_per_year  INTEGER,
    -- База начисления дней и календарь: у MOEX всегда NULL. Значения
    -- по умолчанию здесь запрещены — подставленный day-count даёт
    -- правдоподобно неверный НКД.
    day_count                TEXT,
    calendar                 TEXT,
    default_declared         INTEGER NOT NULL CHECK (default_declared IN (0, 1)),
    default_technical        INTEGER NOT NULL CHECK (default_technical IN (0, 1)),
    recorded_at              TEXT NOT NULL,
    PRIMARY KEY (instrument_id, source_id, observed_at)
) STRICT;

CREATE TRIGGER issue_terms_are_immutable
BEFORE UPDATE ON issue_terms
BEGIN
    SELECT RAISE(ABORT, 'условия выпуска append-only: исправление — новое наблюдение');
END;

CREATE TRIGGER issue_terms_are_not_deletable
BEFORE DELETE ON issue_terms
BEGIN
    SELECT RAISE(ABORT, 'условия выпуска append-only: удаление запрещено');
END;

-- Словарь кодов источника — тот же механизм, что broker_operation_kinds
-- (0009), и по тем же причинам: вид права по оферте у MOEX это свободный
-- русский текст, а один источник даёт два кода на одну валюту (SUR в
-- описании выпуска и RUB в графике одного выпуска). Зашитый в разборщик
-- match ломается от правки формулировки на стороне биржи.
--
-- Члена 'other' здесь нет намеренно: «код, которого нет в словаре»
-- выражается ОТСУТСТВИЕМ строки и даёт явный отказ.
CREATE TABLE market_source_codes (
    source_id   TEXT NOT NULL,
    -- Что именно классифицируется: валюта, вид возврата номинала,
    -- вид права по оферте.
    domain      TEXT NOT NULL,
    source_code TEXT NOT NULL,
    meaning     TEXT NOT NULL,
    -- Откуда строка: наш засев или решение владельца. Без этого решение
    -- владельца неотличимо от засева и молча затирается им.
    origin      TEXT NOT NULL,
    dictionary  TEXT,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (source_id, domain, source_code),
    CHECK (domain IN ('currency', 'principal_repayment_kind', 'offer_kind')),
    CHECK (origin IN ('seed', 'owner'))
) STRICT;

-- Полнота — три независимых утверждения, а не один флаг (§2.10).
-- Полностью вычитанный источник с дырой внутри проходил бы как полный.
CREATE TABLE schedule_completeness (
    snapshot_id            TEXT NOT NULL REFERENCES schedule_snapshots(id),
    -- Источник вычитан до конца по его собственным правилам.
    fetch_exhausted        INTEGER NOT NULL CHECK (fetch_exhausted IN (0, 1)),
    -- Доменные инварианты профиля источника выполнены.
    structurally_validated INTEGER NOT NULL CHECK (structurally_validated IN (0, 1)),
    -- Причина, если инварианты нарушены. Не 'complete_prefix': усечённый
    -- график выглядит замкнутым и правдоподобным.
    incomplete_reason      TEXT,
    -- Просмотренные смещения страниц — след запуска.
    pages_seen             TEXT NOT NULL DEFAULT '[]',
    updated_at             TEXT NOT NULL,
    PRIMARY KEY (snapshot_id),
    CHECK ((structurally_validated = 1) = (incomplete_reason IS NULL))
) STRICT;
```

Изменить `crates/iaam-store/src/schema.rs`: поднять константу и расширить массив.

```rust
/// Версия схемы, которую понимает эта сборка.
pub const SCHEMA_VERSION: u32 = 10;

const MIGRATIONS: [(u32, &str); 10] = [
```

и добавить последним элементом массива:

```rust
    (10, include_str!("../migrations/0010_bond_schedule.sql")),
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-store --test migration_0010`
Expected: PASS, 2 теста.

Run: `cargo test -p iaam-store`
Expected: PASS — прежние миграционные тесты не сломаны.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-store/migrations/0010_bond_schedule.sql crates/iaam-store/src/schema.rs crates/iaam-store/tests/migration_0010.rs
git commit -m "feat(store): схема 0010 — снимки графика выплат (iaam-d8b)"
```

---

### Задача 4: хранилище снимков — запись с дедупом и чтение на координату знания

**Файлы:**
- Создать: `crates/iaam-store/src/schedule.rs`
- Изменить: `crates/iaam-store/src/lib.rs`
- Тест: `crates/iaam-store/tests/bond_schedule.rs`

**Интерфейсы:**
- Потребляет: таблицы из задачи 3.
- Отдаёт: `ScheduleSnapshotRow`, `CouponPeriodRow`, `PrincipalRepaymentRow`, `OfferWindowRow`,
  `StoredSnapshot`, `SqliteStore::record_schedule_snapshot`, `SqliteStore::schedule_at_or_before` —
  на них опирается задача 10.

**Критерии приёмки:**
- Снимок с неизменным `content_hash` повторно не записывается, и это видно в возвращаемом итоге.
- Чтение на `knowledge_as_of` отдаёт **последний** снимок не позже координаты, целиком.
- Строка, отсутствующая в новом снимке, исчезает из результата чтения на более позднюю координату.
- Добавление снимка с более поздним `observed_at` не меняет ответ на меньший `knowledge_as_of`.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-store/tests/bond_schedule.rs`:

```rust
//! Снимки графика выплат: дедуп, чтение на координату знания, исчезновение строк.

use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_store::SqliteStore;
use iaam_store::reference::InstrumentRecord;
use iaam_store::schedule::{
    CouponPeriodRow, OfferWindowRow, PrincipalRepaymentRow, ScheduleSnapshotRow,
};

// Приём взят из `market_observations.rs`: инструмент заводится публичным
// `upsert_instrument`, а не сырым SQL — тест не должен знать схему лучше,
// чем её знает хранилище.
fn store() -> (SqliteStore, InstrumentId) {
    let store = SqliteStore::open_in_memory().expect("база в памяти");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "SU46020RMFS2".to_owned(),
            title: "ОФЗ 46020".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент заведён");
    (store, instrument)
}

fn coupon(period_start: &str, payment: &str) -> CouponPeriodRow {
    CouponPeriodRow {
        period_start: period_start.to_owned(),
        accrual_end: payment.to_owned(),
        payment_date: payment.to_owned(),
        record_date: None,
        amount_status: "undetermined".to_owned(),
        amount_per_unit: None,
        amount_currency: None,
        rate_percent: None,
        source_entry_id: None,
    }
}

fn repayment(date: &str, share: &str) -> PrincipalRepaymentRow {
    PrincipalRepaymentRow {
        repayment_date: date.to_owned(),
        share_percent: share.to_owned(),
        source_kind: "amortization".to_owned(),
        source_entry_id: None,
    }
}

fn snapshot(instrument: InstrumentId, observed_at: &str, hash: &str) -> ScheduleSnapshotRow {
    ScheduleSnapshotRow {
        instrument_id: instrument.inner().to_string(),
        source_id: "moex-iss".to_owned(),
        observed_at: observed_at.to_owned(),
        content_hash: hash.to_owned(),
    }
}

#[test]
fn an_unchanged_snapshot_is_not_written_twice() {
    // Иначе ежедневная синхронизация писала бы неизменный график каждый
    // день, и ряд рос бы в сотни раз без единого нового факта.
    let (mut store, instrument) = store();
    let rows = vec![coupon("2026-02-15", "2026-08-15")];
    let first = store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &rows,
            &[],
            &[],
        )
        .expect("первый снимок");
    let second = store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-28T12:00:00Z", "hash-1"),
            &rows,
            &[],
            &[],
        )
        .expect("повтор с тем же содержимым");
    assert!(first.written, "первый снимок обязан записаться");
    assert!(!second.written, "неизменный снимок писаться не должен");
    assert_eq!(first.snapshot_id, second.snapshot_id);
}

#[test]
fn a_row_missing_from_the_next_snapshot_disappears() {
    // Это то, чего построчная модель не умела: отменённая амортизация
    // обязана исчезнуть, а не остаться рядом с новым графиком.
    let (mut store, instrument) = store();
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &[],
            &[repayment("2034-08-09", "25"), repayment("2035-02-07", "25")],
            &[],
        )
        .expect("снимок с двумя возвратами");
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-28T12:00:00Z", "hash-2"),
            &[],
            &[repayment("2035-02-07", "25")],
            &[],
        )
        .expect("снимок с одним возвратом");

    let later = store
        .schedule_at_or_before(&instrument.inner().to_string(), "moex-iss", "2026-08-29T00:00:00Z")
        .expect("чтение")
        .expect("снимок найден");
    assert_eq!(later.principal_repayments.len(), 1);
    assert_eq!(later.principal_repayments[0].repayment_date, "2035-02-07");
}

#[test]
fn a_later_snapshot_does_not_change_an_earlier_answer() {
    // Свойство монотонности по оси знания: добавление более позднего
    // наблюдения не меняет ответ на меньший knowledge_as_of.
    let (mut store, instrument) = store();
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &[],
            &[repayment("2034-08-09", "25"), repayment("2035-02-07", "25")],
            &[],
        )
        .expect("первый снимок");
    let before = store
        .schedule_at_or_before(&instrument.inner().to_string(), "moex-iss", "2026-08-27T23:59:59Z")
        .expect("чтение")
        .expect("снимок найден");
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-28T12:00:00Z", "hash-2"),
            &[],
            &[repayment("2035-02-07", "25")],
            &[],
        )
        .expect("второй снимок");
    let again = store
        .schedule_at_or_before(&instrument.inner().to_string(), "moex-iss", "2026-08-27T23:59:59Z")
        .expect("чтение")
        .expect("снимок найден");
    assert_eq!(before.principal_repayments, again.principal_repayments);
    assert_eq!(again.principal_repayments.len(), 2);
}

#[test]
fn an_offer_window_without_conditions_reads_back_as_absent_not_zero() {
    // Пустая цена выкупа — незнание условий. Ноль здесь означал бы
    // выкуп даром, и метрика посчиталась бы правдоподобно неверно.
    let (mut store, instrument) = store();
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &[],
            &[],
            &[OfferWindowRow {
                execution_date: "2027-08-26".to_owned(),
                submission_start: None,
                submission_end: None,
                price_percent: None,
                agent: None,
                source_kind: "Оферта".to_owned(),
                source_entry_id: None,
            }],
        )
        .expect("снимок с окном");
    let stored = store
        .schedule_at_or_before(&instrument.inner().to_string(), "moex-iss", "2026-08-27T23:59:59Z")
        .expect("чтение")
        .expect("снимок найден");
    assert_eq!(stored.offer_windows[0].price_percent, None);
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-store --test bond_schedule 2>&1 | head -20`
Expected: FAIL — `could not find schedule in iaam_store`.

- [ ] **Шаг 3: написать хранилище**

Создать `crates/iaam-store/src/schedule.rs`:

```rust
//! Хранение снимков графика выплат (§2.2 спеки E3.4).
//!
//! Хранилище не знает форматов источников: все значения приходят строками
//! и уходят строками. Преобразование доменных типов — на границе
//! приложения, как и у рыночных наблюдений.

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use crate::{SqliteStore, StoreError, now};

/// Заголовок снимка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSnapshotRow {
    pub instrument_id: String,
    pub source_id: String,
    pub observed_at: String,
    pub content_hash: String,
}

/// Строка купонного периода.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouponPeriodRow {
    pub period_start: String,
    pub accrual_end: String,
    pub payment_date: String,
    pub record_date: Option<String>,
    pub amount_status: String,
    pub amount_per_unit: Option<String>,
    pub amount_currency: Option<String>,
    pub rate_percent: Option<String>,
    pub source_entry_id: Option<String>,
}

/// Строка возврата номинала.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalRepaymentRow {
    pub repayment_date: String,
    pub share_percent: String,
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Строка окна оферты.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferWindowRow {
    pub execution_date: String,
    pub submission_start: Option<String>,
    pub submission_end: Option<String>,
    pub price_percent: Option<String>,
    pub agent: Option<String>,
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Итог записи снимка.
///
/// `written = false` означает, что содержимое совпало с последним снимком
/// и новой записи не потребовалось. Это не ошибка и молчать об этом нельзя:
/// «записали» и «уже было то же самое» — разные события для следа запуска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotOutcome {
    pub snapshot_id: String,
    pub written: bool,
}

/// Снимок, прочитанный на координату знания.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSnapshot {
    pub snapshot_id: String,
    pub observed_at: String,
    pub coupon_periods: Vec<CouponPeriodRow>,
    pub principal_repayments: Vec<PrincipalRepaymentRow>,
    pub offer_windows: Vec<OfferWindowRow>,
}

impl SqliteStore {
    /// Записать снимок графика целиком.
    ///
    /// Если содержимое совпадает с последним снимком того же ряда, новая
    /// запись не создаётся: снимок наблюдением не является, если ничего
    /// не наблюдалось заново.
    pub fn record_schedule_snapshot(
        &mut self,
        header: &ScheduleSnapshotRow,
        coupon_periods: &[CouponPeriodRow],
        principal_repayments: &[PrincipalRepaymentRow],
        offer_windows: &[OfferWindowRow],
    ) -> Result<SnapshotOutcome, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let latest: Option<(String, String)> = transaction
            .query_row(
                "SELECT id, content_hash FROM schedule_snapshots
                 WHERE instrument_id = ?1 AND source_id = ?2
                 ORDER BY observed_at DESC, id DESC
                 LIMIT 1",
                params![&header.instrument_id, &header.source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, hash)) = latest
            && hash == header.content_hash
        {
            transaction.commit()?;
            return Ok(SnapshotOutcome {
                snapshot_id: id,
                written: false,
            });
        }

        let id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO schedule_snapshots
                 (id, instrument_id, source_id, observed_at, content_hash, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &id,
                &header.instrument_id,
                &header.source_id,
                &header.observed_at,
                &header.content_hash,
                now(),
            ],
        )?;
        for row in coupon_periods {
            transaction.execute(
                "INSERT INTO schedule_coupon_periods
                     (snapshot_id, period_start, accrual_end, payment_date, record_date,
                      amount_status, amount_per_unit, amount_currency, rate_percent,
                      source_entry_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    &id,
                    &row.period_start,
                    &row.accrual_end,
                    &row.payment_date,
                    &row.record_date,
                    &row.amount_status,
                    &row.amount_per_unit,
                    &row.amount_currency,
                    &row.rate_percent,
                    &row.source_entry_id,
                ],
            )?;
        }
        for row in principal_repayments {
            transaction.execute(
                "INSERT INTO schedule_principal_repayments
                     (snapshot_id, repayment_date, share_percent, source_kind, source_entry_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    &id,
                    &row.repayment_date,
                    &row.share_percent,
                    &row.source_kind,
                    &row.source_entry_id,
                ],
            )?;
        }
        for row in offer_windows {
            transaction.execute(
                "INSERT INTO schedule_offer_windows
                     (snapshot_id, execution_date, submission_start, submission_end,
                      price_percent, agent, source_kind, source_entry_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &id,
                    &row.execution_date,
                    &row.submission_start,
                    &row.submission_end,
                    &row.price_percent,
                    &row.agent,
                    &row.source_kind,
                    &row.source_entry_id,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(SnapshotOutcome {
            snapshot_id: id,
            written: true,
        })
    }

    /// Последний снимок не позже координаты знания, целиком.
    ///
    /// Целиком, а не построчно: строки разных снимков не смешиваются —
    /// собранный из них график описывал бы выпуск, которого не
    /// существовало ни в один момент времени.
    pub fn schedule_at_or_before(
        &self,
        instrument_id: &str,
        source_id: &str,
        knowledge_as_of: &str,
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        let header: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT id, observed_at FROM schedule_snapshots
                 WHERE instrument_id = ?1 AND source_id = ?2 AND observed_at <= ?3
                 ORDER BY observed_at DESC, id DESC
                 LIMIT 1",
                params![instrument_id, source_id, knowledge_as_of],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((snapshot_id, observed_at)) = header else {
            return Ok(None);
        };

        let mut coupons = self.conn.prepare(
            "SELECT period_start, accrual_end, payment_date, record_date, amount_status,
                    amount_per_unit, amount_currency, rate_percent, source_entry_id
             FROM schedule_coupon_periods WHERE snapshot_id = ?1 ORDER BY period_start",
        )?;
        let coupon_periods = coupons
            .query_map([&snapshot_id], |row| {
                Ok(CouponPeriodRow {
                    period_start: row.get(0)?,
                    accrual_end: row.get(1)?,
                    payment_date: row.get(2)?,
                    record_date: row.get(3)?,
                    amount_status: row.get(4)?,
                    amount_per_unit: row.get(5)?,
                    amount_currency: row.get(6)?,
                    rate_percent: row.get(7)?,
                    source_entry_id: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut repayments = self.conn.prepare(
            "SELECT repayment_date, share_percent, source_kind, source_entry_id
             FROM schedule_principal_repayments WHERE snapshot_id = ?1 ORDER BY repayment_date",
        )?;
        let principal_repayments = repayments
            .query_map([&snapshot_id], |row| {
                Ok(PrincipalRepaymentRow {
                    repayment_date: row.get(0)?,
                    share_percent: row.get(1)?,
                    source_kind: row.get(2)?,
                    source_entry_id: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut windows = self.conn.prepare(
            "SELECT execution_date, submission_start, submission_end, price_percent,
                    agent, source_kind, source_entry_id
             FROM schedule_offer_windows WHERE snapshot_id = ?1 ORDER BY execution_date",
        )?;
        let offer_windows = windows
            .query_map([&snapshot_id], |row| {
                Ok(OfferWindowRow {
                    execution_date: row.get(0)?,
                    submission_start: row.get(1)?,
                    submission_end: row.get(2)?,
                    price_percent: row.get(3)?,
                    agent: row.get(4)?,
                    source_kind: row.get(5)?,
                    source_entry_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Some(StoredSnapshot {
            snapshot_id,
            observed_at,
            coupon_periods,
            principal_repayments,
            offer_windows,
        }))
    }
}
```

В `crates/iaam-store/src/lib.rs` объявить модуль рядом с остальными:

```rust
pub mod schedule;
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-store --test bond_schedule`
Expected: PASS, 4 теста.

- [ ] **Шаг 5: завести бид на отложенный `ScheduleDiff`**

```bash
bd create "E3.4: разбор изменений между снимками графика (ScheduleDiff, §2.2)" -t task -p 3 \
  -d "Спека §2.2 объявляет ScheduleDiff = Added | Removed | Moved | Amended | Ambiguous для объяснимости: назвать поимённо, что изменилось между снимками. Расчёт от него не зависит — снимок берётся целиком. Потребителя нет, пока нет отчёта об изменениях графика или экрана аудита.

## Acceptance Criteria

- Разница между двумя снимками названа поимённо, а не числом изменившихся строк.
- Сдвиг даты возврата номинала, неотличимый от объявления нового, помечается Ambiguous и расчёт не блокирует."
```

- [ ] **Шаг 6: коммит**

```bash
git add crates/iaam-store/src/schedule.rs crates/iaam-store/src/lib.rs crates/iaam-store/tests/bond_schedule.rs
git commit -m "feat(store): снимок графика с дедупом и чтением на координату (iaam-d8b)"
```

---

### Задача 5: разбор ответа графика — без единого истолкованного кода

**Файлы:**
- Создать: `crates/iaam-market/src/moex/bondization.rs`
- Изменить: `crates/iaam-market/src/moex/mod.rs`
- Тест: внутри `bondization.rs`, на фикстуре задачи 11 (до неё — на встроенном литерале)

**Интерфейсы:**
- Потребляет: доменные типы задачи 1.
- Отдаёт: `parse_bondization_page(body, instrument, observed_at) -> Result<BondizationPage, MarketError>`,
  `BondizationPage { coupon_periods, principal_repayments, offer_windows }` — на них опираются
  задачи 7, 8, 10.

**Критерии приёмки:**
- Индексы колонок берутся из `columns` по имени, а не зашиваются числами.
- Коды вида возврата номинала и вида права по оферте попадают в доменный тип **как есть**.
- `value` и `valueprc` истолковываются независимо: null у одного не делает нулём другой.
- Поле текущего номинала строки игнорируется полностью.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-market/src/moex/bondization.rs` с блоком тестов:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const PAGE: &str = r#"{
      "amortizations": {
        "columns": ["amortdate", "facevalue", "initialfacevalue", "valueprc",
                    "value", "value_rub", "data_source"],
        "data": [["2034-08-09", 375, 1000, 25, 250, 250, "amortization"]]
      },
      "coupons": {
        "columns": ["coupondate", "recorddate", "startdate", "initialfacevalue",
                    "facevalue", "faceunit", "value", "valueprc", "value_rub"],
        "data": [
          ["2026-08-15", null, "2026-02-15", 1000, 375, "RUB", null, null, null],
          ["2027-02-15", "2027-02-14", "2026-08-15", 1000, 375, "RUB", 34.41, 6.9, 34.41]
        ]
      },
      "offers": {
        "columns": ["offerdate", "offerdatestart", "offerdateend", "price",
                    "value", "agent", "offertype"],
        "data": [["2027-08-26", null, null, null, null, null, "Оферта"]]
      }
    }"#;

    fn parsed() -> BondizationPage {
        parse_bondization_page(
            PAGE.as_bytes(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .expect("страница разобрана")
    }

    #[test]
    fn a_missing_amount_stays_unknown_and_does_not_become_zero() {
        // У проверенного флоатера прошедший купон приходит без суммы и без
        // ставки. Ноль здесь занизил бы и поток, и YTM, и сделал бы это
        // правдоподобно.
        let page = parsed();
        assert_eq!(page.coupon_periods[0].amount, CouponAmount::Undetermined);
    }

    #[test]
    fn a_known_amount_carries_its_currency() {
        let page = parsed();
        assert!(matches!(
            page.coupon_periods[1].amount,
            CouponAmount::AmountFixed { .. }
        ));
    }

    #[test]
    fn the_source_kind_arrives_uninterpreted() {
        // Разборщик кодов не толкует: вид права по оферте у MOEX это
        // свободный русский текст, и match по нему сломается от правки
        // формулировки на стороне биржи.
        let page = parsed();
        assert_eq!(page.principal_repayments[0].source_kind, "amortization");
        assert_eq!(page.offer_windows[0].source_kind, "Оферта");
    }

    #[test]
    fn the_row_face_value_is_ignored_entirely() {
        // Поле номинала в строке — номинал бумаги НА МОМЕНТ ЗАПРОСА:
        // у бумаги, прошедшей часть амортизаций, все строки за все годы
        // показывают текущий остаток. Принять его за номинал периода
        // значит задним числом пересчитать всю историю.
        let page = parsed();
        // Возврат несёт долю первоначального номинала, а не сумму,
        // выведенную из показанных 375.
        assert_eq!(
            page.principal_repayments[0].share_percent.inner().to_string(),
            "25"
        );
    }

    #[test]
    fn an_offer_without_conditions_is_unknown() {
        let page = parsed();
        assert!(matches!(
            page.offer_windows[0].price_percent,
            Knowledge::Unknown
        ));
    }
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-market bondization 2>&1 | head -20`
Expected: FAIL — `cannot find function parse_bondization_page in this scope`.

- [ ] **Шаг 3: написать разбор**

В начало `crates/iaam-market/src/moex/bondization.rs`:

```rust
//! Разбор графика выплат MOEX ISS.
//!
//! Ответ табличный: `columns` с именами и `data` со строками. Индексы
//! берутся из `columns` по имени, а не зашиваются числами: ISS добавляет
//! колонки, и позиционный разбор однажды прочитает долю как дату.
//!
//! Разборщик **не толкует коды**. Вид возврата номинала и вид права по
//! оферте доходят до домена как есть; перевод — словарём (§2.5). У MOEX
//! вид права это свободный русский текст, и `match` по нему сломался бы
//! от правки формулировки на стороне биржи.
//!
//! Поле номинала в строке игнорируется целиком: это номинал бумаги на
//! момент запроса, а не номинал периода (§2.11).

use rust_decimal::Decimal;
use serde_json::Value;
use time::Date;
use time::format_description::well_known::Iso8601;

use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;

use crate::error::MarketError;
use crate::observation::ObservedAt;
use crate::schedule::{CouponAmount, CouponPeriod, Knowledge, OfferWindow, PrincipalRepayment};

/// Одна страница ответа. Пагинация — забота вызывающего (§2.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondizationPage {
    pub coupon_periods: Vec<CouponPeriod>,
    pub principal_repayments: Vec<PrincipalRepayment>,
    pub offer_windows: Vec<OfferWindow>,
    /// Сколько строк пришло во всех блоках вместе.
    ///
    /// Нужно вызывающему, чтобы отличить «страница пуста» от «блок пуст»:
    /// смещение у блоков общее, и амортизации кончаются раньше купонов.
    pub total_rows: usize,
}

fn block<'a>(root: &'a Value, name: &str) -> Result<(&'a Vec<Value>, Vec<String>), MarketError> {
    let node = root
        .get(name)
        .ok_or_else(|| MarketError::Malformed(format!("нет блока {name}")))?;
    let columns = node
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed(format!("нет columns у {name}")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| MarketError::Malformed(format!("имя колонки {name} не строка")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data = node
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed(format!("нет data у {name}")))?;
    Ok((data, columns))
}

fn cell<'a>(columns: &[String], row: &'a Value, name: &str) -> Option<&'a Value> {
    let index = columns.iter().position(|column| column == name)?;
    let value = row.get(index)?;
    if value.is_null() { None } else { Some(value) }
}

fn date_of(columns: &[String], row: &Value, name: &str) -> Result<Option<Date>, MarketError> {
    let Some(value) = cell(columns, row, name) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| MarketError::Malformed(format!("{name} не строка")))?;
    Date::parse(text, &Iso8601::DATE)
        .map(Some)
        .map_err(|_| MarketError::Malformed(format!("{name} не дата: {text}")))
}

fn required_date(columns: &[String], row: &Value, name: &str) -> Result<Date, MarketError> {
    date_of(columns, row, name)?
        .ok_or_else(|| MarketError::Malformed(format!("{name} обязателен и пуст")))
}

fn decimal_of(columns: &[String], row: &Value, name: &str) -> Result<Option<Dec>, MarketError> {
    let Some(value) = cell(columns, row, name) else {
        return Ok(None);
    };
    let text = if let Some(text) = value.as_str() {
        text.to_owned()
    } else {
        value.to_string()
    };
    text.parse::<Decimal>()
        .map(|decimal| Some(Dec::new(decimal)))
        .map_err(|_| MarketError::Malformed(format!("{name} не число: {text}")))
}

fn text_of(columns: &[String], row: &Value, name: &str) -> Option<String> {
    cell(columns, row, name).and_then(|value| value.as_str().map(str::to_owned))
}

fn knowledge<T>(value: Option<T>) -> Knowledge<T> {
    value.map_or(Knowledge::Unknown, Knowledge::Known)
}

/// Разобрать одну страницу ответа `/bondization`.
///
/// `observed_at` назначает вызывающий: доверить ось знания часам источника
/// значит сделать её подделываемой ответом.
pub fn parse_bondization_page(
    body: &[u8],
    _observed_at: ObservedAt,
) -> Result<BondizationPage, MarketError> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|error| MarketError::Malformed(error.to_string()))?;

    let (coupon_rows, coupon_columns) = block(&root, "coupons")?;
    let mut coupon_periods = Vec::with_capacity(coupon_rows.len());
    for row in coupon_rows {
        let period_start = required_date(&coupon_columns, row, "startdate")?;
        // Конец начисления и дата платежа — разные смыслы. Источник даёт
        // одно значение на оба; различие сохраняется, потому что перенос
        // с выходного двигает вторую, но не первый.
        let coupon_date = required_date(&coupon_columns, row, "coupondate")?;
        let per_unit = decimal_of(&coupon_columns, row, "value")?;
        let rate_percent = decimal_of(&coupon_columns, row, "valueprc")?;
        let currency = text_of(&coupon_columns, row, "faceunit");
        // Поля толкуются независимо: null у суммы не делает нулём ставку.
        let amount = match (per_unit, rate_percent, currency) {
            (Some(per_unit), _, Some(code)) => CouponAmount::AmountFixed {
                per_unit,
                currency: crate::moex::parse::currency_of(&code)?,
            },
            (None, Some(rate_percent), _) => {
                CouponAmount::RateFixedAmountUndetermined { rate_percent }
            }
            _ => CouponAmount::Undetermined,
        };
        coupon_periods.push(CouponPeriod {
            period_start,
            accrual_end: coupon_date,
            payment_date: coupon_date,
            record_date: knowledge(date_of(&coupon_columns, row, "recorddate")?),
            amount,
            source_entry_id: None,
        });
    }

    let (amort_rows, amort_columns) = block(&root, "amortizations")?;
    let mut principal_repayments = Vec::with_capacity(amort_rows.len());
    for row in amort_rows {
        principal_repayments.push(PrincipalRepayment {
            repayment_date: required_date(&amort_columns, row, "amortdate")?,
            share_percent: decimal_of(&amort_columns, row, "valueprc")?.ok_or_else(|| {
                MarketError::Malformed("возврат номинала без доли".to_owned())
            })?,
            source_kind: text_of(&amort_columns, row, "data_source").ok_or_else(|| {
                MarketError::Malformed("возврат номинала без вида".to_owned())
            })?,
            source_entry_id: None,
        });
    }

    let (offer_rows, offer_columns) = block(&root, "offers")?;
    let mut offer_windows = Vec::with_capacity(offer_rows.len());
    for row in offer_rows {
        offer_windows.push(OfferWindow {
            execution_date: required_date(&offer_columns, row, "offerdate")?,
            submission_start: knowledge(date_of(&offer_columns, row, "offerdatestart")?),
            submission_end: knowledge(date_of(&offer_columns, row, "offerdateend")?),
            price_percent: knowledge(decimal_of(&offer_columns, row, "price")?),
            agent: knowledge(text_of(&offer_columns, row, "agent")),
            source_kind: text_of(&offer_columns, row, "offertype")
                .ok_or_else(|| MarketError::Malformed("окно оферты без вида".to_owned()))?,
            source_entry_id: None,
        });
    }

    let total_rows = coupon_periods.len() + principal_repayments.len() + offer_windows.len();
    Ok(BondizationPage {
        coupon_periods,
        principal_repayments,
        offer_windows,
        total_rows,
    })
}
```

В `crates/iaam-market/src/moex/mod.rs` объявить подмодуль рядом с `pub mod parse;`:

```rust
pub mod bondization;
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-market bondization`
Expected: PASS, 5 тестов.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-market/src/moex/bondization.rs crates/iaam-market/src/moex/mod.rs
git commit -m "feat(market): разбор графика выплат без толкования кодов (iaam-d8b)"
```

---

### Задача 6: словарь кодов источника — засев и чтение

**Файлы:**
- Создать: `crates/iaam-store/src/market_source_codes.rs`,
  `crates/iaam-market/src/moex/dictionary_seed.rs`
- Изменить: `crates/iaam-store/src/lib.rs`, `crates/iaam-market/src/moex/mod.rs`
- Тест: `crates/iaam-store/tests/market_source_codes.rs`

**Интерфейсы:**
- Потребляет: таблицу `market_source_codes` из задачи 3.
- Отдаёт: `SqliteStore::extend_market_source_codes`, `SqliteStore::market_source_codes`,
  `MOEX_SOURCE_CODES` — на них опирается задача 10.

**Критерии приёмки:**
- Засев не трогает существующие строки: решение владельца не отменяется пополнением.
- Оба кода рубля одного источника (`SUR` и `RUB`) переводятся в одну валюту.
- Неизвестный код в словаре отсутствует, и это выражается отсутствием строки, а не членом `other`.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-store/tests/market_source_codes.rs`:

```rust
//! Словарь кодов рыночного источника: засев, решение владельца, чтение.

use iaam_store::SqliteStore;
use iaam_store::market_source_codes::SourceCodeEntry;

fn store() -> SqliteStore {
    SqliteStore::open_in_memory().expect("база в памяти")
}

fn entry(domain: &str, code: &str, meaning: &str) -> SourceCodeEntry {
    SourceCodeEntry {
        domain: domain.to_owned(),
        source_code: code.to_owned(),
        meaning: meaning.to_owned(),
    }
}

#[test]
fn both_rouble_codes_of_one_source_mean_one_currency() {
    // Один источник даёт SUR в описании выпуска и RUB в графике того же
    // выпуска. Без словаря это две разные валюты, и позиции разъезжаются.
    let mut store = store();
    store
        .extend_market_source_codes(
            "moex-iss",
            "профиль источника 2026-08-27",
            &[
                entry("currency", "SUR", "RUB"),
                entry("currency", "RUB", "RUB"),
            ],
        )
        .expect("засев");
    let dictionary = store
        .market_source_codes("moex-iss", "currency")
        .expect("чтение");
    assert_eq!(dictionary.get("SUR").map(String::as_str), Some("RUB"));
    assert_eq!(dictionary.get("RUB").map(String::as_str), Some("RUB"));
}

#[test]
fn seeding_does_not_override_an_owner_decision() {
    // Иначе решение владельца отменялось бы при каждом заведении источника,
    // и расхождение было бы неотличимо от решения.
    let mut store = store();
    store
        .set_market_source_code(
            "moex-iss",
            &entry("offer_kind", "Оферта", "put_option"),
        )
        .expect("решение владельца");
    let outcome = store
        .extend_market_source_codes(
            "moex-iss",
            "профиль источника 2026-08-27",
            &[entry("offer_kind", "Оферта", "call_option")],
        )
        .expect("засев");
    assert_eq!(outcome.added, 0);
    assert_eq!(outcome.already_known, 1);
    let dictionary = store
        .market_source_codes("moex-iss", "offer_kind")
        .expect("чтение");
    assert_eq!(dictionary.get("Оферта").map(String::as_str), Some("put_option"));
}

#[test]
fn an_unknown_code_is_absent_rather_than_other() {
    // «Кода нет в словаре» выражается отсутствием строки. Член 'other'
    // означал бы принятое решение не разбирать — а такого не принимали.
    let store = store();
    let dictionary = store
        .market_source_codes("moex-iss", "offer_kind")
        .expect("чтение");
    assert!(dictionary.get("Досрочное погашение").is_none());
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-store --test market_source_codes 2>&1 | head -20`
Expected: FAIL — `could not find market_source_codes in iaam_store`.

- [ ] **Шаг 3: написать хранилище словаря и засев**

Создать `crates/iaam-store/src/market_source_codes.rs`, повторяя устройство
`crates/iaam-store/src/broker_operation_kinds.rs` (прочитать его перед написанием — там уже
решены `DictionaryOutcome`, `origin` и «не затирать решение владельца»):

```rust
//! Словарь кодов рыночного источника (§2.5 спеки E3.4).
//!
//! Тот же механизм, что `broker_operation_kinds`, и по той же причине:
//! множество кодов принадлежит источнику, а не нам. Вид права по оферте
//! у MOEX — свободный русский текст, и `match` по нему ломается от правки
//! формулировки на стороне биржи.

use std::collections::BTreeMap;

use rusqlite::{TransactionBehavior, params};

use crate::{SqliteStore, StoreError, now};

/// Строка словаря.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCodeEntry {
    pub domain: String,
    pub source_code: String,
    pub meaning: String,
}

/// Итог пополнения.
///
/// `already_known` считается отдельно: «добавили» и «уже знали» — разные
/// события, и слить их значит потерять признак расхождения с источником.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictionaryOutcome {
    pub added: usize,
    pub already_known: usize,
}

impl SqliteStore {
    /// Пополнить словарь засевом. Существующие строки не трогаются.
    pub fn extend_market_source_codes(
        &mut self,
        source_id: &str,
        dictionary: &str,
        entries: &[SourceCodeEntry],
    ) -> Result<DictionaryOutcome, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut added = 0;
        let mut already_known = 0;
        for entry in entries {
            let inserted = transaction.execute(
                "INSERT OR IGNORE INTO market_source_codes
                     (source_id, domain, source_code, meaning, origin, dictionary, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, 'seed', ?5, ?6)",
                params![
                    source_id,
                    &entry.domain,
                    &entry.source_code,
                    &entry.meaning,
                    dictionary,
                    now(),
                ],
            )?;
            if inserted == 0 {
                already_known += 1;
            } else {
                added += 1;
            }
        }
        transaction.commit()?;
        Ok(DictionaryOutcome {
            added,
            already_known,
        })
    }

    /// Записать решение владельца. Оно перекрывает засев и им не затирается.
    pub fn set_market_source_code(
        &mut self,
        source_id: &str,
        entry: &SourceCodeEntry,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO market_source_codes
                 (source_id, domain, source_code, meaning, origin, dictionary, recorded_at)
             VALUES (?1, ?2, ?3, ?4, 'owner', NULL, ?5)
             ON CONFLICT (source_id, domain, source_code)
             DO UPDATE SET meaning = excluded.meaning, origin = 'owner',
                           dictionary = NULL, recorded_at = excluded.recorded_at",
            params![
                source_id,
                &entry.domain,
                &entry.source_code,
                &entry.meaning,
                now(),
            ],
        )?;
        Ok(())
    }

    /// Прочитать словарь одной области целиком.
    pub fn market_source_codes(
        &self,
        source_id: &str,
        domain: &str,
    ) -> Result<BTreeMap<String, String>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT source_code, meaning FROM market_source_codes
             WHERE source_id = ?1 AND domain = ?2",
        )?;
        let rows = statement
            .query_map(params![source_id, domain], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(rows)
    }
}
```

В `crates/iaam-store/src/lib.rs`:

```rust
pub mod market_source_codes;
```

Создать `crates/iaam-market/src/moex/dictionary_seed.rs`:

```rust
//! Начальный словарь кодов MOEX ISS (§2.5).
//!
//! Это **наше** знание, а не биржевое: источник перечисляет коды, но не
//! сообщает, что `SUR` и `RUB` для нас один рубль, а `maturity` и
//! `amortization` оба возвращают номинал. Поэтому таблица живёт в коде и
//! попадает в базу один раз.
//!
//! Дальше она **не** источник истины: словарь редактируется в базе, и
//! пополнение отсюда существующие строки не трогает.
//!
//! Виды права по оферте перечислены теми формулировками, какие наблюдались
//! живой проверкой 2026-08-27. Формулировка — не код, и биржа вправе её
//! поменять; неперечисленная формулировка даёт отказ, а не тихий пропуск.

/// Тройки «область → код источника → доменный смысл».
pub const MOEX_SOURCE_CODES: &[(&str, &str, &str)] = &[
    // Один источник, два кода на одну валюту.
    ("currency", "SUR", "RUB"),
    ("currency", "RUB", "RUB"),
    ("currency", "USD", "USD"),
    ("currency", "EUR", "EUR"),
    // Код окончательности здесь НЕ толкуется: окончательность выводится
    // из накопленной суммы долей, потому что 'maturity' источник даёт
    // не всегда — из 50 проверенных бумаг у шести его нет вовсе.
    ("principal_repayment_kind", "amortization", "principal_return"),
    ("principal_repayment_kind", "maturity", "principal_return"),
    ("offer_kind", "Оферта", "put_option"),
    ("offer_kind", "Оферта (состоялось)", "put_option_settled"),
];
```

В `crates/iaam-market/src/moex/mod.rs`:

```rust
pub mod dictionary_seed;
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-store --test market_source_codes`
Expected: PASS, 3 теста.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-store/src/market_source_codes.rs crates/iaam-store/src/lib.rs crates/iaam-store/tests/market_source_codes.rs crates/iaam-market/src/moex/dictionary_seed.rs crates/iaam-market/src/moex/mod.rs
git commit -m "feat(store): словарь кодов рыночного источника (iaam-d8b)"
```

---

### Задача 7: запрос графика и пагинация — конец выборки доказывается, а не предполагается

**Файлы:**
- Изменить: `crates/iaam-market/src/moex/mod.rs`
- Тест: внутри `crates/iaam-market/src/moex/mod.rs`

**Интерфейсы:**
- Отдаёт: `ScheduleQuery`, `schedule_request(query) -> HttpRequest`, `PAGE_LIMIT` — на них
  опирается задача 10.

**Критерии приёмки:**
- Смещение страницы входит в запрос параметром и не зашито нулём.
- Размер страницы запрашивается фактическим потолком источника, а не большим числом.
- Доккомментарий называет ловушку молчаливого усечения поимённо.

- [ ] **Шаг 1: написать падающий тест**

Добавить в блок тестов `crates/iaam-market/src/moex/mod.rs`:

```rust
    #[test]
    fn the_schedule_request_carries_an_explicit_offset() {
        // Запрос без смещения возвращает первую страницу, а первая
        // страница у длинного выпуска короче графика на годы — и при этом
        // выглядит замкнутой.
        let request = schedule_request(ScheduleQuery {
            secid: "SU46020RMFS2",
            start: 100,
        });
        let url = request.url();
        assert!(url.contains("/securities/SU46020RMFS2/bondization.json"), "{url}");
        assert!(url.contains("start=100"), "{url}");
    }

    #[test]
    fn the_page_limit_is_the_actual_ceiling_not_a_wish() {
        // Источник молча режет запрошенный лимит до сотни: лимит 1000
        // отдаёт 100 строк без всякой ошибки. Просить больше потолка
        // значит договориться с собой о размере страницы, которого нет.
        assert_eq!(PAGE_LIMIT, 100);
    }
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-market moex 2>&1 | head -20`
Expected: FAIL — `cannot find function schedule_request in this scope`.

- [ ] **Шаг 3: написать запрос**

Добавить в `crates/iaam-market/src/moex/mod.rs` после `history_request`:

```rust
/// Фактический потолок страницы у источника.
///
/// Не пожелание, а измеренная величина: запрошенный лимит 1000 отдаёт
/// 100 строк **без всякой ошибки**. У проверенного выпуска с погашением
/// в 2048 году первая страница вернула 100 купонов с хвостом 2038 и
/// замкнутой цепью периодов — график выглядел полным и был короче на
/// десять лет.
pub const PAGE_LIMIT: u32 = 100;

/// Координата запроса графика выплат.
///
/// Смещение полем, а не константой: пагинация обязательна, и запрос,
/// умеющий только первую страницу, молча укорачивает длинные выпуски.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleQuery<'a> {
    /// Код бумаги на площадке.
    pub secid: &'a str,
    /// Смещение страницы. Общее на все три блока ответа: на второй
    /// странице амортизации и оферты уже пусты, а купоны продолжаются,
    /// поэтому пустота одного блока концом выборки не является.
    pub start: u32,
}

/// Запрос одной страницы графика выплат по бумаге.
#[must_use]
pub fn schedule_request(query: ScheduleQuery<'_>) -> HttpRequest {
    let ScheduleQuery { secid, start } = query;
    let path = format!("/iss/securities/{secid}/bondization.json");
    HttpRequest::get(Destination::MoexIss, &path)
        .with_query("limit", &PAGE_LIMIT.to_string())
        .with_query("start", &start.to_string())
        .with_query("iss.meta", "off")
}
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-market moex`
Expected: PASS — прежние тесты плюс два новых.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-market/src/moex/mod.rs
git commit -m "feat(market): запрос графика выплат со смещением страницы (iaam-d8b)"
```

---

### Задача 8: структурные инварианты профиля источника

**Файлы:**
- Создать: `crates/iaam-market/src/schedule/completeness.rs`
- Изменить: `crates/iaam-market/src/schedule/mod.rs`, `crates/iaam-market/src/lib.rs`

**Интерфейсы:**
- Потребляет: доменные типы задачи 1.
- Отдаёт: `Completeness`, `validate_moex_profile(coupons, repayments) -> Completeness` — на них
  опирается задача 10.

**Критерии приёмки:**
- Замкнутая цепь периодов, совпадение хвоста с последним возвратом и сумма долей 100 % проверяются
  все три.
- Нарушение даёт `Incomplete` с названной причиной, а не `complete_prefix`.
- Выпуск вне области применимости профиля (нет купонов, нет возвратов) даёт `Unknown`,
  а не отклонение.
- Усечённая страница, оборванная после целого периода, отлавливается несовпадением хвоста.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-market/src/schedule/completeness.rs` с блоком тестов:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{CouponAmount, Knowledge};
    use rust_decimal::Decimal;
    use time::macros::date;

    fn coupon(start: Date, end: Date) -> CouponPeriod {
        CouponPeriod {
            period_start: start,
            accrual_end: end,
            payment_date: end,
            record_date: Knowledge::Unknown,
            amount: CouponAmount::Undetermined,
            source_entry_id: None,
        }
    }

    fn repayment(date: Date, share: i64) -> PrincipalRepayment {
        PrincipalRepayment {
            repayment_date: date,
            share_percent: Dec::new(Decimal::from(share)),
            source_kind: "amortization".to_owned(),
            source_entry_id: None,
        }
    }

    #[test]
    fn a_whole_schedule_validates() {
        let coupons = vec![
            coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15)),
            coupon(date!(2026 - 08 - 15), date!(2027 - 02 - 15)),
        ];
        let repayments = vec![repayment(date!(2027 - 02 - 15), 100)];
        assert_eq!(
            validate_moex_profile(&coupons, &repayments),
            Completeness::Validated
        );
    }

    #[test]
    fn a_truncated_page_is_caught_by_the_tail_not_by_the_chain() {
        // Это главная ловушка: усечённая страница обрывается после целого
        // периода, цепь остаётся замкнутой, и график выглядит полным.
        // Ловит его только совпадение хвоста с последним возвратом.
        let coupons = vec![coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15))];
        let repayments = vec![repayment(date!(2036 - 02 - 06), 100)];
        assert!(matches!(
            validate_moex_profile(&coupons, &repayments),
            Completeness::Incomplete { .. }
        ));
    }

    #[test]
    fn a_broken_chain_is_named_as_such() {
        let coupons = vec![
            coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15)),
            coupon(date!(2026 - 09 - 15), date!(2027 - 02 - 15)),
        ];
        let repayments = vec![repayment(date!(2027 - 02 - 15), 100)];
        let outcome = validate_moex_profile(&coupons, &repayments);
        let Completeness::Incomplete { reason } = outcome else {
            panic!("разрыв цепи обязан быть замечен: {outcome:?}");
        };
        assert!(reason.contains("2026-09-15"), "причина обязана назвать место: {reason}");
    }

    #[test]
    fn shares_that_do_not_sum_to_a_hundred_are_incomplete() {
        let coupons = vec![coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15))];
        let repayments = vec![repayment(date!(2026 - 08 - 15), 75)];
        assert!(matches!(
            validate_moex_profile(&coupons, &repayments),
            Completeness::Incomplete { .. }
        ));
    }

    #[test]
    fn an_issue_outside_the_profile_is_unknown_not_rejected() {
        // Инварианты проверены на купонных выпусках с погашением.
        // Бескупонные и бессрочные в выборку не попали, и отвергнуть их
        // корректный график — такая же ошибка, как принять усечённый.
        assert_eq!(validate_moex_profile(&[], &[]), Completeness::Unknown);
    }
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-market completeness 2>&1 | head -20`
Expected: FAIL — `cannot find function validate_moex_profile in this scope`.

- [ ] **Шаг 3: написать инварианты**

В начало `crates/iaam-market/src/schedule/completeness.rs`:

```rust
//! Структурные инварианты полноты графика (§2.10, §2.11).
//!
//! Источник не даёт ни курсора, ни счётчика записей, поэтому сверить
//! количество не с чем. Полнота доказывается структурно, и все три
//! инварианта проверены живой выборкой из 50 бумаг TQOB и TQCB — 50/50
//! по каждому.
//!
//! Инварианты принадлежат **профилю источника**, а не домену, и имеют
//! явную область применимости: бескупонные, бессрочные и юридически
//! нестандартные выпуски в выборку не попали.

use rust_decimal::Decimal;
use time::Date;

use iaam_core::numeric::decimal::Dec;

// `CouponAmount` и `Knowledge` здесь не нужны: инварианты смотрят на
// даты и доли, а не на суммы. Тестам они нужны — и импортируются в блоке
// тестов, а не тут.
use crate::schedule::{CouponPeriod, PrincipalRepayment};

/// Итог структурной проверки.
///
/// `Incomplete` вместо `complete_prefix` намеренно: успешно скачанный,
/// но усечённый график выглядит замкнутым и правдоподобным, и «полный
/// префикс» звучит как «почти всё в порядке».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    Validated,
    Incomplete { reason: String },
    /// Выпуск вне области применимости профиля.
    Unknown,
}

/// Проверить три инварианта профиля MOEX.
#[must_use]
pub fn validate_moex_profile(
    coupons: &[CouponPeriod],
    repayments: &[PrincipalRepayment],
) -> Completeness {
    if coupons.is_empty() || repayments.is_empty() {
        // Ни бескупонного, ни бессрочного выпуска в выборке не было.
        // Отвергнуть их корректный график — такая же ошибка, как принять
        // усечённый, поэтому здесь незнание, а не отказ.
        return Completeness::Unknown;
    }

    // Инвариант 1: цепь купонных периодов замкнута.
    for pair in coupons.windows(2) {
        if pair[0].accrual_end != pair[1].period_start {
            return Completeness::Incomplete {
                reason: format!(
                    "разрыв цепи периодов: период кончается {}, следующий начинается {}",
                    pair[0].accrual_end, pair[1].period_start
                ),
            };
        }
    }

    // Инвариант 2: хвост совпадает с последним возвратом номинала.
    // Ловит именно усечённую страницу: обрыв после целого периода
    // оставляет цепь замкнутой, и больше его ничто не замечает.
    let last_accrual = coupons
        .iter()
        .map(|period| period.accrual_end)
        .max()
        .unwrap_or(Date::MIN);
    let last_return = repayments
        .iter()
        .map(|repayment| repayment.repayment_date)
        .max()
        .unwrap_or(Date::MIN);
    if last_accrual != last_return {
        return Completeness::Incomplete {
            reason: format!(
                "хвост графика {last_accrual} не сходится с последним возвратом {last_return}"
            ),
        };
    }

    // Инвариант 3: доли возвратов суммируются ровно в 100 %.
    // Сложение через Dec::sum, а не через сырой Decimal: переполнение
    // и потеря точности здесь — отказ, а не тихо неверная сумма.
    let shares = repayments
        .iter()
        .map(|repayment| repayment.share_percent)
        .collect::<Vec<_>>();
    let total = match Dec::sum(&shares) {
        Ok(total) => total,
        Err(error) => {
            return Completeness::Incomplete {
                reason: format!("доли возвратов номинала не суммируются: {error}"),
            };
        }
    };
    if total != Dec::new(Decimal::from(100)) {
        return Completeness::Incomplete {
            reason: format!(
                "доли возвратов номинала дают {}, а не 100",
                total.inner()
            ),
        };
    }

    Completeness::Validated
}
```

В `crates/iaam-market/src/schedule/mod.rs`:

```rust
pub mod completeness;
```

В `crates/iaam-market/src/lib.rs`:

```rust
pub use schedule::completeness::{Completeness, validate_moex_profile};
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-market completeness`
Expected: PASS, 5 тестов.

- [ ] **Шаг 5: завести бид на построчную окончательность возврата**

```bash
bd create "E3.4: признак окончательности возврата номинала на строке (§2.1)" -t task -p 2 \
  -d "Правило — возврат окончателен, когда накопленная сумма долей достигает 100 % — реализовано в инварианте полноты (crates/iaam-market/src/schedule/completeness.rs), но на строке графика признак не выставляется. Потребитель — проекция лота и НКД, то есть E3.4.4. Кода окончательности источник даёт не всегда: из 50 проверенных бумаг у шести нет строки maturity вовсе, поэтому признак обязан выводиться, а не читаться.

## Acceptance Criteria

- Признак окончательности выводится из накопленной суммы долей, а не из кода источника.
- Бумага с шестью амортизациями без кода maturity получает окончательный последний возврат.
- Признак наблюдением не записывается: он свойство проекции (ADR-0002)."
```

- [ ] **Шаг 6: коммит**

```bash
git add crates/iaam-market/src/schedule/completeness.rs crates/iaam-market/src/schedule/mod.rs crates/iaam-market/src/lib.rs
git commit -m "feat(market): структурные инварианты полноты графика (iaam-d8b)"
```

---

### Задача 9: разбор условий выпуска

**Файлы:**
- Создать: `crates/iaam-market/src/moex/description.rs`
- Изменить: `crates/iaam-market/src/moex/mod.rs`

**Интерфейсы:**
- Потребляет: `IssueTerms`, `Knowledge`, `DefaultFlags` из задачи 2.
- Отдаёт: `terms_request(secid) -> HttpRequest`,
  `parse_description(body, instrument, observed_at) -> Result<IssueTerms, MarketError>` —
  на них опирается задача 10.

**Критерии приёмки:**
- База начисления дней и календарь получают `Unknown`, а не значение по умолчанию.
- Код валюты попадает в `IssueTerms` как есть, без перевода.
- Признаки дефолта разбираются оба.
- Текущий номинал источника не попадает в `IssueTerms` вовсе.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-market/src/moex/description.rs` с блоком тестов:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::ids::InstrumentId;
    use time::macros::{date, datetime};

    const BODY: &str = r#"{
      "description": {
        "columns": ["name", "title", "value"],
        "data": [
          ["MATDATE", "Дата погашения", "2036-02-06"],
          ["INITIALFACEVALUE", "Первоначальная номинальная стоимость", "1000"],
          ["FACEVALUE", "Номинальная стоимость", "375"],
          ["FACEUNIT", "Валюта номинала", "SUR"],
          ["COUPONFREQUENCY", "Периодичность выплаты купона в год", "2"],
          ["HASDEFAULT", "Допущен дефолт", "0"],
          ["HASTECHNICALDEFAULT", "Допущен технический дефолт", "0"]
        ]
      }
    }"#;

    fn parsed() -> IssueTerms {
        parse_description(
            BODY.as_bytes(),
            InstrumentId::new_random(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .expect("описание разобрано")
    }

    #[test]
    fn day_count_and_calendar_are_unknown_because_the_source_has_none() {
        let terms = parsed();
        assert!(matches!(terms.day_count, Knowledge::Unknown));
        assert!(matches!(terms.calendar, Knowledge::Unknown));
    }

    #[test]
    fn the_currency_code_arrives_untranslated() {
        // SUR здесь и RUB в графике — два кода одного источника на одну
        // валюту. Переводит их словарь, а не разборщик.
        let terms = parsed();
        assert_eq!(terms.face_currency_code, Knowledge::Known("SUR".to_owned()));
    }

    #[test]
    fn effective_from_is_unknown_and_not_backfilled_from_observed_at() {
        let terms = parsed();
        assert!(matches!(terms.effective_from, Knowledge::Unknown));
    }

    #[test]
    fn the_maturity_date_is_read() {
        let terms = parsed();
        assert_eq!(terms.maturity_date, Knowledge::Known(date!(2036 - 02 - 06)));
    }
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-market description 2>&1 | head -20`
Expected: FAIL — `cannot find function parse_description in this scope`.

- [ ] **Шаг 3: написать разбор**

В начало `crates/iaam-market/src/moex/description.rs`:

```rust
//! Разбор описания выпуска MOEX ISS.
//!
//! Ответ приходит парами «имя поля → значение», а не табличной строкой.
//!
//! Базы начисления дней и календаря в ответе нет вовсе — ни здесь, ни в
//! графике. Это `Unknown`, а не повод для значения по умолчанию:
//! подставленный day-count даёт правдоподобно неверный НКД, которого не
//! покажет ни один тест на бумаге с целым числом периодов.
//!
//! Текущий номинал источник даёт, и он сюда НЕ попадает: остаток выводится
//! из первоначального номинала и ряда возвратов, а два источника истины
//! расходятся молча.

use std::collections::BTreeMap;

use iaam_http::{Destination, HttpRequest};
use rust_decimal::Decimal;
use serde_json::Value;
use time::Date;
use time::format_description::well_known::Iso8601;

use iaam_core::ids::InstrumentId;
use iaam_core::numeric::decimal::Dec;

use crate::error::MarketError;
use crate::observation::ObservedAt;
use crate::schedule::terms::{DefaultFlags, IssueTerms};
use crate::schedule::Knowledge;

/// Запрос описания выпуска.
#[must_use]
pub fn terms_request(secid: &str) -> HttpRequest {
    let path = format!("/iss/securities/{secid}.json");
    HttpRequest::get(Destination::MoexIss, &path)
        .with_query("iss.meta", "off")
        .with_query("iss.only", "description")
}

fn fields(root: &Value) -> Result<BTreeMap<String, String>, MarketError> {
    let node = root
        .get("description")
        .ok_or_else(|| MarketError::Malformed("нет блока description".to_owned()))?;
    let columns = node
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет columns у description".to_owned()))?;
    let name_at = columns
        .iter()
        .position(|column| column.as_str() == Some("name"))
        .ok_or_else(|| MarketError::Malformed("нет колонки name".to_owned()))?;
    let value_at = columns
        .iter()
        .position(|column| column.as_str() == Some("value"))
        .ok_or_else(|| MarketError::Malformed("нет колонки value".to_owned()))?;
    let data = node
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет data у description".to_owned()))?;
    let mut map = BTreeMap::new();
    for row in data {
        let (Some(name), Some(value)) = (row.get(name_at), row.get(value_at)) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let value = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        if let Some(name) = name.as_str() {
            map.insert(name.to_owned(), value);
        }
    }
    Ok(map)
}

fn flag(fields: &BTreeMap<String, String>, name: &str) -> bool {
    fields.get(name).map(String::as_str) == Some("1")
}

/// Разобрать описание выпуска в снимок условий.
pub fn parse_description(
    body: &[u8],
    instrument: InstrumentId,
    observed_at: ObservedAt,
) -> Result<IssueTerms, MarketError> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|error| MarketError::Malformed(error.to_string()))?;
    let fields = fields(&root)?;

    let maturity_date = match fields.get("MATDATE") {
        Some(text) => Knowledge::Known(
            Date::parse(text, &Iso8601::DATE)
                .map_err(|_| MarketError::Malformed(format!("MATDATE не дата: {text}")))?,
        ),
        None => Knowledge::Unknown,
    };
    let initial_face_value = match fields.get("INITIALFACEVALUE") {
        Some(text) => Knowledge::Known(Dec::new(text.parse::<Decimal>().map_err(|_| {
            MarketError::Malformed(format!("INITIALFACEVALUE не число: {text}"))
        })?)),
        None => Knowledge::Unknown,
    };
    let coupon_periods_per_year = match fields.get("COUPONFREQUENCY") {
        Some(text) => Knowledge::Known(text.parse::<u32>().map_err(|_| {
            MarketError::Malformed(format!("COUPONFREQUENCY не целое: {text}"))
        })?),
        None => Knowledge::Unknown,
    };

    Ok(IssueTerms {
        instrument,
        observed_at,
        // Источник даты вступления условий в силу не сообщает.
        // Подставить observed_at значит выдать догадку за факт.
        effective_from: Knowledge::Unknown,
        maturity_date,
        initial_face_value,
        face_currency_code: fields
            .get("FACEUNIT")
            .cloned()
            .map_or(Knowledge::Unknown, Knowledge::Known),
        coupon_periods_per_year,
        // Источник не даёт ни того, ни другого — ни здесь, ни в графике.
        day_count: Knowledge::Unknown,
        calendar: Knowledge::Unknown,
        default_flags: DefaultFlags {
            declared: flag(&fields, "HASDEFAULT"),
            technical: flag(&fields, "HASTECHNICALDEFAULT"),
        },
    })
}
```

В `crates/iaam-market/src/moex/mod.rs`:

```rust
pub mod description;
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-market description`
Expected: PASS, 4 теста.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-market/src/moex/description.rs crates/iaam-market/src/moex/mod.rs
git commit -m "feat(market): разбор условий выпуска, day-count остаётся unknown (iaam-d8b)"
```

---

### Задача 10: сценарий синхронизации графика

**Файлы:**
- Создать: `crates/iaam-app/src/scenarios/schedule.rs`
- Изменить: `crates/iaam-app/src/scenarios/mod.rs`, `crates/iaam-store/src/schedule.rs`
- Тест: `crates/iaam-app/tests/schedule_sync.rs`

**Интерфейсы:**
- Потребляет: `schedule_request`, `PAGE_LIMIT`, `ScheduleQuery` (задача 7),
  `parse_bondization_page` (задача 5), `validate_moex_profile`, `Completeness` (задача 8),
  `record_schedule_snapshot` (задача 4), `market_source_codes` (задача 6),
  `OutboundHttp`, `OutboundResponse` (`crates/iaam-app/src/ports.rs:257`).
- Отдаёт: `ScheduleSyncRequest`, `ScheduleSyncResult`, `sync_schedule`, `SOURCE_ID`;
  `SqliteStore::record_schedule_completeness` — на них опирается задача 13.

**Критерии приёмки:**
- Пагинация идёт до пустой страницы **по всем трём блокам сразу**; пустой блок концом не считается.
- Неизвестный код источника даёт отказ с названным кодом, а не пропуск строки.
- Нарушение структурного инварианта записывается как `Incomplete` с причиной и запись снимка
  не отменяет: снимок — то, что источник действительно прислал.
- Просмотренные смещения попадают в `schedule_completeness.pages_seen`.
- Повторный прогон при неизменном графике не создаёт нового снимка.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-app/tests/schedule_sync.rs`:

```rust
//! Синхронизация графика: пагинация, отказ на неизвестный код, дедуп.

use std::sync::Mutex;

use async_trait::async_trait;
use iaam_app::error::AppError;
use iaam_app::ports::{OutboundHttp, OutboundResponse};
use iaam_app::scenarios::schedule::{SOURCE_ID, ScheduleSyncRequest, sync_schedule};
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_http::HttpRequest;
use iaam_market::schedule::completeness::Completeness;
use iaam_store::SqliteStore;
use iaam_store::market_source_codes::SourceCodeEntry;
use iaam_store::reference::InstrumentRecord;

/// Транспорт, отдающий заготовленные страницы **по порядку обращения**.
///
/// По порядку, а не по совпадению URL, намеренно: подделка, отвечающая
/// одним и тем же телом на любой запрос, пропустила бы отсутствие
/// пагинации — сценарий сходил бы один раз и выглядел бы исправным.
struct Pages {
    bodies: Mutex<Vec<&'static str>>,
    urls: Mutex<Vec<String>>,
}

impl Pages {
    fn new(bodies: &[&'static str]) -> Self {
        Self {
            bodies: Mutex::new(bodies.to_vec()),
            urls: Mutex::new(Vec::new()),
        }
    }

    fn urls(&self) -> Vec<String> {
        self.urls.lock().expect("журнал запросов").clone()
    }
}

#[async_trait]
impl OutboundHttp for Pages {
    async fn send(&self, request: HttpRequest) -> Result<OutboundResponse, AppError> {
        self.urls
            .lock()
            .expect("журнал запросов")
            .push(request.url());
        let mut bodies = self.bodies.lock().expect("страницы");
        let body = if bodies.is_empty() {
            EMPTY_PAGE
        } else {
            bodies.remove(0)
        };
        Ok(OutboundResponse {
            status: 200,
            body: body.as_bytes().to_vec(),
            raw_hash: format!("hash-{}", body.len()),
        })
    }
}

const EMPTY_PAGE: &str = r#"{
  "amortizations": {"columns": ["amortdate", "valueprc", "data_source"], "data": []},
  "coupons": {"columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
              "data": []},
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

/// Первая страница: амортизации и оферты уже кончились, купоны — нет.
/// Ровно та форма, на которой остановка по пустому блоку обрезает график.
const PAGE_ONE: &str = r#"{
  "amortizations": {"columns": ["amortdate", "valueprc", "data_source"], "data": []},
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [["2026-08-15", "2026-02-15", 34.41, 6.9, "RUB"]]
  },
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

const PAGE_TWO: &str = r#"{
  "amortizations": {
    "columns": ["amortdate", "valueprc", "data_source"],
    "data": [["2027-02-15", 100, "maturity"]]
  },
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [["2027-02-15", "2026-08-15", 34.41, 6.9, "RUB"]]
  },
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

const PAGE_WITH_UNKNOWN_KIND: &str = r#"{
  "amortizations": {
    "columns": ["amortdate", "valueprc", "data_source"],
    "data": [["2027-02-15", 100, "досрочное погашение"]]
  },
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [["2027-02-15", "2026-08-15", 34.41, 6.9, "RUB"]]
  },
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

fn store() -> (SqliteStore, InstrumentId) {
    let mut store = SqliteStore::open_in_memory().expect("база в памяти");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "SU46020RMFS2".to_owned(),
            title: "ОФЗ 46020".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент заведён");
    store
        .extend_market_source_codes(
            SOURCE_ID,
            "профиль источника 2026-08-27",
            &[
                SourceCodeEntry {
                    domain: "currency".to_owned(),
                    source_code: "RUB".to_owned(),
                    meaning: "RUB".to_owned(),
                },
                SourceCodeEntry {
                    domain: "principal_repayment_kind".to_owned(),
                    source_code: "maturity".to_owned(),
                    meaning: "principal_return".to_owned(),
                },
            ],
        )
        .expect("словарь заселён");
    (store, instrument)
}

fn request(instrument: InstrumentId) -> ScheduleSyncRequest {
    ScheduleSyncRequest {
        instrument,
        secid: "SU46020RMFS2".to_owned(),
    }
}

#[tokio::test]
async fn pagination_continues_while_any_block_still_returns_rows() {
    // Смещение общее на три блока: на второй странице амортизации и
    // оферты пусты, купоны продолжаются. Остановка по пустому блоку
    // обрезала бы график, и он выглядел бы замкнутым.
    let (mut store, instrument) = store();
    let transport = Pages::new(&[PAGE_ONE, PAGE_TWO]);
    let result = sync_schedule(&mut store, &transport, request(instrument))
        .await
        .expect("синхронизация");

    let urls = transport.urls();
    assert_eq!(urls.len(), 3, "две страницы с данными и одна пустая: {urls:?}");
    assert!(urls[0].contains("start=0"), "{urls:?}");
    assert!(urls[1].contains("start=100"), "{urls:?}");
    assert_eq!(result.pages_seen, vec![0, 100, 200]);

    let stored = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("снимок найден");
    assert_eq!(stored.coupon_periods.len(), 2, "купоны обеих страниц");
    assert_eq!(result.completeness, Completeness::Validated);
}

#[tokio::test]
async fn an_unknown_source_code_is_refused_by_name() {
    // Пропуск строки с незнакомым кодом молча укоротил бы график.
    // Отказ обязан назвать код, иначе владельцу нечего вносить в словарь.
    let (mut store, instrument) = store();
    let transport = Pages::new(&[PAGE_WITH_UNKNOWN_KIND]);
    let error = sync_schedule(&mut store, &transport, request(instrument))
        .await
        .expect_err("неизвестный код обязан быть отказом");
    assert!(
        error.to_string().contains("досрочное погашение"),
        "отказ обязан назвать код: {error}"
    );
}

#[tokio::test]
async fn a_second_run_over_an_unchanged_schedule_writes_no_new_snapshot() {
    let (mut store, instrument) = store();
    let first = sync_schedule(
        &mut store,
        &Pages::new(&[PAGE_ONE, PAGE_TWO]),
        request(instrument),
    )
    .await
    .expect("первый прогон");
    let second = sync_schedule(
        &mut store,
        &Pages::new(&[PAGE_ONE, PAGE_TWO]),
        request(instrument),
    )
    .await
    .expect("второй прогон");
    assert!(first.written);
    assert!(!second.written, "неизменный график писаться не должен");
    assert_eq!(first.snapshot_id, second.snapshot_id);
}

#[tokio::test]
async fn a_broken_invariant_does_not_cancel_the_snapshot() {
    // Снимок — то, что источник действительно прислал. Стереть его
    // значит потерять свидетельство. Отменяется пригодность к расчёту,
    // а не запись наблюдения.
    let (mut store, instrument) = store();
    let result = sync_schedule(
        &mut store,
        &Pages::new(&[PAGE_ONE]),
        request(instrument),
    )
    .await
    .expect("синхронизация");
    assert!(matches!(result.completeness, Completeness::Unknown));
    assert!(result.written, "снимок обязан быть записан");
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-app --test schedule_sync 2>&1 | head -20`
Expected: FAIL — `could not find schedule in iaam_app::scenarios`.

- [ ] **Шаг 3: написать сценарий**

Создать `crates/iaam-app/src/scenarios/schedule.rs`:

```rust
//! Синхронизация графика выплат облигации (§2.10 спеки E3.4).
//!
//! Три отличия от `sync_market`, и каждое существует ради конкретной
//! ловушки:
//!
//! 1. **Пагинация.** `sync_market` берёт одну страницу со смещением ноль.
//!    Здесь смещение растёт, пока хоть один блок отдаёт строки: источник
//!    молча режет страницу до сотни, и первый запрос у длинного выпуска
//!    возвращает замкнутый график, короче настоящего на десять лет.
//! 2. **Перевод кодов словарём.** Коды вида возврата номинала, вида права
//!    по оферте и валюты переводятся чтением словаря. Неизвестный код —
//!    отказ с названным кодом, а не пропуск строки: пропущенная строка
//!    укорачивает график молча.
//! 3. **Структурная проверка.** Полнота — три независимых утверждения,
//!    и «источник вычитан до конца» полнотой не является.
//!
//! Нарушение инварианта запись снимка **не отменяет**: снимок — то, что
//! источник действительно прислал, и стереть его значит потерять
//! свидетельство. Отменяется пригодность графика к расчёту.

use iaam_core::ids::InstrumentId;
use iaam_market::moex::bondization::parse_bondization_page;
use iaam_market::moex::{PAGE_LIMIT, ScheduleQuery, schedule_request};
use iaam_market::observation::ObservedAt;
use iaam_market::schedule::completeness::{Completeness, validate_moex_profile};
use iaam_market::schedule::{
    CouponAmount, CouponPeriod, Knowledge, OfferWindow, PrincipalRepayment,
};
use iaam_store::market::MarketStore;
use iaam_store::schedule::{
    CouponPeriodRow, OfferWindowRow, PrincipalRepaymentRow, ScheduleSnapshotRow,
};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::AppError;
use crate::ports::OutboundHttp;

/// Идентификатор источника графика.
pub const SOURCE_ID: &str = "moex-iss";

/// Потолок числа страниц.
///
/// Предохранитель, а не ожидание: у выпуска с ежемесячным купоном на
/// тридцать лет страниц четыре. Выход по счётчику — отказ с причиной,
/// а не тихий возврат: тихий возврат был бы тем же усечением, только
/// нашими руками.
const MAX_PAGES: u32 = 100;

/// Что синхронизируем.
#[derive(Debug, Clone)]
pub struct ScheduleSyncRequest {
    pub instrument: InstrumentId,
    pub secid: String,
}

/// Наблюдаемое состояние запуска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSyncResult {
    pub snapshot_id: String,
    /// Записан ли новый снимок. `false` означает, что содержимое совпало
    /// с прошлым, и это не ошибка, а событие следа запуска.
    pub written: bool,
    pub pages_seen: Vec<u32>,
    pub completeness: Completeness,
}

fn invalid(field: &str, expected: &str, actual: &str) -> AppError {
    AppError::Invalid {
        field: field.to_owned(),
        expected: expected.to_owned(),
        actual: actual.to_owned(),
    }
}

/// Синхронизировать график выплат одного выпуска.
pub async fn sync_schedule(
    store: &mut MarketStore,
    transport: &dyn OutboundHttp,
    request: ScheduleSyncRequest,
) -> Result<ScheduleSyncResult, AppError> {
    let observed_at = ObservedAt(OffsetDateTime::now_utc());

    let mut coupon_periods: Vec<CouponPeriod> = Vec::new();
    let mut principal_repayments: Vec<PrincipalRepayment> = Vec::new();
    let mut offer_windows: Vec<OfferWindow> = Vec::new();
    let mut pages_seen: Vec<u32> = Vec::new();

    for page_index in 0..MAX_PAGES {
        let start = page_index * PAGE_LIMIT;
        let response = transport
            .send(schedule_request(ScheduleQuery {
                secid: &request.secid,
                start,
            }))
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(invalid(
                "status",
                "успешный ответ источника",
                &response.status.to_string(),
            ));
        }
        pages_seen.push(start);
        let page = parse_bondization_page(&response.body, observed_at)
            .map_err(|error| invalid("body", "разбираемый график", &error.to_string()))?;
        // Конец выборки — пустая страница ПО ВСЕМ блокам сразу.
        // Смещение общее, и амортизации кончаются раньше купонов.
        if page.total_rows == 0 {
            break;
        }
        coupon_periods.extend(page.coupon_periods);
        principal_repayments.extend(page.principal_repayments);
        offer_windows.extend(page.offer_windows);

        if page_index + 1 == MAX_PAGES {
            return Err(invalid(
                "pages",
                "график короче потолка страниц",
                &MAX_PAGES.to_string(),
            ));
        }
    }

    let repayment_kinds = store
        .market_source_codes(SOURCE_ID, "principal_repayment_kind")
        .map_err(|error| invalid("dictionary", "словарь видов возврата", &error.to_string()))?;
    let offer_kinds = store
        .market_source_codes(SOURCE_ID, "offer_kind")
        .map_err(|error| invalid("dictionary", "словарь видов оферты", &error.to_string()))?;
    let currencies = store
        .market_source_codes(SOURCE_ID, "currency")
        .map_err(|error| invalid("dictionary", "словарь валют", &error.to_string()))?;

    // Неизвестный код — отказ, названный поимённо. Пропуск строки
    // укоротил бы график молча, а «Other» означал бы принятое решение
    // не разбирать — такого решения не принимали.
    for repayment in &principal_repayments {
        if !repayment_kinds.contains_key(&repayment.source_kind) {
            return Err(invalid(
                "principal_repayment_kind",
                "код, известный словарю источника",
                &repayment.source_kind,
            ));
        }
    }
    for window in &offer_windows {
        if !offer_kinds.contains_key(&window.source_kind) {
            return Err(invalid(
                "offer_kind",
                "код, известный словарю источника",
                &window.source_kind,
            ));
        }
    }
    for period in &coupon_periods {
        if let CouponAmount::AmountFixed { currency, .. } = &period.amount
            && !currencies.contains_key((*currency).code())
        {
            return Err(invalid(
                "currency",
                "код валюты, известный словарю источника",
                (*currency).code(),
            ));
        }
    }

    let completeness = validate_moex_profile(&coupon_periods, &principal_repayments);

    let coupon_rows = coupon_periods.iter().map(coupon_row).collect::<Vec<_>>();
    let repayment_rows = principal_repayments
        .iter()
        .map(repayment_row)
        .collect::<Vec<_>>();
    let window_rows = offer_windows.iter().map(window_row).collect::<Vec<_>>();

    let header = ScheduleSnapshotRow {
        instrument_id: request.instrument.inner().to_string(),
        source_id: SOURCE_ID.to_owned(),
        observed_at: observed_at
            .0
            .format(&Rfc3339)
            .map_err(|error| invalid("observed_at", "RFC 3339", &error.to_string()))?,
        content_hash: content_hash(&coupon_rows, &repayment_rows, &window_rows),
    };
    let outcome = store
        .record_schedule_snapshot(&header, &coupon_rows, &repayment_rows, &window_rows)
        .map_err(|error| invalid("snapshot", "записываемый снимок", &error.to_string()))?;

    let (validated, reason) = match &completeness {
        Completeness::Validated => (true, None),
        Completeness::Incomplete { reason } => (false, Some(reason.clone())),
        // Выпуск вне области применимости профиля: инварианты не
        // применимы, и объявлять их выполненными нельзя.
        Completeness::Unknown => (false, Some("выпуск вне профиля источника".to_owned())),
    };
    store
        .record_schedule_completeness(
            &outcome.snapshot_id,
            true,
            validated,
            reason.as_deref(),
            &pages_seen,
        )
        .map_err(|error| invalid("completeness", "записываемая полнота", &error.to_string()))?;

    Ok(ScheduleSyncResult {
        snapshot_id: outcome.snapshot_id,
        written: outcome.written,
        pages_seen,
        completeness,
    })
}

fn coupon_row(period: &CouponPeriod) -> CouponPeriodRow {
    let (status, per_unit, currency, rate) = match &period.amount {
        CouponAmount::AmountFixed { per_unit, currency } => (
            "amount_fixed",
            Some(per_unit.inner().to_string()),
            Some(currency.code().to_owned()),
            None,
        ),
        CouponAmount::RateFixedAmountUndetermined { rate_percent } => (
            "rate_fixed_amount_undetermined",
            None,
            None,
            Some(rate_percent.inner().to_string()),
        ),
        CouponAmount::Undetermined => ("undetermined", None, None, None),
    };
    CouponPeriodRow {
        period_start: period.period_start.to_string(),
        accrual_end: period.accrual_end.to_string(),
        payment_date: period.payment_date.to_string(),
        record_date: period.record_date.known().map(ToString::to_string),
        amount_status: status.to_owned(),
        amount_per_unit: per_unit,
        amount_currency: currency,
        rate_percent: rate,
        source_entry_id: period.source_entry_id.clone(),
    }
}

fn repayment_row(repayment: &PrincipalRepayment) -> PrincipalRepaymentRow {
    PrincipalRepaymentRow {
        repayment_date: repayment.repayment_date.to_string(),
        share_percent: repayment.share_percent.inner().to_string(),
        source_kind: repayment.source_kind.clone(),
        source_entry_id: repayment.source_entry_id.clone(),
    }
}

fn window_row(window: &OfferWindow) -> OfferWindowRow {
    OfferWindowRow {
        execution_date: window.execution_date.to_string(),
        submission_start: window.submission_start.known().map(ToString::to_string),
        submission_end: window.submission_end.known().map(ToString::to_string),
        price_percent: window
            .price_percent
            .known()
            .map(|value| value.inner().to_string()),
        agent: window.agent.known().cloned(),
        source_kind: window.source_kind.clone(),
        source_entry_id: window.source_entry_id.clone(),
    }
}

/// Хэш содержимого снимка.
///
/// Считается по строкам таблиц, а не по телу ответа: тело меняется от
/// полей, которые в домен не входят (текущий номинал в каждой строке,
/// рублёвый эквивалент, число дней до погашения), и хэш по нему объявлял
/// бы изменившимся неизменившийся график каждый день.
fn content_hash(
    coupons: &[CouponPeriodRow],
    repayments: &[PrincipalRepaymentRow],
    windows: &[OfferWindowRow],
) -> String {
    let mut hasher = Sha256::new();
    for row in coupons {
        hasher.update(
            format!(
                "c|{}|{}|{}|{:?}|{}|{:?}|{:?}|{:?}\n",
                row.period_start,
                row.accrual_end,
                row.payment_date,
                row.record_date,
                row.amount_status,
                row.amount_per_unit,
                row.amount_currency,
                row.rate_percent
            )
            .as_bytes(),
        );
    }
    for row in repayments {
        hasher.update(
            format!(
                "p|{}|{}|{}\n",
                row.repayment_date, row.share_percent, row.source_kind
            )
            .as_bytes(),
        );
    }
    for row in windows {
        hasher.update(
            format!(
                "o|{}|{:?}|{:?}|{:?}|{:?}|{}\n",
                row.execution_date,
                row.submission_start,
                row.submission_end,
                row.price_percent,
                row.agent,
                row.source_kind
            )
            .as_bytes(),
        );
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
```

В `crates/iaam-store/src/schedule.rs` добавить запись полноты:

```rust
    /// Записать три утверждения о полноте снимка.
    ///
    /// Три, а не одно: «источник вычитан до конца» и «график доменно
    /// достаточен» — разные утверждения, и полностью вычитанный источник
    /// с дырой внутри проходил бы как полный.
    pub fn record_schedule_completeness(
        &mut self,
        snapshot_id: &str,
        fetch_exhausted: bool,
        structurally_validated: bool,
        incomplete_reason: Option<&str>,
        pages_seen: &[u32],
    ) -> Result<(), StoreError> {
        let pages = pages_seen
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        self.conn.execute(
            "INSERT INTO schedule_completeness
                 (snapshot_id, fetch_exhausted, structurally_validated,
                  incomplete_reason, pages_seen, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (snapshot_id) DO UPDATE SET
                 fetch_exhausted = excluded.fetch_exhausted,
                 structurally_validated = excluded.structurally_validated,
                 incomplete_reason = excluded.incomplete_reason,
                 pages_seen = excluded.pages_seen,
                 updated_at = excluded.updated_at",
            params![
                snapshot_id,
                i64::from(fetch_exhausted),
                i64::from(structurally_validated),
                incomplete_reason,
                format!("[{pages}]"),
                now(),
            ],
        )?;
        Ok(())
    }
```

В `crates/iaam-app/src/scenarios/mod.rs` объявить модуль рядом с остальными:

```rust
pub mod schedule;
```

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-app --test schedule_sync`
Expected: PASS, 4 теста.

Run: `make check`
Expected: всё зелёное.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-app/src/scenarios/schedule.rs crates/iaam-app/src/scenarios/mod.rs crates/iaam-store/src/schedule.rs crates/iaam-app/tests/schedule_sync.rs
git commit -m "feat(app): синхронизация графика выплат с пагинацией и проверкой полноты (iaam-d8b)"
```

---

### Задача 11: синхронизация условий выпуска

**Файлы:**
- Изменить: `crates/iaam-app/src/scenarios/schedule.rs`, `crates/iaam-store/src/schedule.rs`
- Тест: `crates/iaam-app/tests/schedule_terms.rs`

**Интерфейсы:**
- Потребляет: `terms_request`, `parse_description` (задача 9), `IssueTerms`, `Knowledge`
  (задача 2), `OutboundHttp`.
- Отдаёт: `sync_issue_terms`, `SqliteStore::record_issue_terms`,
  `SqliteStore::issue_terms_at_or_before`.

**Критерии приёмки:**
- База начисления дней и календарь доходят до базы как `NULL`, а не как значение по умолчанию.
- Код валюты записывается как его дал источник; неизвестный словарю код даёт отказ.
- Признаки дефолта доходят до базы оба.
- Наблюдение условий append-only: повторная запись на тот же `observed_at` не переписывает строку.

- [ ] **Шаг 1: написать падающий тест**

Создать `crates/iaam-app/tests/schedule_terms.rs`:

```rust
//! Синхронизация условий выпуска: незнание доходит до базы незнанием.

use std::sync::Mutex;

use async_trait::async_trait;
use iaam_app::error::AppError;
use iaam_app::ports::{OutboundHttp, OutboundResponse};
use iaam_app::scenarios::schedule::{SOURCE_ID, sync_issue_terms};
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_http::HttpRequest;
use iaam_store::SqliteStore;
use iaam_store::market_source_codes::SourceCodeEntry;
use iaam_store::reference::InstrumentRecord;

const DESCRIPTION: &str = r#"{
  "description": {
    "columns": ["name", "title", "value"],
    "data": [
      ["MATDATE", "Дата погашения", "2036-02-06"],
      ["INITIALFACEVALUE", "Первоначальная номинальная стоимость", "1000"],
      ["FACEVALUE", "Номинальная стоимость", "375"],
      ["FACEUNIT", "Валюта номинала", "SUR"],
      ["COUPONFREQUENCY", "Периодичность выплаты купона в год", "2"],
      ["HASDEFAULT", "Допущен дефолт", "0"],
      ["HASTECHNICALDEFAULT", "Допущен технический дефолт", "1"]
    ]
  }
}"#;

struct Body(&'static str, Mutex<Vec<String>>);

#[async_trait]
impl OutboundHttp for Body {
    async fn send(&self, request: HttpRequest) -> Result<OutboundResponse, AppError> {
        self.1.lock().expect("журнал").push(request.url());
        Ok(OutboundResponse {
            status: 200,
            body: self.0.as_bytes().to_vec(),
            raw_hash: "hash-terms".to_owned(),
        })
    }
}

fn store() -> (SqliteStore, InstrumentId) {
    let mut store = SqliteStore::open_in_memory().expect("база в памяти");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "SU46020RMFS2".to_owned(),
            title: "ОФЗ 46020".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент заведён");
    store
        .extend_market_source_codes(
            SOURCE_ID,
            "профиль источника 2026-08-27",
            &[SourceCodeEntry {
                domain: "currency".to_owned(),
                source_code: "SUR".to_owned(),
                meaning: "RUB".to_owned(),
            }],
        )
        .expect("словарь заселён");
    (store, instrument)
}

#[tokio::test]
async fn unknown_day_count_reaches_the_database_as_null() {
    // Источник не даёт ни базы начисления дней, ни календаря. Значение
    // по умолчанию дало бы правдоподобно неверный НКД, которого не
    // покажет ни один тест на бумаге с целым числом периодов.
    let (mut store, instrument) = store();
    let transport = Body(DESCRIPTION, Mutex::new(Vec::new()));
    sync_issue_terms(&mut store, &transport, instrument, "SU46020RMFS2")
        .await
        .expect("синхронизация условий");

    let terms = store
        .issue_terms_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("условия найдены");
    assert_eq!(terms.day_count, None);
    assert_eq!(terms.calendar, None);
    assert_eq!(terms.effective_from, None);
}

#[tokio::test]
async fn the_source_currency_code_is_stored_verbatim() {
    // SUR здесь и RUB в графике — два кода одного источника на одну
    // валюту. Хранится код источника, переводит его словарь.
    let (mut store, instrument) = store();
    let transport = Body(DESCRIPTION, Mutex::new(Vec::new()));
    sync_issue_terms(&mut store, &transport, instrument, "SU46020RMFS2")
        .await
        .expect("синхронизация условий");
    let terms = store
        .issue_terms_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("условия найдены");
    assert_eq!(terms.face_currency_code.as_deref(), Some("SUR"));
}

#[tokio::test]
async fn both_default_flags_survive_the_trip() {
    // Объявленный дефолт делает будущий график недостоверным. Потерять
    // признак по дороге значит посчитать метрику так, будто выплаты
    // состоятся.
    let (mut store, instrument) = store();
    let transport = Body(DESCRIPTION, Mutex::new(Vec::new()));
    sync_issue_terms(&mut store, &transport, instrument, "SU46020RMFS2")
        .await
        .expect("синхронизация условий");
    let terms = store
        .issue_terms_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("условия найдены");
    assert!(!terms.default_declared);
    assert!(terms.default_technical);
}
```

- [ ] **Шаг 2: запустить тест и убедиться, что он падает**

Run: `cargo test -p iaam-app --test schedule_terms 2>&1 | head -20`
Expected: FAIL — `cannot find function sync_issue_terms`.

- [ ] **Шаг 3: написать хранение и сценарий**

В `crates/iaam-store/src/schedule.rs` добавить строку условий и два метода:

```rust
/// Строка условий выпуска. Все значения строками, как и везде в
/// хранилище: форматов источников оно не знает.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueTermsRow {
    pub instrument_id: String,
    pub source_id: String,
    pub observed_at: String,
    pub effective_from: Option<String>,
    pub maturity_date: Option<String>,
    pub initial_face_value: Option<String>,
    pub face_currency_code: Option<String>,
    pub coupon_periods_per_year: Option<i64>,
    pub day_count: Option<String>,
    pub calendar: Option<String>,
    pub default_declared: bool,
    pub default_technical: bool,
}

impl SqliteStore {
    /// Записать наблюдение условий выпуска.
    ///
    /// `INSERT OR IGNORE`, а не `UPSERT`: наблюдение append-only, и
    /// повторная запись на тот же `observed_at` — это то же наблюдение,
    /// а не исправление. Исправление приходит новым `observed_at`.
    pub fn record_issue_terms(&mut self, row: &IssueTermsRow) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO issue_terms
                 (instrument_id, source_id, observed_at, effective_from, maturity_date,
                  initial_face_value, face_currency_code, coupon_periods_per_year,
                  day_count, calendar, default_declared, default_technical, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &row.instrument_id,
                &row.source_id,
                &row.observed_at,
                &row.effective_from,
                &row.maturity_date,
                &row.initial_face_value,
                &row.face_currency_code,
                &row.coupon_periods_per_year,
                &row.day_count,
                &row.calendar,
                i64::from(row.default_declared),
                i64::from(row.default_technical),
                now(),
            ],
        )?;
        Ok(())
    }

    /// Последнее наблюдение условий не позже координаты знания.
    pub fn issue_terms_at_or_before(
        &self,
        instrument_id: &str,
        source_id: &str,
        knowledge_as_of: &str,
    ) -> Result<Option<IssueTermsRow>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT instrument_id, source_id, observed_at, effective_from, maturity_date,
                        initial_face_value, face_currency_code, coupon_periods_per_year,
                        day_count, calendar, default_declared, default_technical
                 FROM issue_terms
                 WHERE instrument_id = ?1 AND source_id = ?2 AND observed_at <= ?3
                 ORDER BY observed_at DESC
                 LIMIT 1",
                params![instrument_id, source_id, knowledge_as_of],
                |row| {
                    Ok(IssueTermsRow {
                        instrument_id: row.get(0)?,
                        source_id: row.get(1)?,
                        observed_at: row.get(2)?,
                        effective_from: row.get(3)?,
                        maturity_date: row.get(4)?,
                        initial_face_value: row.get(5)?,
                        face_currency_code: row.get(6)?,
                        coupon_periods_per_year: row.get(7)?,
                        day_count: row.get(8)?,
                        calendar: row.get(9)?,
                        default_declared: row.get::<_, i64>(10)? != 0,
                        default_technical: row.get::<_, i64>(11)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }
}
```

В `crates/iaam-app/src/scenarios/schedule.rs` добавить сценарий:

```rust
/// Синхронизировать условия выпуска.
///
/// Отдельный сценарий, а не шаг синхронизации графика: у условий свой
/// эндпойнт, своя ось действия (`effective_from`) и своя append-only
/// таблица. Слить их значило бы записывать новое наблюдение условий
/// каждый раз, когда поменялся график, и наоборот.
pub async fn sync_issue_terms(
    store: &mut MarketStore,
    transport: &dyn OutboundHttp,
    instrument: InstrumentId,
    secid: &str,
) -> Result<(), AppError> {
    let observed_at = ObservedAt(OffsetDateTime::now_utc());
    let response = transport.send(terms_request(secid)).await?;
    if !(200..300).contains(&response.status) {
        return Err(invalid(
            "status",
            "успешный ответ источника",
            &response.status.to_string(),
        ));
    }
    let terms = parse_description(&response.body, instrument, observed_at)
        .map_err(|error| invalid("body", "разбираемое описание", &error.to_string()))?;

    // Код валюты хранится как его дал источник, но словарь обязан его
    // знать: неизвестный код, дошедший до базы, станет второй валютой
    // рядом с рублём, и позиции разъедутся молча.
    if let Knowledge::Known(code) = &terms.face_currency_code {
        let currencies = store
            .market_source_codes(SOURCE_ID, "currency")
            .map_err(|error| invalid("dictionary", "словарь валют", &error.to_string()))?;
        if !currencies.contains_key(code) {
            return Err(invalid(
                "currency",
                "код валюты, известный словарю источника",
                code,
            ));
        }
    }

    store
        .record_issue_terms(&IssueTermsRow {
            instrument_id: instrument.inner().to_string(),
            source_id: SOURCE_ID.to_owned(),
            observed_at: observed_at
                .0
                .format(&Rfc3339)
                .map_err(|error| invalid("observed_at", "RFC 3339", &error.to_string()))?,
            // Неизвестное доходит до базы NULL. Значение по умолчанию
            // здесь — правдоподобно неверный НКД.
            effective_from: terms.effective_from.known().map(ToString::to_string),
            maturity_date: terms.maturity_date.known().map(ToString::to_string),
            initial_face_value: terms
                .initial_face_value
                .known()
                .map(|value| value.inner().to_string()),
            face_currency_code: terms.face_currency_code.known().cloned(),
            coupon_periods_per_year: terms
                .coupon_periods_per_year
                .known()
                .map(|value| i64::from(*value)),
            day_count: terms.day_count.known().cloned(),
            calendar: terms.calendar.known().cloned(),
            default_declared: terms.default_flags.declared,
            default_technical: terms.default_flags.technical,
        })
        .map_err(|error| invalid("issue_terms", "записываемые условия", &error.to_string()))?;
    Ok(())
}
```

Дополнить импорты модуля: `iaam_market::moex::description::{parse_description, terms_request}`
и `iaam_store::schedule::IssueTermsRow`.

- [ ] **Шаг 4: запустить тесты и убедиться, что они проходят**

Run: `cargo test -p iaam-app --test schedule_terms`
Expected: PASS, 3 теста.

- [ ] **Шаг 5: коммит**

```bash
git add crates/iaam-app/src/scenarios/schedule.rs crates/iaam-store/src/schedule.rs crates/iaam-app/tests/schedule_terms.rs
git commit -m "feat(app): синхронизация условий выпуска без значений по умолчанию (iaam-d8b)"
```

---

### Задача 12: замороженные фикстуры (правка политики, отдельный коммит)

**Файлы:**
- Создать: `tests/fixtures/market/moex-iss-bondization-fixed-coupon.json`,
  `moex-iss-bondization-floater.json`, `moex-iss-bondization-amortised.json`,
  `moex-iss-bondization-offers.json`, `moex-iss-bondization-foreign-face.json`,
  `moex-iss-bondization-page-1.json`, `moex-iss-bondization-page-2.json`,
  `moex-iss-description-amortised.json`
- Изменить: `tests/fixtures/MANIFEST.sha256`
- Изменить: тесты задач 5 и 9 — перевести со встроенных литералов на фикстуры

**Критерии приёмки:**
- Фикстуры сняты живыми вызовами, а не сконструированы по памяти.
- Каждый класс представлен: фиксированный купон, флоатер с неизвестными суммами, амортизируемый
  выпуск, выпуск с несколькими офертами, валютный номинал, выпуск длиннее одной страницы.
- `make fixtures` зелёный, мёртвых фикстур нет.
- Правка политики идёт **отдельным** коммитом.

- [ ] **Шаг 1: снять фикстуры живыми вызовами**

```bash
cd tests/fixtures/market
curl -sS "https://iss.moex.com/iss/securities/SU26238RMFS4/bondization.json?limit=100&start=0&iss.meta=off" -o moex-iss-bondization-fixed-coupon.json
curl -sS "https://iss.moex.com/iss/securities/SU29014RMFS6/bondization.json?limit=100&start=0&iss.meta=off" -o moex-iss-bondization-floater.json
curl -sS "https://iss.moex.com/iss/securities/SU46020RMFS2/bondization.json?limit=100&start=0&iss.meta=off" -o moex-iss-bondization-amortised.json
curl -sS "https://iss.moex.com/iss/securities/RU000A0JS4Z7/bondization.json?limit=100&start=0&iss.meta=off" -o moex-iss-bondization-offers.json
curl -sS "https://iss.moex.com/iss/securities/BYM000001818/bondization.json?limit=100&start=0&iss.meta=off" -o moex-iss-bondization-foreign-face.json
curl -sS "https://iss.moex.com/iss/securities/RU000A0JTYJ6/bondization.json?limit=100&start=0&iss.meta=off" -o moex-iss-bondization-page-1.json
curl -sS "https://iss.moex.com/iss/securities/RU000A0JTYJ6/bondization.json?limit=100&start=100&iss.meta=off" -o moex-iss-bondization-page-2.json
curl -sS "https://iss.moex.com/iss/securities/SU46020RMFS2.json?iss.meta=off&iss.only=description" -o moex-iss-description-amortised.json
```

`RU000A0JTYJ6` выбран не случайно: это выпуск, на котором первая страница даёт **замкнутый**
график, короче настоящего на десять лет. Эталон существует ради этой ловушки.

- [ ] **Шаг 2: обновить манифест**

Свериться со `scripts/check-fixtures.sh` — он определяет формат `MANIFEST.sha256` и способ
пересчёта. Пересчитать хэши так, как предписывает скрипт.

Run: `make fixtures`
Expected: зелёное.

- [ ] **Шаг 3: перевести тесты задач 5 и 9 на фикстуры**

В `crates/iaam-market/src/moex/bondization.rs` и `description.rs` заменить встроенные литералы
на `include_bytes!` с путями до фикстур, сохранив прежние утверждения. Добавить тест на ловушку:

```rust
    #[test]
    fn the_first_page_of_a_long_issue_looks_whole_and_is_not() {
        // Эталон конкретной ловушки: первая страница замкнута по цепи и
        // короче настоящего графика на десять лет. Ловит её только
        // несовпадение хвоста с последним возвратом номинала.
        let page = parse_bondization_page(
            include_bytes!("../../../../tests/fixtures/market/moex-iss-bondization-page-1.json"),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .expect("первая страница");
        assert_eq!(page.coupon_periods.len(), 100);
        let outcome = crate::schedule::completeness::validate_moex_profile(
            &page.coupon_periods,
            &page.principal_repayments,
        );
        assert!(
            matches!(
                outcome,
                crate::schedule::completeness::Completeness::Incomplete { .. }
            ),
            "усечённая страница обязана давать Incomplete: {outcome:?}"
        );
    }
```

Run: `cargo test -p iaam-market`
Expected: PASS.

- [ ] **Шаг 4: два раздельных коммита**

```bash
git add crates/iaam-market/src/moex/bondization.rs crates/iaam-market/src/moex/description.rs
git commit -m "test(market): разбор графика проверяется на замороженных эталонах (iaam-d8b)"

git add tests/fixtures
POLICY_CHANGE_APPROVED=1 git commit -m "chore(policy): эталоны графика выплат MOEX ISS (iaam-d8b)"
```

PR помечается меткой `policy-change`, иначе заслон не пропустит.

---

### Задача 13: заслоны — мутанты и метаморфное свойство

**Файлы:**
- Изменить: `scripts/check-mutants.sh` (список `MODULES`, строка 20 и далее)
- Тест: `crates/iaam-app/tests/schedule_metamorphic.rs`

**Критерии приёмки:**
- Новые модули политики графика стоят в списке мутационного заслона.
- Повторная синхронизация того же выпуска не меняет ни одного ответа при неизменной координате.
- `make mutants` по новым модулям без выживших.
- Правка `scripts` идёт отдельным коммитом.

- [ ] **Шаг 1: написать метаморфный тест**

Создать `crates/iaam-app/tests/schedule_metamorphic.rs`. Подделка транспорта повторяет `Pages`
из `schedule_sync.rs` — повторить её здесь целиком, а не выносить в общий модуль: тестовый
хелпер, общий на два файла, связывает два теста, и правка одного молча меняет другой.

```rust
//! Метаморфное свойство: повторная синхронизация ничего не меняет.

use std::sync::Mutex;

use async_trait::async_trait;
use iaam_app::error::AppError;
use iaam_app::ports::{OutboundHttp, OutboundResponse};
use iaam_app::scenarios::schedule::{SOURCE_ID, ScheduleSyncRequest, sync_schedule};
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_http::HttpRequest;
use iaam_store::SqliteStore;
use iaam_store::market_source_codes::SourceCodeEntry;
use iaam_store::reference::InstrumentRecord;

const WHOLE: &str = r#"{
  "amortizations": {
    "columns": ["amortdate", "valueprc", "data_source"],
    "data": [["2027-02-15", 100, "maturity"]]
  },
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [
      ["2026-08-15", "2026-02-15", 34.41, 6.9, "RUB"],
      ["2027-02-15", "2026-08-15", 34.41, 6.9, "RUB"]
    ]
  },
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

const EMPTY: &str = r#"{
  "amortizations": {"columns": ["amortdate", "valueprc", "data_source"], "data": []},
  "coupons": {"columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
              "data": []},
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

struct Pages(Mutex<Vec<&'static str>>);

#[async_trait]
impl OutboundHttp for Pages {
    async fn send(&self, _request: HttpRequest) -> Result<OutboundResponse, AppError> {
        let mut bodies = self.0.lock().expect("страницы");
        let body = if bodies.is_empty() { EMPTY } else { bodies.remove(0) };
        Ok(OutboundResponse {
            status: 200,
            body: body.as_bytes().to_vec(),
            raw_hash: format!("hash-{}", body.len()),
        })
    }
}

fn store() -> (SqliteStore, InstrumentId) {
    let mut store = SqliteStore::open_in_memory().expect("база в памяти");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "SU46020RMFS2".to_owned(),
            title: "ОФЗ 46020".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент заведён");
    store
        .extend_market_source_codes(
            SOURCE_ID,
            "профиль источника 2026-08-27",
            &[
                SourceCodeEntry {
                    domain: "currency".to_owned(),
                    source_code: "RUB".to_owned(),
                    meaning: "RUB".to_owned(),
                },
                SourceCodeEntry {
                    domain: "principal_repayment_kind".to_owned(),
                    source_code: "maturity".to_owned(),
                    meaning: "principal_return".to_owned(),
                },
            ],
        )
        .expect("словарь заселён");
    (store, instrument)
}

#[tokio::test]
async fn a_second_sync_of_an_unchanged_schedule_changes_nothing() {
    // Синхронизация — не событие: если источник прислал то же самое,
    // нового снимка быть не должно, и чтение на любую координату обязано
    // дать тот же ответ. Иначе ежедневный прогон раздувает ряд и делает
    // ось «когда мы узнали» бессмысленной.
    let (mut store, instrument) = store();
    let request = || ScheduleSyncRequest {
        instrument,
        secid: "SU46020RMFS2".to_owned(),
    };

    sync_schedule(&mut store, &Pages(Mutex::new(vec![WHOLE])), request())
        .await
        .expect("первый прогон");
    let after_first = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("снимок найден");

    sync_schedule(&mut store, &Pages(Mutex::new(vec![WHOLE])), request())
        .await
        .expect("второй прогон");
    let after_second = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("снимок найден");

    assert_eq!(after_first, after_second, "повтор изменил ответ");
}
```

- [ ] **Шаг 2: запустить тест**

Run: `cargo test -p iaam-app --test schedule_metamorphic`
Expected: PASS.

- [ ] **Шаг 3: расширить мутационный заслон**

В `scripts/check-mutants.sh`, в массив `MODULES`, добавить с комментарием о причине:

```bash
  # График выплат (E3.4 часть 2). Ошибка в инвариантах полноты не меняет
  # ни одной суммы — она меняет то, что система считает полным графиком,
  # и усечённый ряд молча укорачивает W_T.
  "crates/iaam-market/src/schedule/completeness.rs"
  "crates/iaam-market/src/moex/bondization.rs"
  "crates/iaam-app/src/scenarios/schedule.rs"
```

- [ ] **Шаг 4: прогнать мутантов**

Run: `make mutants`
Expected: выживших по новым модулям нет. Выживший мутант — недостающий тест, а не шум.

- [ ] **Шаг 5: два раздельных коммита**

```bash
git add crates/iaam-app/tests/schedule_metamorphic.rs
git commit -m "test(app): повторная синхронизация графика ничего не меняет (iaam-d8b)"

git add scripts/check-mutants.sh
POLICY_CHANGE_APPROVED=1 git commit -m "chore(policy): модули графика выплат в мутационном заслоне (iaam-d8b)"
```

---

## Порядок и зависимости

```
З1 (доменные типы) ─┬─> З2 (условия выпуска) ─> З9 (разбор описания) ──> З11 (условия)
                    ├─> З5 (разбор графика) ────────────────────────┐        ^
                    └─> З8 (инварианты полноты) ────────────────────┤        │
                                                                     │        │
З3 (миграция 0010) ─┬─> З4 (хранилище снимков) ────────────────────┤        │
                    └─> З6 (словарь кодов) ───────────────────────┤        │
                                                                     v        │
З7 (запрос и пагинация) ──────────────────────> З10 (сценарий графика) ──────┘
                                                          │
                                                          v
                                        З12 (фикстуры) ─> З13 (заслоны)
```

Задачи 1, 3 и 7 независимы и открывают три параллельные ветки. Задача 10 — единственная,
где сходится всё; до неё дерево остаётся зелёным на каждом коммите. Задача 11 идёт после 10,
потому что переиспользует `invalid`, `SOURCE_ID` и импорты того же модуля.

Задачи 12 и 13 трогают файлы политики и требуют отдельных коммитов с `POLICY_CHANGE_APPROVED=1`
и метки PR `policy-change`.
