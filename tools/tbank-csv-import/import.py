"""Submit a T-Bank operations export to iaam.

Knows the bank's export format and nothing about anybody's accounts: names
arrive through --account-map and are resolved against GET /v1/accounts at run
time. Nothing identifying may be written into this file — it is checked in.
"""

import argparse
import csv
import hashlib
import json
import os
import sys
import urllib.request
from collections import defaultdict
from datetime import datetime
from decimal import Decimal


CHANNEL_DEFAULT = "file"
# Default only: the export this tool was written against posts the two legs of
# an internal transfer about a second apart. Another institution posts them
# further apart, so it is a flag rather than a constant — but a WIDE window
# starts joining two genuinely separate transfers of the same round amount,
# which is why the default is tight.
PAIR_TOLERANCE_SECONDS_DEFAULT = 5


def amount_of(row):
    """Return the account-currency amount as a decimal-shaped string."""
    return row["Сумма в валюте счёта"].replace("\xa0", "").replace(" ", "").replace(",", ".")


def date_of(row):
    """The export prints DD.MM.YYYY HH:MM:SS; the API wants ISO dates."""
    return datetime.strptime(row["Дата операции"], "%d.%m.%Y %H:%M:%S")


def row_key(account_id, channel, raw_line, ordinal):
    """Build the row key required by the import design, section 3."""
    digest = hashlib.sha256(raw_line.encode("utf-8")).hexdigest()
    return f"{account_id}/{channel}/{digest}/{ordinal}"


def is_internal_transfer(row):
    return row["Описание"].strip() == "Между своими счетами"


def pair_legs(rows, tolerance_seconds=PAIR_TOLERANCE_SECONDS_DEFAULT):
    """Pair internal-transfer legs by amount and nearest time.

    T-Bank posts the two legs separately, so exact timestamps are not required.
    A tight five-second limit avoids joining separate transfers with the same
    amount. Unmatched legs are returned for explicit reporting by the caller.
    """
    pairs, singles = [], []
    legs, others = [], []
    for row in rows:
        (legs if is_internal_transfer(row) else others).append(row)

    outgoing = sorted(
        (row for row in legs if Decimal(amount_of(row)) < 0), key=date_of
    )
    incoming = sorted(
        (row for row in legs if Decimal(amount_of(row)) > 0), key=date_of
    )
    taken = set()
    for out_row in outgoing:
        best, best_gap = None, None
        for index, in_row in enumerate(incoming):
            if index in taken:
                continue
            if abs(Decimal(amount_of(in_row))) != abs(Decimal(amount_of(out_row))):
                continue
            gap = abs((date_of(in_row) - date_of(out_row)).total_seconds())
            if gap <= tolerance_seconds and (best_gap is None or gap < best_gap):
                best, best_gap = index, gap
        if best is None:
            singles.append(out_row)
        else:
            taken.add(best)
            pairs.append((out_row, incoming[best]))

    singles.extend(
        row for index, row in enumerate(incoming) if index not in taken
    )
    singles.extend(others)
    return pairs, singles


def get(base_url, path, token=None):
    request = urllib.request.Request(base_url.rstrip("/") + path)
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request) as response:
        return json.load(response)


def post(base_url, path, token, payload):
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(base_url.rstrip("/") + path, data=body)
    request.add_header("Authorization", f"Bearer {token}")
    request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request) as response:
        return json.load(response)


def resolve_accounts(base_url, token, account_map):
    """Resolve export names against the live account directory."""
    by_title = {account["title"]: account["id"] for account in get(base_url, "/v1/accounts", token)}
    missing = [title for title in account_map.values() if title not in by_title]
    if missing:
        raise SystemExit(
            f"no such account in the system: {', '.join(sorted(missing))}"
        )
    return {
        export_name: by_title[title]
        for export_name, title in account_map.items()
    }


# What the bank calls a row when the money was earned by the balance itself
# rather than sent by someone. These are not receipts from outside: a household
# report that counts interest as income overstates what actually arrived, and a
# returns calculation that counts it as a contribution is corrupted outright.
# The value on the right is the API's income kind, or None where the source
# does not say enough to name one — substituting a kind the source never stated
# would record an invention.
EARNED_BY_CAPITAL = {
    "Проценты": "deposit_interest",
    "Бонусы": None,
}

# The bank's categories under which money genuinely arrives from outside: a
# transfer somebody sent, or a top-up. Everything else the bank categorises is
# spending, so a POSITIVE row carrying a spending category is the merchant
# giving money back — a refund, which reverses the purchase rather than adding
# to income. Reading a returned appliance as income would overstate what
# arrived by its full price.
ARRIVES_FROM_OUTSIDE = {"Переводы", "Пополнения"}


def operation_of(row, account_id, currency="RUB"):
    value = amount_of(row)
    source_category = row["Категория по-умолчанию"]
    common = {
        "account": account_id,
        "currency": currency,
        "dates": {"cash_posted": date_of(row).date().isoformat()},
        "source_category": source_category,
        "description": row["Описание"],
    }
    if Decimal(value) > 0:
        if source_category in EARNED_BY_CAPITAL:
            operation = dict(common, type="income", amount=value.lstrip("-"))
            income_kind = EARNED_BY_CAPITAL[source_category]
            if income_kind is not None:
                operation["kind"] = income_kind
            return operation
        kind = "deposit" if source_category in ARRIVES_FROM_OUTSIDE else "refund"
        return dict(common, type=kind, amount=value.lstrip("-"))
    return dict(common, type="withdrawal", amount=value.lstrip("-"))


def transfer_to_own_account(row, statement_account_id, other_account_id, currency="RUB"):
    """A row the statement shows as an ordinary payment, which the operator says
    is a movement between two accounts of their own.

    Nothing in an export distinguishes a payment to a stranger from a top-up of
    the same person's account at another bank, so the answer arrives through
    --counterparty-map at run time and is never written into this file. The
    direction follows the sign: money leaving the statement's account goes to
    the other one, and money arriving came from it, which the API expresses as a
    transfer filed on the sending account."""
    value = amount_of(row)
    outgoing = Decimal(value) < 0
    sender = statement_account_id if outgoing else other_account_id
    receiver = other_account_id if outgoing else statement_account_id
    return {
        "account": sender,
        "type": "transfer",
        "to_account": receiver,
        "amount": value.lstrip("-"),
        "currency": currency,
        "dates": {"cash_posted": date_of(row).date().isoformat()},
        "source_category": row["Категория по-умолчанию"],
        "description": row["Описание"],
    }


def build(
    rows,
    accounts,
    channel,
    raw_lines,
    counterparties=None,
    tolerance_seconds=PAIR_TOLERANCE_SECONDS_DEFAULT,
):
    """Return operations and accounting counters for the input rows.

    An account absent from ``accounts`` is outside the contour. Its own rows
    are counted and skipped; a paired transfer crossing the contour becomes a
    real movement on the in-contour side.
    """
    pairs, singles = pair_legs(rows, tolerance_seconds)
    positions = {id(row): index for index, row in enumerate(rows)}
    canonical_accounts = {}
    for out_row, in_row in pairs:
        out_id = accounts.get(out_row["Имя счёта"])
        in_id = accounts.get(in_row["Имя счёта"])
        if out_id and in_id:
            canonical_accounts[id(out_row)] = out_id
        elif out_id or in_id:
            inside = out_row if out_id else in_row
            canonical_accounts[id(inside)] = out_id or in_id
    for row in singles:
        account_id = accounts.get(row["Имя счёта"])
        if account_id and not is_internal_transfer(row):
            canonical_accounts[id(row)] = account_id

    ordinals = defaultdict(int)
    row_ordinals = {}
    for row in rows:
        account_id = canonical_accounts.get(id(row))
        if account_id is None:
            continue
        day = date_of(row).date().isoformat()
        digest = hashlib.sha256(raw_lines[id(row)].encode("utf-8")).hexdigest()
        ordinal_key = (account_id, day, digest)
        ordinals[ordinal_key] += 1
        row_ordinals[id(row)] = ordinals[ordinal_key]

    operations_with_positions = []
    summary = {
        "submitted": 0,
        "dropped_second_leg": 0,
        "skipped_outside_contour": 0,
        "unmatched_legs": 0,
        "own_transfers": 0,
    }

    def key_for(row, account_id):
        return row_key(
            account_id,
            channel,
            raw_lines[id(row)],
            row_ordinals[id(row)],
        )

    for out_row, in_row in pairs:
        out_id = accounts.get(out_row["Имя счёта"])
        in_id = accounts.get(in_row["Имя счёта"])
        if out_id and in_id:
            operation = operation_of(out_row, out_id)
            operation["type"] = "transfer"
            operation["to_account"] = in_id
            operation["idempotency_key"] = key_for(out_row, out_id)
            operations_with_positions.append((positions[id(out_row)], operation))
            summary["dropped_second_leg"] += 1
        elif out_id or in_id:
            inside = out_row if out_id else in_row
            account_id = out_id or in_id
            operation = operation_of(inside, account_id)
            operation["idempotency_key"] = key_for(inside, account_id)
            operations_with_positions.append((positions[id(inside)], operation))
            summary["skipped_outside_contour"] += 1
        else:
            summary["skipped_outside_contour"] += 2

    for row in singles:
        account_id = accounts.get(row["Имя счёта"])
        if not account_id:
            summary["skipped_outside_contour"] += 1
            continue
        if is_internal_transfer(row):
            summary["unmatched_legs"] += 1
            print(
                f"unmatched transfer leg: {row['Дата операции']} {amount_of(row)}",
                file=sys.stderr,
            )
            continue
        other_id = (counterparties or {}).get(row["Описание"].strip())
        if other_id:
            # The operator has declared this counterparty to be an account of
            # their own, so the money never left the contour. The key still carries the statement's account:
            # it identifies the ROW, which belongs to that statement whichever
            # account the resulting operation is filed on.
            operation = transfer_to_own_account(row, account_id, other_id)
            operation["idempotency_key"] = key_for(row, account_id)
            summary["own_transfers"] += 1
        else:
            operation = operation_of(row, account_id)
            operation["idempotency_key"] = key_for(row, account_id)
        operations_with_positions.append((positions[id(row)], operation))

    operations = [
        operation
        for _, operation in sorted(operations_with_positions, key=lambda item: item[0])
    ]
    summary["submitted"] = len(operations)
    return operations, summary


def operation_summary(operations):
    """Create the compact, stable summary used by the synthetic proof."""
    hash_counts = defaultdict(int)
    for operation in operations:
        key_parts = operation["idempotency_key"].split("/")
        digest = key_parts[-2]
        hash_counts[
            (operation["account"], operation["dates"]["cash_posted"], digest)
        ] += 1

    result = []
    for operation in operations:
        item = {
            "kind": operation["type"],
            "amount": operation["amount"],
        }
        key_parts = operation["idempotency_key"].split("/")
        ordinal = int(key_parts[-1])
        digest = key_parts[-2]
        if hash_counts[
            (operation["account"], operation["dates"]["cash_posted"], digest)
        ] > 1:
            item["ordinal"] = ordinal
        result.append(item)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--export", required=True)
    parser.add_argument(
        "--account-map",
        required=True,
        help='JSON: {"<name in the export>": "<account title in iaam>"}',
    )
    parser.add_argument("--base-url", default="http://127.0.0.1:8080")
    parser.add_argument("--token-env", default="IAAM_TOKEN")
    parser.add_argument("--channel", default=CHANNEL_DEFAULT)
    parser.add_argument(
        "--pair-tolerance-seconds",
        type=int,
        default=PAIR_TOLERANCE_SECONDS_DEFAULT,
        help="How far apart the two legs of one internal transfer may be posted. "
        "Widen it only if this institution posts them further apart: a wide "
        "window joins two separate transfers of the same amount into one.",
    )
    parser.add_argument(
        "--counterparty-map",
        help='JSON: {"<description in the export>": "<account title in iaam>"}. '
        "Counterparties the operator declares to be their own accounts; their "
        "rows become transfers "
        "between his own accounts instead of money crossing the contour. Owner "
        "knowledge, supplied per run and never stored in this tool.",
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true")
    mode.add_argument("--submit", action="store_true")
    args = parser.parse_args()

    with open(args.export, encoding="utf-8-sig", newline="") as handle:
        text = handle.read()
    lines = text.splitlines()
    rows = list(csv.DictReader(lines, delimiter=";"))
    raw_lines = {id(row): line for row, line in zip(rows, lines[1:])}
    with open(args.account_map, encoding="utf-8") as handle:
        account_map = json.load(handle)

    token = os.environ.get(args.token_env, "")
    if args.dry_run:
        accounts = {
            export_name: export_name for export_name in account_map
        }
    else:
        accounts = resolve_accounts(args.base_url, token, account_map)

    counterparty_map = (
        json.load(open(args.counterparty_map, encoding="utf-8"))
        if args.counterparty_map
        else {}
    )
    if args.dry_run:
        counterparties = {name: title for name, title in counterparty_map.items()}
    else:
        counterparties = resolve_accounts(args.base_url, token, counterparty_map)

    operations, summary = build(
        rows,
        accounts,
        args.channel,
        raw_lines,
        counterparties,
        args.pair_tolerance_seconds,
    )
    summary["rows_in_file"] = len(rows)
    accounted = (
        summary["submitted"]
        + summary["dropped_second_leg"]
        + summary["skipped_outside_contour"]
        + summary["unmatched_legs"]
    )
    summary["unaccounted"] = len(rows) - accounted
    summary["operations"] = operation_summary(operations)
    summary["rejected"] = 0

    if args.dry_run:
        print(json.dumps(summary, ensure_ascii=False, indent=2))
        return

    by_account = defaultdict(list)
    for operation in operations:
        by_account[operation["account"]].append(operation)
    verdicts_by_kind = defaultdict(int)
    for account_id, batch in by_account.items():
        verdicts = post(
            args.base_url,
            "/v1/ingest/operations",
            token,
            {
                "source_label": f"tbank-export {os.path.basename(args.export)}",
                "source": {"account": account_id, "channel": args.channel},
                "operations": batch,
            },
        )
        for verdict in verdicts:
            verdicts_by_kind[verdict["verdict"]] += 1
            if verdict["verdict"] == "rejected":
                print(json.dumps(verdict, ensure_ascii=False), file=sys.stderr)
    # The whole tally, not a chosen pair of counters. "submitted" is what was
    # sent; only the verdicts say what the journal did with it, and a re-import
    # that changed nothing must be able to prove it rather than look identical
    # to the first run. The vocabulary is the API's: accepted, provisional,
    # duplicate, possible_duplicate, discrepancy, needs_reconciliation,
    # rejected.
    summary["verdicts"] = dict(sorted(verdicts_by_kind.items()))
    summary["rejected"] = verdicts_by_kind["rejected"]
    summary["already_known"] = verdicts_by_kind["duplicate"]
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
