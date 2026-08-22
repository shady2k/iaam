#!/usr/bin/env bash
# Мутационное тестирование с порогом по КАЖДОМУ критичному модулю (§15.7).
# Общий порог по проекту позволяет спрятать непокрытый модуль за хорошо
# покрытыми, поэтому список модулей задан явно и каждый проверяется отдельно.
set -euo pipefail

# Корень ищется от каталога самого скрипта, а не от cwd вызывающего: иначе
# запуск из не-git каталога даёт пустую строку, `cd ""` (в bash это успешный
# no-op) и заслон, проверяющий не тот каталог. Не определили корень — это
# отказ заслона, а не его успех.
if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "МУТАНТЫ: не удалось определить корень репозитория от $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

# Критичные модули. Список растёт вместе с ядром; удаление строки отсюда
# ловится заслоном политики в check-diff-lint.sh (каталог scripts/ входит
# в его список файлов политики).
MODULES=(
  "crates/iaam-core/src/numeric/exact.rs"
  "crates/iaam-core/src/money.rs"
  "crates/iaam-core/src/dates.rs"
  "crates/iaam-core/src/event/kind.rs"
  "crates/iaam-core/src/event/mod.rs"
  "crates/iaam-core/src/event/correction.rs"
  "crates/iaam-core/src/contour.rs"
  "crates/iaam-core/src/rules/lot_disposal.rs"
  # Арифметика Dec — числовое основание всех денежных расчётов.
  # В первой редакции плана её в списке не было: расширено при
  # исполнении задачи 9 (iaam-1fk.22).
  "crates/iaam-core/src/numeric/decimal.rs"
  "crates/iaam-core/src/projection/balances.rs"
  "crates/iaam-core/src/projection/lots.rs"
  "crates/iaam-core/src/projection/flows.rs"
  "crates/iaam-core/src/projection/invariants.rs"
  "crates/iaam-core/src/projection/state.rs"
  "crates/iaam-core/src/projection/mod.rs"
  "crates/iaam-core/src/numeric/xirr.rs"
  "crates/iaam-core/src/returns/xirr.rs"
  # Контракт отчёта: именно здесь решается, доверять ли цифре.
  "crates/iaam-core/src/returns/mod.rs"
  "crates/iaam-core/src/valuation.rs"
  # Хранилище держит границу владельца и append-only журнала: это
  # свойства безопасности, а не удобства. План их в заслон не вносил;
  # добавлено при исполнении задачи 11, где прогон дал десять выживших.
  "crates/iaam-store/src/events.rs"
  "crates/iaam-store/src/snapshots.rs"
  "crates/iaam-store/src/reference.rs"
  "crates/iaam-store/src/tokens.rs"
  # Приёмка строит знаки и ноги события: ошибка здесь записывает
  # в append-only журнал факт, которого не было.
  "crates/iaam-ingest/src/operation.rs"
  "crates/iaam-ingest/src/csv_source.rs"
  "crates/iaam-ingest/src/verdict.rs"
  # Эталон мутируется наравне с продакшеном: ошибка в эталоне маскирует
  # ошибку в продакшене ровно так же, как наоборот (§15.4).
  "crates/iaam-oracle/src/lots_reference.rs"
)

# Пустой список — это заслон, который проходит всегда. Опустошение массива
# должно быть отказом, а не «проверено ноль модулей, нарушений нет».
if [ "${#MODULES[@]}" -eq 0 ]; then
  echo "МУТАНТЫ: список критичных модулей пуст — заслон не проверяет ничего." >&2
  exit 1
fi

# Инструменты проверяются заранее: `command not found` посреди пайпа читается
# хуже, чем явное сообщение, а под `|| true` вообще прошёл бы как успех.
for tool in cargo jq; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "МУТАНТЫ: $tool недоступен — заслон не может быть проверен." >&2
    exit 1
  fi
done
if ! cargo mutants --version >/dev/null 2>&1; then
  echo "МУТАНТЫ: cargo-mutants недоступен — заслон не может быть проверен." >&2
  exit 1
fi

ERR_FILE=$(mktemp)
trap 'rm -f "$ERR_FILE"' EXIT

# cargo metadata читается ОДИН раз: вызов в цикле — это N шансов, что один
# из них молча упадёт. Падение самого cargo metadata — отказ заслона.
if ! META=$(cargo metadata --no-deps --format-version 1 2>"$ERR_FILE"); then
  echo "МУТАНТЫ: cargo metadata не выполнился — заслон не может быть проверен." >&2
  cat "$ERR_FILE" >&2
  exit 1
fi

# Имя пакета берётся из cargo metadata по манифесту крейты, а не из имени
# каталога: они совпадают по соглашению, но заслон не должен держаться
# на соглашении. Не нашли пакет — отказ, а не пропуск.
package_of() {
  local module="$1" crate_dir manifest name
  crate_dir=$(printf '%s\n' "$module" | cut -d/ -f1-2)
  manifest="$REPO_ROOT/$crate_dir/Cargo.toml"
  name=$(printf '%s' "$META" | jq -r --arg m "$manifest" \
    '.packages[] | select(.manifest_path == $m) | .name')
  printf '%s' "$name"
}

# Число строк в выводе `--list`. Пустая строка — это ноль мутантов, а не один:
# `printf '%s\n' "" | wc -l` вернул бы 1 и спрятал пустой список.
count_lines() {
  if [ -z "$1" ]; then
    printf '0'
  else
    printf '%s\n' "$1" | wc -l | tr -d ' '
  fi
}

# `cargo mutants --list` для модуля. Код возврата проверяется явно: `|| true`
# на пайплайне превратил бы падение инструмента в «мутантов нет».
list_mutants() {
  local package="$1" module="$2"
  shift 2
  local out
  if ! out=$(cargo mutants --list --package "$package" --file "$module" "$@" 2>"$ERR_FILE"); then
    echo "МУТАНТЫ: cargo mutants --list не выполнился для $module" >&2
    cat "$ERR_FILE" >&2
    return 1
  fi
  printf '%s' "$out"
}

fail=0
checked=0
skipped=0
inert=0

for module in "${MODULES[@]}"; do
  if [ ! -f "$module" ]; then
    echo "пропуск (ещё не создан): $module"
    skipped=$((skipped + 1))
    continue
  fi

  echo "=== $module ==="

  package=$(package_of "$module")
  if [ -z "$package" ]; then
    echo "  ОТКАЗ: не удалось определить пакет для $module по cargo metadata" >&2
    fail=1
    continue
  fi

  # --- Заслон против «настроенного, но не работающего» заслона ---
  # `cargo mutants` завершается кодом 0, когда мутантов НОЛЬ: и когда файл
  # исключён через exclude_globs/exclude_re в .cargo/mutants.toml, и когда
  # путь в списке модулей содержит опечатку. Проверено исполнением на
  # cargo-mutants 27.1.0: «Found 0 mutants to test», код возврата 0.
  # Без этой проверки помодульный прогон печатал бы «выживших нет» для
  # модуля, который вообще не тестировался — то есть исключение доменного
  # модуля из конфигурации выглядело бы как пройденный заслон.
  #
  # Различаем две причины пустого списка сравнением с --no-config:
  #   конфиг подавляет мутантов -> отказ, домен прятать нельзя;
  #   мутантов нет и без конфига -> в файле нет мутируемого кода.
  if ! with_config=$(list_mutants "$package" "$module"); then
    fail=1
    continue
  fi
  if ! without_config=$(list_mutants "$package" "$module" --no-config); then
    fail=1
    continue
  fi
  n_with=$(count_lines "$with_config")
  n_without=$(count_lines "$without_config")

  if [ "$n_with" -eq 0 ] && [ "$n_without" -gt 0 ]; then
    echo "  ОТКАЗ: конфигурация подавляет мутантов в $module" >&2
    echo "  без конфигурации мутантов: $n_without, с конфигурацией: 0." >&2
    echo "  Исключение доменного модуля из мутационного тестирования — способ" >&2
    echo "  спрятать подложные тесты. Уберите модуль из .cargo/mutants.toml." >&2
    fail=1
    continue
  fi

  if [ "$n_with" -eq 0 ]; then
    # Файл существует и не подавлен, но мутируемого кода в нём нет
    # (например, одни объявления типов). Молчать нельзя: со стороны это
    # неотличимо от пройденной проверки.
    echo "  БЕЗ МУТАНТОВ: в $module нет мутируемого кода — проверять нечего."
    inert=$((inert + 1))
    continue
  fi

  echo "  мутантов к проверке: $n_with"
  out_dir="target/mutants/$(printf '%s' "$module" | tr '/' '_')"
  # `--output DIR` не создаёт промежуточные каталоги: без mkdir прогон падает
  # с «create output parent directory», а по коду возврата это неотличимо
  # от выживших мутантов.
  rm -rf "$out_dir"
  mkdir -p "$out_dir"

  # `--output DIR` создаёт mutants.out ВНУТРИ DIR — отчёт лежит
  # в "$out_dir/mutants.out/", а не в "$out_dir/".
  report="$out_dir/mutants.out"

  if cargo mutants --package "$package" --file "$module" --output "$out_dir"; then
    echo "  выживших нет ($n_with мутантов убито)"
    checked=$((checked + 1))
    continue
  fi

  fail=1

  # Ненулевой код возврата — не обязательно выжившие мутанты: так же
  # завершаются сбой сборки, таймаут и нежизнеспособные мутанты. Причина
  # берётся из отчёта, а не угадывается по коду возврата. Нет отчёта —
  # это сбой самого прогона, и называть его «выжившими» нельзя.
  if [ ! -f "$report/outcomes.json" ]; then
    echo "  ОТКАЗ: прогон $module завершился с ошибкой и не оставил отчёта" >&2
    echo "  ($report/outcomes.json отсутствует). Это сбой инструмента," >&2
    echo "  а не результат проверки." >&2
    continue
  fi

  if ! counters=$(jq -r '[.missed, .timeout, .unviable, .total_mutants] | @tsv' \
      "$report/outcomes.json" 2>"$ERR_FILE"); then
    echo "  ОТКАЗ: не удалось разобрать $report/outcomes.json" >&2
    cat "$ERR_FILE" >&2
    continue
  fi
  IFS=$'\t' read -r n_missed n_timeout n_unviable n_total <<<"$counters"
  echo "  всего: $n_total, выжило: $n_missed, таймаут: $n_timeout, нежизнеспособных: $n_unviable" >&2

  if [ "${n_missed:-0}" -gt 0 ]; then
    echo "  ВЫЖИВШИЕ МУТАНТЫ в $module:" >&2
    jq -r '.outcomes[] | select(.summary=="MissedMutant") | "    " + .scenario.Mutant.name' \
      "$report/outcomes.json" >&2
  else
    echo "  Прогон не прошёл без выживших мутантов — смотрите $report/" >&2
  fi
done

echo ""
echo "Модулей: проверено $checked, без мутируемого кода $inert, пропущено (не создано) $skipped."

if [ "$fail" -ne 0 ]; then
  echo "" >&2
  echo "Выживший мутант означает, что какой-то тест ничего не проверяет." >&2
  echo "Объявить мутанта эквивалентным можно только с письменным" >&2
  echo "обоснованием в описании бида (§15.7)." >&2
  exit 1
fi
echo "Мутационное тестирование пройдено по всем существующим модулям."
