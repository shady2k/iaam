//! Справочники: счета, инструменты, версии контуров.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, InstrumentId, OwnerId};
use rusqlite::params;

use crate::{SqliteStore, StoreError, now};

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
            "INSERT INTO instruments
                 (id, kind, symbol, title,
                  denomination_currency, settlement_currency, quote_currency,
                  lineage_parent, lineage_reason, created_at)
             VALUES (?1, NULL, ?2, ?3, ?4, ?4, ?4, NULL, NULL, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 symbol = excluded.symbol,
                 title = excluded.title,
                 denomination_currency = excluded.denomination_currency,
                 settlement_currency = excluded.settlement_currency,
                 quote_currency = excluded.quote_currency",
            params![
                instrument.id.inner().to_string(),
                instrument.symbol,
                instrument.title,
                instrument.currency,
                now(),
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
