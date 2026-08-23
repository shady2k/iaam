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
///
/// **Порядок вариантов задаёт силу статуса** и используется через
/// `Ord`: `max` повышает, `min` берёт худший. Сравнение вынесено
/// в производный `Ord` намеренно — написанное руками `>` даёт ветвь,
/// в которой замена на `>=` ничего не меняет (равные статусы
/// тождественны), и такой мутант невозможно убить тестом.
///
/// Расхождение стоит **ниже** отсутствия подтверждения: «не сошлось» —
/// найденная проблема, «пока не проверяли» — нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DimensionStatus {
    Discrepant,
    Provisional,
    AcceptedInternal,
    AcceptedIndependent,
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
    /// Сборка реестра из журнала без исключений периметра.
    ///
    /// Логика вынесена из конструктора с именем `new` намеренно (§15.7).
    pub fn build(events: &[Event]) -> Result<Self, ObserveError> {
        Self::build_with(events, &crate::perimeter::PerimeterExceptions::default())
    }

    /// Сборка реестра с исключениями периметра (§11).
    ///
    /// Расхождение, накрытое исключением, становится `Excepted`:
    /// система знает, почему цифры не сходятся, и не отправляет
    /// владельца чинить то, что не поддерживает. Подтверждением такой
    /// исход не является — «знаем причину» не равно «сошлось».
    pub fn build_with(
        events: &[Event],
        exceptions: &crate::perimeter::PerimeterExceptions,
    ) -> Result<Self, ObserveError> {
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
                        outcome: apply_exceptions(
                            check_claim(claim, &observed),
                            group.account,
                            claim.dimension(),
                            exceptions,
                        ),
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
            // Худший из накрывающих дату: два утверждения об одном дне,
            // одно из которых не сошлось, дают расхождение.
            result = Some(result.map_or(candidate, |current| current.min(candidate)));
        }
        result.unwrap_or(DimensionStatus::Provisional)
    }
}

/// Замена расхождения исключением периметра (§11).
///
/// Заменяется **только** расхождение: несравнимость исключением не
/// объясняется, а совпадение объяснять незачем.
fn apply_exceptions(
    outcome: ClaimOutcome,
    account: AccountId,
    dimension: Dimension,
    exceptions: &crate::perimeter::PerimeterExceptions,
) -> ClaimOutcome {
    match (outcome, exceptions.covers(account, dimension)) {
        (ClaimOutcome::Discrepant(_), Some(exception)) => ClaimOutcome::Excepted { exception },
        (outcome, _) => outcome,
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
        *slot = (*slot).max(level);
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
        // Расхождение поглощает при слиянии с любой стороны: иначе
        // подтверждение из второго документа отменяло бы уже найденную
        // проблему. Во всех остальных случаях берётся сильнейшее.
        *slot = if *value == DimensionStatus::Discrepant || *slot == DimensionStatus::Discrepant {
            DimensionStatus::Discrepant
        } else {
            (*slot).max(*value)
        };
    }
    existing.evidence.extend(status.evidence);
    existing.outcomes.extend(status.outcomes);
}

/// Тесты внутренних функций реестра.
///
/// Живут здесь, а не в интеграционных тестах, потому что проверяют
/// решения, которые снаружи видны только косвенно: слияние статусов
/// одного интервала из разных документов, непрерывность утверждений
/// и правило «повышение не понижает». Мутационный заслон показал, что
/// через публичный вход эти ветви не достаются (§15.7).
#[cfg(test)]
mod internals {
    use super::*;
    use crate::event::provenance::{ParserVersion, RawHash};
    use crate::ids::{CustodyId, InstrumentId, SourceId};
    use crate::money::{CurrencyCode, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use time::macros::date;

    /// Канал с документом, выведенным из имени парсера.
    ///
    /// Документ обязан отличаться вместе с парсером: одинаковый хеш
    /// у разных каналов означал бы один и тот же файл, и независимости
    /// по правилу §10.3 не было бы — что и есть верное поведение,
    /// но не то, которое проверяет тест.
    fn channel(parser: &str) -> SourceChannel {
        let mut hex: String = parser.bytes().map(|byte| format!("{byte:02x}")).collect();
        hex.truncate(64);
        while hex.len() < 64 {
            hex.push('0');
        }
        SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion(parser.to_owned()),
            document: Some(RawHash::parse(&hex).unwrap()),
        }
    }

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }

    fn april() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30)).unwrap()
    }

    fn cash(amount: i64, at: BalancePoint) -> ControlClaim {
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(amount),
            at,
        }
    }

    fn group(period: AssertionPeriod, parser: &str, claims: Vec<ControlClaim>) -> StatementGroup {
        StatementGroup {
            account: AccountId::new_random(),
            period,
            channel: channel(parser),
            claims,
        }
    }

    #[test]
    fn continuity_requires_the_same_currency_and_the_same_amount() {
        // Непрерывность — это совпадение конечного остатка одного
        // отчёта с начальным следующего. Ослабление любого условия
        // объявило бы непрерывными документы, между которыми разрыв.
        let closing = cash(100_000, BalancePoint::Closing);
        assert!(continuous(closing, cash(100_000, BalancePoint::Opening)));
        assert!(
            !continuous(closing, cash(99_999, BalancePoint::Opening)),
            "разные суммы непрерывности не дают"
        );
        assert!(
            !continuous(
                closing,
                ControlClaim::CashBalance {
                    currency: CurrencyCode::Usd,
                    amount: PostedMinor::new(100_000),
                    at: BalancePoint::Opening,
                }
            ),
            "разные валюты непрерывности не дают"
        );
        assert!(
            !continuous(closing, cash(100_000, BalancePoint::Closing)),
            "два конечных остатка — это не непрерывность"
        );
        assert!(
            !continuous(
                cash(100_000, BalancePoint::Opening),
                cash(100_000, BalancePoint::Opening)
            ),
            "непрерывность идёт от конца к началу, а не наоборот"
        );
    }

    #[test]
    fn position_continuity_requires_the_same_instrument_and_custody() {
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let quantity = Quantity(Dec::one());
        let closing = ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at: BalancePoint::Closing,
        };
        let opening = ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at: BalancePoint::Opening,
        };
        assert!(continuous(closing, opening));

        let elsewhere = ControlClaim::PositionQuantity {
            instrument,
            custody: CustodyId::new_random(),
            quantity,
            at: BalancePoint::Opening,
        };
        assert!(
            !continuous(closing, elsewhere),
            "то же количество в другом депозитарии — другая позиция"
        );

        let other_paper = ControlClaim::PositionQuantity {
            instrument: InstrumentId::new_random(),
            custody,
            quantity,
            at: BalancePoint::Opening,
        };
        assert!(!continuous(closing, other_paper));
    }

    #[test]
    fn a_claim_of_one_kind_is_never_continuous_with_another() {
        // Оборот и остаток не сравниваются между собой: у них разный
        // смысл, и объявить их непрерывными значило бы выдать
        // совпадение случайных чисел за подтверждение.
        let turnover = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(100_000),
            credit: PostedMinor::new(0),
        };
        assert!(!continuous(turnover, cash(100_000, BalancePoint::Opening)));
        assert!(!continuous(cash(100_000, BalancePoint::Closing), turnover));
    }

    #[test]
    fn continuity_holds_only_between_documents_that_do_not_overlap() {
        // Отчёты за пересекающиеся периоды непрерывными не являются:
        // непрерывность — это стык, а не наложение.
        let account = AccountId::new_random();
        let mut earlier = group(
            march(),
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        let mut later = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        earlier.account = account;
        later.account = account;

        let found = ground_two(&[earlier.clone(), later.clone()]);
        assert_eq!(found.len(), 1, "стык марта и апреля даёт основание");
        assert_eq!(found[0].1, march(), "подтверждается более ранний период");

        // Тот же документ, наложенный сам на себя, основания не даёт.
        assert!(ground_two(&[earlier.clone(), earlier]).is_empty());
    }

    #[test]
    fn continuity_is_not_claimed_across_accounts() {
        let earlier = group(
            march(),
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        let later = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        assert!(
            ground_two(&[earlier, later]).is_empty(),
            "у разных счетов непрерывности нет"
        );
    }

    #[test]
    fn raising_never_lowers_an_already_reached_level() {
        // Повышение статуса — это максимум, а не последнее записанное
        // значение. Иначе слабое основание, пришедшее позже, отменяло бы
        // сильное.
        let mut dimensions = BTreeMap::new();
        let only_cash: BTreeSet<Dimension> = [Dimension::Cash].into_iter().collect();

        raise(
            &mut dimensions,
            &only_cash,
            DimensionStatus::AcceptedIndependent,
        );
        raise(
            &mut dimensions,
            &only_cash,
            DimensionStatus::AcceptedInternal,
        );
        assert_eq!(
            dimensions.get(&Dimension::Cash),
            Some(&DimensionStatus::AcceptedIndependent),
            "слабое основание не понижает достигнутый уровень"
        );

        raise(
            &mut dimensions,
            &only_cash,
            DimensionStatus::AcceptedIndependent,
        );
        assert_eq!(
            dimensions.get(&Dimension::Cash),
            Some(&DimensionStatus::AcceptedIndependent),
            "повтор того же уровня ничего не меняет"
        );
    }

    fn status_with(
        account: AccountId,
        period: AssertionPeriod,
        dimension: Dimension,
        value: DimensionStatus,
    ) -> ReconciliationStatus {
        let mut dimensions = BTreeMap::new();
        dimensions.insert(dimension, value);
        ReconciliationStatus {
            account,
            period,
            dimensions,
            evidence: Vec::new(),
            outcomes: Vec::new(),
        }
    }

    #[test]
    fn merging_takes_the_best_confirmation_of_the_same_period() {
        // Два документа об одном периоде: подтверждение сильнейшего
        // остаётся. Иначе порядок чтения документов решал бы уровень.
        let account = AccountId::new_random();
        let mut statuses = vec![status_with(
            account,
            march(),
            Dimension::Cash,
            DimensionStatus::AcceptedInternal,
        )];
        merge_status(
            &mut statuses,
            status_with(
                account,
                march(),
                Dimension::Cash,
                DimensionStatus::AcceptedIndependent,
            ),
        );
        assert_eq!(statuses.len(), 1, "статусы одного периода слились");
        assert_eq!(
            statuses[0].dimension(Dimension::Cash),
            DimensionStatus::AcceptedIndependent
        );
    }

    #[test]
    fn merging_keeps_a_discrepancy_whichever_side_it_came_from() {
        // Расхождение поглощает при слиянии в обе стороны: и когда оно
        // пришло вторым, и когда первым. Односторонняя проверка
        // пропустила бы половину случаев.
        let account = AccountId::new_random();

        let mut first = vec![status_with(
            account,
            march(),
            Dimension::Cash,
            DimensionStatus::AcceptedIndependent,
        )];
        merge_status(
            &mut first,
            status_with(
                account,
                march(),
                Dimension::Cash,
                DimensionStatus::Discrepant,
            ),
        );
        assert_eq!(
            first[0].dimension(Dimension::Cash),
            DimensionStatus::Discrepant
        );

        let mut second = vec![status_with(
            account,
            march(),
            Dimension::Cash,
            DimensionStatus::Discrepant,
        )];
        merge_status(
            &mut second,
            status_with(
                account,
                march(),
                Dimension::Cash,
                DimensionStatus::AcceptedIndependent,
            ),
        );
        assert_eq!(
            second[0].dimension(Dimension::Cash),
            DimensionStatus::Discrepant,
            "подтверждение не отменяет уже найденное расхождение"
        );
    }

    #[test]
    fn statuses_of_different_accounts_or_periods_do_not_merge() {
        let account = AccountId::new_random();
        let mut statuses = vec![status_with(
            account,
            march(),
            Dimension::Cash,
            DimensionStatus::AcceptedInternal,
        )];
        merge_status(
            &mut statuses,
            status_with(
                account,
                april(),
                Dimension::Cash,
                DimensionStatus::Discrepant,
            ),
        );
        assert_eq!(statuses.len(), 2, "разные периоды не сливаются");

        merge_status(
            &mut statuses,
            status_with(
                AccountId::new_random(),
                march(),
                Dimension::Cash,
                DimensionStatus::Discrepant,
            ),
        );
        assert_eq!(statuses.len(), 3, "разные счета не сливаются");
    }

    #[test]
    fn the_worst_status_wins_across_overlapping_periods() {
        // Два утверждения накрывают один день, и одно не сошлось.
        // Взять лучшее значило бы позволить лишнему документу закрыть
        // собой проблему.
        let account = AccountId::new_random();
        let year = AssertionPeriod::between(date!(2026 - 01 - 01), date!(2026 - 12 - 31)).unwrap();
        let ledger = ReconciliationLedger {
            statuses: vec![
                status_with(
                    account,
                    year,
                    Dimension::Cash,
                    DimensionStatus::AcceptedIndependent,
                ),
                status_with(
                    account,
                    march(),
                    Dimension::Cash,
                    DimensionStatus::Discrepant,
                ),
            ],
        };
        assert_eq!(
            ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
            DimensionStatus::Discrepant
        );
        assert_eq!(
            ledger.status_for(account, date!(2026 - 07 - 15), Dimension::Cash),
            DimensionStatus::AcceptedIndependent,
            "за пределами мартовского интервала расхождение не действует"
        );
        assert_eq!(
            ledger.status_for(
                AccountId::new_random(),
                date!(2026 - 03 - 15),
                Dimension::Cash
            ),
            DimensionStatus::Provisional,
            "о чужом счёте реестр не утверждает ничего"
        );
    }

    #[test]
    fn external_evidence_lands_on_the_matching_period_and_creates_one_otherwise() {
        // Основания 4, 6, 7 и 8 приходят извне. Они обязаны попасть
        // в существующий статус, а если такого нет — завести его:
        // иначе подтверждение депозитария просто исчезло бы.
        let account = AccountId::new_random();
        let evidence = Evidence::from_match(
            Ground::DepositaryReportConfirms,
            channel("depositary/1"),
            channel("report/1"),
            [Dimension::Positions].into_iter().collect(),
        )
        .expect("основание");

        let existing = ReconciliationLedger {
            statuses: vec![status_with(
                account,
                march(),
                Dimension::Cash,
                DimensionStatus::AcceptedInternal,
            )],
        }
        .with_external_evidence(vec![(account, march(), evidence.clone())]);
        assert_eq!(existing.statuses().count(), 1, "статус не задвоился");
        assert_eq!(
            existing.status_for(account, date!(2026 - 03 - 15), Dimension::Positions),
            DimensionStatus::AcceptedIndependent
        );
        assert_eq!(
            existing.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
            DimensionStatus::AcceptedInternal,
            "чужое измерение не тронуто"
        );

        let fresh = ReconciliationLedger::default().with_external_evidence(vec![(
            account,
            march(),
            evidence,
        )]);
        assert_eq!(fresh.statuses().count(), 1, "статус заведён");
        assert_eq!(
            fresh.status_for(account, date!(2026 - 03 - 15), Dimension::Positions),
            DimensionStatus::AcceptedIndependent
        );
    }

    #[test]
    fn external_evidence_does_not_leak_between_periods_of_one_account() {
        // Основание, присланное для марта, не имеет права повысить
        // апрель. Ослабление ключа поиска до «счёт ИЛИ период» отдало бы
        // подтверждение депозитария первому попавшемуся статусу.
        let account = AccountId::new_random();
        let evidence = Evidence::from_match(
            Ground::DepositaryReportConfirms,
            channel("depositary/1"),
            channel("report/1"),
            [Dimension::Positions].into_iter().collect(),
        )
        .expect("основание");

        let ledger = ReconciliationLedger {
            statuses: vec![
                status_with(
                    account,
                    april(),
                    Dimension::Positions,
                    DimensionStatus::Provisional,
                ),
                status_with(
                    account,
                    march(),
                    Dimension::Positions,
                    DimensionStatus::Provisional,
                ),
            ],
        }
        .with_external_evidence(vec![(account, march(), evidence)]);

        assert_eq!(
            ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Positions),
            DimensionStatus::AcceptedIndependent,
            "март подтверждён"
        );
        assert_eq!(
            ledger.status_for(account, date!(2026 - 04 - 15), Dimension::Positions),
            DimensionStatus::Provisional,
            "апрель мартовским основанием не подтверждается"
        );
        assert_eq!(ledger.statuses().count(), 2, "статусы не слились");
    }

    #[test]
    fn ground_one_ignores_an_opening_claim_that_did_not_match() {
        // Основание 1 требует, чтобы сошёлся именно НАЧАЛЬНЫЙ остаток.
        // Ни несошедшийся начальный, ни сошедшийся конечный его не дают:
        // первый ничего не подтверждает, второй говорит о своём периоде.
        let account = AccountId::new_random();
        let mut current = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        current.account = account;
        let mut prior = group(
            march(),
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        prior.account = account;
        let groups = [prior, current.clone()];

        let unmatched_opening = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Opening),
            outcome: ClaimOutcome::Discrepant(check::Discrepancy {
                field: "amount",
                claimed: check::ClaimValue::Money {
                    amount: PostedMinor::new(100_000),
                    currency: CurrencyCode::Rub,
                },
                observed: check::ClaimValue::Money {
                    amount: PostedMinor::new(1),
                    currency: CurrencyCode::Rub,
                },
                delta: check::ClaimValue::Money {
                    amount: PostedMinor::new(99_999),
                    currency: CurrencyCode::Rub,
                },
            }),
        }];
        assert!(
            ground_one(&current, &unmatched_opening, &groups).is_none(),
            "несошедшийся начальный остаток ничего не подтверждает"
        );

        let matched_closing = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Closing),
            outcome: ClaimOutcome::Matched,
        }];
        assert!(
            ground_one(&current, &matched_closing, &groups).is_none(),
            "конечный остаток говорит о своём периоде, а не о предыдущем"
        );
    }

    #[test]
    fn a_prior_statement_must_end_before_the_current_one_starts() {
        // Отчёт, заканчивающийся в день начала текущего, предыдущим
        // не является: их периоды соприкасаются, и общий день попал бы
        // в оба. Подтверждать период его же собственным днём нельзя.
        let account = AccountId::new_random();
        let mut current = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        current.account = account;
        let touching = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 04 - 01))
            .expect("интервал");
        let mut overlapping = group(
            touching,
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        overlapping.account = account;

        let outcomes = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Opening),
            outcome: ClaimOutcome::Matched,
        }];
        assert!(
            ground_one(&current, &outcomes, &[overlapping, current.clone()]).is_none(),
            "соприкасающийся отчёт предыдущим не считается"
        );
    }

    #[test]
    fn ground_three_needs_all_three_conditions_at_once() {
        // Основание 3 требует одновременно: тот же счёт, тот же период
        // и независимые каналы. Ослабление любого условия объявило бы
        // независимым подтверждением совпадение чужих цифр.
        let account = AccountId::new_random();
        let matched = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Closing),
            outcome: ClaimOutcome::Matched,
        }];

        let mut left = group(
            march(),
            "report/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        left.account = account;

        // Тот же счёт и независимый канал, но ДРУГОЙ период.
        let mut other_period = group(april(), "api/1", vec![cash(100_000, BalancePoint::Closing)]);
        other_period.account = account;
        assert!(
            ground_three(
                &[left.clone(), other_period],
                &[matched.clone(), matched.clone()]
            )
            .is_empty(),
            "подтверждение за другой период основанием не является"
        );

        // Тот же период и независимый канал, но ДРУГОЙ счёт.
        let other_account = group(march(), "api/1", vec![cash(100_000, BalancePoint::Closing)]);
        assert!(
            ground_three(
                &[left.clone(), other_account],
                &[matched.clone(), matched.clone()]
            )
            .is_empty(),
            "подтверждение по чужому счёту основанием не является"
        );

        // Тот же счёт и период, но канал НЕ независим.
        let mut same_channel = StatementGroup {
            account,
            period: march(),
            channel: left.channel.clone(),
            claims: vec![cash(100_000, BalancePoint::Closing)],
        };
        same_channel.account = account;
        assert!(
            ground_three(
                &[left.clone(), same_channel],
                &[matched.clone(), matched.clone()]
            )
            .is_empty(),
            "тот же канал независимости не даёт"
        );

        // Все три условия выполнены — основание есть.
        let mut independent = group(march(), "api/1", vec![cash(100_000, BalancePoint::Closing)]);
        independent.account = account;
        let found = ground_three(&[left, independent], &[matched.clone(), matched]);
        assert_eq!(found.len(), 1, "независимый канал за тот же период");
        assert_eq!(found[0].1, march());
    }

    #[test]
    fn ground_one_needs_a_strictly_earlier_statement() {
        // Подтверждается предыдущий период. Отчёт, пересекающийся
        // с текущим, предыдущим не является, и брать его значило бы
        // подтверждать период его же собственными данными.
        let account = AccountId::new_random();
        let mut current = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        current.account = account;
        let outcomes = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Opening),
            outcome: ClaimOutcome::Matched,
        }];

        assert!(
            ground_one(&current, &outcomes, std::slice::from_ref(&current)).is_none(),
            "сам себе предыдущим отчётом счёт быть не может"
        );

        let mut prior = group(
            march(),
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        prior.account = account;
        let found = ground_one(&current, &outcomes, &[prior, current.clone()])
            .expect("предыдущий отчёт найден");
        assert_eq!(found.0, march(), "подтверждается предыдущий период");

        // Несошедшийся начальный остаток основания не даёт.
        let broken = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Opening),
            outcome: ClaimOutcome::NotComparable {
                reason: check::NotComparable::NoJournalCoverage,
            },
        }];
        assert!(ground_one(&current, &broken, &[current.clone()]).is_none());
    }
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
    fn every_confidence_level_has_a_distinct_machine_readable_code() {
        // Уровень уходит наружу кодом: внешний агент решает по нему,
        // показывать ли предупреждение. Пустая строка неотличима от
        // «уровня нет», а один код на три — от «данные какие-то».
        let all = [
            ConfidenceLevel::Provisional,
            ConfidenceLevel::AcceptedInternal,
            ConfidenceLevel::AcceptedIndependent,
        ];
        let codes: Vec<&str> = all.iter().map(|level| level.code()).collect();
        assert_eq!(
            codes,
            vec!["provisional", "accepted_internal", "accepted_independent"]
        );
    }

    #[test]
    fn confidence_levels_are_ordered_from_weakest_to_strongest() {
        // Порядок используется для повышения статуса. Перепутанный
        // порядок молча превратил бы повышение в понижение.
        assert!(ConfidenceLevel::Provisional < ConfidenceLevel::AcceptedInternal);
        assert!(ConfidenceLevel::AcceptedInternal < ConfidenceLevel::AcceptedIndependent);
    }

    #[test]
    fn every_dimension_status_has_a_distinct_machine_readable_code() {
        let all = [
            DimensionStatus::Provisional,
            DimensionStatus::AcceptedInternal,
            DimensionStatus::AcceptedIndependent,
            DimensionStatus::Discrepant,
        ];
        let codes: Vec<&str> = all.iter().map(|status| status.code()).collect();
        assert_eq!(
            codes,
            vec![
                "provisional",
                "accepted_internal",
                "accepted_independent",
                "discrepant"
            ]
        );
    }

    #[test]
    fn a_discrepancy_ranks_below_an_unconfirmed_state() {
        // «Не сошлось» — найденная проблема, «пока не проверяли» — нет.
        // Если бы расхождение стояло выше, худший статус среди периодов
        // выбирал бы не расхождение, и проблема пряталась бы.
        assert!(DimensionStatus::Discrepant < DimensionStatus::Provisional);
        assert!(DimensionStatus::Provisional < DimensionStatus::AcceptedInternal);
        assert!(DimensionStatus::AcceptedInternal < DimensionStatus::AcceptedIndependent);
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
