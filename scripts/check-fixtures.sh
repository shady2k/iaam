#!/usr/bin/env bash
# Замороженные эталоны (§15.7).
# Агенту запрещено править ожидаемое значение, чтобы починить падающий тест.
set -euo pipefail

# Корень — от каталога скрипта, а не от cwd (см. check-architecture.sh).
if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "ФИКСТУРЫ: не удалось определить корень репозитория от $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

FIXTURE_DIR="tests/fixtures"
MANIFEST="$FIXTURE_DIR/MANIFEST.sha256"

if [ ! -f "$MANIFEST" ]; then
  echo "Манифест $MANIFEST отсутствует." >&2
  exit 1
fi

# 1. Содержимое фикстур не изменилось
if ! sha256sum -c "$MANIFEST" --quiet; then
  echo "" >&2
  echo "Замороженная фикстура изменена (§15.7)." >&2
  echo "Ожидаемые значения приходят из независимого источника и не правятся" >&2
  echo "ради зелёного теста. Если изменение обосновано — обновите манифест" >&2
  echo "ОТДЕЛЬНЫМ коммитом с обоснованием и подтверждением владельца:" >&2
  echo "  sha256sum $FIXTURE_DIR/*.json > $MANIFEST" >&2
  exit 1
fi

# Пути из манифеста. Формат sha256sum: <64 hex><пробел><' ' или '*'><путь>.
# Строка, не совпавшая с этим шаблоном, отбрасывается здесь и одновременно
# игнорируется самим sha256sum -c (он лишь печатает WARNING и возвращает 0),
# поэтому её отсутствие среди путей ниже превращается в отказ на шаге 3.
manifest_paths=$(sed -nE 's/^[0-9a-fA-F]{64} [ *](.+)$/\1/p' "$MANIFEST")

if [ -z "$manifest_paths" ]; then
  echo "Манифест $MANIFEST не содержит ни одной корректной строки контрольной суммы." >&2
  exit 1
fi

# 2. Каждая фикстура из манифеста действительно читается тестами
missing=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  name=$(basename -- "$path")
  # grep -q здесь безопасен: это простая команда, а не хвост пайплайна,
  # так что досрочно закрыть нечего. -F — имя файла ищется как текст,
  # чтобы точка и прочие метасимволы regex не расширяли поиск.
  # --include обязан стоять ДО --: после -- он становится операндом-именем
  # файла, фильтр по *.rs молча не применяется (упоминание в README крейты
  # сошло бы за ссылку из теста), а grep возвращает 2 из-за ненайденного
  # «файла» --include=*.rs, и результат зависит от порядка обхода каталога.
  if ! grep -rqF --include='*.rs' -- "$name" crates/; then
    echo "Фикстура $name не упоминается ни в одном тесте — мёртвый эталон." >&2
    missing=1
  fi
done <<<"$manifest_paths"

# 3. В tests/fixtures/ нет файлов мимо манифеста
# Без этой проверки незамороженный эталон — файл, добавленный в каталог, но
# не внесённый в манифест, — проходит заслон: sha256sum -c сверяет только то,
# что перечислено, и молчит обо всём остальном.
unmanifested=$(comm -13 \
  <(printf '%s\n' "$manifest_paths" | LC_ALL=C sort) \
  <(find "$FIXTURE_DIR" -type f ! -name 'MANIFEST.sha256' -print | LC_ALL=C sort))
if [ -n "$unmanifested" ]; then
  echo "Файлы в $FIXTURE_DIR вне манифеста — незамороженные эталоны (§15.7):" >&2
  echo "$unmanifested" >&2
  echo "Внесите их в $MANIFEST или удалите." >&2
  missing=1
fi

if [ "$missing" -ne 0 ]; then
  exit 1
fi

echo "Фикстуры проверены."
