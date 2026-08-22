#!/usr/bin/env bash
# Архитектурные заслоны (§3.1, §3.2 спецификации).
# Проверяет то, что компилятор не проверяет сам.
set -euo pipefail

# Заслоны работают из корня репозитория независимо от того, откуда вызваны.
# Корень ищется от каталога самого скрипта, а не от cwd вызывающего: иначе
# запуск из не-git каталога даёт пустую строку, `cd ""` и заслон, проверяющий
# не тот каталог. Не определили корень — это отказ заслона, а не успех.
if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "АРХИТЕКТУРА: не удалось определить корень репозитория от $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

fail=0
err() { echo "АРХИТЕКТУРА: $*" >&2; fail=1; }

CORE_SRC="crates/iaam-core/src"

# Отбрасывает строки, содержимое которых является комментарием.
# Без этого заслон падает на doc-комментарии, объясняющем сам запрет:
# в шапке ядра написано «ни `async`, ни `Mutex`» — это верный код, а не нарушение.
# На вход подаётся вывод `grep -rn`, то есть «путь:номер:тело».
strip_comments() {
  awk '{
    body = $0
    sub(/^[^:]*:[0-9]+:/, "", body)
    if (body !~ /^[[:space:]]*(\/\/|\*\/|\*|\/\*)/) print
  }'
}

# cargo metadata читается ОДИН раз: четыре вызова в цикле заслона — это
# четыре шанса, что один из них молча упадёт и заслон пропустит нарушение.
# Падение самого cargo metadata — это отказ заслона, а не его успех.
meta_err=$(mktemp)
trap 'rm -f "$meta_err"' EXIT
if ! META=$(cargo metadata --no-deps --format-version 1 2>"$meta_err"); then
  echo "АРХИТЕКТУРА: cargo metadata не выполнился — заслон не может быть проверен" >&2
  cat "$meta_err" >&2
  exit 1
fi
meta() { printf '%s' "$META"; }

# --- 1. iaam-core не зависит ни от одной крейты воркспейса ---
core_deps=$(meta \
  | jq -r '.packages[] | select(.name=="iaam-core") | .dependencies[].name' \
  | { grep '^iaam-' || true; })
if [ -n "$core_deps" ]; then
  err "iaam-core зависит от крейт воркспейса: $core_deps (§3.2)"
fi

# --- 2. Библиотека iaam-server не зависит от адаптеров ---
# Точка сборки живёт в отдельной крейте iaam-bootstrap: собрать конкретные
# адаптеры где-то нужно, но это не повод давать транспорту знать про SQLite.
bad=$(meta \
  | jq -r '.packages[] | select(.name=="iaam-server") | .dependencies[]
           | select(.kind == null) | .name' \
  | { grep -E '^iaam-(store|market|ingest)$' || true; })
if [ -n "$bad" ]; then
  err "iaam-server зависит от адаптеров: $bad — их место в iaam-bootstrap (§3.2)"
fi

# --- 2a. Адаптер знает только ядро ---
# iaam-store — адаптер хранилища. Он переводит доменные типы в строки
# базы и обратно, и потому обязан знать ядро — но ни приложение, ни
# транспорт, ни другой адаптер. Зависимость в обратную сторону превратила
# бы слои в клубок, и «оболочка не считает» перестало бы быть проверяемым:
# считать начал бы адаптер.
bad=$(meta \
  | jq -r '.packages[] | select(.name=="iaam-store") | .dependencies[]
           | select(.kind == null) | .name' \
  | { grep -E '^iaam-(app|server|bootstrap|ingest|market)$' || true; })
if [ -n "$bad" ]; then
  err "iaam-store зависит от вышележащих слоёв: $bad (§3.2)"
fi

# --- 3. Никаких shared/common/utils крейт ---
for forbidden in shared common utils; do
  if [ -d "crates/iaam-$forbidden" ]; then
    err "крейта iaam-$forbidden запрещена (§3.2)"
  fi
done

# --- 4. Эталон не попадает в продакшн-зависимости ---
# grep -q здесь нельзя: он закрывает пайп, jq умирает по SIGPIPE, и при
# pipefail код пайплайна становится ненулевым — то есть настоящее нарушение
# читалось бы как «проверка пройдена». Ловим текстом, а не кодом возврата.
oracle_leak=$(meta \
  | jq -r '.packages[] | select(.name!="iaam-oracle") | .dependencies[]
           | select(.kind == null or .kind == "build") | .name' \
  | { grep -x 'iaam-oracle' || true; })
if [ -n "$oracle_leak" ]; then
  err "iaam-oracle попал в продакшн- или build-зависимости (§15.4)"
fi

# --- 5. Двоичная плавающая точка в ядре только в объявленных файлах ---
# Приближённый режим (§6.6) живёт в двух файлах и только в них: политика
# и результат с границей погрешности (approx.rs) и сам решатель ставки
# (xirr.rs). Список задан поимённо, а не маской каталога: маска позволила бы
# завести третий файл с плавающей точкой незаметно.
APPROX_FILES=(
  "numeric/approx.rs"
  "numeric/xirr.rs"
)
if [ -d "$CORE_SRC" ]; then
  hits=$(grep -rn '\bf64\b\|\bf32\b' "$CORE_SRC" --include='*.rs' || true)
  for allowed in "${APPROX_FILES[@]}"; do
    hits=$(printf '%s' "$hits" | { grep -v "^${CORE_SRC}/${allowed}:" || true; })
  done
  hits=$(printf '%s' "$hits" | strip_comments || true)
  if [ -n "$hits" ]; then
    err "двоичная плавающая точка вне приближённого режима (§6.6):"
    echo "$hits" >&2
  fi
fi

# --- 6. Ядро синхронно и без разделяемого состояния ---
# Ищем конструкции кода, а не слова: Mutex< и RwLock< с угловой скобкой,
# async fn с ключевым словом. Комментарии отброшены выше.
if [ -d "$CORE_SRC" ]; then
  hits=$(grep -rn 'async fn\|\bMutex<\|\bRwLock<\|tokio::' "$CORE_SRC" --include='*.rs' \
    | strip_comments || true)
  if [ -n "$hits" ]; then
    err "async / Mutex / RwLock / tokio в ядре (§3.1):"
    echo "$hits" >&2
  fi
fi

# --- 7. Каждая крейта наследует линты воркспейса ---
# unsafe запрещён таблицей [workspace.lints.rust], но она применяется
# к крейте только при [lints] workspace = true. Крейта без этой строки
# молча выпадает из-под запрета, и ничто об этом не сообщает.
for manifest in crates/*/Cargo.toml; do
  [ -f "$manifest" ] || continue
  if ! awk '
      /^[[:space:]]*\[lints\]/            { in_lints = 1; next }
      /^[[:space:]]*\[/                   { in_lints = 0 }
      in_lints && /^[[:space:]]*workspace[[:space:]]*=[[:space:]]*true/ { found = 1 }
      END                                 { exit !found }
    ' "$manifest"; then
    err "$manifest не наследует линты воркспейса: нужна секция [lints] с workspace = true (§15.1)"
  fi
done

# --- 8. Приближённый режим не разрастается в теневой расчётный слой ---
# Исключение файла из заслона №5 опасно: в нём можно разместить денежную
# арифметику. Ограничение размера делает это заметным. Порог у каждого файла
# свой: решатель со сканированием диапазона и оценкой погрешности объективно
# длиннее объявления политики. Считаются ВСЕ строки файла, включая тесты, —
# так же, как считались для approx.rs; порог задан с учётом этого.
APPROX_LIMITS=(
  "numeric/approx.rs:200"
  "numeric/xirr.rs:420"
)
for entry in "${APPROX_LIMITS[@]}"; do
  file="$CORE_SRC/${entry%%:*}"
  limit="${entry##*:}"
  [ -f "$file" ] || continue
  lines=$(wc -l < "$file")
  if [ "$lines" -gt "$limit" ]; then
    err "$file разросся до $lines строк при пороге $limit."
    err "Приближённый режим должен оставаться тонким (§6.6)."
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "" >&2
  echo "Архитектурные заслоны не пройдены. Правьте код, а не заслон." >&2
  exit 1
fi
echo "Архитектурные заслоны пройдены."
