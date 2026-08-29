//! Правило сопоставления запланированной выплаты с фактом (§7.2).

use serde::{Deserialize, Serialize};
use time::{Date, Duration};

use crate::projection::income::ReceivedPosting;
use crate::projection::ownership::Ownership;
use crate::returns::UnverifiableReason;
use crate::rules::cashflow::ScheduledPosting;

/// Версия правила сопоставления. Хранение датированных фактов
/// версионируется `PROJECTION_VERSION`; здесь версионируется само
/// **сопоставление**: ширина окна, его односторонность и жадность.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PostingMatchVersion(pub u16);

/// Итог проверки одной запланированной выплаты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Выплата причиталась, но факта нет.
    NotReceived,
    /// Нельзя сделать вывод, потому что не хватает доказательства.
    Unverifiable(UnverifiableReason),
    /// Выплата не причиталась либо подтверждена фактом.
    Silent,
}

/// Вторая версия правила сопоставления: право определяется на дату
/// фиксации, а не на дату платежа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostingMatchV2 {
    window_days: u16,
}

impl Default for PostingMatchV2 {
    fn default() -> Self {
        Self::new()
    }
}

impl PostingMatchV2 {
    #[must_use]
    pub const fn new() -> Self {
        Self { window_days: 21 }
    }

    /// Версия, под которой правило войдёт в расчётный контур.
    #[must_use]
    pub const fn version() -> PostingMatchVersion {
        PostingMatchVersion(2)
    }

    /// Судить все выплаты, распределив каждый факт не более одного раза.
    #[must_use]
    pub fn judge_all(
        &self,
        postings: &[(ScheduledPosting, Ownership)],
        facts: &[ReceivedPosting],
    ) -> Vec<Verdict> {
        let mut ordered: Vec<(usize, &(ScheduledPosting, Ownership))> =
            postings.iter().enumerate().collect();
        ordered.sort_by_key(|(_, (posting, _))| *posting);

        let mut available: Vec<&ReceivedPosting> = facts.iter().collect();
        available.sort_by_key(|fact| (fact.date, fact.event));
        let mut used = vec![false; available.len()];
        let mut fact_found = vec![false; postings.len()];
        let window = Duration::days(i64::from(self.window_days));

        // Факты распределяются по всем выплатам до классификации вердиктов,
        // включая Unknown и EntitlementDateUnknown: иначе удалённая заранее
        // недоказуемая выплата отдаст свой факт соседней и скроет пропуск.
        for (index, (posting, _)) in ordered {
            let deadline = posting.date.saturating_add(window);
            let matched = (0..available.len()).find(|&fact_index| {
                let fact = available[fact_index];
                !used[fact_index] && fact_matches(posting, fact, deadline)
            });
            if let Some(fact_index) = matched {
                used[fact_index] = true;
                fact_found[index] = true;
            }
        }

        postings
            .iter()
            .enumerate()
            .map(|(index, (posting, ownership))| self.judge(posting, *ownership, fact_found[index]))
            .collect()
    }

    /// Классифицировать одну выплату по уже распределённому факту.
    fn judge(&self, posting: &ScheduledPosting, ownership: Ownership, fact_found: bool) -> Verdict {
        if posting.entitlement.is_none() {
            return Verdict::Unverifiable(UnverifiableReason::EntitlementDateUnknown);
        }
        if ownership == Ownership::Unknown {
            return Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown);
        }
        if ownership == Ownership::NotOwned || fact_found {
            return Verdict::Silent;
        }
        // Молчание допустимо только при доказанном отсутствии права
        // или найденном факте: неопределённость должна быть дефектом,
        // а не оправданием отсутствия проблемы.
        Verdict::NotReceived
    }
}

fn fact_matches(expected: &ScheduledPosting, fact: &ReceivedPosting, deadline: Date) -> bool {
    fact.kind == expected.kind && fact.date >= expected.date && fact.date <= deadline
}

/// Первая версия правила.
///
/// Окно — 21 календарный день. Депозитарная цепочка занимает около
/// десяти рабочих дней: эмитент перечисляет в НРД до двух рабочих дней,
/// НРД депозитарию брокера — на следующий рабочий день, а депозитарий
/// конечному владельцу — не позднее семи рабочих дней после дня
/// получения (ст. 8.7 ФЗ 39-ФЗ, «иные депоненты»: номинальным держателям
/// и управляющим тот же пункт даёт срок короче, но конечный владелец в
/// эту категорию не попадает). Десять рабочих дней — это минимум
/// четырнадцать календарных, а через новогодние или майские растягивается
/// до двадцати одного. Отсюда 21.
///
/// Окно задано в календарных днях, а не в рабочих, потому что
/// производственного календаря в ядре нет вовсе, а заводить его — вносить
/// внешний ежегодно публикуемый источник. Правило версионировано именно
/// затем, чтобы это решение можно было пересмотреть.
///
/// Граница применимости: самый плотный реальный график — ежемесячный
/// купон, около тридцати дней. Двадцать один меньше тридцати, поэтому
/// окно до соседней выплаты не дотягивается, но запас всего девять дней.
/// Бумага с более частой выплатой потребует новой версии правила.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostingMatchV1 {
    window_days: u16,
}

impl Default for PostingMatchV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl PostingMatchV1 {
    #[must_use]
    pub const fn new() -> Self {
        Self { window_days: 21 }
    }

    #[must_use]
    pub const fn window_days(self) -> u16 {
        self.window_days
    }

    /// Истёк ли срок ожидания выплаты к дате отчёта.
    ///
    /// Деньги по депозитарной цепочке идут те же самые 21 день, что
    /// задают окно сопоставления, поэтому отсрочка перед тревогой равна
    /// окну и живёт здесь, а не в сверке: сузить окно, не сузив
    /// отсрочку, значило бы обвинять бумагу за дни, которые правило
    /// само признаёт нормальным сроком доставки. Одно число — одна
    /// версия правила.
    ///
    /// Выплата, срок которой ещё идёт, не проверяется совсем: она
    /// не «не получена» — про неё пока нечего сказать.
    #[must_use]
    pub fn is_due(&self, scheduled: &ScheduledPosting, as_of: Date) -> bool {
        // `saturating_add`: сложение дат паникует при выходе за границу
        // календаря, а вердикт обязан быть у любого входа.
        scheduled
            .date
            .saturating_add(Duration::days(i64::from(self.window_days)))
            <= as_of
    }

    /// Запланированные выплаты, под которые факта не нашлось.
    ///
    /// Факт закрывает выплату, если совпал вид и он пришёл не раньше
    /// плановой даты и не позже неё плюс окно. Окно одностороннее:
    /// деньги приходят позже плана, а не раньше, поэтому факт до
    /// плановой даты — это другая выплата, а не ранний приход этой.
    ///
    /// Сопоставление жадное по возрастанию даты и **one-to-one**: факт
    /// расходуется и второй раз не используется. Иначе пропуск в плотном
    /// графике исчез бы — один пришедший купон закрыл бы и себя, и
    /// пропущенного соседа.
    ///
    /// Оба среза сортируются внутри, причём факты при равной дате — по
    /// `EventId`, чтобы порядок был полным. Поэтому результат не зависит
    /// от порядка событий в журнале (§15.3).
    ///
    /// Выплата, у которой срок ожидания к `as_of` ещё не истёк,
    /// не проверяется вовсе: см. [`Self::is_due`].
    #[must_use]
    pub fn unreceived(
        &self,
        scheduled: &[ScheduledPosting],
        facts: &[ReceivedPosting],
        as_of: Date,
    ) -> Vec<ScheduledPosting> {
        let mut plan = scheduled.to_vec();
        plan.sort();

        let mut available: Vec<&ReceivedPosting> = facts.iter().collect();
        available.sort_by_key(|fact| (fact.date, fact.event));

        let mut used = vec![false; available.len()];
        let mut missing = Vec::new();
        let window = Duration::days(i64::from(self.window_days));

        for expected in plan {
            // Срок ещё идёт — про эту выплату сказать нечего, и факт
            // она не расходует: иначе исключённая выплата съела бы
            // подтверждение соседней и создала бы пропуск на пустом
            // месте.
            if !self.is_due(&expected, as_of) {
                continue;
            }
            // `saturating_add` вместо `+`: сложение дат паникует при
            // выходе за границу календаря, а правило обязано вернуть
            // вердикт по любому входу, а не уронить ядро.
            let deadline = expected.date.saturating_add(window);
            let matched = (0..available.len()).find(|&index| {
                let fact = available[index];
                !used[index] && fact_matches(&expected, fact, deadline)
            });
            match matched {
                Some(index) => used[index] = true,
                None => missing.push(expected),
            }
        }
        missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::EventId;
    use crate::money::{CurrencyCode, Money, PostedMinor};
    use crate::projection::income::ReceivedPosting;
    use crate::rules::cashflow::{PostingKind, ScheduledPosting};
    use time::Date;
    use time::macros::date;
    use uuid::Uuid;

    fn march(day: u8) -> Date {
        Date::from_calendar_date(2026, time::Month::March, day).expect("день марта существует")
    }

    fn scheduled(day: u8, kind: PostingKind) -> ScheduledPosting {
        ScheduledPosting {
            date: march(day),
            kind,
            entitlement: None,
        }
    }

    fn coupon(day: u8) -> ScheduledPosting {
        scheduled(day, PostingKind::Coupon)
    }

    fn scheduled_with_entitlement(day: u8, entitlement: u8) -> ScheduledPosting {
        ScheduledPosting {
            date: march(day),
            kind: PostingKind::Coupon,
            entitlement: Some(march(entitlement)),
        }
    }

    /// Идентификатор факта выводится из его номера, а не из `new_random`:
    /// ядро детерминировано, и порядок фактов одной даты должен быть
    /// воспроизводим от прогона к прогону.
    fn received(day: u8, kind: PostingKind, event: u128) -> ReceivedPosting {
        ReceivedPosting {
            event: EventId(Uuid::from_u128(event)),
            date: march(day),
            amount: Money::new(PostedMinor::new(1_000), CurrencyCode::Rub),
            kind,
        }
    }

    fn fact(day: u8) -> ReceivedPosting {
        received(day, PostingKind::Coupon, u128::from(day))
    }

    /// Дата отчёта, к которой сроки ожидания всех выплат этих тестов
    /// давно истекли. Нужна, чтобы проверка сопоставления не зависела
    /// от отсрочки: её граница проверяется отдельными тестами ниже.
    fn late_enough() -> Date {
        date!(2026 - 05 - 01)
    }

    fn judge_single(
        posting: ScheduledPosting,
        ownership: Ownership,
        facts: &[ReceivedPosting],
    ) -> Verdict {
        PostingMatchV2::new()
            .judge_all(&[(posting, ownership)], facts)
            .into_iter()
            .next()
            .expect("одна выплата должна дать один вердикт")
    }

    #[test]
    fn a_payment_whose_waiting_window_has_not_expired_is_not_checked_at_all() {
        // День в день после плановой даты деньги ещё идут по
        // депозитарной цепочке. Требовать под них факт — обвинять
        // здоровую бумагу в дефекте.
        let rule = PostingMatchV1::new();
        assert!(rule.unreceived(&[coupon(1)], &[], march(1)).is_empty());
    }

    #[test]
    fn the_waiting_window_is_exactly_the_matching_window() {
        // Граница: +20 дней срок ещё идёт, +21 истёк, +22 тем более.
        // Отсрочка равна окну ровно затем, чтобы сузить окно значило
        // сузить и отсрочку.
        let rule = PostingMatchV1::new();
        assert!(rule.unreceived(&[coupon(1)], &[], march(21)).is_empty());
        assert_eq!(
            rule.unreceived(&[coupon(1)], &[], march(22)),
            vec![coupon(1)]
        );
        assert_eq!(
            rule.unreceived(&[coupon(1)], &[], march(23)),
            vec![coupon(1)]
        );
    }

    #[test]
    fn a_payment_whose_waiting_window_has_not_expired_never_consumes_a_fact() {
        // Выплата исключается целиком, а не откладывается «до лучших
        // фактов»: иначе она съела бы факт соседней выплаты и создала
        // пропуск там, где его нет.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(&[coupon(1), coupon(10)], &[fact(2)], march(22))
                .is_empty()
        );
    }

    #[test]
    fn the_window_is_twenty_one_days() {
        assert_eq!(PostingMatchV1::new().window_days(), 21);
    }

    #[test]
    fn a_payment_inside_the_window_is_received() {
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(&[coupon(15)], &[fact(18)], late_enough())
                .is_empty()
        );
    }

    #[test]
    fn a_fact_on_the_scheduled_day_closes_it() {
        // Нижняя граница окна включающая: деньги, пришедшие день в день,
        // — это исполнение плана, а не другая выплата.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(&[coupon(15)], &[fact(15)], late_enough())
                .is_empty()
        );
    }

    #[test]
    fn the_window_edge_is_inclusive_and_the_day_after_is_not() {
        // 21 календарный день — это 10 рабочих дней депозитарной цепочки
        // (ст. 8.7 ФЗ 39-ФЗ), растянутые через праздничный период.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(&[coupon(1)], &[fact(22)], late_enough())
                .is_empty()
        );
        assert_eq!(
            rule.unreceived(&[coupon(1)], &[fact(23)], late_enough()),
            vec![coupon(1)]
        );
    }

    #[test]
    fn money_never_arrives_before_the_schedule_says_it_should() {
        // Окно одностороннее. Факт раньше плановой даты — это другая
        // выплата, а не ранний приход этой.
        let rule = PostingMatchV1::new();
        assert_eq!(
            rule.unreceived(&[coupon(15)], &[fact(14)], late_enough()),
            vec![coupon(15)]
        );
    }

    #[test]
    fn a_coupon_fact_does_not_confirm_a_principal_return() {
        let rule = PostingMatchV1::new();
        let principal = scheduled(15, PostingKind::PrincipalReturn);
        assert_eq!(
            rule.unreceived(&[principal], &[fact(18)], late_enough()),
            vec![principal]
        );
    }

    #[test]
    fn a_principal_return_fact_does_not_confirm_a_coupon() {
        let rule = PostingMatchV1::new();
        let principal_fact = received(18, PostingKind::PrincipalReturn, 18);
        assert_eq!(
            rule.unreceived(&[coupon(15)], &[principal_fact], late_enough()),
            vec![coupon(15)]
        );
    }

    #[test]
    fn an_offer_settlement_is_confirmed_only_by_its_own_kind() {
        let rule = PostingMatchV1::new();
        let offer = scheduled(15, PostingKind::OfferSettlement);
        assert_eq!(
            rule.unreceived(&[offer], &[fact(18)], late_enough()),
            vec![offer]
        );
        assert!(
            rule.unreceived(
                &[offer],
                &[received(18, PostingKind::OfferSettlement, 18)],
                late_enough()
            )
            .is_empty()
        );
    }

    #[test]
    fn one_fact_cannot_close_two_scheduled_payments() {
        // Иначе пропуск в плотном графике исчез бы: один пришедший купон
        // закрыл бы и себя, и соседа.
        let rule = PostingMatchV1::new();
        assert_eq!(
            rule.unreceived(&[coupon(1), coupon(10)], &[fact(11)], late_enough()),
            vec![coupon(10)]
        );
    }

    #[test]
    fn two_facts_close_two_scheduled_payments() {
        // Расходуется ровно один факт на выплату: второй факт обязан
        // достаться второй выплате, а не пропасть вместе с первым.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(
                &[coupon(1), coupon(10)],
                &[fact(11), fact(12)],
                late_enough()
            )
            .is_empty()
        );
    }

    #[test]
    fn the_earliest_scheduled_payment_takes_the_earliest_fact() {
        // Жадность по возрастанию: ранний факт уходит ранней выплате,
        // поэтому поздняя выплата остаётся с поздним фактом, а не пустой.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(
                &[coupon(1), coupon(20)],
                &[fact(2), fact(21)],
                late_enough()
            )
            .is_empty()
        );
    }

    #[test]
    fn the_verdict_does_not_depend_on_the_order_of_the_inputs() {
        let rule = PostingMatchV1::new();
        let forward = rule.unreceived(
            &[coupon(1), coupon(10)],
            &[fact(2), fact(11)],
            late_enough(),
        );
        let reversed = rule.unreceived(
            &[coupon(10), coupon(1)],
            &[fact(11), fact(2)],
            late_enough(),
        );
        assert_eq!(forward, reversed);
        assert!(forward.is_empty());
    }

    #[test]
    fn facts_of_the_same_day_are_ordered_by_event_id() {
        // Два факта одного дня и вида различимы только идентификатором.
        // Порядок между ними доопределён им, поэтому вердикт не зависит
        // от порядка событий в журнале (§15.3).
        let rule = PostingMatchV1::new();
        let first = received(11, PostingKind::Coupon, 1);
        let second = received(11, PostingKind::Coupon, 2);
        let forward = rule.unreceived(&[coupon(1), coupon(10)], &[first, second], late_enough());
        let reversed = rule.unreceived(&[coupon(10), coupon(1)], &[second, first], late_enough());
        assert_eq!(forward, reversed);
        assert!(forward.is_empty());
    }

    #[test]
    fn without_facts_every_scheduled_payment_is_unreceived() {
        let rule = PostingMatchV1::new();
        assert_eq!(
            rule.unreceived(&[coupon(10), coupon(1)], &[], late_enough()),
            vec![coupon(1), coupon(10)]
        );
    }

    #[test]
    fn without_a_schedule_there_is_nothing_to_confirm() {
        let rule = PostingMatchV1::new();
        assert!(rule.unreceived(&[], &[fact(11)], late_enough()).is_empty());
    }

    #[test]
    fn a_repeated_scheduled_payment_needs_a_second_fact() {
        // Один и тот же день может нести две выплаты одного вида
        // (купон по двум периодам, сдвинутым на выходные): один факт
        // закрывает ровно одну из них.
        let rule = PostingMatchV1::new();
        let twice = [coupon(10), coupon(10)];
        assert_eq!(
            rule.unreceived(&twice, &[fact(11)], late_enough()),
            vec![coupon(10)]
        );
        assert!(
            rule.unreceived(&twice, &[fact(11), fact(12)], late_enough())
                .is_empty()
        );
    }

    #[test]
    fn the_window_is_measured_across_a_month_boundary() {
        // Окно календарное, поэтому конец месяца ему не граница.
        let rule = PostingMatchV1::new();
        let scheduled_in_march = ScheduledPosting {
            date: date!(2026 - 03 - 25),
            kind: PostingKind::Coupon,
            entitlement: None,
        };
        let fact_in_april = ReceivedPosting {
            event: EventId(Uuid::from_u128(100)),
            date: date!(2026 - 04 - 15),
            amount: Money::new(PostedMinor::new(1_000), CurrencyCode::Rub),
            kind: PostingKind::Coupon,
        };
        assert!(
            rule.unreceived(&[scheduled_in_march], &[fact_in_april], late_enough())
                .is_empty()
        );
    }
    #[test]
    fn posting_match_v2_has_version_two() {
        // Версия фиксирует новое правило отдельно: подключение к обходу
        // лотов позже не должно незаметно изменить уже выпущенный V1.
        assert_eq!(PostingMatchV2::version(), PostingMatchVersion(2));
    }

    #[test]
    fn known_entitlement_owned_with_fact_is_silent() {
        // Известная дата фиксации и владение на неё, подтверждённые фактом,
        // означают, что выплата доказанно не является проблемой.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::Owned, &[fact(18)]),
            Verdict::Silent
        );
    }

    #[test]
    fn known_entitlement_owned_without_fact_is_not_received() {
        // При доказанном праве отсутствие подходящего факта — это
        // доказанный пропуск выплаты, а не недоказуемость владения.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::Owned, &[]),
            Verdict::NotReceived
        );
    }

    #[test]
    fn known_entitlement_not_owned_is_silent() {
        // Доказанное отсутствие бумаги на дату фиксации означает, что
        // выплата не причиталась и отсутствие факта не является дефектом.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::NotOwned, &[]),
            Verdict::Silent
        );
    }

    #[test]
    fn known_entitlement_unknown_ownership_is_unverifiable() {
        // Неизвестное владение не позволяет решить, было ли право на
        // выплату, поэтому молчание стало бы оправданием незнания.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::Unknown, &[]),
            Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown)
        );
    }

    #[test]
    fn unknown_entitlement_date_is_unverifiable_for_any_ownership() {
        // Без даты фиксации нельзя выбрать день для проверки владения,
        // поэтому дата права важнее любого другого входного факта.
        let posting = coupon(15);
        assert_eq!(
            judge_single(posting, Ownership::NotOwned, &[fact(18)]),
            Verdict::Unverifiable(UnverifiableReason::EntitlementDateUnknown)
        );
    }

    #[test]
    fn silence_is_only_for_proven_absence_of_entitlement() {
        // Молчание означает «выплата не причиталась». Любая неопределённость
        // обязана выходить дефектной недоказуемостью, иначе незнание
        // становится оправданием.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::NotOwned, &[]),
            Verdict::Silent
        );
        assert_eq!(
            judge_single(posting, Ownership::Unknown, &[]),
            Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown)
        );
    }
    #[test]
    fn one_fact_closes_only_the_first_of_overlapping_postings() {
        // При перекрывающихся окнах один факт обязан закрыть только первую
        // выплату: иначе настоящий пропуск в плотном графике исчезает.
        let postings = [
            (scheduled_with_entitlement(1, 1), Ownership::Owned),
            (scheduled_with_entitlement(10, 10), Ownership::Owned),
        ];
        assert_eq!(
            PostingMatchV2::new().judge_all(&postings, &[fact(11)]),
            vec![Verdict::Silent, Verdict::NotReceived]
        );
    }

    #[test]
    fn an_unverifiable_posting_still_consumes_its_matching_fact() {
        // Недоказуемую выплату нельзя убрать до распределения: её факт не
        // должен достаться соседней выплате и скрыть её настоящий пропуск.
        let postings = [
            (scheduled_with_entitlement(1, 1), Ownership::Unknown),
            (scheduled_with_entitlement(10, 10), Ownership::Owned),
        ];
        assert_eq!(
            PostingMatchV2::new().judge_all(&postings, &[fact(11)]),
            vec![
                Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown),
                Verdict::NotReceived,
            ]
        );
    }
}
