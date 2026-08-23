//! Объектобезопасные порты. Единственное место, где они существуют (§3.2).

use async_trait::async_trait;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::projection::Snapshot;
use iaam_core::reconciliation::claim::ControlClaim;
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_core::rules::LotRuleVersion;
use iaam_ingest::SubmittedOperation;
use time::Date;
use uuid::Uuid;
use zeroize::Zeroizing;

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

/// Заведённый доступ в том виде, в каком его показывают владельцу.
///
/// Ни токена, ни шифротекста здесь нет и быть не может: то, чего порт
/// не вернул, транспорт не может отдать наружу ни ответом, ни логом (§14).
/// Момент заведения и момент отзыва — строки хранилища: часы одни на
/// всю крейту хранилища, и пересобирать их тип на границе значило бы
/// завести вторые часы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerAccessView {
    pub id: Uuid,
    pub broker: String,
    pub scope: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

/// Хранилище брокерских доступов.
///
/// Отдельный порт, а не метод `Store`: заведение доступа требует ключа
/// шифрования, которого у хранилища фактов нет и быть не должно.
///
/// Токен принимается в `Zeroizing<String>`, а не в `String`: открытым
/// он живёт ровно до шифрования и зануляется при уничтожении. Обычная
/// строка оставила бы его в освобождённой памяти процесса.
#[async_trait]
pub trait BrokerVault: Send + Sync {
    /// Завести доступ. Возвращает идентификатор записи — по нему доступ
    /// отзывают. Сам токен не возвращается: то, чего вызывающий не
    /// получил, он не может выдать наружу.
    async fn add_access(
        &self,
        owner: OwnerId,
        broker: String,
        token: Zeroizing<String>,
    ) -> Result<BrokerAccessView, AppError>;

    /// Все доступы владельца, включая отозванные: «когда система
    /// перестала ходить к брокеру» является вопросом, на который
    /// нужен ответ.
    async fn list_access(&self, owner: OwnerId) -> Result<Vec<BrokerAccessView>, AppError>;

    /// Отозвать доступ. Не удаление: отозванный остаётся историей.
    async fn revoke_access(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError>;
}

/// Почему обращение к брокеру не удалось.
///
/// Тип принадлежит порту, а не адаптеру: иначе сценарий узнал бы про
/// HTTP и про конкретного брокера через возвращаемое значение.
///
/// Ни один вариант не несёт токена: сообщение об ошибке — это то, что
/// точно попадёт в лог (§14).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrokerError {
    #[error("доступ к брокеру {broker} не заведён")]
    NoAccess { broker: String },
    #[error("доступ к брокеру {broker} заведён с правами, отличными от чтения")]
    ScopeNotReadOnly { broker: String },
    #[error("брокер {broker} отказал: {detail}")]
    Refused { broker: String, detail: String },
    #[error("брокер {broker} недоступен: {detail}")]
    Unreachable { broker: String, detail: String },
    #[error("ответ брокера {broker} не разобран: {detail}")]
    Unparsable { broker: String, detail: String },
}

/// Канал брокера: второй способ получить те же данные.
///
/// Существует ради независимости (§10.3): совпадение разобранного
/// отчёта с ответом API — основание 3, и только оно даёт
/// `accepted_independent` на реальных данных. Поэтому **реализация
/// этого порта не делит код разбора с парсерами отчётов**: общая
/// функция нормализации исказила бы обе стороны одной ошибкой, и
/// сверка её не заметила бы.
///
/// У брокера запрашивается только доступ на чтение. Метода, что-либо
/// отправляющего брокеру, здесь нет и не появится (§14).
#[async_trait]
pub trait BrokerChannel: Send + Sync {
    /// Операции счёта за интервал.
    async fn fetch_operations(
        &self,
        account: AccountId,
        from: Date,
        to: Date,
    ) -> Result<Vec<SubmittedOperation>, BrokerError>;

    /// Контрольные величины на дату: остатки и количества.
    ///
    /// Возвращает утверждения источника, а не расчёт: с ними потом
    /// сходится посчитанное по журналу.
    async fn fetch_portfolio(
        &self,
        account: AccountId,
        at: Date,
    ) -> Result<Vec<ControlClaim>, BrokerError>;

    /// Чем именно получены данные. Версия разбора и отсутствие
    /// документа — то, из чего выводится независимость канала.
    fn channel(&self) -> SourceChannel;
}

/// Системные часы.
pub struct SystemClock;

impl Clock for SystemClock {
    fn today(&self) -> Date {
        time::OffsetDateTime::now_utc().date()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_only_scope_may_neither_submit_nor_administer() {
        // Область действия токена — заслон безопасности, а не подсказка.
        // Слипшиеся значения дают либо читателя, который пишет в журнал,
        // либо владельца, который не может ничего.
        assert!(Scope::Owner.may_submit());
        assert!(Scope::Agent.may_submit());
        assert!(!Scope::ReadOnly.may_submit());

        assert!(Scope::Owner.may_administer());
        assert!(
            !Scope::Agent.may_administer(),
            "агент отправляет операции, но не управляет токенами (§14)"
        );
        assert!(!Scope::ReadOnly.may_administer());
    }

    #[test]
    fn every_scope_has_a_distinct_machine_readable_code() {
        assert_eq!(Scope::Owner.code(), "owner");
        assert_eq!(Scope::Agent.code(), "agent");
        assert_eq!(Scope::ReadOnly.code(), "read_only");
    }
}
