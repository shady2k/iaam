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
