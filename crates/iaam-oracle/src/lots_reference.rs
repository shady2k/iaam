//! Эталонная реализация списания лотов (§15.4).
//!
//! **Намеренно другой алгоритм.** Продакшн использует итеративный проход
//! с изменяемым остатком и `Decimal`. Эталон — рекурсию с накоплением
//! и целочисленную арифметику. Общего кода нет, поэтому общая ошибка
//! проявиться не может.
//!
//! Количества здесь целые: эталон покрывает биржевые бумаги, где дробных
//! количеств не бывает. Дробные случаи (крипта) проверяются фикстурами.

use core::cmp::Ordering;

/// Лот в эталонном представлении: количество и стоимость в минимальных единицах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefLot {
    pub quantity: i64,
    pub basis_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefDisposal {
    pub basis_released_minor: i64,
    pub remaining: Vec<RefLot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefError {
    InsufficientQuantity,
}

/// Списание по принципу «первые по времени приобретения».
///
/// Реализовано рекурсией: на каждом шаге обрабатывается голова списка,
/// хвост передаётся дальше. Накопитель несёт списанную стоимость.
pub fn dispose_fifo_rational(lots: &[RefLot], quantity: i64) -> Result<RefDisposal, RefError> {
    fn go(lots: &[RefLot], left: i64, released: i64) -> Result<RefDisposal, RefError> {
        match lots.split_first() {
            None if left == 0 => Ok(RefDisposal {
                basis_released_minor: released,
                remaining: vec![],
            }),
            None => Err(RefError::InsufficientQuantity),
            Some((head, tail)) if left == 0 => {
                let mut remaining = vec![*head];
                remaining.extend_from_slice(tail);
                Ok(RefDisposal {
                    basis_released_minor: released,
                    remaining,
                })
            }
            Some((head, tail)) if head.quantity <= left => {
                go(tail, left - head.quantity, released + head.basis_minor)
            }
            Some((head, tail)) => {
                // Пропорциональное разнесение через целочисленную арифметику
                // с округлением половины к чётному — как в продакшене,
                // но выраженное иначе.
                let taken = round_half_to_even(head.basis_minor, left, head.quantity);
                let kept = head.basis_minor - taken;
                let mut remaining = vec![RefLot {
                    quantity: head.quantity - left,
                    basis_minor: kept,
                }];
                remaining.extend_from_slice(tail);
                Ok(RefDisposal {
                    basis_released_minor: released + taken,
                    remaining,
                })
            }
        }
    }
    go(lots, quantity, 0)
}

/// `total * num / den` с округлением половины к чётному, без плавающей точки.
fn round_half_to_even(total: i64, num: i64, den: i64) -> i64 {
    debug_assert!(den > 0);
    let product = i128::from(total) * i128::from(num);
    let den = i128::from(den);
    let quotient = product.div_euclid(den);
    let remainder = product.rem_euclid(den);
    let twice = remainder * 2;
    // Три ветви вместо цепочки `if`: две из них дают одно и то же значение,
    // и цепочка на них не компилируется (`clippy::if_same_then_else`).
    // Решение — считать не значение, а признак «округляем вверх».
    let round_up = match twice.cmp(&den) {
        Ordering::Greater => true,
        Ordering::Less => false,
        // Ничья: к чётному. Нечётное частное поднимаем, чётное оставляем.
        Ordering::Equal => quotient % 2 != 0,
    };
    let result = if round_up { quotient + 1 } else { quotient };
    i64::try_from(result).expect("стоимость лота не выходит за i64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_to_even_rounds_ties_to_even() {
        // 5 * 1 / 2 = 2,5 → 2 (чётное)
        assert_eq!(round_half_to_even(5, 1, 2), 2);
        // 7 * 1 / 2 = 3,5 → 4 (чётное)
        assert_eq!(round_half_to_even(7, 1, 2), 4);
    }

    #[test]
    fn selling_more_than_held_is_an_error() {
        // Ветка InsufficientQuantity эталона не покрывалась ничем: в фикстуре
        // parity-теста нет случая с перепродажей, а собственных тестов
        // у эталона не было. Мутационный заслон это и показал.
        let lots = [RefLot {
            quantity: 10,
            basis_minor: 100_000,
        }];
        assert_eq!(
            dispose_fifo_rational(&lots, 11),
            Err(RefError::InsufficientQuantity)
        );
    }

    #[test]
    fn selling_nothing_consumes_no_lot_even_if_it_is_empty() {
        // Лот с нулевым количеством и ненулевой стоимостью различает
        // «ничего не продаём» и «списываем лот целиком»: при провале
        // в следующую ветку `0 <= 0` истинно, и стоимость ушла бы
        // в списанное. Вырожденный вход, но именно он отделяет
        // одно намерение от другого.
        let lots = [
            RefLot {
                quantity: 0,
                basis_minor: 500,
            },
            RefLot {
                quantity: 10,
                basis_minor: 100_000,
            },
        ];
        let out = dispose_fifo_rational(&lots, 0).unwrap();
        assert_eq!(
            out.basis_released_minor, 0,
            "ничего не продано — ничего не списано"
        );
        assert_eq!(out.remaining.len(), 2, "оба лота остались нетронутыми");
    }

    #[test]
    fn taking_first_lot_whole() {
        let lots = [RefLot {
            quantity: 10,
            basis_minor: 100_000,
        }];
        let out = dispose_fifo_rational(&lots, 10).unwrap();
        assert_eq!(out.basis_released_minor, 100_000);
        assert!(out.remaining.is_empty());
    }
}
