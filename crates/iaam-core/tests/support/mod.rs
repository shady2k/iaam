//! Сборка событий для интеграционных тестов ядра.
//!
//! Живёт отдельным модулем, потому что `test_support` внутри крейта
//! доступен только модульным тестам: интеграционный тест — внешний
//! потребитель и обязан собирать событие через публичный интерфейс.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use time::Date;

/// Канал получения данных в тестах: источник, версия разбора, документ.
///
/// Существует как отдельная сущность, а не как три аргумента, потому
/// что **источник входит в тождество канала**. Выдать каждому событию
/// свой случайный `SourceId` значит разложить один документ на столько
/// каналов, сколько в нём строк, — и ни одно основание, требующее
/// нескольких секций одного документа, не сработает (§10.3).
pub struct TestChannel {
    source: SourceId,
    parser: ParserVersion,
    document: RawHash,
}

impl TestChannel {
    /// Один документ, разобранный одним парсером.
    #[must_use]
    pub fn new(parser: &str, document: &str) -> Self {
        Self {
            source: SourceId::new_random(),
            parser: ParserVersion(parser.to_owned()),
            document: document_hash(document),
        }
    }

    fn provenance(&self) -> Provenance {
        Provenance::new(self.source, self.document.clone(), self.parser.clone())
    }
}

/// Хеш документа из читаемого имени.
///
/// Имя кодируется шестнадцатерично и дополняется до шестидесяти четырёх
/// знаков: `RawHash` принимает только корректный SHA-256, а тесту нужны
/// различимые и узнаваемые в отладке документы, а не настоящие хеши.
#[must_use]
pub fn document_hash(name: &str) -> RawHash {
    let mut hex: String = name.bytes().map(|byte| format!("{byte:02x}")).collect();
    assert!(hex.len() <= 64, "имя документа {name} слишком длинное");
    while hex.len() < 64 {
        hex.push('0');
    }
    RawHash::parse(&hex).expect("шестнадцатеричный хеш")
}

/// Куда и когда записывается событие.
///
/// Собрано структурой, а не четырьмя аргументами: у помощника, который
/// принимает подряд два идентификатора, дату и число, перепутать
/// аргументы местами легко, а заметить — нет.
#[derive(Debug, Clone, Copy)]
pub struct Posting {
    pub owner: OwnerId,
    pub account: AccountId,
    pub day: Date,
    pub sequence: u32,
}

/// Событие, пришедшее заданным каналом.
#[must_use]
pub fn event_on(channel: &TestChannel, posting: Posting, kind: EventKind, legs: Vec<Leg>) -> Event {
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner: posting.owner,
        account: posting.account,
        kind,
        dates: EventDates::for_cash(CashPostedDate(posting.day)),
        order: EffectiveOrder::new(posting.day, posting.sequence),
        legs,
        provenance: channel.provenance(),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}
