#!/usr/bin/env bash
# Мутанты ТОЛЬКО в изменённых строках — быстрая проверка рабочего цикла.
#
# Это НЕ заслон. Заслон — scripts/check-mutants.sh, он гоняет весь список
# критичных модулей целиком, и в CI вызывается именно он. Здесь
# проверяются мутанты лишь в тех строках, которые тронуты диффом:
# правка теста в одном модуле может воскресить мутанта в другом,
# и диффом такое не ловится.
#
# Смысл в цене обратной связи. Полный прогон — тысячи мутантов и часы;
# после правки одного файла осмысленных из них десятки.
set -euo pipefail

if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "МУТАНТЫ-ДИФФ: не удалось определить корень репозитория" >&2
  exit 1
fi
cd "$REPO_ROOT"

BASE="${BASE:-main}"

for tool in cargo git; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "МУТАНТЫ-ДИФФ: $tool недоступен." >&2
    exit 1
  fi
done
if ! cargo mutants --version >/dev/null 2>&1; then
  echo "МУТАНТЫ-ДИФФ: cargo-mutants недоступен." >&2
  exit 1
fi

# Три точки: сравнение с точкой расхождения, а не с текущим состоянием
# базовой ветки. Иначе чужие коммиты в main приезжают в дифф как свои.
if ! git rev-parse --verify --quiet "$BASE" >/dev/null; then
  echo "МУТАНТЫ-ДИФФ: ветка '$BASE' не найдена (задайте BASE=...)." >&2
  exit 1
fi

DIFF_FILE=$(mktemp)
trap 'rm -f "$DIFF_FILE"' EXIT

# Неподтверждённые правки входят в дифф намеренно: проверять хочется
# то, что написано сейчас, а не то, что уже закоммичено.
#
# Один дифф от точки расхождения до рабочего дерева, а не склейка
# `BASE...HEAD` и `HEAD`. Склейка давала два набора заголовков по одному
# пути, когда файл изменён и в коммитах ветки, и в дереве; cargo-mutants
# такой дифф применить не может и падает кодом, неотличимым от выживших
# мутантов (iaam-387k).
if ! MERGE_BASE=$(git merge-base "$BASE" HEAD 2>/dev/null); then
  echo "МУТАНТЫ-ДИФФ: у HEAD и '$BASE' нет общего предка — дифф не построить." >&2
  exit 1
fi
if ! git diff "$MERGE_BASE" >"$DIFF_FILE" 2>/dev/null; then
  echo "МУТАНТЫ-ДИФФ: не удалось построить дифф от $MERGE_BASE." >&2
  echo "Это сбой построения диффа, а не выжившие мутанты." >&2
  exit 1
fi

if [ ! -s "$DIFF_FILE" ]; then
  echo "МУТАНТЫ-ДИФФ: относительно $BASE ничего не изменилось — проверять нечего."
  exit 0
fi

echo "Мутанты в строках, изменённых относительно $BASE."
echo "ВНИМАНИЕ: это не заслон. Полная проверка — make mutants."
echo ""

# --error только когда дифф трогает iaam-core: типы объявлены там,
# и в любом другом пакете такой мутант не собирается никогда, а полную
# сборку ради этого вывода оплачивает (см. scripts/check-mutants.sh).
error_args=()
if grep -q '^+++ b/crates/iaam-core/' "$DIFF_FILE"; then
  error_args=(
    --error 'crate::numeric::NumericError::Overflow'
    --error 'crate::money::MoneyError::Overflow'
  )
fi

cargo mutants \
  --in-diff "$DIFF_FILE" \
  "${error_args[@]}" \
  --profile mutant \
  --jobs 1 \
  --output target/mutants-in-diff \
  "$@"
