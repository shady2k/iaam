//! Решатель ставки внутренней доходности (§6.1, §6.6).
//!
//! Второй и последний файл ядра, где разрешена двоичная плавающая точка:
//! ставка требует возведения в дробную степень, которого `rust_decimal`
//! не умеет. Результат решателя **никогда** не входит в денежное
//! тождество — он производная от сумм, а не их компонент (§6.6).
//!
//! **Уникальность корня доказывается правилом знаков, а не сканированием.**
//! Замена `x = 1/(1 + r)` превращает `NPV` в обобщённый многочлен
//! `Σ aᵢ·x^tᵢ` с положительными показателями, для которого число
//! положительных корней не превосходит числа перемен знака в
//! упорядоченной по времени последовательности сумм. Одна перемена
//! знака — корень не более одного; вместе с интервалом, на границах
//! которого знаки различны, это ровно один корень. Сканирование сетки
//! служит только поиску такого интервала: считать по нему корни нельзя —
//! оно пропускает корни чётной кратности и пары корней внутри шага.

use thiserror::Error;

use super::approx::{ApproxValue, SolverPolicy, dec_to_f64};
use super::decimal::Dec;

/// База начисления дней. Зафиксирована в результате: без неё ставка
/// не воспроизводима.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayCount {
    /// Фактические дни, год 365. Конвенция XIRR.
    Act365,
}

impl DayCount {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Act365 => "act/365",
        }
    }

    const fn year_length(self) -> f64 {
        match self {
            Self::Act365 => 365.0,
        }
    }
}

/// Поток для решателя: смещение в днях от первого потока и сумма
/// в валюте отчёта.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolverFlow {
    pub day_offset: i64,
    pub amount: Dec,
}

/// Отказ решателя. Отказ — результат, а не исключение: уравнение NPV
/// при чередующихся знаках потоков может не иметь корней, иметь
/// несколько или не позволять доказать единственность, и произвольно
/// выбранное число хуже честного отказа (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SolverRefusal {
    #[error("потоков меньше двух: ставка не определена")]
    TooFewFlows,
    #[error("все потоки одного знака: уравнение NPV корня не имеет")]
    NoSignChange,
    #[error("корень не локализован в заданном диапазоне ставок")]
    RootNotBracketed,
    #[error("в диапазоне ставок найдено интервалов со сменой знака: {count}; корень не один")]
    MultipleRoots { count: u32 },
    #[error(
        "знак потоков меняется {sign_changes} раз: единственность корня не доказуема, \
         и выбирать один из возможных нельзя"
    )]
    UniquenessNotProven { sign_changes: u32 },
    #[error("метод не сошёлся за {iterations} итераций")]
    NotConverged { iterations: u32 },
    #[error("сумма потока не переводится в приближённый режим или не является числом")]
    NotRepresentable,
    #[error("диапазон локализации задан неверно: нижняя граница не меньше верхней")]
    BadBracket,
    #[error("все потоки нулевые: ставка не определена")]
    AllZero,
}

impl SolverRefusal {
    /// Машиночитаемый код отказа. Нужен API: текст предназначен человеку,
    /// а внешний агент разбирает код (§13).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooFewFlows => "too_few_flows",
            Self::NoSignChange => "no_sign_change",
            Self::RootNotBracketed => "root_not_bracketed",
            Self::MultipleRoots { .. } => "multiple_roots",
            Self::UniquenessNotProven { .. } => "uniqueness_not_proven",
            Self::NotConverged { .. } => "not_converged",
            Self::NotRepresentable => "not_representable",
            Self::BadBracket => "bad_bracket",
            Self::AllZero => "all_zero",
        }
    }
}

/// Найденная ставка вместе с политикой, по которой она найдена.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateOutcome {
    rate: ApproxValue,
    policy: SolverPolicy,
    day_count: DayCount,
}

impl RateOutcome {
    #[must_use]
    pub const fn rate(&self) -> ApproxValue {
        self.rate
    }

    #[must_use]
    pub const fn policy(&self) -> SolverPolicy {
        self.policy
    }

    #[must_use]
    pub const fn day_count(&self) -> DayCount {
        self.day_count
    }
}

/// Число точек сканирования диапазона ставок.
///
/// При диапазоне по умолчанию (−99,99 %…+10 000 %) шаг составляет около
/// 0,1 — то есть примерно десять процентных пунктов ставки. Этого
/// достаточно, чтобы найти интервал со сменой знака у денежной серии
/// с одной переменой знака, и **недостаточно**, чтобы делать выводы
/// о числе корней: выводы делает правило знаков.
const SCAN_POINTS: u32 = 1_000;

/// Внутренняя денежная серия в приближённом режиме.
struct Series {
    /// Пары «доля года от первого потока, сумма», в порядке времени.
    terms: Vec<(f64, f64)>,
}

impl Series {
    fn build(flows: &[SolverFlow], day_count: DayCount) -> Result<Self, SolverRefusal> {
        if flows.len() < 2 {
            return Err(SolverRefusal::TooFewFlows);
        }
        let mut terms = Vec::with_capacity(flows.len());
        // Сумма модулей: нужна ровно для одного вывода — серия
        // из одних нулей ставки не имеет. В структуре не хранится,
        // потому что больше нигде не используется.
        let mut magnitude = 0.0_f64;
        for flow in flows {
            let amount = dec_to_f64(&flow.amount).ok_or(SolverRefusal::NotRepresentable)?;
            if !amount.is_finite() {
                return Err(SolverRefusal::NotRepresentable);
            }
            let years = flow.day_offset as f64 / day_count.year_length();
            if !years.is_finite() {
                return Err(SolverRefusal::NotRepresentable);
            }
            magnitude += amount.abs();
            terms.push((years, amount));
        }
        // Сумма модулей строго положительна для непустой ненулевой серии.
        // Проверяется именно так, а не «равна нулю»: отрицательное
        // значение означало бы ошибку в самом накоплении, и молчаливо
        // пропускать её нельзя.
        if !magnitude.is_finite() {
            return Err(SolverRefusal::NotRepresentable);
        }
        if magnitude <= 0.0 {
            return Err(SolverRefusal::AllZero);
        }
        Ok(Self { terms })
    }

    /// Число перемен знака в упорядоченной по времени последовательности
    /// сумм. Нулевые потоки пропускаются: ноль знака не имеет.
    fn sign_changes(&self) -> u32 {
        let mut changes = 0;
        let mut previous = 0.0_f64;
        for (_, amount) in &self.terms {
            if *amount == 0.0 {
                continue;
            }
            if previous != 0.0 && previous.signum() != amount.signum() {
                changes += 1;
            }
            previous = *amount;
        }
        changes
    }

    fn npv(&self, rate: f64) -> f64 {
        self.terms
            .iter()
            .map(|(years, amount)| amount / (1.0 + rate).powf(*years))
            .sum()
    }
}

/// Интервалы сканирования, на границах которых NPV меняет знак.
///
/// Нечисловые значения NPV прекращают поиск отказом: `NaN` не сравнивается
/// сам с собой, и наивная проверка знака превратила бы его в мнимый корень.
fn brackets(series: &Series, policy: SolverPolicy) -> Result<Vec<(f64, f64)>, SolverRefusal> {
    // Границы обязаны быть сравнимыми и упорядоченными: NaN в политике
    // означает, что диапазон задан неверно, а не «любой диапазон».
    if !policy.bracket_low.is_finite() || !policy.bracket_high.is_finite() {
        return Err(SolverRefusal::BadBracket);
    }
    if policy.bracket_low >= policy.bracket_high {
        return Err(SolverRefusal::BadBracket);
    }
    // Ставка −100 % обращает основание степени в ноль, ниже — делает его
    // отрицательным, а дробная степень отрицательного числа не определена.
    // Диапазон обязан начинаться строго выше: условие на верхнюю границу
    // тут не нужно и только создавало бы вторую, непроверяемую ветку.
    if policy.bracket_low <= -1.0 {
        return Err(SolverRefusal::BadBracket);
    }
    let step = (policy.bracket_high - policy.bracket_low) / f64::from(SCAN_POINTS);
    let mut found = Vec::new();
    let mut previous_rate = policy.bracket_low;
    let mut previous_value = series.npv(previous_rate);
    if !previous_value.is_finite() {
        return Err(SolverRefusal::NotRepresentable);
    }
    for i in 1..=SCAN_POINTS {
        let rate = policy.bracket_low + step * f64::from(i);
        let value = series.npv(rate);
        if !value.is_finite() {
            return Err(SolverRefusal::NotRepresentable);
        }
        if value == 0.0 {
            found.push((rate, rate));
        } else if previous_value != 0.0 && previous_value.signum() != value.signum() {
            found.push((previous_rate, rate));
        }
        previous_rate = rate;
        previous_value = value;
    }
    Ok(found)
}

/// Уточнение корня методом Илинойса (§6.1).
///
/// Это модифицированный метод ложного положения: он **никогда** не теряет
/// локализующий интервал — оба конца всегда дают значения разных знаков, —
/// и при этом сходится сверхлинейно, потому что при застревании одного
/// конца его значение вдвое уменьшается и следующая секущая перескакивает
/// на другую сторону.
///
/// Почему не Ньютон с откатом на бисекцию, как было в первой редакции:
/// шаг Ньютона, попадая близко к корню, почти не двигает дальний конец
/// интервала, а объявленная погрешность считается именно по интервалу.
/// Защита «не сократился вдвое — бисекция» срабатывала почти всегда,
/// и метод вырождался в чистую бисекцию: тридцать семь итераций там,
/// где достаточно единиц. Проверено исполнением.
///
/// Остановка — по **ширине интервала**, а не по величине невязки:
/// невязка возле пологого корня мала при большой ошибке ставки.
/// Отдельная проверка невязки не нужна: корень заключён в интервале
/// по построению, поэтому половина ширины — доказанная граница,
/// а не оценка.
fn refine(
    series: &Series,
    bracket: (f64, f64),
    policy: SolverPolicy,
) -> Result<ApproxValue, SolverRefusal> {
    // Концы интервала — точки сканирования, и их значения уже проверены
    // на численность в `brackets`: интервал возвращается только тогда,
    // когда оба значения конечны и разных знаков. Повторная проверка
    // здесь была бы мёртвой веткой, а мёртвая проверка создаёт ложное
    // впечатление, что случай обрабатывается.
    let (mut low, mut high) = bracket;
    let mut low_value = series.npv(low);
    let mut high_value = series.npv(high);
    if high - low <= policy.rate_tolerance {
        return Ok(finish(low, high, 0));
    }

    for iteration in 1..=policy.max_iterations {
        let denominator = high_value - low_value;
        let secant = high - high_value * (high - low) / denominator;
        let guess = if secant_is_inside(secant, low, high) {
            secant
        } else {
            (low + high) / 2.0
        };

        let value = series.npv(guess);
        if !value.is_finite() {
            return Err(SolverRefusal::NotRepresentable);
        }
        if value == 0.0 {
            return Ok(finish(guess, guess, iteration));
        }

        if value.signum() == high_value.signum() {
            high = guess;
            high_value = value;
            // Приём Илинойса: застоявшийся конец «слабеет», и следующая
            // секущая перепрыгивает на другую сторону корня.
            low_value /= 2.0;
        } else {
            low = high;
            low_value = high_value;
            high = guess;
            high_value = value;
        }

        let (left, right) = if low <= high {
            (low, high)
        } else {
            (high, low)
        };
        if right - left <= policy.rate_tolerance {
            return Ok(finish(left, right, iteration));
        }
    }
    Err(SolverRefusal::NotConverged {
        iterations: policy.max_iterations,
    })
}

/// Принимается ли секущая: строго внутри локализующего интервала.
///
/// Одно сравнение покрывает всё, что может пойти не так: `NaN` не больше
/// и не меньше ничего, бесконечность (нулевой знаменатель) не меньше
/// верхней границы, вышедшая за край секущая не проходит по определению.
///
/// Вынесено отдельной функцией не ради читаемости, а ради проверяемости:
/// внутри цикла эта ветка недостижима — для пары значений разных знаков
/// секущая математически лежит между концами, — и мутационный заслон
/// справедливо называл её условия эквивалентными. Отдельная функция
/// проверяется напрямую, и защита перестаёт быть непроверяемой.
const fn secant_is_inside(secant: f64, low: f64, high: f64) -> bool {
    secant > low && secant < high
}

/// Середина интервала как значение, половина ширины как доказанная
/// граница погрешности.
fn finish(low: f64, high: f64, iterations: u32) -> ApproxValue {
    ApproxValue::new((low + high) / 2.0, (high - low).abs() / 2.0, iterations)
}

/// Ставка, при которой приведённая стоимость потоков равна нулю.
pub fn solve(
    flows: &[SolverFlow],
    policy: SolverPolicy,
    day_count: DayCount,
) -> Result<RateOutcome, SolverRefusal> {
    let series = Series::build(flows, day_count)?;
    let sign_changes = series.sign_changes();
    if sign_changes == 0 {
        return Err(SolverRefusal::NoSignChange);
    }

    let found = brackets(&series, policy)?;
    let bracket = match found.len() {
        0 => return Err(SolverRefusal::RootNotBracketed),
        1 => found[0],
        n => {
            return Err(SolverRefusal::MultipleRoots {
                count: u32::try_from(n).unwrap_or(u32::MAX),
            });
        }
    };

    // Единственный найденный интервал доказывает единственность корня
    // только при одной перемене знака у потоков. При большем числе перемен
    // сетка могла пропустить корень чётной кратности или пару корней
    // внутри шага — и выдать одно из нескольких значений за ответ.
    if sign_changes > 1 {
        return Err(SolverRefusal::UniquenessNotProven { sign_changes });
    }

    let rate = refine(&series, bracket, policy)?;
    Ok(RateOutcome {
        rate,
        policy,
        day_count,
    })
}
#[cfg(test)]
mod tests {
    use super::secant_is_inside;

    /// Защита секущей проверяется напрямую: внутри цикла она недостижима,
    /// а недостижимая проверка — это проверка, про которую неизвестно,
    /// работает ли она.
    #[test]
    fn only_a_strictly_interior_secant_is_accepted() {
        assert!(secant_is_inside(0.5, 0.0, 1.0));
        // Границы не годятся: приняв конец интервала, метод перестал бы
        // его сокращать и зациклился бы.
        assert!(!secant_is_inside(0.0, 0.0, 1.0));
        assert!(!secant_is_inside(1.0, 0.0, 1.0));
        // Выход за интервал — потеря локализации, то есть потеря
        // доказанной границы погрешности.
        assert!(!secant_is_inside(-0.1, 0.0, 1.0));
        assert!(!secant_is_inside(1.1, 0.0, 1.0));
        // Нечисловое значение и бесконечности из нулевого знаменателя.
        assert!(!secant_is_inside(f64::NAN, 0.0, 1.0));
        assert!(!secant_is_inside(f64::INFINITY, 0.0, 1.0));
        assert!(!secant_is_inside(f64::NEG_INFINITY, 0.0, 1.0));
    }
}
