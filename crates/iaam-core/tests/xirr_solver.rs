//! Решатель ставки: отказы, единственность корня, масштабная инвариантность.
//!
//! Тесты вынесены из `src/numeric/xirr.rs` в отдельный файл намеренно:
//! заслон архитектуры ограничивает размер файлов приближённого режима,
//! чтобы в них не завёлся теневой расчётный слой, — а тестовый код,
//! разрастаясь, съедал бы этот предел и вынуждал его поднимать.

use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::numeric::xirr::{DayCount, SolverFlow, SolverRefusal, solve};
use rust_decimal::Decimal;

fn flow(day_offset: i64, amount: i64) -> SolverFlow {
    SolverFlow {
        day_offset,
        amount: Dec::new(Decimal::from(amount)),
    }
}

#[test]
fn a_single_year_of_ten_percent_is_ten_percent() {
    // Вложено 1000, через 365 дней получено 1100. Ставка известна
    // из условия задачи, а не из вывода программы (§15.5).
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!((outcome.rate().value() - 0.1).abs() < 1e-9);
    assert_eq!(outcome.day_count(), DayCount::Act365);
    // Погрешность — половина ширины локализующего интервала,
    // то есть доказанная граница, а не разность приближений.
    assert!(outcome.rate().error_bound() <= SolverPolicy::returns_default().rate_tolerance);
}

#[test]
fn the_rate_does_not_depend_on_the_scale_of_the_flows() {
    // Масштабная инвариантность (§15.3): абсолютный допуск по невязке
    // нарушал бы её, потому что зависел бы от размера сумм.
    let small = [flow(0, -1_000), flow(365, 1_100)];
    let large = [flow(0, -1_000_000_000), flow(365, 1_100_000_000)];
    let policy = SolverPolicy::returns_default();
    let left = solve(&small, policy, DayCount::Act365).unwrap();
    let right = solve(&large, policy, DayCount::Act365).unwrap();
    assert!((left.rate().value() - right.rate().value()).abs() < 1e-9);
}

#[test]
fn flows_of_one_sign_have_no_rate() {
    let flows = [flow(0, -1_000), flow(365, -1_100)];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::NoSignChange)
    );
}

#[test]
fn fewer_than_two_flows_have_no_rate() {
    let flows = [flow(0, -1_000)];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::TooFewFlows)
    );
}

#[test]
fn all_zero_flows_have_no_rate() {
    let flows = [flow(0, 0), flow(365, 0)];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::AllZero)
    );
}

#[test]
fn two_sign_changes_are_refused_even_when_the_grid_finds_one_bracket() {
    // Классический знакопеременный ряд. Сетка может найти один
    // интервал со сменой знака и «доказать» единственность — но она
    // пропускает корни чётной кратности и пары корней внутри шага.
    // Отказ обязателен, даже когда число выглядит правдоподобным.
    let flows = [flow(0, -1_000), flow(365, 2_500), flow(730, -1_540)];
    let refusal = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap_err();
    assert!(
        matches!(
            refusal,
            SolverRefusal::MultipleRoots { .. } | SolverRefusal::UniquenessNotProven { .. }
        ),
        "получено {refusal:?}"
    );
}

#[test]
fn a_coupon_series_with_one_sign_change_is_solved() {
    // Купоны между вложением и погашением знак не меняют: перемена
    // одна, корень один, отказа быть не должно.
    let flows = [
        flow(0, -98_000),
        flow(182, 4_500),
        flow(365, 4_500),
        flow(547, 4_500),
        flow(731, 104_500),
    ];
    let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!(outcome.rate().value() > 0.0);
}

#[test]
fn an_inverted_bracket_is_refused() {
    let policy = SolverPolicy {
        bracket_low: 1.0,
        bracket_high: -1.0,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::BadBracket)
    );
}

#[test]
fn a_bracket_reaching_minus_one_hundred_percent_is_refused() {
    // При ставке −100 % основание степени равно нулю: NPV не определён.
    let policy = SolverPolicy {
        bracket_low: -1.0,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::BadBracket)
    );
}

#[test]
fn a_root_outside_the_bracket_is_refused() {
    let policy = SolverPolicy {
        bracket_low: 0.0,
        bracket_high: 0.01,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::RootNotBracketed)
    );
}

#[test]
fn every_refusal_has_a_machine_readable_code() {
    assert_eq!(SolverRefusal::TooFewFlows.code(), "too_few_flows");
    assert_eq!(SolverRefusal::NoSignChange.code(), "no_sign_change");
    assert_eq!(SolverRefusal::RootNotBracketed.code(), "root_not_bracketed");
    assert_eq!(
        SolverRefusal::MultipleRoots { count: 2 }.code(),
        "multiple_roots"
    );
    assert_eq!(
        SolverRefusal::UniquenessNotProven { sign_changes: 3 }.code(),
        "uniqueness_not_proven"
    );
    assert_eq!(
        SolverRefusal::NotConverged { iterations: 1 }.code(),
        "not_converged"
    );
    assert_eq!(SolverRefusal::NotRepresentable.code(), "not_representable");
    assert_eq!(SolverRefusal::BadBracket.code(), "bad_bracket");
    assert_eq!(SolverRefusal::AllZero.code(), "all_zero");
}

#[test]
fn the_day_count_has_a_stable_code() {
    // Код уходит в отчёт и в снапшот: без него ставка невоспроизводима.
    assert_eq!(DayCount::Act365.code(), "act/365");
}

#[test]
fn the_solver_converges_superlinearly_not_by_halving() {
    // Метод Илинойса обязан сходиться заметно быстрее бисекции: на
    // интервале шириной около 0,1 до допуска 1e-10 чистой бисекции
    // нужно порядка тридцати шагов. Проверка числа итераций —
    // единственный способ заметить, что приём Илинойса сломан: ответ
    // при этом остаётся верным, просто добывается вдвое дольше.
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!(
        outcome.rate().iterations() <= 20,
        "итераций {}: метод вырождается в бисекцию",
        outcome.rate().iterations()
    );
}

#[test]
fn a_degenerate_bracket_is_refused() {
    let policy = SolverPolicy {
        bracket_low: 0.5,
        bracket_high: 0.5,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::BadBracket)
    );
}

#[test]
fn a_non_numeric_bracket_is_refused() {
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    for policy in [
        SolverPolicy {
            bracket_low: f64::NAN,
            ..SolverPolicy::returns_default()
        },
        SolverPolicy {
            bracket_high: f64::INFINITY,
            ..SolverPolicy::returns_default()
        },
    ] {
        assert_eq!(
            solve(&flows, policy, DayCount::Act365),
            Err(SolverRefusal::BadBracket)
        );
    }
}

#[test]
fn any_bracket_reaching_minus_one_hundred_percent_is_refused() {
    // Ставка −100 % обращает основание степени в ноль, ниже — делает его
    // отрицательным. Отказ обязан приходить от нижней границы независимо
    // от верхней: и когда диапазон пересекает −1, и когда он весь ниже.
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    for (low, high) in [(-1.0, 100.0), (-2.0, -1.0), (-3.0, -2.0)] {
        let policy = SolverPolicy {
            bracket_low: low,
            bracket_high: high,
            ..SolverPolicy::returns_default()
        };
        assert_eq!(
            solve(&flows, policy, DayCount::Act365),
            Err(SolverRefusal::BadBracket),
            "диапазон [{low}, {high}]"
        );
    }
}

#[test]
fn the_scan_step_covers_exactly_the_requested_range() {
    // Шаг — это (высокая − низкая) / точки. Симметричный диапазон ловит
    // подмену вычитания сложением: сумма границ там равна нулю, шаг
    // обращается в ноль, и сканирование перестаёт двигаться.
    let policy = SolverPolicy {
        bracket_low: -0.5,
        bracket_high: 0.5,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, policy, DayCount::Act365).unwrap();
    assert!((outcome.rate().value() - 0.1).abs() < 1e-9);
}

#[test]
fn a_bracket_already_within_tolerance_needs_no_iterations() {
    // Если найденный интервал уже уложился в допуск, уточнять нечего,
    // и объявленная погрешность равна ровно половине его ширины.
    //
    // Ширина здесь известна точно: это шаг сканирования, то есть
    // (100 − (−0,9999)) / 1000 ≈ 0,10100. Половина — около 0,05050.
    // Проверка привязана к числу точек сканирования намеренно: без
    // точного ожидания «половина ширины» неотличима от «ширина»
    // и от «две ширины».
    let policy = SolverPolicy {
        rate_tolerance: 1.0,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, policy, DayCount::Act365).unwrap();
    assert_eq!(outcome.rate().iterations(), 0);
    let bound = outcome.rate().error_bound();
    assert!(
        (0.0504..0.0506).contains(&bound),
        "граница погрешности {bound}: это не половина ширины интервала"
    );
}

#[test]
fn a_series_with_three_sign_changes_is_refused_by_the_sign_rule() {
    // Сетка находит здесь ровно один интервал со сменой знака — то есть
    // «доказала» бы единственность. Единственность отвергает правило
    // знаков, и только оно: без него система вернула бы одно из
    // возможных значений как ответ.
    let flows = [
        flow(0, -1_000),
        flow(365, 2_000),
        flow(730, -1_000),
        flow(1_095, 400),
    ];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::UniquenessNotProven { sign_changes: 3 })
    );
}

#[test]
fn the_error_bound_shrinks_with_the_requested_tolerance() {
    // Погрешность — половина ширины локализующего интервала, а не
    // произвольное число: ужесточение допуска обязано её уменьшать.
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let loose = solve(
        &flows,
        SolverPolicy {
            rate_tolerance: 1e-4,
            ..SolverPolicy::returns_default()
        },
        DayCount::Act365,
    )
    .unwrap();
    let tight = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!(loose.rate().error_bound() > tight.rate().error_bound());
    // Граница — ПОЛОВИНА ширины интервала, а не ширина: допуск задаёт
    // ширину, значит объявленная погрешность вдвое меньше него.
    assert!(loose.rate().error_bound() <= 1e-4 / 2.0 + f64::EPSILON);
}

#[test]
fn the_stopping_test_is_the_width_of_the_bracket_not_the_sum_of_its_ends() {
    // Граничный случай уточнения: интервал, найденный сканированием, уже
    // уже допуска — уточнять нечего, и результат обязан прийти за ноль
    // итераций. Условие остановки — ШИРИНА интервала, то есть разность
    // концов; сумма концов не является ни шириной, ни чем-либо ещё.
    // Корень около +500 % годовых выбран специально: там разность концов
    // мала, а сумма велика, и подмена одного другим видна.
    let flows = [flow(0, -1_000), flow(365, 6_000)];
    let policy = SolverPolicy {
        rate_tolerance: 0.5,
        max_iterations: 200,
        bracket_low: 4.0,
        bracket_high: 6.0,
    };
    let outcome = solve(&flows, policy, DayCount::Act365).unwrap();
    assert_eq!(
        outcome.rate().iterations(),
        0,
        "интервал уже допуска — уточнять нечего"
    );
    assert!((outcome.rate().value() - 5.0).abs() < 0.01);
    // Погрешность — половина ширины ячейки сканирования: 2.0 / 1000 / 2.
    assert!(outcome.rate().error_bound() <= 0.001);
}
