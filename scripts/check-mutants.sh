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
  # Правило выбора цены решает, КАКОЕ число покажет отчёт: мутант,
  # сдвинувший границу полосы возраста или порядок критериев, не ломает
  # ни одной суммы — он молча меняет цену позиции на другую, столь же
  # правдоподобную. Добавлено при исполнении E3.3 (iaam-d8b.1).
  "crates/iaam-core/src/rules/valuation.rs"
  # Граница legacy: мутант, вернувший кандидата вместо LegacyDerived,
  # отмывает старый перенос цены как свежее наблюдение. По цифрам это
  # неотличимо от честного пересчёта (решение 0002).
  "crates/iaam-core/src/valuation/candidate.rs"
  # Полуинтервал действия псевдонима решает, какой инструмент стоит за
  # внешним кодом на дату. Ошибка здесь не искажает ни одной суммы —
  # она подменяет бумагу, и заметить это по самим цифрам невозможно.
  # Мутант, меняющий `<` на `<=` в `AliasInterval::covers`, делает день
  # смены ISIN принадлежащим сразу двум выпускам. Добавлено при
  # исполнении E3.1 (iaam-30v) с разрешения владельца.
  "crates/iaam-core/src/instrument.rs"
  # Сверка решает, можно ли верить цифре. Ошибка здесь не искажает
  # ни одной суммы — она объявляет непроверенные данные проверенными,
  # и заметить это по самим цифрам невозможно (§10.3). Добавлено
  # планом E2, задача 9.
  "crates/iaam-core/src/reconciliation/claim.rs"
  "crates/iaam-core/src/reconciliation/observed.rs"
  "crates/iaam-core/src/reconciliation/check.rs"
  "crates/iaam-core/src/reconciliation/evidence.rs"
  "crates/iaam-core/src/reconciliation/mod.rs"
  # Периметр решает, где система отказывается считать (§11). Мутант,
  # снимающий отказ, выдаёт экономику неподдерживаемого финансирования
  # за посчитанную.
  "crates/iaam-core/src/perimeter.rs"
  # Хранилище держит границу владельца и append-only журнала: это
  # свойства безопасности, а не удобства. План их в заслон не вносил;
  # добавлено при исполнении задачи 11, где прогон дал десять выживших.
  "crates/iaam-store/src/events.rs"
  "crates/iaam-store/src/snapshots.rs"
  "crates/iaam-store/src/reference.rs"
  "crates/iaam-store/src/tokens.rs"
  "crates/iaam-store/src/bundle.rs"
  # Приёмка строит знаки и ноги события: ошибка здесь записывает
  # в append-only журнал факт, которого не было.
  "crates/iaam-ingest/src/operation.rs"
  "crates/iaam-ingest/src/csv_source.rs"
  "crates/iaam-ingest/src/verdict.rs"
  # Приложение и транспорт: здесь живут область действия токена,
  # ограничитель частоты и нумерация вердиктов.
  #
  # Два файла оболочки в список НЕ входят, и это решение, а не упущение
  # (§15.7 требует письменного обоснования — вот оно; полный разбор —
  # в описании бида iaam-1fk.18):
  #
  #   adapters/sqlite.rs — чтение снимка `load_snapshot` заменяется на
  #   «снимка нет» без единого наблюдаемого следствия. Так и задумано:
  #   снимок является кэшем, и тождество «advance равен полному
  #   пересчёту» — центральный инвариант проекции. Мутант меняет объём
  #   работы, а не ответ. Остальные методы файла — делегирование в
  #   iaam-store (он в списке выше) и покрыты контрактными тестами.
  #
  #   scenarios/reports.rs — условие «нарушение инварианта не повод
  #   пересчитывать» заменяется на «пересчитывать всегда»: полный
  #   пересчёт даёт ровно то же нарушение и тот же ответ. Это тоже
  #   объём работы, а не ответ. Сами предикаты (snapshot_may_be_saved,
  #   recompute_is_worth_it) вынесены отдельными функциями и проверяются
  #   модульными тестами напрямую.
  "crates/iaam-app/src/ports.rs"
  "crates/iaam-app/src/error.rs"
  "crates/iaam-app/src/scenarios/ingest.rs"
  "crates/iaam-server/src/routes.rs"
  "crates/iaam-server/src/auth.rs"
  "crates/iaam-server/src/rate_limit.rs"
  "crates/iaam-server/src/dto.rs"
  # Эталон мутируется наравне с продакшеном: ошибка в эталоне маскирует
  # ошибку в продакшене ровно так же, как наоборот (§15.4).
  "crates/iaam-oracle/src/lots_reference.rs"
  # Исходящий HTTP. Добавлено при исполнении E3.2 части 1 (iaam-faf)
  # с разрешения владельца. Первый же прогон дал 13 выживших из 57,
  # и четыре из них были не косметикой:
  #
  #   client_for -> Ok(Default::default()) выживал — то есть подмена всей
  #   сборки клиента на клиент по умолчанию, без вшитого корня и без
  #   tls_certs_only, не ловилась ничем. Проверялась таблица якорей,
  #   то есть НАМЕРЕНИЕ, но не то, что якорь применён.
  #
  #   Secret::expose -> "" и HttpRequest::bearer -> None выживали:
  #   ничто не проверяло, что токен доезжает до запроса. Пустой токен
  #   дал бы отказ авторизации, неотличимый от отказа шлюза.
  #
  #   Debug для Secret -> Ok(Default::default()) выживал: тест проверял
  #   только отрицание («секрета в выводе нет»), и пустой вывод его
  #   устраивал. Тот же класс уже ловили на IssuedToken.
  #
  # destination.rs в списке потому, что в нём лежат адреса, и ошибка
  # в них не искажает ни одной суммы — она уводит запрос в другой узел
  # или другую среду, откуда приходит правдоподобный ответ. Заметить
  # такое по самим цифрам невозможно. Мутанты здесь скромнее самой
  # опасности: cargo-mutants подменяет возврат base_url на пустую строку
  # или мусор, но не переставляет ветки match местами. Заслон ловит
  # «адреса вообще не проверяются», а не «бой перепутан с песочницей»;
  # второе стережёт тест the_sandbox_is_a_different_host_not_a_different_path.
  "crates/iaam-http/src/trust.rs"
  "crates/iaam-http/src/destination.rs"
  "crates/iaam-http/src/request.rs"
  "crates/iaam-http/src/response.rs"
  "crates/iaam-http/src/resilience.rs"
  "crates/iaam-http/src/client.rs"
  # Разбор ответов источников. Добавлено при исполнении E3.2 части 2
  # (iaam-tv2) с разрешения владельца. Первый прогон дал 16 выживших
  # из 139, и все шестнадцать сидели в модуле ЦБ:
  #
  #   parse_daily -> Ok(vec![]) выживал, потому что тест утверждал
  #   «сырых записей больше, чем наблюдений» — а это верно и когда
  #   наблюдений НОЛЬ. Тест, охранявший пропуск незнакомых валют,
  #   проходил в случае, когда пропущены все. Тот же класс, что описан
  #   в ADR-0002: проверка только отрицания.
  #
  #   currency_from_iso -> None и удаление каждой ветки отображения
  #   выживали: ничто не проверяло, что USD, EUR, CNY разрешаются.
  #
  #   dotted -> String::new() выживал: формат даты в запросе к ЦБ
  #   не утверждался ничем. Пустая строка даёт не ошибку, а ответ
  #   за другой период — то есть тихо неверные данные.
  #
  # Ошибка разбора здесь не искажает сумму, а подменяет наблюдение,
  # на котором сумма потом считается. Заметить это по самим цифрам
  # невозможно — ровно та же причина, по которой в списке лежат
  # instrument.rs и reconciliation.
  "crates/iaam-market/src/cbr/fx.rs"
  "crates/iaam-market/src/cbr/key_rate.rs"
  "crates/iaam-market/src/cbr/mod.rs"
  "crates/iaam-market/src/moex/parse.rs"
  "crates/iaam-market/src/moex/mod.rs"
  # Граница полноты решает, выдаётся ли частичная выгрузка за полную
  # (iaam-023.5). Прогон дал шесть выживших: счётчик записанных строк
  # никем не проверялся, а две замены `||` на `&&` в условии аренды
  # разрешали двум запускам писать в одну серию вперемешку и обоим
  # двигать границу. Ни один из шести не меняет ни единого числа —
  # они меняют то, чему это число соответствует.
  "crates/iaam-store/src/market.rs"
  # Новые диспетчеры событий и амортизации. Добавлено при исполнении
  # E3.4.1 (iaam-8mv) с разрешения владельца; прогон дал 64 мутанта,
  # выживших нет. legs.rs решает, принимается ли событие с посторонним
  # движением; amortisation.rs решает, сколько стоимости возвращается
  # вместе с номиналом; offers.rs держит инвариант цепочки заявки.
  # Ни один мутант здесь не меняет суммы напрямую — они меняют то, чему
  # сумма соответствует. Тот же класс, что instrument.rs и reconciliation.
  "crates/iaam-core/src/event/corporate_action.rs"
  "crates/iaam-core/src/event/offer.rs"
  "crates/iaam-core/src/event/legs.rs"
  "crates/iaam-core/src/rules/amortisation.rs"
  # Правило НКД и выводы по графику. Добавлено при исполнении E3.4.4
  # (iaam-pa0m). accrued_interest.rs держит полуоткрытость границы
  # периода и стратегию округления: обе меняют сумму при одинаковом
  # inputs_hash, и мутант, сдвинувший `<` в `<=`, выдал бы на конце
  # периода целый купон вместо нуля. finality.rs решает, возвращён ли
  # номинал целиком; posting.rs решает, по какому полю берётся дата
  # выплаты — payment_date против accrual_end. Ни один из этих мутантов
  # не меняет числа напрямую, они меняют то, чему число соответствует.
  "crates/iaam-core/src/rules/accrued_interest.rs"
  "crates/iaam-core/src/bond/finality.rs"
  "crates/iaam-core/src/bond/posting.rs"
  "crates/iaam-core/src/projection/offers.rs"
  # График выплат (E3.4 часть 2). Ошибка в инвариантах полноты не меняет
  # ни одной суммы — она меняет то, что система считает полным графиком,
  # и усечённый ряд молча укорачивает W_T.
  "crates/iaam-market/src/schedule/completeness.rs"
  "crates/iaam-market/src/moex/bondization.rs"
  "crates/iaam-app/src/scenarios/schedule.rs"
  # observation.rs в список НЕ входит намеренно: там одни объявления
  # типов, мутировать в них нечего, и `cargo mutants --list` даёт ноль.
  # Заслон такой модуль объявляет отказом — и правильно: ноль мутантов
  # неотличим от опечатки в пути. Гарантию «оси времени не перепутать»
  # даёт компилятор, а не мутационный прогон.
  #
  # rules/quotation.rs в список НЕ входит намеренно (§15.7 требует
  # письменного обоснования — вот оно; разбор в биде iaam-rjvb).
  # `cargo mutants --list` даёт три мутанта, и все три нежизнеспособны:
  # money_per_unit возвращает Result<(Dec, CurrencyCode), QuotationError>,
  # а подставить замену нечем. Default не реализуют ни Dec, ни
  # CurrencyCode — у валюты намеренно, валюты по умолчанию не бывает;
  # Err(NumericError::Overflow) и Err(MoneyError::Overflow) не являются
  # QuotationError, конверсии нет. Трёхветочный match по QuotationBasis
  # мутационным заслоном не покрывается вовсе, и строка в списке сделала
  # бы заслон либо красным (ноль мутантов — отказ), либо ложно-зелёным.
  # Ветки закреплены шестью модульными тестами в самом файле и свойством
  # линейности стоимости по остаточному номиналу (tests/properties.rs).
  "crates/iaam-core/src/rules/cashflow.rs"
  "crates/iaam-core/src/returns/zero_reinvestment.rs"
  "crates/iaam-core/src/bond/offer.rs"
)

# Пустой список — это заслон, который проходит всегда. Опустошение массива
# должно быть отказом, а не «проверено ноль модулей, нарушений нет».
if [ "${#MODULES[@]}" -eq 0 ]; then
  echo "МУТАНТЫ: список критичных модулей пуст — заслон не проверяет ничего." >&2
  exit 1
fi

# Прогон однопоточный, и это решение, а не умолчание. Параллельные
# потоки cargo-mutants не делят каталог сборки: каждый поток заводит
# свой временный каталог и заново компилирует зависимости пакета —
# от 25 с на iaam-core до 134 с на iaam-store. Замер на 20 мутантах
# iaam-core при трёх потоках: 5 мин против 2,7 мин пофайлового прогона,
# то есть тройная холодная пересборка съедает выигрыш целиком. На шести
# ядрах с 12 ГБ памяти, из которых 3 ГБ уже в свопе, добавлять потоки
# нечем. Ускорение ищется в числе проверок, а не в их одновременности.

# Инструменты проверяются заранее: `command not found` посреди пайпа читается
# хуже, чем явное сообщение, а под `|| true` вообще прошёл бы как успех.
for tool in cargo jq awk; do
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

# Сколько строк вывода `--list` относится к файлу. Сравнение по префиксу
# через `index`, а не grep по регулярке: точка в пути под регуляркой
# совпадает с любым символом, и счётчики поехали бы между соседними
# модулями. Пустой вывод — это ноль строк, а не одна: `printf '%s\n' ""`
# дал бы `wc -l` = 1 и спрятал пустой список.
count_for_file() {
  local listing="$1" file="$2"
  if [ -z "$listing" ]; then
    printf '0'
    return
  fi
  printf '%s\n' "$listing" |
    awk -v prefix="$file:" 'index($0, prefix) == 1' | wc -l | tr -d ' '
}

# `cargo mutants --list` для пакета сразу по всем его модулям. Код возврата
# проверяется явно: `|| true` на пайплайне превратил бы падение инструмента
# в «мутантов нет».
list_mutants() {
  local package="$1"
  shift
  local out
  if ! out=$(cargo mutants --list --package "$package" "$@" 2>"$ERR_FILE"); then
    echo "МУТАНТЫ: cargo mutants --list не выполнился для пакета $package" >&2
    cat "$ERR_FILE" >&2
    return 1
  fi
  printf '%s' "$out"
}

fail=0
checked=0
skipped=0
inert=0

# Модули группируются по пакетам, и `cargo mutants` вызывается ОДИН раз
# на пакет со всеми его файлами сразу.
#
# Раньше вызов был пофайловым, и каждый из них заново собирал baseline:
# по логу полного прогона от 25 с на iaam-core до 134 с на iaam-store,
# то есть больше часа чистой пересборки на 66 модулей. Отвечает заслон
# по-прежнему за модуль: список, порог и разбор выживших ниже идут
# пофайлово, файл каждого мутанта берётся из outcomes.json. Дешевеет
# исполнение заслона, а не его строгость.
declare -A PACKAGE_FILES=()
PACKAGES=()

for module in "${MODULES[@]}"; do
  if [ ! -f "$module" ]; then
    echo "пропуск (ещё не создан): $module"
    skipped=$((skipped + 1))
    continue
  fi

  package=$(package_of "$module")
  if [ -z "$package" ]; then
    echo "  ОТКАЗ: не удалось определить пакет для $module по cargo metadata" >&2
    fail=1
    continue
  fi

  if [ -z "${PACKAGE_FILES[$package]+присутствует}" ]; then
    PACKAGES+=("$package")
    PACKAGE_FILES[$package]="$module"
  else
    PACKAGE_FILES[$package]+=$'\n'"$module"
  fi
done

# Значения ошибок для мутантов из функций, возвращающих Result. Раньше
# лежали в .cargo/mutants.toml и применялись ко ВСЕМ пакетам, хотя
# `crate::numeric::NumericError` и `crate::money::MoneyError` объявлены
# только в iaam-core: в любом другом пакете такой мутант не собирается
# никогда, но полную сборку ради этого вывода оплачивает. По отчётам
# прогона в iaam-store нежизнеспособны все 70 из 70 таких мутантов —
# половина мутантов пакета. В iaam-core из 206 таких мутантов 31 убит
# тестами, поэтому здесь они остаются.
#
# Флаги идут и в `--list`, и в прогон: разойдись они, объявленное
# «мутантов к проверке» перестало бы совпадать с проверенным.
error_values_for() {
  case "$1" in
    iaam-core)
      printf '%s\n' \
        --error 'crate::numeric::NumericError::Overflow' \
        --error 'crate::money::MoneyError::Overflow'
      ;;
  esac
}

for package in "${PACKAGES[@]}"; do
  mapfile -t files <<<"${PACKAGE_FILES[$package]}"
  echo "=== пакет $package: модулей ${#files[@]} ==="

  mapfile -t error_args < <(error_values_for "$package")

  list_args=()
  for file in "${files[@]}"; do
    list_args+=(--file "$file")
  done

  # --- Заслон против «настроенного, но не работающего» заслона ---
  # `cargo mutants` завершается кодом 0, когда мутантов НОЛЬ: и когда файл
  # исключён через exclude_globs/exclude_re в .cargo/mutants.toml, и когда
  # путь в списке модулей содержит опечатку. Проверено исполнением на
  # cargo-mutants 27.1.0: «Found 0 mutants to test», код возврата 0.
  # Без этой проверки прогон печатал бы «выживших нет» для модуля,
  # который вообще не тестировался — то есть исключение доменного модуля
  # из конфигурации выглядело бы как пройденный заслон.
  #
  # Различаем две причины пустого списка сравнением с --no-config:
  #   конфиг подавляет мутантов -> отказ, домен прятать нельзя;
  #   мутантов нет и без конфига -> в файле нет мутируемого кода.
  if ! with_config=$(list_mutants "$package" "${list_args[@]}" "${error_args[@]}"); then
    fail=1
    continue
  fi
  if ! without_config=$(list_mutants "$package" "${list_args[@]}" "${error_args[@]}" --no-config); then
    fail=1
    continue
  fi

  run_files=()
  run_args=()
  for file in "${files[@]}"; do
    n_with=$(count_for_file "$with_config" "$file")
    n_without=$(count_for_file "$without_config" "$file")

    if [ "$n_with" -eq 0 ] && [ "$n_without" -gt 0 ]; then
      echo "  ОТКАЗ: конфигурация подавляет мутантов в $file" >&2
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
      echo "  БЕЗ МУТАНТОВ: в $file нет мутируемого кода — проверять нечего."
      inert=$((inert + 1))
      continue
    fi

    echo "  $file: мутантов к проверке $n_with"
    run_files+=("$file")
    run_args+=(--file "$file")
  done

  if [ "${#run_files[@]}" -eq 0 ]; then
    continue
  fi

  # Какими тестами проверять пакет.
  #
  # По умолчанию cargo-mutants при `--package X` гоняет тесты ТОЛЬКО
  # пакета X. Для большинства модулей это верно: их тесты лежат рядом.
  # Но сценарии приложения проверяются контрактными тестами, которые
  # живут в iaam-server, и без явного указания заслон печатал бы
  # «выживших нет» для кода, который никто не тестировал. Проверено
  # исполнением: 46 выживших против 35 на одном и том же коде.
  #
  # Указываются именно нужные пакеты, а не `--test-workspace true`:
  # прогон всего набора тестов на каждого мутанта поднимает цену
  # с полутора секунд до тринадцати, то есть примерно в девять раз.
  extra_test_packages=()
  case "$package" in
    iaam-app)
      extra_test_packages=(--test-package iaam-app --test-package iaam-server)
      ;;
  esac

  out_dir="target/mutants/$package"
  # `--output DIR` не создаёт промежуточные каталоги: без mkdir прогон падает
  # с «create output parent directory», а по коду возврата это неотличимо
  # от выживших мутантов.
  rm -rf "$out_dir"
  mkdir -p "$out_dir"

  # `--output DIR` создаёт mutants.out ВНУТРИ DIR — отчёт лежит
  # в "$out_dir/mutants.out/", а не в "$out_dir/".
  report="$out_dir/mutants.out"

  # `--profile mutant` — отдельный профиль с урезанной отладочной
  # информацией (обоснование в Cargo.toml). Мутант — это линковка всех
  # тестовых целей пакета заново, а при `debug = 2` три четверти
  # тестового бинарника составляет отладочная информация.
  if cargo mutants --package "$package" "${run_args[@]}" \
      "${extra_test_packages[@]}" "${error_args[@]}" \
      --profile mutant --jobs 1 --output "$out_dir"; then
    for file in "${run_files[@]}"; do
      echo "  выживших нет: $file"
      checked=$((checked + 1))
    done
    continue
  fi

  fail=1

  # Ненулевой код возврата — не обязательно выжившие мутанты: так же
  # завершаются сбой сборки, таймаут и нежизнеспособные мутанты. Причина
  # берётся из отчёта, а не угадывается по коду возврата. Нет отчёта —
  # это сбой самого прогона, и называть его «выжившими» нельзя.
  if [ ! -f "$report/outcomes.json" ]; then
    echo "  ОТКАЗ: прогон пакета $package завершился с ошибкой и не оставил отчёта" >&2
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
  echo "  пакет $package: всего $n_total, выжило $n_missed, таймаут $n_timeout," \
    "нежизнеспособных $n_unviable" >&2

  if [ "${n_missed:-0}" -eq 0 ] && [ "${n_timeout:-0}" -eq 0 ]; then
    # Прогон упал не на мутантах: сборка, окружение, сам инструмент.
    # Приписывать это модулям нельзя, и «проверено» они тоже не получают.
    echo "  Прогон пакета $package не прошёл без выживших мутантов —" >&2
    echo "  смотрите $report/" >&2
    continue
  fi

  # Разбор идёт по файлам: пакетный прогон дешевле пофайлового, но
  # отвечать заслон обязан за модуль. Иначе один выживший пачкает весь
  # пакет, и искать его негде.
  for file in "${run_files[@]}"; do
    if ! survivors=$(jq -r --arg f "$file" \
        '.outcomes[]
         | select(.summary == "MissedMutant" or .summary == "Timeout")
         | select(.scenario.Mutant.file == $f)
         | "    " + .summary + ": " + .scenario.Mutant.name' \
        "$report/outcomes.json" 2>"$ERR_FILE"); then
      echo "  ОТКАЗ: не удалось разобрать выживших по $file" >&2
      cat "$ERR_FILE" >&2
      continue
    fi

    if [ -n "$survivors" ]; then
      echo "  ВЫЖИВШИЕ МУТАНТЫ в $file:" >&2
      printf '%s\n' "$survivors" >&2
    else
      checked=$((checked + 1))
    fi
  done
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
