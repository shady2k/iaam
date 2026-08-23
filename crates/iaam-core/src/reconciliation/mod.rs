//! Сверка: статус полноты счёта на интервале по измерению (§10.3).
//!
//! **Статус присваивается не операции.** Операция либо записана, либо
//! нет; утверждать про неё «подтверждена» бессмысленно — подтверждается
//! полнота интервала: что за март по деньгам учтено всё и ничего
//! лишнего. Поэтому единицей статуса является пара интервал×измерение,
//! а не событие, и поля «уровень достоверности» у события не существует.

pub mod check;
pub mod claim;
pub mod evidence;
pub mod observed;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use time::Date;

use crate::event::{Event, kind::EventKind};
use crate::ids::AccountId;
use check::{ClaimOutcome, check_claim};
use claim::{AssertionPeriod, BalancePoint, ControlClaim};
use evidence::{Evidence, Ground, SourceChannel};
use observed::{ObserveError, observe};

/// Измерение, о полноте которого делается утверждение (§10.3).
///
/// Разделение обязательно: подтверждённый остаток принимает деньги и
/// количества, но **не подтверждает** налоговую стоимость и
/// классификацию доходов. Одно измерение на всё превратило бы
/// «остаток сошёлся» в «налоги посчитаны верно».
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Dimension {
    Cash,
    Positions,
    TaxBasis,
    Income,
}

impl Dimension {
    /// Машиночитаемый код для API (§13).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Cash => "cash",
            Self::Positions => "positions",
            Self::TaxBasis => "tax_basis",
            Self::Income => "income",
        }
    }

    /// Все измерения одним списком.
    ///
    /// Обход по измерениям пишется через него, а не литералом на месте
    /// вызова: литерал с пропущенным вариантом компилируется, и
    /// пропавшее измерение молча не получает статуса.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Cash, Self::Positions, Self::TaxBasis, Self::Income]
    }
}

/// Уровень достоверности утверждения (§10.3).
///
/// Порядок значим: сравнение используется для повышения статуса.
/// Уровней три, а не два, потому что операции и контрольные остатки
/// извлекаются одним парсером из одного документа: общая ошибка разбора
/// исказит обе стороны проверки одинаково, и сверка её не заметит.
/// Средний уровень существует ровно для этого случая и называет вещи
/// своими именами — «сошлось внутри одного источника».
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ConfidenceLevel {
    Provisional,
    AcceptedInternal,
    AcceptedIndependent,
}

impl ConfidenceLevel {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::AcceptedInternal => "accepted_internal",
            Self::AcceptedIndependent => "accepted_independent",
        }
    }
}

/// Статус измерения на интервале (§10.3).
///
/// Четыре значения спеки. `Discrepant` — не уровень, а поглощающее
/// состояние: несошедшаяся цифра не перестаёт быть несошедшейся оттого,
/// что рядом сошлась другая.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DimensionStatus {
    Provisional,
    AcceptedInternal,
    AcceptedIndependent,
    Discrepant,
}

impl DimensionStatus {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::AcceptedInternal => "accepted_internal",
            Self::AcceptedIndependent => "accepted_independent",
            Self::Discrepant => "discrepant",
        }
    }

    const fn from_level(level: ConfidenceLevel) -> Self {
        match level {
            ConfidenceLevel::Provisional => Self::Provisional,
            ConfidenceLevel::AcceptedInternal => Self::AcceptedInternal,
            ConfidenceLevel::AcceptedIndependent => Self::AcceptedIndependent,
        }
    }

    /// Насколько статус хорош. Расхождение — худшее из возможного,
    /// поэтому стоит ниже отсутствия подтверждения: «не сошлось» хуже,
    /// чем «пока не проверяли».
    const fn rank(self) -> u8 {
        match self {
            Self::Discrepant => 0,
            Self::Provisional => 1,
            Self::AcceptedInternal => 2,
            Self::AcceptedIndependent => 3,
        }
    }
}

/// Одно проверенное утверждение вместе с исходом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimCheck {
    pub claim: ControlClaim,
    pub outcome: ClaimOutcome,
}

/// Утверждение о полноте счёта на интервале.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationStatus {
    account: AccountId,
    period: AssertionPeriod,
    dimensions: BTreeMap<Dimension, DimensionStatus>,
    evidence: Vec<Evidence>,
    outcomes: Vec<ClaimCheck>,
}

impl ReconciliationStatus {
    #[must_use]
    pub const fn account(&self) -> AccountId {
        self.account
    }

    #[must_use]
    pub const fn period(&self) -> AssertionPeriod {
        self.period
    }

    /// Статус измерения.
    ///
    /// Отсутствие записи означает `Provisional`: об измерении, о котором
    /// ничего не утверждали, ничего и не известно.
    #[must_use]
    pub fn dimension(&self, dimension: Dimension) -> DimensionStatus {
        self.dimensions
            .get(&dimension)
            .copied()
            .unwrap_or(DimensionStatus::Provisional)
    }

    #[must_use]
    pub fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }

    #[must_use]
    pub fn outcomes(&self) -> &[ClaimCheck] {
        &self.outcomes
    }
}

/// Группа утверждений одного документа об одном счёте за один интервал.
///
/// Группируется линейным поиском, а не картой: канал не упорядочен
/// осмысленно, а документов у владельца единицы. Карта потребовала бы
/// порядка ради порядка.
#[derive(Debug, Clone)]
struct StatementGroup {
    account: AccountId,
    period: AssertionPeriod,
    channel: SourceChannel,
    claims: Vec<ControlClaim>,
}

/// Реестр статусов: чистая функция от журнала (§3.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconciliationLedger {
    statuses: Vec<ReconciliationStatus>,
}

impl ReconciliationLedger {
    /// Сборка реестра из журнала.
    ///
    /// Логика вынесена из конструктора с именем `new` намеренно (§15.7).
    pub fn build(events: &[Event]) -> Result<Self, ObserveError> {
        let groups = collect_groups(events);

        // Шаг 1: каждая группа сверяется со своей проекцией.
        let mut checked: Vec<Vec<ClaimCheck>> = Vec::with_capacity(groups.len());
        for group in &groups {
            let observed = observe(events, group.account, group.period)?;
            checked.push(
                group
                    .claims
                    .iter()
                    .map(|claim| ClaimCheck {
                        claim: *claim,
                        outcome: check_claim(claim, &observed),
                    })
                    .collect(),
            );
        }

        // Шаг 2: основания, которые журнал в состоянии породить сам.
        let mut evidence: Vec<(AccountId, AssertionPeriod, Evidence)> = Vec::new();
        for (index, outcomes) in checked.iter().enumerate() {
            let group = &groups[index];
            if let Some(item) = ground_five(group, outcomes) {
                evidence.push((group.account, group.period, item));
            }
            if let Some((period, item)) = ground_one(group, outcomes, &groups) {
                evidence.push((group.account, period, item));
            }
        }
        evidence.extend(ground_two(&groups));
        evidence.extend(ground_three(&groups, &checked));

        // Шаг 3: статусы.
        let mut statuses: Vec<ReconciliationStatus> = Vec::new();
        for (index, outcomes) in checked.into_iter().enumerate() {
            merge_status(
                &mut statuses,
                build_status(&groups[index], outcomes, &evidence),
            );
        }
        Ok(Self { statuses })
    }

    /// Добавление оснований, которые журнал породить пока не может:
    /// депозитарный отчёт, параметры выпуска, справка налогового агента,
    /// подтверждение графика выплат (E3, E5, E7).
    #[must_use]
    pub fn with_external_evidence(
        mut self,
        items: Vec<(AccountId, AssertionPeriod, Evidence)>,
    ) -> Self {
        for (account, period, item) in items {
            let level = DimensionStatus::from_level(item.level());
            let dimensions = item.dimensions();
            if let Some(status) = self
                .statuses
                .iter_mut()
                .find(|status| status.account == account && status.period == period)
            {
                raise(&mut status.dimensions, &dimensions, level);
                status.evidence.push(item);
            } else {
                let mut map = BTreeMap::new();
                raise(&mut map, &dimensions, level);
                self.statuses.push(ReconciliationStatus {
                    account,
                    period,
                    dimensions: map,
                    evidence: vec![item],
                    outcomes: Vec::new(),
                });
            }
        }
        self
    }

    pub fn statuses(&self) -> impl Iterator<Item = &ReconciliationStatus> {
        self.statuses.iter()
    }

    /// Статус измерения на дату.
    ///
    /// Берётся **худший** статус среди интервалов, накрывающих дату: два
    /// утверждения об одном дне, одно из которых не сошлось, дают
    /// расхождение. Взять лучший значило бы позволить лишнему документу
    /// закрыть собой проблему.
    #[must_use]
    pub fn status_for(
        &self,
        account: AccountId,
        date: Date,
        dimension: Dimension,
    ) -> DimensionStatus {
        let mut result: Option<DimensionStatus> = None;
        for status in &self.statuses {
            if status.account != account || !status.period.contains(date) {
                continue;
            }
            let candidate = status.dimension(dimension);
            result = Some(match result {
                None => candidate,
                Some(current) => {
                    if candidate.rank() < current.rank() {
                        candidate
                    } else {
                        current
                    }
                }
            });
        }
        result.unwrap_or(DimensionStatus::Provisional)
    }
}

fn collect_groups(events: &[Event]) -> Vec<StatementGroup> {
    let mut groups: Vec<StatementGroup> = Vec::new();
    for event in events {
        let EventKind::ControlAssertion { period, claim } = event.kind else {
            continue;
        };
        let channel = SourceChannel {
            source: event.provenance.source(),
            parser_version: event.provenance.parser_version().clone(),
            document: Some(event.provenance.raw_hash().clone()),
        };
        if let Some(group) = groups.iter_mut().find(|group| {
            group.account == event.account && group.period == period && group.channel == channel
        }) {
            group.claims.push(claim);
        } else {
            groups.push(StatementGroup {
                account: event.account,
                period,
                channel,
                claims: vec![claim],
            });
        }
    }
    groups
}

/// Основание 5: раздельные контрольные секции одного документа сошлись
/// одновременно.
///
/// Требуется и остаток, и оборотная величина: они считаются по-разному,
/// и совпадение обеих является независимым уравнением. Один сошедшийся
/// остаток подтверждает сам себя и основанием не является.
fn ground_five(group: &StatementGroup, outcomes: &[ClaimCheck]) -> Option<Evidence> {
    if outcomes.is_empty() || !outcomes.iter().all(|check| check.outcome.confirms()) {
        return None;
    }
    let has_balance = group.claims.iter().any(|claim| {
        matches!(
            claim,
            ControlClaim::CashBalance { .. } | ControlClaim::PositionQuantity { .. }
        )
    });
    let has_flow = group.claims.iter().any(|claim| {
        matches!(
            claim,
            ControlClaim::CashTurnover { .. }
                | ControlClaim::FeesTotal { .. }
                | ControlClaim::IncomeTotal { .. }
        )
    });
    if !has_balance || !has_flow {
        return None;
    }
    let dimensions: BTreeSet<Dimension> =
        group.claims.iter().map(ControlClaim::dimension).collect();
    Evidence::from_match(
        Ground::SeparateSectionsAgree,
        group.channel.clone(),
        group.channel.clone(),
        dimensions,
    )
}

/// Основание 1: начальный остаток следующего отчёта совпал с
/// вычисленным остатком предыдущего периода.
///
/// Повышается **предыдущий** период: подтверждается именно он. Повысить
/// текущий значило бы засчитать подтверждение данных, которых в нём
/// ещё нет.
fn ground_one(
    group: &StatementGroup,
    outcomes: &[ClaimCheck],
    groups: &[StatementGroup],
) -> Option<(AssertionPeriod, Evidence)> {
    let opening_matched: BTreeSet<Dimension> = outcomes
        .iter()
        .filter(|check| {
            check.outcome.confirms()
                && matches!(
                    check.claim,
                    ControlClaim::CashBalance {
                        at: BalancePoint::Opening,
                        ..
                    } | ControlClaim::PositionQuantity {
                        at: BalancePoint::Opening,
                        ..
                    }
                )
        })
        .map(|check| check.claim.dimension())
        .collect();
    if opening_matched.is_empty() {
        return None;
    }
    let prior = groups
        .iter()
        .filter(|other| other.account == group.account && other.period.to < group.period.from)
        .max_by_key(|other| other.period.to)?;
    let evidence = Evidence::from_match(
        Ground::OpeningMatchesPriorClosing,
        group.channel.clone(),
        prior.channel.clone(),
        opening_matched,
    )?;
    Some((prior.period, evidence))
}

/// Основание 2: конечный остаток одного отчёта совпал с начальным
/// следующего.
///
/// Сравниваются два **утверждения источника**, а не утверждение с
/// проекцией: это проверка непрерывности документов между собой.
fn ground_two(groups: &[StatementGroup]) -> Vec<(AccountId, AssertionPeriod, Evidence)> {
    let mut found = Vec::new();
    for earlier in groups {
        for later in groups {
            if earlier.account != later.account || later.period.from <= earlier.period.to {
                continue;
            }
            let mut dimensions = BTreeSet::new();
            for closing in &earlier.claims {
                for opening in &later.claims {
                    if continuous(*closing, *opening) {
                        dimensions.insert(closing.dimension());
                    }
                }
            }
            if let Some(evidence) = Evidence::from_match(
                Ground::ContinuityBetweenStatements,
                later.channel.clone(),
                earlier.channel.clone(),
                dimensions,
            ) {
                found.push((earlier.account, earlier.period, evidence));
            }
        }
    }
    found
}

/// Совпадают ли конечное утверждение одного отчёта и начальное другого.
fn continuous(closing: ControlClaim, opening: ControlClaim) -> bool {
    match (closing, opening) {
        (
            ControlClaim::CashBalance {
                currency: left_currency,
                amount: left,
                at: BalancePoint::Closing,
            },
            ControlClaim::CashBalance {
                currency: right_currency,
                amount: right,
                at: BalancePoint::Opening,
            },
        ) => left_currency == right_currency && left == right,
        (
            ControlClaim::PositionQuantity {
                instrument: left_instrument,
                custody: left_custody,
                quantity: left,
                at: BalancePoint::Closing,
            },
            ControlClaim::PositionQuantity {
                instrument: right_instrument,
                custody: right_custody,
                quantity: right,
                at: BalancePoint::Opening,
            },
        ) => left_instrument == right_instrument && left_custody == right_custody && left == right,
        _ => false,
    }
}

/// Основание 3: два независимых канала за один интервал.
///
/// Пара берётся один раз (`i < j`): отношение независимости симметрично,
/// и вторая копия того же основания удвоила бы список доказательств,
/// ничего не добавив.
fn ground_three(
    groups: &[StatementGroup],
    checked: &[Vec<ClaimCheck>],
) -> Vec<(AccountId, AssertionPeriod, Evidence)> {
    let mut found = Vec::new();
    for (left_index, left_outcomes) in checked.iter().enumerate() {
        for (offset, right_outcomes) in checked.iter().skip(left_index + 1).enumerate() {
            let right_index = left_index + 1 + offset;
            let left = &groups[left_index];
            let right = &groups[right_index];
            if left.account != right.account
                || left.period != right.period
                || !left.channel.is_independent_of(&right.channel)
            {
                continue;
            }
            let confirmed: BTreeSet<Dimension> = confirmed_dimensions(left_outcomes)
                .intersection(&confirmed_dimensions(right_outcomes))
                .copied()
                .collect();
            if let Some(evidence) = Evidence::from_match(
                Ground::BrokerApiAgreesWithStatement,
                right.channel.clone(),
                left.channel.clone(),
                confirmed,
            ) {
                found.push((left.account, left.period, evidence));
            }
        }
    }
    found
}

/// Измерения, по которым в группе сошлось хоть что-то и не разошлось
/// ничего.
fn confirmed_dimensions(outcomes: &[ClaimCheck]) -> BTreeSet<Dimension> {
    let mut confirmed = BTreeSet::new();
    let mut broken = BTreeSet::new();
    for check in outcomes {
        let dimension = check.claim.dimension();
        match check.outcome {
            ClaimOutcome::Matched => {
                confirmed.insert(dimension);
            }
            ClaimOutcome::Discrepant(_) => {
                broken.insert(dimension);
            }
            // Несравнимое и исключённое периметром не подтверждают
            // и не ломают: они молчат.
            ClaimOutcome::NotComparable { .. } | ClaimOutcome::Excepted { .. } => {}
        }
    }
    confirmed.retain(|dimension| !broken.contains(dimension));
    confirmed
}

fn build_status(
    group: &StatementGroup,
    outcomes: Vec<ClaimCheck>,
    evidence: &[(AccountId, AssertionPeriod, Evidence)],
) -> ReconciliationStatus {
    let mut dimensions: BTreeMap<Dimension, DimensionStatus> = BTreeMap::new();
    let mut own_evidence = Vec::new();
    for (account, period, item) in evidence {
        if *account == group.account && *period == group.period {
            raise(
                &mut dimensions,
                &item.dimensions(),
                DimensionStatus::from_level(item.level()),
            );
            own_evidence.push(item.clone());
        }
    }
    // Расхождение поглощает: ставится после повышений и не снимается.
    for check in &outcomes {
        if matches!(check.outcome, ClaimOutcome::Discrepant(_)) {
            dimensions.insert(check.claim.dimension(), DimensionStatus::Discrepant);
        }
    }
    ReconciliationStatus {
        account: group.account,
        period: group.period,
        dimensions,
        evidence: own_evidence,
        outcomes,
    }
}

/// Повышение статуса измерений до уровня основания. Понижения нет:
/// основание слабее уже достигнутого ничего не меняет.
fn raise(
    dimensions: &mut BTreeMap<Dimension, DimensionStatus>,
    of: &BTreeSet<Dimension>,
    level: DimensionStatus,
) {
    for dimension in of {
        let slot = dimensions
            .entry(*dimension)
            .or_insert(DimensionStatus::Provisional);
        if level.rank() > slot.rank() {
            *slot = level;
        }
    }
}

/// Слияние статусов одного счёта и интервала, пришедших из разных
/// документов: берётся лучшее подтверждение и все расхождения.
fn merge_status(into: &mut Vec<ReconciliationStatus>, status: ReconciliationStatus) {
    let Some(existing) = into
        .iter_mut()
        .find(|item| item.account == status.account && item.period == status.period)
    else {
        into.push(status);
        return;
    };
    for (dimension, value) in &status.dimensions {
        let slot = existing
            .dimensions
            .entry(*dimension)
            .or_insert(DimensionStatus::Provisional);
        *slot = if *value == DimensionStatus::Discrepant || *slot == DimensionStatus::Discrepant {
            DimensionStatus::Discrepant
        } else if value.rank() > slot.rank() {
            *value
        } else {
            *slot
        };
    }
    existing.evidence.extend(status.evidence);
    existing.outcomes.extend(status.outcomes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dimension_has_a_distinct_machine_readable_code() {
        let codes: Vec<&str> = Dimension::all().iter().map(|d| d.code()).collect();
        assert_eq!(codes, vec!["cash", "positions", "tax_basis", "income"]);
    }

    #[test]
    fn the_list_of_dimensions_covers_every_variant_once() {
        // Список задан руками, поэтому он обязан быть проверен: забытое
        // измерение не получает статуса и выглядит как «подтверждать
        // нечего», а продублированное считается дважды.
        for dimension in Dimension::all() {
            let found = Dimension::all().iter().filter(|d| **d == dimension).count();
            assert_eq!(found, 1, "измерение {dimension:?} встречается не один раз");
        }
        assert_eq!(Dimension::all().len(), 4);
    }
}
