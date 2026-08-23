//! Сверка: статус полноты счёта на интервале по измерению (§10.3).
//!
//! **Статус присваивается не операции.** Операция либо записана, либо
//! нет; утверждать про неё «подтверждена» бессмысленно — подтверждается
//! полнота интервала: что за март по деньгам учтено всё и ничего
//! лишнего. Поэтому единицей статуса является пара интервал×измерение,
//! а не событие, и поля «уровень достоверности» у события не существует.

pub mod claim;
pub mod observed;

use serde::{Deserialize, Serialize};

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
