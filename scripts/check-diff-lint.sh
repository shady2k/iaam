#!/usr/bin/env bash
# Запрет ослабления проверок в диффе (§15.7).
# Агент, столкнувшись с падающим линтом, склонен добавить allow вместо исправления.
set -euo pipefail

# Корень ищется от каталога самого скрипта, а не от cwd вызывающего: запуск
# из не-git каталога иначе даёт пустую строку и `cd ""`, то есть заслон
# проверяет не тот каталог. Не определили корень — это отказ заслона.
if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "DIFF-LINT: не удалось определить корень репозитория от $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

BASE="${1:-}"

if [ -z "$BASE" ]; then
  echo "ОШИБКА: база для сравнения не передана." >&2
  echo "Заслон, который молча пропускает себя при отсутствии базы, бесполезен:" >&2
  echo "именно в этом состоянии через него и пройдёт ослабление проверки." >&2
  exit 1
fi

# База может быть коммитом (обычный случай) или деревом: CI при первом push
# в ветку подставляет хеш пустого дерева. Формы диффа для них РАЗНЫЕ.
# `git diff <tree>...HEAD` — фатальная ошибка «is a tree, not a commit»,
# и с `|| true` на пайплайне она читалась бы как «нарушений нет».
# Поэтому база разбирается явно, а `git diff` вызывается без маскировки кода.
if BASE_RESOLVED=$(git rev-parse --verify --quiet "${BASE}^{commit}"); then
  DIFF_RANGE=("${BASE_RESOLVED}...HEAD")
elif BASE_RESOLVED=$(git rev-parse --verify --quiet "${BASE}^{tree}"); then
  DIFF_RANGE=("$BASE_RESOLVED" "HEAD")
else
  echo "ОШИБКА: база $BASE недоступна (ни коммит, ни дерево). Заслон не может отработать." >&2
  exit 1
fi

# Пустой диапазон — законная ситуация (например, коммит без .rs-файлов),
# но она не должна маскировать отсутствие базы, проверенное выше.

# Только добавленные строки в .rs файлах.
# `git diff` вызывается отдельно и его код возврата проверяется: `|| true`,
# привязанный ко всему пайплайну, спрятал бы падение самого git.
if ! diff_out=$(git diff "${DIFF_RANGE[@]}" -- '*.rs'); then
  echo "ОШИБКА: git diff ${DIFF_RANGE[*]} не выполнился — заслон не может быть проверен." >&2
  exit 1
fi
# awk вместо `grep '^+' | grep -v '^+++'`: одна команда, всегда код 0,
# нечего маскировать. Заголовки файлов (+++) отбрасываются.
added=$(printf '%s\n' "$diff_out" | awk '/^\+\+\+/ { next } /^\+/ { print }')

fail=0
check() {
  local pattern="$1" msg="$2"
  local hits
  # Herestring, а не пайп: под pipefail пайп с `|| true` на конце скрывает
  # падение источника. grep без -q — досрочного закрытия пайпа нет.
  hits=$(grep -E -- "$pattern" <<<"$added" || true)
  if [ -n "$hits" ]; then
    echo "ЗАПРЕЩЕНО: $msg" >&2
    echo "$hits" >&2
    echo "" >&2
    fail=1
  fi
}

check '#!?\[allow\(' 'новый allow(...) — исправьте причину, а не подавляйте линт'
check '#!?\[expect\(' 'новый expect(...) — то же самое другими словами'
check 'cfg_attr\(.*allow\(' 'подавление линта через cfg_attr'
check '#\[ignore\]' 'новый #[ignore] — отключённый тест не считается тестом'
check '\btodo!\(|\bunimplemented!\(' 'todo!/unimplemented! в коде'
check '#\[cfg\(ignore\)\]' 'отключение кода через cfg(ignore)'

# --- Изменения самих заслонов и их конфигурации ---
# Ослабить проверку можно не только в коде: достаточно снять -D warnings,
# исключить модуль из мутационного тестирования или поправить сам скрипт.
# Пути заданы каталогами: pathspec каталога покрывает всё под ним и не
# зависит от режима globbing. Манифесты крейт сюда не входят намеренно —
# потерю `[lints] workspace = true` ловит scripts/check-architecture.sh.
if ! policy_files=$(git diff --name-only "${DIFF_RANGE[@]}" -- \
  '.github/workflows' 'scripts' 'deny.toml' 'clippy.toml' \
  '.cargo/mutants.toml' 'Cargo.toml' 'tests/fixtures'); then
  echo "ОШИБКА: git diff --name-only не выполнился — заслон не может быть проверен." >&2
  exit 1
fi
if [ -n "$policy_files" ]; then
  echo "ВНИМАНИЕ: изменены файлы политики качества:" >&2
  echo "$policy_files" >&2
  echo "Такие изменения допустимы только с обоснованием в описании бида." >&2
  echo "Пометьте PR меткой 'policy-change', иначе заслон не пропустит." >&2
  if [ "${POLICY_CHANGE_APPROVED:-0}" != "1" ]; then
    fail=1
  fi
fi

if [ "$fail" -ne 0 ]; then
  echo "Если ослабление действительно необходимо — обоснуйте его в описании бида" >&2
  echo "и добавьте исключение в этот скрипт отдельным коммитом." >&2
  exit 1
fi
echo "Diff-lint пройден."
