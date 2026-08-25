//! Объектобезопасные порты. Единственное место, где они существуют (§3.2).

use async_trait::async_trait;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId};
use iaam_core::projection::Snapshot;
use iaam_core::reconciliation::claim::ControlClaim;
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_core::rules::LotRuleVersion;
use iaam_ingest::SubmittedOperation;
use serde_json::Value;
use std::sync::Arc;
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
/// Инструмент как его видит транспорт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentView {
    pub id: InstrumentId,
    /// `None` — род не установлен. Оценка такого инструмента неполна,
    /// и подставлять акцию по умолчанию запрещено (§4.9).
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
}

/// Действующий псевдоним инструмента.
///
/// Поля `source` здесь нет намеренно: справочник глобален и читается
/// всеми, а `SourceId` указывает на документ конкретного владельца.
/// Отдать его наружу означало бы раскрыть существование чужой
/// загрузки (§14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasView {
    pub namespace: String,
    pub value: String,
    pub instrument: InstrumentId,
    pub valid_from: Date,
    pub valid_to: Option<Date>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyView {
    pub id: CustodyId,
    pub title: String,
    pub institution: Option<String>,
}

/// Справочник инструментов (§4.5, §4.7).
#[async_trait]
pub trait InstrumentDirectory: Send + Sync {
    /// Инструмент по внешнему коду на дату.
    ///
    /// Дата обязательна и умолчания «сегодня» не имеет: ISIN меняется
    /// корпоративным действием, поэтому «текущего» ответа на вопрос
    /// «какой инструмент стоит за этим кодом» не существует (§4.7).
    async fn resolve(
        &self,
        namespace: &str,
        value: &str,
        on: Date,
    ) -> Result<InstrumentId, AppError>;

    async fn instrument(&self, id: InstrumentId) -> Result<Option<InstrumentView>, AppError>;

    async fn list_instruments(&self) -> Result<Vec<InstrumentView>, AppError>;

    /// Все псевдонимы со своими интервалами.
    ///
    /// Отдаются целиком, одним запросом: разбор документа иначе ходил бы
    /// в базу на каждую строку.
    async fn list_aliases(&self) -> Result<Vec<AliasView>, AppError>;

    async fn list_custody_places(&self, owner: OwnerId) -> Result<Vec<CustodyView>, AppError>;
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
    /// Среда брокера: боевая или песочница. Часть ответа намеренно —
    /// по списку доступов должно быть видно, куда система ходит.
    pub environment: String,
    pub scope: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

/// Сохранённое правило в форме, которую может отдавать транспорт.
///
/// Сами JSON matcher/outcome остаются непрозрачными для хранилища и
/// возвращаются без повторной трактовки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationRuleView {
    pub id: uuid::Uuid,
    pub version: u32,
    pub matcher: String,
    pub outcome: String,
    pub created_at: String,
    pub retired_at: Option<String>,
    pub replaces: Option<uuid::Uuid>,
}

/// Порт исторических правил классификации.
#[async_trait]
pub trait ClassificationRuleStore: Send + Sync {
    async fn list_rules(&self, owner: OwnerId) -> Result<Vec<ClassificationRuleView>, AppError>;
    async fn create_rule(
        &self,
        owner: OwnerId,
        matcher: String,
        outcome: String,
        replaces: Option<uuid::Uuid>,
    ) -> Result<ClassificationRuleView, AppError>;
    async fn retire_rule(&self, owner: OwnerId, id: uuid::Uuid) -> Result<(), AppError>;
}

/// Среда брокера в словаре порта.
///
/// Отдельное перечисление, а не тип `iaam-broker`: транспорт зовёт порт
/// и адаптера знать не должен — как уже сделано для области прав
/// (`Scope` порта против `BrokerScope` брокера). Адрес шлюза сюда
/// не входит: он свойство адаптера, который в эту среду ходит.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerEnvironment {
    Prod,
    Sandbox,
}

impl BrokerEnvironment {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Prod => "prod",
            Self::Sandbox => "sandbox",
        }
    }
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
        environment: BrokerEnvironment,
        token: Zeroizing<String>,
    ) -> Result<BrokerAccessView, AppError>;

    /// Все доступы владельца, включая отозванные: «когда система
    /// перестала ходить к брокеру» является вопросом, на который
    /// нужен ответ.
    async fn list_access(&self, owner: OwnerId) -> Result<Vec<BrokerAccessView>, AppError>;

    /// Отозвать доступ. Не удаление: отозванный остаётся историей.
    async fn revoke_access(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError>;
}

/// Кого считать владельцем, когда его не назвали явно.
///
/// Идентификатор владельца нигде не печатается — при выпуске токена
/// наружу уходит только сам токен, — и человеку взять его неоткуда.
/// Поэтому единственного владельца система узнаёт сама. Тип принадлежит
/// порту, а не хранилищу: иначе транспорт узнал бы про SQLite через
/// возвращаемое значение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoleOwner {
    /// Токен ещё не выпускался: экземпляр не присвоен.
    None,
    Single(OwnerId),
    /// Владельцев несколько. Для однопользовательской системы это
    /// следы поломки, а не состояние: выбирать за человека нельзя.
    Several,
}

/// Только что выпущенный токен.
///
/// Сам токен здесь **обычная** `String`, а не `Zeroizing`: он выпущен
/// затем, чтобы уйти в тело ответа, и путь до сокета всё равно
/// проходит через буферы сериализации, которые занулить нечем.
/// Зануляемая обёртка обещала бы гарантию, которой на этом пути нет;
/// брокерский токен — другое дело, он наружу не возвращается никогда.
/// Показывается **один раз**: в базе остаётся только хеш.
#[derive(Clone)]
pub struct IssuedToken {
    pub id: Uuid,
    pub token: String,
    pub label: String,
    pub scope: Scope,
}

/// `Debug` вручную: производный вывел бы токен в первый же лог,
/// а лог — это то, что переживает процесс.
impl std::fmt::Debug for IssuedToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IssuedToken")
            .field("id", &self.id)
            .field("token", &"<скрыт>")
            .field("label", &self.label)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Выданный токен в том виде, в каком его показывают владельцу.
///
/// Ни токена, ни его хеша здесь нет и быть не может: хеш — это то, что
/// достаточно подставить в запрос поиска, чтобы система признала
/// предъявителя своим. То, чего порт не вернул, транспорт не может
/// отдать наружу ни ответом, ни логом (§14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenView {
    pub id: Uuid,
    pub label: String,
    pub scope: Scope,
    pub created_at: String,
    /// Момент отзыва. `None` — токен действует.
    pub revoked_at: Option<String>,
}

/// Управление токенами.
///
/// Отдельный порт, а не методы `Store`: `Store` читает и пишет факты
/// портфеля, а здесь выдаются и отбираются права на них. Смешение
/// означало бы, что всякий, кому дали читать журнал, получил заодно
/// возможность выписать себе второй токен.
///
/// **Выпуск токена существует только здесь.** Раньше он жил в точке
/// сборки, а маршруту понадобился бы свой; две реализации «случайные
/// байты, хеш в базу, токен наружу» расходятся молча и дают токены
/// разной стойкости, причём слабый выглядит ровно как сильный.
#[async_trait]
pub trait TokenAdmin: Send + Sync {
    /// Владелец, если он в системе один.
    ///
    /// Нужен и присвоению экземпляра (владелец уже есть — присваивать
    /// нечего), и консольной выдаче токена (выпускать второму владельцу
    /// нельзя).
    async fn sole_owner(&self) -> Result<SoleOwner, AppError>;

    /// Выпустить токен. Возвращает его открытым **один раз**: в базе
    /// остаётся хеш, и второй раз показать токен неоткуда.
    async fn issue_token(
        &self,
        owner: OwnerId,
        label: String,
        scope: Scope,
    ) -> Result<IssuedToken, AppError>;

    /// Все токены владельца, включая отозванные: «когда токен перестал
    /// пускать» является вопросом, на который нужен ответ.
    async fn list_tokens(&self, owner: OwnerId) -> Result<Vec<TokenView>, AppError>;

    /// Отозвать токен. Не удаление: отозванный остаётся историей.
    /// Чужой и несуществующий дают одинаковый отказ намеренно (§14).
    async fn revoke_token(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError>;
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

/// Операция канала, которую нельзя принять в журнал.
///
/// Исходный JSON сохраняется без перекодирования в другой доменный тип:
/// вызывающий должен иметь возможность объяснить расхождение по полям
/// ответа брокера.
#[derive(Debug, Clone, PartialEq)]
pub struct Quarantined {
    pub raw: Value,
    pub reason: String,
}

/// Результат получения страницы операций брокерского канала.
///
/// Отказанные строки не теряются: они отделены от принятых операций, но
/// доезжают до вызывающего вместе с причиной и исходным JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOperations {
    pub accepted: Vec<SubmittedOperation>,
    pub quarantined: Vec<Quarantined>,
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
    /// Операции счёта за интервал: принятые и отправленные в карантин.
    async fn fetch_operations(
        &self,
        account: AccountId,
        from: Date,
        to: Date,
    ) -> Result<ParsedOperations, BrokerError>;

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

/// Фабрика канала, скрывающая от сценария хранение и расшифровку доступа.
///
/// Секрет пересекает границу только внутри реализации адаптера и никогда
/// не возвращается в приложение или транспорт.
#[async_trait]
pub trait BrokerChannelFactory: Send + Sync {
    async fn open(&self, owner: OwnerId, broker: &str) -> Result<Arc<dyn BrokerChannel>, AppError>;
}

/// Явная заглушка точки сборки без настроенного адаптера.
pub struct UnavailableBrokerChannelFactory;

#[async_trait]
impl BrokerChannelFactory for UnavailableBrokerChannelFactory {
    async fn open(
        &self,
        _owner: OwnerId,
        _broker: &str,
    ) -> Result<Arc<dyn BrokerChannel>, AppError> {
        Err(AppError::NotConfigured {
            what: "канал брокера",
        })
    }
}

/// Явная заглушка порта правил для тестовых сборок без хранилища правил.
pub struct UnavailableClassificationRuleStore;

#[async_trait]
impl ClassificationRuleStore for UnavailableClassificationRuleStore {
    async fn list_rules(&self, _owner: OwnerId) -> Result<Vec<ClassificationRuleView>, AppError> {
        Err(AppError::NotConfigured {
            what: "правила классификации",
        })
    }

    async fn create_rule(
        &self,
        _owner: OwnerId,
        _matcher: String,
        _outcome: String,
        _replaces: Option<uuid::Uuid>,
    ) -> Result<ClassificationRuleView, AppError> {
        Err(AppError::NotConfigured {
            what: "правила классификации",
        })
    }

    async fn retire_rule(&self, _owner: OwnerId, _id: uuid::Uuid) -> Result<(), AppError> {
        Err(AppError::NotConfigured {
            what: "правила классификации",
        })
    }
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
    /// Порт обязан быть объектобезопасным: точка сборки держит
    /// адаптеры за `Arc<dyn ...>`, и выбор адаптера не должен
    /// подниматься в типы на этапе компиляции (§3.2).
    #[test]
    fn the_instrument_directory_port_is_object_safe() {
        fn accepts(_: &dyn InstrumentDirectory) {}
        let _: fn(&dyn InstrumentDirectory) = accepts;
    }
}
