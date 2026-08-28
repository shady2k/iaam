//! Идемпотентность и дедупликация (§10.6).
//!
//! Иерархия ключей и порядок, в котором из них выбирают:
//!
//! | §10.6 | Ключ | Когда действует |
//! |---|---|---|
//! | 1 | `SourceOperationId` | источник дал стабильный идентификатор |
//! | 2 | `IdempotencyKey` | клиент назвал подачу |
//! | 4 | `DocumentRow` | известен документ **и** локатор строки |
//! | 3 | `NormalizedFingerprint` | документ известен, а локатора нет |
//! | 5 | подсказка по отпечатку | совпадение с записью другого документа |
//!
//! **Порядок выбора — 1, 2, 4, 3, а не 1, 2, 3, 4.** Спека нумерует
//! отпечаток третьим, но она же прямым текстом запрещает считать
//! дубликатом две законные одинаковые покупки в один день, а отпечаток
//! у них совпадает. Одно из двух обязано уступить, и уступает
//! нумерация: **внутри документа тождество строки — это её локатор,
//! а не её содержимое**. Документ и есть свидетельство того, что
//! операций было две: парсер увидел две строки.
//!
//! Отсюда же следует, что совпадение отпечатка внутри **одного**
//! документа на другом локаторе — `Fresh`, и даже не подсказка: иначе
//! отчёт с двумя одинаковыми покупками завалил бы владельца
//! подсказками на ровном месте. Между разными документами тот же
//! отпечаток — подсказка пятого уровня, которая ничего не удаляет.
//!
//! Естественный ключ «счёт + дата + сумма» не используется нигде: он
//! даёт ложные совпадения и не ловит дубликаты после нормализации.

use iaam_core::event::provenance::RawHash;
use iaam_core::ids::{AccountId, EventId};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::journal_event::{JournalFact, SubmittedJournalEvent};
use crate::operation::{OperationDates, OperationKind, SubmittedOperation};

/// Версия канонической формы отпечатка.
///
/// Входит в саму форму: по отпечаткам уже дедуплицировано, и смена
/// формы обязана быть видимой, а не тихой.
const CANONICAL_VERSION: u8 = 1;

/// Уровень иерархии §10.6, по которому принято решение.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DedupLevel {
    SourceOperationId,
    IdempotencyKey,
    NormalizedFingerprint,
    DocumentRow,
    /// Вероятностная оценка. Показывается владельцу, ничего не удаляет.
    Probabilistic,
}

impl DedupLevel {
    /// Номер уровня в §10.6. Нужен ответу владельцу: «почему система
    /// решила, что это уже было» — это ссылка на уровень спеки.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::SourceOperationId => 1,
            Self::IdempotencyKey => 2,
            Self::NormalizedFingerprint => 3,
            Self::DocumentRow => 4,
            Self::Probabilistic => 5,
        }
    }
}

/// Ключ, по которому строка признаётся уже виденной.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupKey {
    SourceOperationId(String),
    IdempotencyKey(String),
    NormalizedFingerprint {
        document: RawHash,
        fingerprint: RawHash,
    },
    DocumentRow {
        document: RawHash,
        sheet: Option<String>,
        row: u64,
    },
}

impl DedupKey {
    #[must_use]
    pub const fn level(&self) -> DedupLevel {
        match self {
            Self::SourceOperationId(_) => DedupLevel::SourceOperationId,
            Self::IdempotencyKey(_) => DedupLevel::IdempotencyKey,
            Self::NormalizedFingerprint { .. } => DedupLevel::NormalizedFingerprint,
            Self::DocumentRow { .. } => DedupLevel::DocumentRow,
        }
    }

    /// Порядок выбора: чем меньше, тем сильнее.
    ///
    /// Отличается от номера уровня §10.6 намеренно — см. заголовок
    /// модуля. Вынесен отдельным числом, чтобы выбор сильнейшего был
    /// проверяемым, а не следствием порядка ветвей в `choose_key`.
    #[must_use]
    pub const fn precedence(&self) -> u8 {
        match self {
            Self::SourceOperationId(_) => 1,
            Self::IdempotencyKey(_) => 2,
            Self::DocumentRow { .. } => 3,
            Self::NormalizedFingerprint { .. } => 4,
        }
    }
}

/// Откуда пришла строка.
///
/// `document: None` — канал без файла: ответ API брокера это поток,
/// а не документ, и `None` означает именно «файла не было».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentContext {
    pub document: Option<RawHash>,
    pub sheet: Option<String>,
    pub row: Option<u64>,
}

/// Уже записанный факт, с которым сравнивают.
///
/// Собирается оболочкой из журнала: всё перечисленное лежит
/// в `provenance` события.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownRecord {
    pub event: EventId,
    pub source_operation_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub fingerprint: RawHash,
    pub document: Option<RawHash>,
    pub sheet: Option<String>,
    pub row: Option<u64>,
}

/// Что делать со строкой.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupDecision {
    /// Уже записано ранее.
    Duplicate { key: DedupKey, existing: EventId },
    /// Не встречалось.
    Fresh,
    /// Похоже на дубликат, но доказательства нет.
    ///
    /// **Никогда** не приводит к удалению: показывается владельцу
    /// вместе с записанной строкой (§10.6).
    PossibleDuplicate { of: EventId, level: DedupLevel },
}

impl DedupDecision {
    /// Записывается ли строка.
    ///
    /// Существует ради того, чтобы «вероятностный дубликат не
    /// отбрасывается» было проверяемым свойством, а не обещанием
    /// в комментарии.
    #[must_use]
    pub const fn records_the_row(&self) -> bool {
        match self {
            Self::Fresh | Self::PossibleDuplicate { .. } => true,
            Self::Duplicate { .. } => false,
        }
    }
}

/// Сильнейший доступный ключ. `None` — строку не называет ничто.
#[must_use]
pub fn choose_key(operation: &SubmittedOperation, context: &DocumentContext) -> Option<DedupKey> {
    let mut available: Vec<DedupKey> = Vec::new();
    // Порядок добавления намеренно не совпадает с иерархией: её задаёт
    // `precedence`, а не случайный порядок появления кандидатов.
    if let Some(document) = context.document.clone() {
        match context.row {
            Some(row) => available.push(DedupKey::DocumentRow {
                document,
                sheet: context.sheet.clone(),
                row,
            }),
            None => available.push(DedupKey::NormalizedFingerprint {
                fingerprint: fingerprint(operation),
                document,
            }),
        }
    }
    if let Some(id) = operation.source_operation_id.as_deref() {
        available.push(DedupKey::SourceOperationId(id.to_owned()));
    }
    if let Some(key) = operation.idempotency_key.as_deref() {
        available.push(DedupKey::IdempotencyKey(key.to_owned()));
    }
    available.sort_by_key(DedupKey::precedence);
    available.into_iter().next()
}

/// Решение по строке.
///
/// Порядок: точное совпадение выбранного ключа — дубликат; иначе
/// совпадение отпечатка с записью **другого** документа или канала без
/// файла — подсказка; иначе строка новая.
#[must_use]
pub fn assess(
    key: Option<&DedupKey>,
    fingerprint: &RawHash,
    context: &DocumentContext,
    known: &[KnownRecord],
) -> DedupDecision {
    if let Some(key) = key
        && let Some(existing) = known.iter().find(|record| matches_key(record, key))
    {
        return DedupDecision::Duplicate {
            key: key.clone(),
            existing: existing.event,
        };
    }
    known
        .iter()
        .find(|record| &record.fingerprint == fingerprint && !same_document(record, context))
        .map_or(DedupDecision::Fresh, |record| {
            DedupDecision::PossibleDuplicate {
                of: record.event,
                level: DedupLevel::Probabilistic,
            }
        })
}

/// Доказано ли, что запись и строка пришли из одного документа.
///
/// Только доказанное совпадение снимает подсказку: документ —
/// свидетельство того, что операций было две. Два канала без файла
/// такого свидетельства не дают, и `None == None` здесь означало бы
/// «оба ниоткуда, значит из одного места» — молчаливый пропуск
/// повторной выгрузки из API.
fn same_document(record: &KnownRecord, context: &DocumentContext) -> bool {
    matches!(
        (record.document.as_ref(), context.document.as_ref()),
        (Some(known), Some(incoming)) if known == incoming
    )
}

/// Совпадает ли запись с ключом.
///
/// Исчерпывающий `match`: новый вид ключа обязан сломать сборку здесь,
/// а не молча перестать ловить дубликаты.
fn matches_key(record: &KnownRecord, key: &DedupKey) -> bool {
    match key {
        DedupKey::SourceOperationId(id) => record.source_operation_id.as_deref() == Some(id),
        DedupKey::IdempotencyKey(value) => record.idempotency_key.as_deref() == Some(value),
        DedupKey::NormalizedFingerprint {
            document,
            fingerprint,
        } => record.document.as_ref() == Some(document) && &record.fingerprint == fingerprint,
        DedupKey::DocumentRow {
            document,
            sheet,
            row,
        } => {
            record.document.as_ref() == Some(document)
                && record.sheet.as_deref() == sheet.as_deref()
                && record.row == Some(*row)
        }
    }
}

/// Каноническая форма операции: то, от чего считается отпечаток.
///
/// Ключ идемпотентности и идентификатор операции источника в неё
/// **не входят**: они называют подачу, а не операцию. Одна и та же
/// операция, посланная с разными ключами, обязана давать один
/// отпечаток — иначе третий уровень не поймает ничего.
#[must_use]
pub fn canonical_form(operation: &SubmittedOperation) -> String {
    let canonical = Canonical {
        v: CANONICAL_VERSION,
        account: operation.account,
        kind: &operation.kind,
        dates: CanonicalDates::of(operation.dates),
    };
    serde_json::to_string(&canonical).unwrap_or_else(|_| unrepresentable_operation())
}

/// Отпечаток нормализованной записи (§10.6, третий уровень).
#[must_use]
pub fn fingerprint(operation: &SubmittedOperation) -> RawHash {
    let digest = Sha256::digest(canonical_form(operation).as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    // Длина и алфавит гарантированы SHA-256, поэтому разбор не может
    // отказать; но подставлять заглушку в случае отказа нельзя —
    // отпечаток без хеша не должен существовать.
    RawHash::parse(&hex).unwrap_or_else(|| unreachable_hash())
}

/// Отпечаток журнального факта (§10.6, третий уровень).
///
/// Отдельная функция, а не общая с операциями: канонические формы
/// разные, и объединение их одним `enum` сделало бы формат операции
/// зависимым от появления второй семьи. Отпечаток — это формат, и он
/// не должен меняться от того, что рядом завели новый вход.
#[must_use]
pub fn fingerprint_journal_event(submitted: &SubmittedJournalEvent) -> RawHash {
    let digest = Sha256::digest(canonical_journal_form(submitted).as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    RawHash::parse(&hex).unwrap_or_else(|| unreachable_hash())
}

/// Каноническая форма журнального факта.
///
/// Ключ идемпотентности и идентификатор факта в источнике в неё
/// **не входят** — по той же причине, что и у операций: они называют
/// подачу, а не факт.
#[must_use]
pub fn canonical_journal_form(submitted: &SubmittedJournalEvent) -> String {
    let canonical = CanonicalJournalEvent {
        v: CANONICAL_VERSION,
        account: submitted.account,
        fact: &submitted.fact,
    };
    serde_json::to_string(&canonical).unwrap_or_else(|_| unrepresentable_operation())
}

/// Каноническая форма журнального факта. Даты внутри самого факта
/// и сериализуются его собственным представлением: именно им факт
/// уходит в хранилище (`iaam-store/src/events.rs`), и вторая запись
/// того же факта другой формой разошлась бы с первой.
#[derive(Serialize)]
struct CanonicalJournalEvent<'a> {
    v: u8,
    account: AccountId,
    fact: &'a JournalFact,
}

/// Каноническая форма. Поля в порядке объявления — этот порядок и есть
/// формат, поэтому структура отдельная, а не заимствованная у DTO.
#[derive(Serialize)]
struct Canonical<'a> {
    v: u8,
    account: AccountId,
    kind: &'a OperationKind,
    dates: CanonicalDates,
}

/// Даты в канонической форме — строки ISO 8601.
///
/// Собственная сериализация `time::Date` даёт порядковую дату
/// (`[2026, 91]`): она зависит от внутреннего представления библиотеки
/// и нечитаема глазом, а формат отпечатка обязан быть и тем, и другим.
#[derive(Serialize)]
struct CanonicalDates {
    trade: Option<String>,
    settled: Option<String>,
    cash_posted: Option<String>,
    paid: Option<String>,
}

impl CanonicalDates {
    /// `Display` у `time::Date` — это ISO 8601 календарная дата, и
    /// отказать он не может, в отличие от форматирования по описанию.
    fn of(dates: OperationDates) -> Self {
        Self {
            trade: dates.trade.map(|day| day.to_string()),
            settled: dates.settled.map(|day| day.to_string()),
            cash_posted: dates.cash_posted.map(|day| day.to_string()),
            paid: dates.paid.map(|day| day.to_string()),
        }
    }
}

/// Отдельные функции вместо `unwrap`: `unwrap` в этих местах читался бы
/// как «а вдруг», хотя оба варианта невозможны по построению.
fn unrepresentable_operation() -> ! {
    panic!("операция состоит из чисел, строк и дат: JSON её представляет всегда")
}

fn unreachable_hash() -> ! {
    panic!("SHA-256 всегда даёт 64 шестнадцатеричных знака")
}
