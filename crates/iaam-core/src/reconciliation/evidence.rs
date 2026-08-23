//! Основания автоматического повышения статуса (§10.3).
//!
//! Восемь оснований спеки плюс девятое — названный владельцем остаток
//! (§10.4). Участия человека ни одно из первых восьми не требует.
//!
//! **Уровень определяется независимостью канала, а не типом
//! основания.** Это главное правило модуля: основание задаёт лишь
//! потолок, а фактический уровень получается понижением потолка до
//! `internal`, если независимость не доказана.

use std::collections::BTreeSet;

use super::{ConfidenceLevel, Dimension};
use crate::event::provenance::{ParserVersion, RawHash};
use crate::ids::SourceId;

/// Канал, которым получены данные.
///
/// Документ — хеш файла, из которого разобраны данные. У ответа API
/// документа нет: это поток, а не файл, и `None` здесь означает именно
/// «файла не было», а не «хеш не посчитали».
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
    /// Названный владельцем остаток (§10.4).
    ///
    /// В восемь автоматических оснований не входит: требует участия
    /// человека. Уровень ограничен `internal` намеренно — владелец мог
    /// прочитать ту же цифру в том же отчёте, который мы разобрали,
    /// и независимость здесь не доказана, а §10.3 требует именно
    /// доказательства, а не типа основания.
    OwnerStatedBalance,
}

impl Ground {
    /// Потолок уровня, который основание может дать в принципе.
    ///
    /// Основания 1, 2 и 5 ограничены `internal` **по устройству**: они
    /// сравнивают данные, прошедшие через один и тот же парсер. Опустить
    /// это ограничение и положиться на проверку независимости нельзя:
    /// у оснований 1 и 2 документы разные, и проверка пропустила бы их,
    /// если бы версия парсера вдруг тоже отличалась.
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

    /// Все основания одним списком — для обходов и проверок полноты.
    #[must_use]
    pub const fn all() -> [Self; 9] {
        [
            Self::OpeningMatchesPriorClosing,
            Self::ContinuityBetweenStatements,
            Self::BrokerApiAgreesWithStatement,
            Self::DepositaryReportConfirms,
            Self::SeparateSectionsAgree,
            Self::PayoutConfirmsSchedule,
            Self::CorporateActionMatchesIssueTerms,
            Self::TaxAgentCertificate,
            Self::OwnerStatedBalance,
        ]
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
    /// Возвращает `None`, когда основание не подтверждает ни одного из
    /// сошедшихся измерений: основание, ничего не подтверждающее,
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
    /// канала не доказана. Обратного хода нет: основание, ограниченное
    /// `internal` по устройству, ограничено им всегда — проверка канала
    /// его не повышает.
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
