//! Словарь видов операций канала (§14, эпик iaam-d8b.2.2).
//!
//! Соответствие «код источника → вид операции» — это **данные**,
//! а не код: множество кодов принадлежит брокеру, меняется без нашего
//! участия и пополняется чаще, чем выходят релизы. Пока оно жило
//! в `match`, о новом коде система узнавала из отклонённой строки
//! импорта — то есть тогда, когда владелец уже пытался что-то посчитать.
//!
//! Вид хранится строкой и хранилищем **не толкуется** — по той же
//! причине, что область прав и среда доступа: разбирает её `iaam-broker`,
//! которому этот словарь принадлежит по смыслу. Закрытость списка
//! стережёт `CHECK` схемы, а не код этого модуля.
//!
//! Владельца в ключе нет: словарь описывает брокерское API, а не
//! владельца. `OPERATION_TYPE_COUPON` означает купон у всех, кто ходит
//! в T-Invest.

use std::collections::BTreeMap;

use rusqlite::{TransactionBehavior, params};

use crate::documents::BrokerCode;
use crate::{SqliteStore, StoreError, now};

/// Откуда взялась строка словаря.
///
/// Различать обязательно: обновление словаря из контракта не имеет
/// права затирать решение владельца, а без происхождения эти две
/// строки неотличимы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindOrigin {
    /// Из опубликованного контракта брокера.
    Contract,
    /// Решение владельца.
    Owner,
}

impl KindOrigin {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::Owner => "owner",
        }
    }
}

/// Строка словаря, предлагаемая к записи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerOperationKind {
    /// Как вид назвал канал.
    pub source_kind: String,
    /// Во что он превращается. Толкует `iaam-broker`.
    pub kind: String,
}

/// Сколько строк словарь принял и сколько уже знал.
///
/// Возвращается, а не пишется в журнал: обновление словаря обязано
/// уметь сказать, что именно изменилось, иначе «прошло успешно»
/// неотличимо от «не сделало ничего».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DictionaryOutcome {
    pub added: usize,
    pub already_known: usize,
}

impl SqliteStore {
    /// Пополнить словарь канала из контракта.
    ///
    /// Существующие строки **не** трогаются, и решение владельца
    /// в том числе: обновление добавляет то, чего не было, а не
    /// переписывает то, что есть. Иначе ночной прогон бесшумно
    /// отменял бы разбор, заведённый руками.
    pub fn extend_broker_operation_kinds(
        &mut self,
        broker: &BrokerCode,
        dictionary: &str,
        entries: &[BrokerOperationKind],
    ) -> Result<DictionaryOutcome, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut outcome = DictionaryOutcome::default();
        {
            let mut statement = transaction.prepare(
                "INSERT INTO broker_operation_kinds
                     (broker, source_kind, kind, origin, dictionary, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (broker, source_kind) DO NOTHING",
            )?;
            for entry in entries {
                let inserted = statement.execute(params![
                    broker.as_str(),
                    entry.source_kind,
                    entry.kind,
                    KindOrigin::Contract.code(),
                    dictionary,
                    now(),
                ])?;
                if inserted == 0 {
                    outcome.already_known += 1;
                } else {
                    outcome.added += 1;
                }
            }
        }
        transaction.commit()?;
        Ok(outcome)
    }

    /// Записать решение владельца о виде.
    ///
    /// Оно перекрывает строку из контракта: владелец знает про свой
    /// портфель то, чего в контракте нет.
    pub fn set_broker_operation_kind(
        &mut self,
        broker: &BrokerCode,
        entry: &BrokerOperationKind,
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO broker_operation_kinds
                 (broker, source_kind, kind, origin, dictionary, recorded_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5)
             ON CONFLICT (broker, source_kind) DO UPDATE SET
                 kind = excluded.kind,
                 origin = excluded.origin,
                 dictionary = NULL,
                 recorded_at = excluded.recorded_at",
            params![
                broker.as_str(),
                entry.source_kind,
                entry.kind,
                KindOrigin::Owner.code(),
                now(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Весь словарь канала одним чтением.
    ///
    /// Не «вид по коду»: разбор идёт пачкой, и запрос на строку
    /// превратил бы одну выгрузку в тысячу обращений к базе.
    pub fn broker_operation_kinds(
        &self,
        broker: &BrokerCode,
    ) -> Result<BTreeMap<String, String>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT source_kind, kind FROM broker_operation_kinds WHERE broker = ?1")?;
        let rows = statement.query_map(params![broker.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut dictionary = BTreeMap::new();
        for row in rows {
            let (source_kind, kind) = row?;
            dictionary.insert(source_kind, kind);
        }
        Ok(dictionary)
    }
}
