"""Submit a T-Bank operations export to iaam.

Knows the bank's export format and nothing about anybody's accounts: names
arrive through --account-map and are resolved against GET /v1/accounts at run
time. Nothing identifying may be written into this file — it is checked in.
"""

import argparse
import csv
import email.utils
import hashlib
import json
import math
import os
import sys
import time
import urllib.error
import urllib.request
from collections import defaultdict
from datetime import datetime, timezone
from decimal import Decimal
from urllib.parse import urlencode


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


JOURNAL_PAGE_SIZE = 200


def import_label(export_path):
    """Name the import exactly as the submit and retract declarations do."""
    return f"tbank-export {os.path.basename(export_path)}"


def retry_after_seconds(error):
    """Return the server's retry window, with a safe fallback."""
    raw = error.headers.get("Retry-After") if error.headers else None
    if raw:
        try:
            return max(0, int(raw))
        except ValueError:
            try:
                retry_at = email.utils.parsedate_to_datetime(raw)
                if retry_at.tzinfo is None:
                    retry_at = retry_at.replace(tzinfo=timezone.utc)
                return max(
                    0,
                    math.ceil(
                        (retry_at - datetime.now(timezone.utc)).total_seconds()
                    ),
                )
            except (TypeError, ValueError, OverflowError):
                pass
    return 1


def journal_events(base_url, token, **filters):
    """Read every journal page for the supplied filters."""
    rows = []
    filters.setdefault("limit", JOURNAL_PAGE_SIZE)
    while True:
        query = urlencode(
            {key: value for key, value in filters.items() if value is not None}
        )
        path = "/v1/journal/events"
        if query:
            path += f"?{query}"
        try:
            page = get(base_url, path, token)
        except urllib.error.HTTPError as error:
            if error.code != 429:
                raise
            delay = retry_after_seconds(error)
            print(
                "journal rate limit: waiting "
                f"{delay} seconds before retrying; fetched rows are kept",
                file=sys.stderr,
                flush=True,
            )
            time.sleep(delay)
            continue
        rows.extend(page["rows"])
        if page.get("next") is None:
            return rows
        filters["after"] = page["next"]


def replacement_targets(base_url, token, account_id, channel, label, operations):
    """Find the withdrawn event each operation must replace.

    The declaration names the import directly. The UUID remains published on
    each row, but the importer already holds the account, channel and label it
    submitted under and must not scan the account's whole journal to rediscover
    that same identity.
    """
    wanted = {operation["idempotency_key"] for operation in operations}
    imported_rows = journal_events(
        base_url,
        token,
        source_account=account_id,
        source_channel=channel,
        source_label=label,
    )
    by_key = {
        row["idempotency_key"]: row
        for row in imported_rows
        if row.get("idempotency_key") in wanted
    }
    if set(by_key) != wanted:
        raise SystemExit(
            "the declared journal import does not contain exactly the rows in this export"
        )
    return {key: by_key[key]["event"] for key in wanted}


def resolve_accounts(base_url, token, account_map):
    """Resolve export names against the live account directory, by title.

    The pre-0004 path, kept for an offline preview and for an operator who has
    not yet declared his accounts' identities. It resolves by TITLE, so it
    breaks on a rename at either end -- which is the defect decision 0004 was
    written about and decision 0005 retires. Prefer running without
    --account-map.
    """
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


def identify_accounts(base_url, token, export_names):
    """Resolve the export's own account names against the identity iaam holds.

    Decision 0004 gave an account the identity its source prints: an opaque
    ``provider_account_id``, plus aliases for the cards over it. So the name
    this export prints in its account column is a value the operator declares
    ONCE, on the account, and this tool reads it back instead of taking a file
    that maps the same name to a title.

    Exact equality on the declared identity and its aliases, and nothing else.
    The server's own resolution has three tiers and falls back to the title;
    this deliberately does not, because a title match here would silently
    restore the behaviour the map is being retired for. An export name the
    directory does not identify is simply outside the contour -- reported to
    the caller, never guessed at.

    A value two accounts answer to identifies neither, exactly as the server
    refuses an ambiguity rather than picking between candidates.
    """
    accounts = get(base_url, "/v1/accounts", token)
    reached = defaultdict(set)
    for account in accounts:
        printed = account.get("provider_account_id")
        if printed:
            reached[printed].add(account["id"])
        for alias in account.get("aliases", []):
            reached[alias["value"]].add(account["id"])
    resolved = {}
    for name in export_names:
        candidates = reached.get(name, set())
        if len(candidates) == 1:
            resolved[name] = next(iter(candidates))
        elif len(candidates) > 1:
            raise SystemExit(
                f"«{name}» is declared on {len(candidates)} accounts: give exactly "
                "one of them that identifier, or name the account by its iaam "
                "identifier"
            )
    return resolved


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
        "owner_category": row["Ваша категория"] or None,
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
        "owner_category": row["Ваша категория"] or None,
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

    # An unmatched internal-transfer leg is never submitted, so it never earns a
    # canonical account above -- but it still needs the same ordinal a submitted
    # row would get, because the identifier reported for it below (`key_for`)
    # must be computable. Sharing `ordinals` rather than a second counter is
    # deliberate: a row that collides on (account, day, digest) with something
    # else is the same disambiguation this counter already exists for.
    for row in singles:
        if id(row) in row_ordinals:
            continue
        account_id = accounts.get(row["Имя счёта"])
        if account_id is None or not is_internal_transfer(row):
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
        "dropped_legs": [],
        "skipped_outside_contour": 0,
        "unmatched_legs": 0,
        "unmatched": [],
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
            # The counter alone does not say WHICH row vanished or where it
            # went. `folded_into` is the surviving operation's idempotency_key,
            # so this leg is locatable both in the source (line/date/amount/
            # description) and, once the batch is submitted, in the journal
            # through `event_id` -- filled in below from that operation's own
            # verdict.
            summary["dropped_legs"].append(
                {
                    "line": positions[id(in_row)] + 2,
                    "account": in_row["Имя счёта"],
                    "date": in_row["Дата операции"],
                    "amount": amount_of(in_row),
                    "description": in_row["Описание"],
                    "folded_into": operation["idempotency_key"],
                    "event_id": None,
                }
            )
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
            summary["unmatched"].append(
                {
                    "line": positions[id(row)] + 2,
                    "account": row["Имя счёта"],
                    "date": row["Дата операции"],
                    "amount": amount_of(row),
                    "description": row["Описание"],
                    # The identifier this row would carry if it were ever
                    # submitted on its own -- never sent anywhere, since an
                    # unmatched leg is dropped rather than submitted, but a
                    # stable handle beyond "search by date and amount" once the
                    # tolerance is widened and the export is run again.
                    "idempotency_key": key_for(row, account_id),
                }
            )
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


def operation_summary(operations, account_names=None):
    """Create the compact, stable summary used by the synthetic proof."""
    account_names = account_names or {}
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
            "account": account_names.get(operation["account"], operation["account"]),
            "kind": operation["type"],
            "amount": operation["amount"],
            "source_category": operation.get("source_category"),
            "owner_category": operation.get("owner_category"),
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


def account_summary(operations, account_names=None):
    """Summarise converted rows as money in, money out, and net movement."""
    account_names = account_names or {}
    totals = defaultdict(
        lambda: {
            "converted_rows": 0,
            "inflow": Decimal("0.00"),
            "outflow": Decimal("0.00"),
        }
    )
    for operation in operations:
        name = account_names.get(operation["account"], operation["account"])
        amount = Decimal(operation["amount"])
        totals[name]["converted_rows"] += 1
        if operation["type"] in {"withdrawal", "transfer"}:
            totals[name]["outflow"] += amount
        else:
            totals[name]["inflow"] += amount
    return [
        {
            "account": name,
            "converted_rows": values["converted_rows"],
            "inflow": format(values["inflow"], "f"),
            "outflow": format(values["outflow"], "f"),
            "net": format(values["inflow"] - values["outflow"], "f"),
        }
        for name, values in sorted(totals.items())
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--export", required=True)
    parser.add_argument(
        "--account-map",
        help='JSON: {"<name in the export>": "<account title in iaam>"}. The '
        "pre-0004 fallback, resolved by TITLE and therefore broken by a rename "
        "at either end. Omit it: the export's account names are then resolved "
        "against the identity iaam holds for each account "
        "(provider_account_id and its aliases), which the operator declares "
        "once instead of passing a file on every run.",
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
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--dry-run", action="store_true")
    mode.add_argument("--submit", action="store_true")
    parser.add_argument(
        "--replace-retracted",
        action="store_true",
        help="replace the withdrawn events of this export through the corrections "
        "route; combine with --dry-run to preview, or use alone to submit; the "
        "original channel is never changed",
    )
    args = parser.parse_args()
    if not (args.dry_run or args.submit or args.replace_retracted):
        parser.error("choose --dry-run, --submit, or --replace-retracted")

    with open(args.export, encoding="utf-8-sig", newline="") as handle:
        text = handle.read()
    lines = text.splitlines()
    rows = list(csv.DictReader(lines, delimiter=";"))
    raw_lines = {id(row): line for row, line in zip(rows, lines[1:])}

    token = os.environ.get(args.token_env, "")
    offline_preview = args.dry_run and not args.replace_retracted
    account_names = {}
    if args.account_map:
        with open(args.account_map, encoding="utf-8") as handle:
            account_map = json.load(handle)
        # The offline preview needs no directory: a name maps to itself.
        if offline_preview:
            accounts = {export_name: export_name for export_name in account_map}
            account_names = {
                export_name: title for export_name, title in account_map.items()
            }
        else:
            accounts = resolve_accounts(args.base_url, token, account_map)
            account_names = {
                accounts[export_name]: title
                for export_name, title in account_map.items()
            }
    else:
        # No file: the contour is what iaam identifies. That is a directory
        # read, and a dry run performs it -- a dry run writes nothing, which is
        # not the same promise as touching nothing.
        printed_names = sorted({row["Имя счёта"] for row in rows})
        accounts = identify_accounts(args.base_url, token, printed_names)
        account_names = {
            accounts[name]: name for name in printed_names if name in accounts
        }
        # An unrecognised name is a whole account's month dropped in silence,
        # where a missing line in the map used to be a refusal. It is reported
        # before anything is submitted, because the fix is to declare that
        # identifier on the account and run again.
        for name in printed_names:
            if name not in accounts:
                print(
                    f"outside the contour: no account of yours is identified by "
                    f"«{name}»",
                    file=sys.stderr,
                )

    counterparty_map = (
        json.load(open(args.counterparty_map, encoding="utf-8"))
        if args.counterparty_map
        else {}
    )
    if offline_preview:
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
    summary["operations"] = operation_summary(operations, account_names)
    summary["accounts"] = account_summary(operations, account_names)
    summary["rejected"] = 0
    source_label = import_label(args.export)

    by_account = defaultdict(list)
    for operation in operations:
        by_account[operation["account"]].append(operation)
    targets_by_account = {}
    if args.replace_retracted:
        for account_id, batch in by_account.items():
            targets_by_account[account_id] = replacement_targets(
                args.base_url, token, account_id, args.channel, source_label, batch
            )
        summary["replacements"] = [
            {
                "account": account_names.get(account_id, account_id),
                "rows": len(batch),
                "events": len(targets_by_account[account_id]),
            }
            for account_id, batch in by_account.items()
        ]

    if args.dry_run:
        print(json.dumps(summary, ensure_ascii=False, indent=2))
        return

    verdicts_by_kind = defaultdict(int)
    duplicates = []
    event_id_by_key = {}
    for account_id, batch in by_account.items():
        if args.replace_retracted:
            targets = targets_by_account[account_id]
            corrections = []
            for operation in batch:
                target = targets[operation["idempotency_key"]]
                replacement = dict(operation)
                replacement["idempotency_key"] = f"correction/replacement/{target}"
                corrections.append(
                    {
                        "relation": "replacement",
                        "target": target,
                        "operation": replacement,
                    }
                )
            verdicts = post(
                args.base_url,
                "/v1/corrections",
                token,
                {
                    "acknowledge_retraction": True,
                    "corrections": corrections,
                },
            )
        else:
            verdicts = post(
                args.base_url,
                "/v1/ingest/operations",
                token,
                {
                    # This declaration names the original import. A retracted
                    # import is replaced through /v1/corrections instead of
                    # changing this channel to evade its withdrawn keys.
                    "source": {
                        "account": account_id,
                        "channel": args.channel,
                        "label": source_label,
                    },
                    "operations": batch,
                },
            )
        for verdict in verdicts:
            verdict_by_kind = verdict["verdict"]
            verdicts_by_kind[verdict_by_kind] += 1
            # `batch` holds the ORIGINAL operations regardless of mode: in
            # replace-retracted mode the request wraps a copy under a
            # `correction/replacement/...` key, but `row` still indexes this
            # same list, so this is the one idempotency_key that also appears
            # in `dropped_legs` and everywhere else in this summary.
            row_index = verdict.get("row")
            idempotency_key = (
                batch[row_index]["idempotency_key"]
                if row_index is not None and row_index < len(batch)
                else None
            )
            event_id = verdict.get("event_id")
            if idempotency_key is not None and event_id is not None:
                event_id_by_key[idempotency_key] = event_id
            if verdict_by_kind == "duplicate":
                # A count alone is not an answer to "what did I collide with":
                # the API already names the existing event on every duplicate
                # verdict (crates/iaam-server/src/dto.rs Verdict::Duplicate),
                # so it is kept here rather than thrown away.
                duplicates.append(
                    {"idempotency_key": idempotency_key, "event_id": event_id}
                )
            if verdict_by_kind == "rejected":
                print(json.dumps(verdict, ensure_ascii=False), file=sys.stderr)
    # The whole tally, not a chosen pair of counters. "submitted" is what was
    # sent; only the verdicts say what the journal did, and a re-import
    # that changed nothing must be able to prove it rather than look identical
    # to the first run. The vocabulary is the API's: accepted, provisional,
    # duplicate, possible_duplicate, discrepancy, needs_reconciliation,
    # rejected.
    summary["verdicts"] = dict(sorted(verdicts_by_kind.items()))
    summary["rejected"] = verdicts_by_kind["rejected"]
    summary["already_known"] = verdicts_by_kind["duplicate"]
    summary["duplicates"] = duplicates

    # Many rows collapsing onto ONE existing event is almost always an
    # idempotency-key defect -- a carried or None component came out the same
    # for rows that are not in fact the same operation -- rather than an
    # ordinary re-import. That must not have to be inferred from a total, so
    # it is grouped and called out here rather than left for the reader to
    # notice only after querying the journal directly.
    collisions_by_event = defaultdict(list)
    for duplicate in duplicates:
        if duplicate["event_id"] is not None:
            collisions_by_event[duplicate["event_id"]].append(
                duplicate["idempotency_key"]
            )
    summary["duplicate_collisions"] = [
        {
            "event_id": event_id,
            "row_count": len(keys),
            "idempotency_keys": keys,
        }
        for event_id, keys in sorted(collisions_by_event.items())
        if len(keys) > 1
    ]
    for collision in summary["duplicate_collisions"]:
        print(
            f"duplicate collision: {collision['row_count']} rows all collided "
            f"with event {collision['event_id']} -- almost certainly an "
            "idempotency key defect, not a re-import",
            file=sys.stderr,
        )

    # The dropped leg's own row was never submitted, but the operation it was
    # folded into was -- so once that operation has a verdict, the leg's entry
    # can carry the same event_id rather than staying a promise of one.
    for leg in summary["dropped_legs"]:
        folded_event_id = event_id_by_key.get(leg["folded_into"])
        if folded_event_id is not None:
            leg["event_id"] = folded_event_id

    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
