#!/usr/bin/env python3
"""Минимальная книга XLSX для проверки обвязки чтения отчётов.

Настоящий брокерский отчёт фикстурой быть не может: он содержит
персональные данные владельца. Здесь лежит книга ровно того устройства,
которое обвязке нужно уметь прочитать, — лист с именем, текстовые
ячейки и числовая ячейка.

Файл собирается скриптом, а не кладётся руками: двоичная фикстура,
происхождение которой неизвестно, ничем не отличается от случайных
байтов.

    python3 scripts/gen-report-fixtures.py
"""

import pathlib
import zipfile

OUT = pathlib.Path("crates/iaam-ingest/tests/fixtures/minimal_workbook.xlsx")
SHEET_NAME = "Сделки"

# Дата в XLSX — число со стилем даты: 46154 = 12 мая 2026 года от эпохи
# Excel (30 декабря 1899). Без стиля та же ячейка неотличима от обычного
# числа, поэтому в книге есть styles.xml — иначе путь чтения дат остался
# бы непроверенным.
DATE_SERIAL = 46154


class DateCell:
    def __init__(self, serial):
        self.serial = serial


ROWS = [
    ["Дата", "Тип", "Сумма"],
    [DateCell(DATE_SERIAL), "Покупка", 1234.56],
]

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"""

ROOT_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

WORKBOOK_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"""

# Два формата ячейки: обычный (xf 0) и встроенный формат даты
# numFmtId="14" (xf 1). Ссылка s="1" в ячейке и делает её датой.
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

WORKBOOK = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="{SHEET_NAME}" sheetId="1" r:id="rId1"/></sheets>
</workbook>"""


def column(index):
    """A, B, C ... по индексу с нуля."""
    name = ""
    index += 1
    while index:
        index, rest = divmod(index - 1, 26)
        name = chr(ord("A") + rest) + name
    return name


def escape(text):
    return (
        text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    )


def cell(reference, value):
    if isinstance(value, DateCell):
        return f'<c r="{reference}" s="1"><v>{value.serial}</v></c>'
    if isinstance(value, (int, float)):
        return f'<c r="{reference}"><v>{value}</v></c>'
    return (
        f'<c r="{reference}" t="inlineStr"><is><t>{escape(value)}</t></is></c>'
    )


def sheet_xml():
    rows = []
    for row_index, values in enumerate(ROWS, start=1):
        cells = "".join(
            cell(f"{column(i)}{row_index}", value) for i, value in enumerate(values)
        )
        rows.append(f'<row r="{row_index}">{cells}</row>')
    body = "".join(rows)
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f"<sheetData>{body}</sheetData></worksheet>"
    )


def main():
    OUT.parent.mkdir(parents=True, exist_ok=True)
    # ZIP_STORED и фиксированная дата: файл обязан собираться байт в байт
    # одинаковым, иначе контрольная сумма фикстуры меняется от запуска.
    with zipfile.ZipFile(OUT, "w", zipfile.ZIP_STORED) as book:
        for name, text in [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("xl/workbook.xml", WORKBOOK),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/styles.xml", STYLES),
            ("xl/worksheets/sheet1.xml", sheet_xml()),
        ]:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            book.writestr(info, text)
    print(f"записано: {OUT}")


if __name__ == "__main__":
    main()
