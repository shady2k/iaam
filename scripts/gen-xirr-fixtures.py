#!/usr/bin/env python3
"""Независимый эталон XIRR (§15.4).

Реализация намеренно другая, чем в ядре: бисекция на 50 знаках десятичной
арифметики (`decimal`), без метода Ньютона и без двоичной плавающей точки.
Общего кода с продакшеном нет — он на другом языке.

Значения замораживаются в tests/fixtures/xirr_cases.json и после этого
не пересчитываются ради зелёного теста (§15.7).
"""

import datetime
import json
from decimal import Decimal, getcontext

getcontext().prec = 50

YEAR = Decimal(365)


def sign(value: Decimal) -> int:
    """Знак числа. Decimal.copy_sign сюда не годится: он возвращает величину
    со знаком аргумента, то есть сравнение copy_sign(1) сравнивало бы модули
    и всегда давало бы «знак не менялся»."""
    if value > 0:
        return 1
    if value < 0:
        return -1
    return 0


def npv(rate: Decimal, flows) -> Decimal:
    """Приведённая стоимость. Степень с дробным показателем считается
    через exp(ln), потому что Decimal не умеет возведения в дробную степень."""
    base = Decimal(1) + rate
    total = Decimal(0)
    d0 = flows[0][0]
    for day, amount in flows:
        years = (Decimal((day - d0).days)) / YEAR
        total += amount / (base ** years if years == int(years) else (years * base.ln()).exp())
    return total


def xirr(flows, low=Decimal("-0.9999"), high=Decimal(100)) -> Decimal:
    """Бисекция до 40 знаков. Никакого Ньютона — эталон обязан отличаться
    от проверяемой реализации не только числами, но и методом."""
    f_low = npv(low, flows)
    if sign(f_low) == sign(npv(high, flows)):
        raise ValueError("знак не меняется на границах диапазона")
    for _ in range(400):
        mid = (low + high) / 2
        f_mid = npv(mid, flows)
        if f_mid == 0:
            return mid
        if sign(f_mid) == sign(f_low):
            low, f_low = mid, f_mid
        else:
            high = mid
    return (low + high) / 2


def d(s: str) -> datetime.date:
    return datetime.date.fromisoformat(s)


CASES = [
    {
        "name": "один год, ровно десять процентов",
        "comment": "Вложено 1000, через 365 дней получено 1100.",
        "flows": [("2025-01-01", "-1000"), ("2026-01-01", "1100")],
    },
    {
        "name": "високосный год не даёт ровной ставки",
        "comment": "366 дней между потоками: ставка чуть ниже 10 %.",
        "flows": [("2024-01-01", "-1000"), ("2025-01-01", "1100")],
    },
    {
        "name": "два пополнения и вывод",
        "comment": "Типичный портфель: докупка через полгода.",
        "flows": [
            ("2024-03-01", "-100000"),
            ("2024-09-01", "-50000"),
            ("2026-03-01", "175000"),
        ],
    },
    {
        "name": "убыток",
        "comment": "Вложено 100000, изъято 80000 через два года.",
        "flows": [("2024-01-01", "-100000"), ("2026-01-01", "80000")],
    },
    {
        "name": "купоны между пополнением и выводом",
        "comment": "Облигационный поток: четыре купона и погашение.",
        "flows": [
            ("2024-01-15", "-98000"),
            ("2024-07-15", "4500"),
            ("2025-01-15", "4500"),
            ("2025-07-15", "4500"),
            ("2026-01-15", "104500"),
        ],
    },
    {
        "name": "внутридневная серия не схлопывается",
        "comment": "Два потока в один день плюс вывод через год.",
        "flows": [
            ("2025-02-10", "-30000"),
            ("2025-02-10", "-20000"),
            ("2026-02-10", "56000"),
        ],
    },
]


def main() -> None:
    out = {
        "source": "scripts/gen-xirr-fixtures.py, decimal.getcontext().prec = 50, бисекция 400 шагов",
        "day_count": "act/365",
        "cases": [],
    }
    for case in CASES:
        flows = [(d(day), Decimal(amount)) for day, amount in case["flows"]]
        rate = xirr(flows)
        out["cases"].append(
            {
                "name": case["name"],
                "comment": case["comment"],
                "flows": [{"date": day, "amount": amount} for day, amount in case["flows"]],
                "expected_rate": str(rate.quantize(Decimal("1.000000000000"))),
            }
        )
    print(json.dumps(out, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()