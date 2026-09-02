"""Create iaam categories from an Actual Budget export.

Reads an Actual export and writes its two-level category list into iaam through
the documented API. Nothing identifying lives in this file — it is checked in:
the names come from the export at run time, and what to leave behind is named
with --exclude.

Actual models income as a flag on the group rather than as a separate concept,
and so does iaam; the flag is carried across unchanged.
"""

import argparse
import json
import os
import sqlite3
import sys
import tempfile
import urllib.request
import zipfile


def open_export(path):
    """Accept either the .zip Actual produces or the db.sqlite inside it."""
    if not zipfile.is_zipfile(path):
        return sqlite3.connect(path)
    with zipfile.ZipFile(path) as archive:
        name = next(n for n in archive.namelist() if n.endswith("db.sqlite"))
        target = os.path.join(tempfile.mkdtemp(), "db.sqlite")
        with archive.open(name) as source, open(target, "wb") as out:
            out.write(source.read())
    return sqlite3.connect(target)


def read_list(connection):
    """Groups in their own order, each with its live categories.

    A tombstoned row is Actual's deletion: carrying it across would resurrect a
    category the owner has already retired.
    """
    groups = []
    rows = connection.execute(
        "SELECT id, name, is_income FROM category_groups "
        "WHERE tombstone = 0 ORDER BY is_income, sort_order"
    ).fetchall()
    for group_id, name, is_income in rows:
        categories = [
            title
            for (title,) in connection.execute(
                "SELECT name FROM categories WHERE cat_group = ? AND tombstone = 0 "
                "ORDER BY sort_order",
                (group_id,),
            )
        ]
        groups.append({"title": name, "is_income": bool(is_income), "categories": categories})
    return groups


def call(base_url, path, token, payload=None):
    request = urllib.request.Request(base_url.rstrip("/") + path)
    request.add_header("Authorization", f"Bearer {token}")
    if payload is not None:
        request.data = json.dumps(payload).encode("utf-8")
        request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request) as response:
        return json.load(response)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--export", required=True, help="Actual export: .zip or db.sqlite")
    parser.add_argument(
        "--exclude",
        action="append",
        default=[],
        metavar="NAME",
        help="A category or group to leave behind, by its name in the export. "
        "Repeatable. Use it for what the old tool needed and the new model does "
        "not — a catch-all bucket, an opening-balance placeholder, a category "
        "that is now an internal transfer.",
    )
    parser.add_argument("--base-url", default="http://127.0.0.1:8080")
    parser.add_argument("--token-env", default="IAAM_TOKEN")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true")
    mode.add_argument("--apply", action="store_true")
    args = parser.parse_args()

    excluded = set(args.exclude)
    groups = read_list(open_export(args.export))

    planned, skipped = [], []
    for group in groups:
        if group["title"] in excluded:
            skipped.extend(f"{group['title']}/{c}" for c in group["categories"])
            continue
        keep = [c for c in group["categories"] if c not in excluded]
        skipped.extend(
            f"{group['title']}/{c}" for c in group["categories"] if c in excluded
        )
        if keep:
            planned.append({**group, "categories": keep})

    summary = {
        "groups": len(planned),
        "categories": sum(len(g["categories"]) for g in planned),
        "skipped": sorted(skipped),
        "plan": [
            {"group": g["title"], "is_income": g["is_income"], "categories": g["categories"]}
            for g in planned
        ],
    }

    if args.dry_run:
        print(json.dumps(summary, ensure_ascii=False, indent=2))
        return

    token = os.environ.get(args.token_env, "")
    existing_groups = {g["title"]: g["id"] for g in call(args.base_url, "/v1/category-groups", token)}
    existing_categories = {
        (c["group"], c["title"]) for c in call(args.base_url, "/v1/categories", token)
    }
    created = {"groups": 0, "categories": 0, "already_present": 0}
    for group in planned:
        group_id = existing_groups.get(group["title"])
        if group_id is None:
            group_id = call(
                args.base_url,
                "/v1/category-groups",
                token,
                {"title": group["title"], "is_income": group["is_income"]},
            )["id"]
            created["groups"] += 1
        for title in group["categories"]:
            if (group_id, title) in existing_categories:
                created["already_present"] += 1
                continue
            call(
                args.base_url,
                "/v1/categories",
                token,
                {"group": group_id, "title": title},
            )
            created["categories"] += 1
    summary["created"] = created
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
