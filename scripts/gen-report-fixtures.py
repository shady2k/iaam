#!/usr/bin/env python3
"""Generator for synthetic broker report books.

The T-Investments workbook reproduces the export structure: a header with a period,
operational sheets and control sections. All values are invented, and
the fixture is assembled deterministically, so that it can be rebuilt.

    python3 scripts/gen-report-fixtures.py
"""

import datetime as dt
import pathlib
import zipfile

OUT = pathlib.Path("tests/fixtures/reports/tinkoff-synthetic.xlsx")
MINIMAL_OUT = pathlib.Path("crates/iaam-ingest/tests/fixtures/minimal_workbook.xlsx")


class DateCell:
    def __init__(self, value):
        self.value = value


class NumberCell:
    def __init__(self, value):
        self.value = value


def excel_date(year, month, day):
    epoch = dt.date(1899, 12, 30)
    return DateCell((dt.date(year, month, day) - epoch).days)


RUB = "RUB"
ACCOUNT = "INVEST-001"
CUSTODY = "НРД"
BOND = "BOND-X"

SHEETS = {
    "Общие сведения": [
        ["БРОКЕРСКИЙ ОТЧЕТ Т-ИНВЕСТИЦИЙ"],
        ["Период отчета", excel_date(2026, 3, 1), excel_date(2026, 3, 31)],
    ],
    "Сделки": [
        [
            "Дата",
            "Номер поручения",
            "Номер сделки",
            "Тикер",
            "ISIN",
            "Наименование",
            "Операция",
            "Количество",
            "Цена",
            "Сумма сделки",
            "НКД",
            "Комиссия",
            "Валюта",
            "Счет",
            "Место хранения",
        ],
        [
            excel_date(2026, 3, 5),
            "ORD-100",
            "TRD-100",
            BOND,
            "RU000000001",
            "Синтетическая облигация",
            "Покупка",
            NumberCell("10"),
            NumberCell("1000.00"),
            NumberCell("10000.00"),
            NumberCell("120.50"),
            NumberCell("10.00"),
            RUB,
            ACCOUNT,
            CUSTODY,
        ],
        [
            excel_date(2026, 3, 20),
            "ORD-101",
            "TRD-101",
            BOND,
            "RU000000001",
            "Синтетическая облигация",
            "Продажа",
            NumberCell("5"),
            NumberCell("2000.00"),
            NumberCell("10000.00"),
            None,
            NumberCell("15.00"),
            RUB,
            ACCOUNT,
            CUSTODY,
        ],
    ],
    "Денежные операции": [
        ["Дата", "Операция", "Сумма", "Валюта", "Счет"],
        [excel_date(2026, 3, 2), "Пополнение", NumberCell("50000.00"), RUB, ACCOUNT],
        [excel_date(2026, 3, 25), "Вывод", NumberCell("7000.00"), RUB, ACCOUNT],
        [excel_date(2026, 3, 28), "Корректировка", NumberCell("1.00"), RUB, ACCOUNT],
    ],
    "Комиссии": [
        ["Дата", "Сумма", "Валюта", "Счет"],
        [excel_date(2026, 3, 22), NumberCell("25.00"), RUB, ACCOUNT],
    ],
    "Купоны и дивиденды": [
        ["Дата", "Вид выплаты", "Тикер", "Сумма", "Валюта", "Счет"],
        [excel_date(2026, 3, 18), "Купон", BOND, NumberCell("450.00"), RUB, ACCOUNT],
        [excel_date(2026, 3, 19), "Дивиденд", None, NumberCell("300.00"), RUB, ACCOUNT],
    ],
    "РЕПО": [
        ["Дата", "Вид операции", "Тикер", "Количество", "Сумма", "Валюта"],
        [excel_date(2026, 3, 27), "РЕПО", BOND, NumberCell("2"), NumberCell("4000.00"), RUB],
    ],
    "Остатки денежных средств": [
        ["Валюта", "Остаток на начало", "Остаток на конец"],
        [RUB, NumberCell("12000.00"), NumberCell("55579.50")],
    ],
    "Обороты": [
        ["Валюта", "Дебет", "Кредит"],
        [RUB, NumberCell("17155.50"), NumberCell("60735.00")],
    ],
    "Остатки ценных бумаг": [
        ["Тикер", "Место хранения", "Количество на начало", "Количество на конец"],
        [BOND, CUSTODY, NumberCell("0"), NumberCell("5")],
    ],
    "Итоги": [
        ["Показатель", "Сумма", "Валюта"],
        ["Комиссии", NumberCell("25.00"), RUB],
        ["Купоны и дивиденды", NumberCell("750.00"), RUB],
        ["Удержанный налог", NumberCell("75.00"), RUB],
    ],
}
FINAM_OUT = pathlib.Path("tests/fixtures/reports/finam-synthetic.xls")

FINAM_SHEETS = {
    "Сведения": [
        ["ОТЧЕТ БРОКЕРА ФИНАМ"],
        ["Отчетный период", excel_date(2026, 4, 1), excel_date(2026, 4, 30)],
    ],
    "Сделки": [
        [
            "Дата сделки",
            "Дата расчетов",
            "Код договора",
            "Код инструмента",
            "Направление",
            "Объем",
            "Сумма сделки",
            "Комиссия",
            "НКД",
            "Валюта расчетов",
            "Место учета",
            "Идентификатор сделки",
        ],
        [
            excel_date(2026, 4, 3),
            excel_date(2026, 4, 6),
            ACCOUNT,
            "FIN-BOND",
            "Купля",
            NumberCell("12"),
            NumberCell("24000.00"),
            NumberCell("24.00"),
            NumberCell("150.00"),
            RUB,
            CUSTODY,
            "FIN-TRD-1",
        ],
        [
            excel_date(2026, 4, 18),
            excel_date(2026, 4, 21),
            ACCOUNT,
            "FIN-BOND",
            "Продажа",
            NumberCell("7"),
            NumberCell("17500.00"),
            NumberCell("17.50"),
            None,
            RUB,
            CUSTODY,
            "FIN-TRD-2",
        ],
    ],
    "Денежные движения": [
        ["Дата проводки", "Код договора", "Вид движения", "Валюта", "Сумма"],
        [excel_date(2026, 4, 2), ACCOUNT, "Ввод денежных средств", RUB, NumberCell("60000.00")],
        [excel_date(2026, 4, 22), ACCOUNT, "Вывод денежных средств", RUB, NumberCell("8000.00")],
        [excel_date(2026, 4, 28), ACCOUNT, "Корректировка", RUB, NumberCell("1.00")],
    ],
    "Списания комиссий": [
        ["Дата проводки", "Код договора", "Валюта", "Сумма комиссии"],
        [excel_date(2026, 4, 23), ACCOUNT, RUB, NumberCell("30.00")],
    ],
    "Выплаты": [
        ["Дата выплаты", "Код договора", "Событие", "Код инструмента", "Сумма", "Валюта"],
        [excel_date(2026, 4, 12), ACCOUNT, "Купон", "FIN-BOND", NumberCell("500.00"), RUB],
        [excel_date(2026, 4, 13), ACCOUNT, "Дивиденд", None, NumberCell("250.00"), RUB],
    ],
    "РЕПО": [
        ["Дата сделки", "Вид операции", "Код инструмента", "Объем", "Сумма", "Валюта"],
        [excel_date(2026, 4, 25), "РЕПО", "FIN-BOND", NumberCell("2"), NumberCell("4000.00"), RUB],
    ],
    "Денежные остатки": [
        ["Валюта расчетов", "На начало периода", "На конец периода"],
        [RUB, NumberCell("10000.00"), NumberCell("56196.00")],
    ],
    "Обороты денежных средств": [
        ["Валюта расчетов", "Списано", "Зачислено"],
        [RUB, NumberCell("32054.00"), NumberCell("78250.00")],
    ],
    "Позиции": [
        ["Код инструмента", "Место учета", "Остаток на начало", "Остаток на конец"],
        ["FIN-BOND", CUSTODY, NumberCell("3"), NumberCell("8")],
    ],
    "Сводные итоги": [
        ["Показатель", "Сумма", "Валюта"],
        ["Комиссии", NumberCell("715.00"), RUB],
        ["Купоны и дивиденды", NumberCell("750.00"), RUB],
        ["Удержанный налог", NumberCell("100.00"), RUB],
    ],
}

MINIMAL_ROWS = [
    ["Дата", "Тип", "Сумма"],
    [excel_date(2026, 5, 12), "Покупка", NumberCell("1234.56")],
]

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
%s
</Types>""" % "\n".join(
    f'<Override PartName="/xl/worksheets/sheet{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
    for i in range(1, len(SHEETS) + 1)
)

ROOT_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

WORKBOOK_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
%s
<Relationship Id="rId%d" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>""" % (
    "\n".join(
        f'<Relationship Id="rId{i}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{i}.xml"/>'
        for i in range(1, len(SHEETS) + 1)
    ),
    len(SHEETS) + 1,
)

# xf 1 is the built-in Excel date format. Without styles.xml a date is just a number.
STYLES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<fonts count="1"><font/></fonts>
<fills count="1"><fill/></fills>
<borders count="1"><border/></borders>
<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
<cellXfs count="2">
<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
<xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
</cellXfs>
</styleSheet>"""


def workbook_xml():
    sheets = "".join(
        f'<sheet name="{escape(name)}" sheetId="{i}" r:id="rId{i}"/>'
        for i, name in enumerate(SHEETS, start=1)
    )
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        f"<sheets>{sheets}</sheets></workbook>"
    )

def finam_content_types():
    return """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
%s
</Types>""" % "\n".join(
        f'<Override PartName="/xl/worksheets/sheet{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        for i in range(1, len(FINAM_SHEETS) + 1)
    )


def finam_workbook_rels():
    return """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
%s
<Relationship Id="rId%d" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>""" % (
        "\n".join(
            f'<Relationship Id="rId{i}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{i}.xml"/>'
            for i in range(1, len(FINAM_SHEETS) + 1)
        ),
        len(FINAM_SHEETS) + 1,
    )


def finam_workbook_xml():
    sheets = "".join(
        f'<sheet name="{escape(name)}" sheetId="{i}" r:id="rId{i}"/>'
        for i, name in enumerate(FINAM_SHEETS, start=1)
    )
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        f"<sheets>{sheets}</sheets></workbook>"
    )


def column(index):
    name = ""
    index += 1
    while index:
        index, rest = divmod(index - 1, 26)
        name = chr(ord("A") + rest) + name
    return name


def escape(text):
    return str(text).replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def cell(reference, value):
    if value is None:
        return ""
    if isinstance(value, DateCell):
        return f'<c r="{reference}" s="1"><v>{value.value}</v></c>'
    if isinstance(value, NumberCell):
        return f'<c r="{reference}"><v>{escape(value.value)}</v></c>'
    return f'<c r="{reference}" t="inlineStr"><is><t>{escape(value)}</t></is></c>'


def sheet_xml(rows):
    xml_rows = []
    for row_index, values in enumerate(rows, start=1):
        cells = "".join(cell(f"{column(i)}{row_index}", value) for i, value in enumerate(values))
        xml_rows.append(f'<row r="{row_index}">{cells}</row>')
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f"<sheetData>{''.join(xml_rows)}</sheetData></worksheet>"
    )


def write_minimal_workbook():
    content_types = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"""
    workbook = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Сделки" sheetId="1" r:id="rId1"/></sheets>
</workbook>"""
    workbook_rels = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"""
    MINIMAL_OUT.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(MINIMAL_OUT, "w", zipfile.ZIP_STORED) as book:
        for name, text in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", ROOT_RELS),
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", workbook_rels),
            ("xl/styles.xml", STYLES),
            ("xl/worksheets/sheet1.xml", sheet_xml(MINIMAL_ROWS)),
        ]:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            book.writestr(info, text)
    print(f"written: {MINIMAL_OUT}")


def write_finam_workbook():
    FINAM_OUT.parent.mkdir(parents=True, exist_ok=True)
    entries = [
        ("[Content_Types].xml", finam_content_types()),
        ("_rels/.rels", ROOT_RELS),
        ("xl/workbook.xml", finam_workbook_xml()),
        ("xl/_rels/workbook.xml.rels", finam_workbook_rels()),
        ("xl/styles.xml", STYLES),
    ]
    entries.extend(
        (f"xl/worksheets/sheet{i}.xml", sheet_xml(rows))
        for i, rows in enumerate(FINAM_SHEETS.values(), start=1)
    )
    with zipfile.ZipFile(FINAM_OUT, "w", zipfile.ZIP_STORED) as book:
        for name, text in entries:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            book.writestr(info, text)
    print(f"written: {FINAM_OUT}")


def main():
    OUT.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(OUT, "w", zipfile.ZIP_STORED) as book:
        entries = [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("xl/workbook.xml", workbook_xml()),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/styles.xml", STYLES),
        ]
        entries.extend(
            (f"xl/worksheets/sheet{i}.xml", sheet_xml(rows))
            for i, rows in enumerate(SHEETS.values(), start=1)
        )
        for name, text in entries:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            book.writestr(info, text)
    print(f"written: {OUT}")
    write_finam_workbook()
    write_minimal_workbook()


if __name__ == "__main__":
    main()
