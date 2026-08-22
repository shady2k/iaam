# E0 + E1 (часть 1): каркас и необратимая схема журнала

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.
>
> **Epics:** E0 = `iaam-a4x`, E1 = `iaam-1fk`
> **Spec:** `.internal/specs/2026-08-22-investment-tracker-design.md`

**Goal:** Собрать воспроизводимый Rust-каркас с механическими заслонами качества и зафиксировать **необратимую схему журнала фактов** (§16.1) — то, отсутствие чего позже заставит менять смысл уже записанных событий.

**Границы плана.** Проекции со снимками, XIRR, хранилище SQLite, `iaam-app` и REST-слой выносятся во **второй план** (`2026-08-22-e1-part2-*.md`), который пишется после того, как схема существует как работающий код. Эпик `iaam-1fk` закрывается по завершении второго плана, не этого.

Обоснование разделения: схема — единственная часть, которую нельзя переделать без миграции журнала. Всё остальное аддитивно (§16.2) и выигрывает от того, что проектируется поверх уже проверенного кода, а не одновременно с ним.

**Architecture:** Функциональное ядро, императивная оболочка (§3.1). `iaam-core` — чистые синхронные функции над загруженным срезом данных, без ввода-вывода, без `async`, без `Mutex`. Адаптеры (`iaam-store`, позже `iaam-market`, `iaam-ingest`) собирают данные, `iaam-app` оркестрирует сценарии и владеет портами, `iaam-server` отдаёт REST. Журнал фактов append-only, всё остальное — проекции, пересчитываемые с нуля.

**Tech Stack:** Rust (тулчейн закреплён Nix-флейком), `rust_decimal`, `rusqlite` (bundled), `axum`, `utoipa`, `serde`, `thiserror`, `tracing`, `proptest`, `insta`, `cargo-nextest`, `cargo-mutants`, `cargo-deny`, `cargo-llvm-cov`, `cargo-hack`.

---

## ⚠️ Статус проверки кода

**Ни одна строка кода в этом плане не была скомпилирована.** На машине, где план писался, отсутствовал тулчейн Rust. Код написан идиоматично и по спецификации, но:

- сигнатуры внешних крейт могли измениться между версиями;
- имена методов `rust_decimal`, `rusqlite`, `axum` и `utoipa` следует сверять с документацией закреплённой версии;
- любое расхождение исправляется в пользу компилятора, а не в пользу плана.

**Задача 1 существует именно для того, чтобы это перестало быть правдой.** До её завершения ни одна другая задача не начинается. После неё каждая задача обязана заканчиваться зелёной сборкой.

Если код из плана не компилируется — это ожидаемо, чините по месту. Если код компилируется, но **тест приходится ослабить, чтобы он прошёл** — это не ожидаемо, останавливайтесь и эскалируйте (§15.7 спеки).

---

## Global Constraints

Требования, действующие для **каждой** задачи. Нарушение любого — основание отклонить задачу на ревью.

| Правило | Источник |
|---|---|
| `unsafe` запрещён во всех крейтах первой стороны. Обеспечивается таблицей `[workspace.lints.rust]` в корневом `Cargo.toml` **плюс** `[lints] workspace = true` в каждой крейте. Крейта без второй строки молча выпадает из-под запрета — это проверяется заслоном архитектуры (задача 3) | §15.1 |
| `f64` запрещён в доменных величинах (проведённые суммы, налоговая база, члены тождества). Допустим только внутри решателей ставок с документированной границей погрешности | §6.6, §15.1 |
| `iaam-core` — синхронная, без `async`, без `Mutex`, без ввода-вывода, без зависимостей на другие крейты воркспейса | §3.1, §3.2 |
| Строковые дискриминаторы запрещены там, где возможен `enum` | §15.1 |
| Неизвестное значение — `Option<T>`, **никогда** не нулевая заглушка | §4.9 |
| Проведённые суммы и расчётные величины — разные типы, не смешиваются | §3.4 |
| Каждое событие несёт `provenance` | §4.1 |
| Ожидаемое значение теста **никогда** не берётся из вывода программы | §15.5 |
| Замороженную фикстуру нельзя править, чтобы починить тест | §15.7 |
| Нарушение инварианта — типизированная ошибка и `not_computable`, а не число с предупреждением | §15.2 |
| Общего крейта `shared` / `common` / `utils` не существует | §3.2 |
| `clippy -D warnings` обязателен; новые `allow`, `ignore`, `todo!`, `unimplemented!` и `_ =>` в доменных `enum` запрещены | §15.7 |
| **Логика конструктора не живёт в `new`.** `cargo-mutants` молча пропускает любую функцию с именем `new` — проверено: тождественное тело под именем `build` даёт 4 мутанта, под именем `new` ноль. Конструктор с проверками должен делегировать приватной функции (`from_ratio`, `from_parts`, …), иначе его логика невидима мутационному заслону, а заслон печатает «выживших нет» | §15.7 |
| **Литералы вида `100_00` (рубли_копейки) запрещены.** `clippy::inconsistent_digit_grouping` входит в `all`, а `all = deny`, поэтому такая запись не компилируется. Пишите `10_000`, `25_050`. Читаемость теряется — это цена за единый набор линтов без исключений | §15.1 |
| Коммит после каждой задачи, с идентификатором бида в сообщении | — |

---

## File Structure

```
flake.nix                        закреплённый тулчейн и cargo-инструменты
.envrc                           автозагрузка окружения через nix-direnv
flake.lock                       пины
rust-toolchain.toml              версия для не-Nix окружений
Cargo.toml                       воркспейс
clippy.toml, rustfmt.toml        стиль
deny.toml                        политика зависимостей
.cargo/mutants.toml              настройки мутационного тестирования
.github/workflows/ci.yml         заслоны
.github/workflows/mutants.yml    мутационное тестирование
scripts/check-architecture.sh    направление зависимостей, чистота ядра
scripts/check-diff-lint.sh       запрет ослабления проверок и правки политики
scripts/check-fixtures.sh        манифест контрольных сумм фикстур
scripts/check-mutants.sh         порог выживаемости по каждому модулю
tests/fixtures/MANIFEST.sha256   замороженные эталоны

crates/
  iaam-core/                     ЯДРО. Ни I/O, ни async, ни зависимостей на воркспейс
    src/lib.rs
    src/numeric/mod.rs           три числовых режима (§6.6)
    src/numeric/decimal.rs       денежный режим
    src/numeric/exact.rs         точный режим (рациональные)
    src/numeric/approx.rs        приближённый режим, политика решателей
    src/money.rs                 Money<C>, PostedMinor, Quantity
    src/dates.rs                 шесть семантических дат, EffectiveOrder
    src/ids.rs                   OwnerId, AccountId, CustodyId, InstrumentId, SourceId
    src/event/mod.rs             envelope
    src/event/kind.rs            семейство типов событий
    src/event/leg.rs             типизированные ноги движения
    src/event/provenance.rs      происхождение
    src/event/correction.rs      reversal + replacement, разрешение конфликтов
    src/contour.rs               контуры и их версии
    src/rules/mod.rs             RuleRegistry
    src/rules/lot_disposal.rs    LotDisposalRule + FIFO v1
    src/projection/mod.rs        project / advance / снимки
    src/projection/lots.rs       экономические лоты
    src/projection/positions.rs  позиции
    src/projection/cashflow.rs   потоки и граница контура
    src/returns/xirr.rs          XIRR
    src/invariants.rs            инварианты и типизированная ошибка
    src/error.rs

  iaam-oracle/                   независимые эталоны для тестов (§15.4)
    src/lib.rs
    src/lots_reference.rs        другой алгоритм списания, рациональная арифметика
    src/npv_reference.rs         проверка корня XIRR

  iaam-store/                    SQLite
    src/lib.rs
    src/schema.rs
    src/migrations/
    src/event_store.rs

  iaam-app/                      сценарии, порты, кэш снимков
    src/lib.rs
    src/ports.rs
    src/services.rs
    src/usecase/record_event.rs
    src/usecase/report_returns.rs

  iaam-server/                   REST (библиотека, не бинарник)
    src/lib.rs
    src/auth.rs
    src/dto.rs
    src/routes/mod.rs
    src/openapi.rs

  iaam-cli/
    src/lib.rs

  iaam-bootstrap/                точка сборки: связывает адаптеры с iaam-app
    src/lib.rs
    src/bin/iaam-server.rs
    src/bin/iaam-cli.rs

docs/agent-skill.md              скилл для внешнего агента (§13)
```

---

# Эпик E0 — Каркас проекта (`iaam-a4x`)

Заслоны из §15.7 несут содержательную нагрузку: они единственное, что стоит между агентской разработкой и тестами, которые проходят впустую. Поэтому критерий готовности каждого — **не «настроен», а «падает на намеренно внесённом нарушении»**.

---

### Task 1: Воспроизводимый тулчейн и пустой воркспейс

**Files:**
- Create: `flake.nix`
- Create: `rust-toolchain.toml`
- Create: `Cargo.toml`
- Create: `crates/iaam-core/Cargo.toml`
- Create: `crates/iaam-core/src/lib.rs`
- Modify: `.gitignore`

**Interfaces:**
- Consumes: ничего
- Produces: работающие `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt` внутри `nix develop`; крейта `iaam-core` существует и собирается

**Acceptance Criteria:**
- `nix develop -c cargo --version` печатает версию без ошибок
- `nix develop -c cargo test --workspace` завершается успешно
- `flake.lock` закоммичен, то есть версия тулчейна воспроизводима
- `cd` в каталог проекта даёт работающий `cargo` без префикса `nix develop -c`
- Версия тулчейна и редакция Rust записаны в план-факт (см. шаг 6)

- [ ] **Step 1: Создать `flake.nix`**

```nix
{
  description = "IAAM — учёт инвестиций";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" "llvm-tools-preview" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.cargo-nextest
            pkgs.cargo-deny
            pkgs.cargo-llvm-cov
            pkgs.cargo-hack
            pkgs.cargo-mutants
            pkgs.cargo-audit
            pkgs.jq
            pkgs.sqlite
          ];
          # rusqlite с feature "bundled" компилирует SQLite из исходников
          shellHook = ''
            echo "iaam dev shell · $(rustc --version)"
          '';
        };
      });
}
```

- [ ] **Step 2: Создать `rust-toolchain.toml`**

Нужен для окружений без Nix (например, чужой CI или контейнер).

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy", "llvm-tools-preview"]
```

- [ ] **Step 3: Создать корневой `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/iaam-core"]

[workspace.package]
edition = "2024"
license = "MIT"
rust-version = "1.85"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
```

> **Редакция — проверено на исполнении.** `2024` и `rust-version = "1.85"` —
> **связанная пара**. С `edition = "2024"` и `rust-version = "1.75"` cargo
> отказывается собирать: редакция 2024 требует rustc ≥ 1.85. Менять одно
> без другого нельзя.
>
> Фактический тулчейн на 2026-08-22: `rustc 1.98.0`, `cargo 1.98.0`,
> nixos-unstable @ `ffb3c9b7`. Если ваш тулчейн старше 1.85 — откатитесь
> на `edition = "2021"` и `rust-version = "1.75"` **обеими** строками.

- [ ] **Step 4: Создать крейту `iaam-core`**

`crates/iaam-core/Cargo.toml`:

```toml
[package]
name = "iaam-core"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]

[lints]
workspace = true
```

`crates/iaam-core/src/lib.rs`:

```rust
//! Ядро учёта инвестиций.
//!
//! Чистые синхронные функции над загруженным срезом данных.
//! Ни ввода-вывода, ни `async`, ни `Mutex`, ни зависимостей на другие крейты воркспейса.
//! См. §3.1 спецификации.

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 5: Проиндексировать файлы и проверить сборку**

**Сначала `git add`, потом `nix develop`.** Флейк в git-репозитории видит
только файлы в индексе; с untracked `flake.nix` команда падает с
`path '/nix/store/...' does not exist`, и сообщение никак не намекает
на причину.

```bash
git add flake.nix rust-toolchain.toml Cargo.toml crates/
nix develop -c cargo build --workspace
nix develop -c cargo test --workspace
```

Ожидается: обе команды завершаются успешно, тест `workspace_builds` проходит.

Если `nix develop` падает на разрешении входов — выполните `nix flake update` один раз и повторите.

> **Правило на весь проект.** Новый модуль подключается в `lib.rs`
> (`pub mod <имя>;`) **на том же шаге, где создаётся файл с падающими
> тестами**, а не в конце задачи. Иначе файл не входит в дерево компиляции,
> и шаг «убедитесь, что тест падает» даёт зелёный прогон ни о чём —
> это выглядит как «тест не написан», а не как «тест падает».

> **Правило на весь проект.** Любой новый файл, который читает **сам флейк**,
> должен быть в индексе git до вызова `nix develop`. К исходникам под
> `crates/` это не относится — флейк их не вычисляет, — но к `flake.nix`,
> `flake.lock` и всему, на что они ссылаются, относится всегда.
>
> Отдельно: пока рабочее дерево грязное, `nix develop` печатает
> `warning: Git tree ... is dirty`. Это шум флейка, а не ошибка.

- [ ] **Step 6: Зафиксировать версии в README**

Создать `README.md`. Содержимое приведено ниже **описанием, а не блоком кода**:
вложенный markdown-fence внутри fence обрывает разметку самого плана.

- Заголовок первого уровня: `IAAM — учёт инвестиций`
- Раздел `## Разработка`
- Строка: все команды выполняются внутри `nix develop`
- Пункт списка с фактической версией из вывода `rustc --version` и `cargo --version`
- Пункт списка с фактической редакцией Rust
- Блок команд с `nix develop` и `cargo test --workspace`
- Абзац про `direnv`: с файлом `.envrc` окружение подхватывается при входе
  в каталог, и префикс `nix develop -c` не нужен

Подставьте фактические значения, а не заглушки — README является
единственным местом, где версия тулчейна записана человекочитаемо.

- [ ] **Step 7: Создать `.envrc` для direnv**

`nix-direnv` установлен в системе, поэтому окружение подхватывается при входе
в каталог, и префикс `nix develop -c` в остальных командах становится лишним.
Кэш `nix-direnv` избавляет от пересчёта окружения при каждом `cd`.

```bash
cat > .envrc <<'EOF'
use flake
EOF
direnv allow
```

Проверка:

```bash
direnv exec . cargo --version
```

Ожидается: версия печатается **без** обёртки `nix develop -c`, а `direnv`
сообщает `using flake`.

> Вариант через `cd .. && cd <каталог>` неприменим при исполнении агентом:
> в неинтерактивной оболочке hook `direnv` не установлен, поэтому смена
> каталога ничего не подхватывает. `direnv exec .` проверяет то же самое
> свойство напрямую.

> Все команды в плане записаны с префиксом `nix develop -c`, чтобы работать
> и без direnv. С direnv префикс можно опускать — поведение то же.

- [ ] **Step 8: Обновить `.gitignore`**

Дописать в конец:

```gitignore
# Rust
/target/
**/*.rs.bk

# Nix
result
result-*

# direnv
.direnv/
```

`.envrc` **коммитится** (это часть настройки проекта), `.direnv/` — нет.

- [ ] **Step 9: Коммит**

```bash
git add flake.nix flake.lock rust-toolchain.toml Cargo.toml Cargo.lock crates/ README.md .gitignore .envrc
git commit -m "build: воспроизводимый тулчейн через Nix, direnv и пустой воркспейс (iaam-a4x)"
```

---

### Task 2: Стиль, линты и базовый CI

**Files:**
- Create: `rustfmt.toml`
- Create: `clippy.toml`
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: воркспейс из Task 1
- Produces: `cargo fmt --check` и `cargo clippy -D warnings` как обязательные ворота; CI, запускающийся на push и PR

**Acceptance Criteria:**
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` проходит
- Намеренно внесённое `unsafe { }` в `iaam-core` **не компилируется**
- `clippy.toml` доказанно читается — проверено несуществующим ключом
- Workflow создан и синтаксически корректен. **Зелёный прогон CI в критерии не входит**: он требует пуша, а пуш в задачах запрещён. Проверяется на закрытии эпика.

- [ ] **Step 1: Создать `rustfmt.toml`**

```toml
edition = "2024"
max_width = 100
use_field_init_shorthand = true
```

- [ ] **Step 2: Создать `clippy.toml`**

```toml
# Порог сложности: функции сложнее порога требуют разбиения (§17, ограничение агентского кода)
cognitive-complexity-threshold = 20
too-many-arguments-threshold = 6
```

- [ ] **Step 3: Создать `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: true

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: cachix/install-nix-action@v27
        with:
          extra_nix_config: "experimental-features = nix-command flakes"
      - name: Format
        run: nix develop -c cargo fmt --all -- --check
      - name: Clippy
        run: nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
      - name: Test
        run: nix develop -c cargo test --workspace --all-features
```

- [ ] **Step 4: Проверить, что заслон `unsafe` работает**

Временно добавьте в `crates/iaam-core/src/lib.rs`:

```rust
pub fn deliberately_broken() {
    unsafe { std::ptr::null::<u8>().read() };
}
```

Запустите:

```bash
nix develop -c cargo build --workspace
```

Ожидается **ошибка компиляции** ровно такого вида:

```
error: usage of an `unsafe` block
  = note: requested on the command line with `-F unsafe-code`
```

> **Слова `forbidden` в тексте нет** — проверено на исполнении. Флаг
> `-F unsafe-code` cargo выводит из таблицы `[workspace.lints.rust]`.
> Если ждать буквального «forbidden», можно решить, что сработал не тот
> заслон.

Уберите эту функцию. Заслон проверен.

- [ ] **Step 5: Проверить, что clippy действительно валит сборку**

Временно добавьте функцию **перед** `#[cfg(test)] mod tests`, а не в конец
файла: код после тестового модуля даёт вдобавок `items_after_test_module`,
и целевая претензия теряется в шуме.

```rust
pub fn deliberately_lint_bad(v: &Vec<i32>) -> usize {
    v.len()
}
```

Запустите:

```bash
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Ожидается **error**, а не warning:

```
error: writing `&Vec` instead of `&[_]` involves a new object where a slice will do
  = note: `-D clippy::ptr-arg` implied by `-D clippy::all`
```

Уберите функцию.

- [ ] **Step 5a: Проверить, что `clippy.toml` вообще читается**

Конфигурация, которую никто не читает, — тот же непроверенный заслон:
оба заданных ключа валидны, поэтому молчание неотличимо от игнорирования.

Временно допишите в `clippy.toml` несуществующий ключ:

```toml
this-key-does-not-exist = 1
```

```bash
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Ожидается: ошибка с указанием пути `/…/clippy.toml:4:1` — то есть файл
прочитан. Уберите ключ.

- [ ] **Step 6: Коммит**

```bash
git add rustfmt.toml clippy.toml .github/workflows/ci.yml
git commit -m "ci: rustfmt, clippy -D warnings, forbid unsafe; заслоны проверены (iaam-a4x)"
```

> **Трейлер.** Во все коммиты, сделанные агентом, добавляется
> `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.
> В командах плана он не выписан ради краткости, но ставится всегда.

---

### Task 3: Архитектурные заслоны

Проверяют то, что компилятор сам не проверит: направление зависимостей (§3.2) и чистоту ядра (§3.1).

**Files:**
- Create: `scripts/check-architecture.sh`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: воркспейс, CI из Task 2
- Produces: `scripts/check-architecture.sh` — исполняемый заслон, вызываемый в CI и локально

**Acceptance Criteria:**
- Скрипт проходит на текущем дереве
- Добавление зависимости `iaam-core` на любую крейту воркспейса роняет скрипт
- Появление `f64` в `iaam-core` вне `src/numeric/approx.rs` роняет скрипт
- Появление `async fn`, `Mutex` или `tokio` в `iaam-core` роняет скрипт
- Крейта без `[lints] workspace = true` роняет скрипт — иначе она молча выпадает из-под запрета `unsafe`

- [ ] **Step 1: Создать `scripts/check-architecture.sh`**

```bash
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

# --- 5. Двоичная плавающая точка в ядре только в numeric/approx.rs ---
if [ -d "$CORE_SRC" ]; then
  hits=$(grep -rn '\bf64\b\|\bf32\b' "$CORE_SRC" --include='*.rs' \
    | { grep -v "^${CORE_SRC//./\\.}/numeric/approx\.rs:" || true; } \
    | strip_comments || true)
  if [ -n "$hits" ]; then
    err "двоичная плавающая точка вне numeric/approx.rs (§6.6):"
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

# --- 8. approx.rs не разрастается в теневой расчётный слой ---
# Исключение целого файла из заслона №5 опасно: в нём можно разместить
# денежную арифметику. Ограничение размера делает это заметным.
APPROX="$CORE_SRC/numeric/approx.rs"
if [ -f "$APPROX" ]; then
  lines=$(wc -l < "$APPROX")
  if [ "$lines" -gt 200 ]; then
    err "numeric/approx.rs разросся до $lines строк при пороге 200."
    err "Приближённый режим должен оставаться тонким (§6.6)."
  fi
fi

if [ "$fail" -ne 0 ]; then
  echo "" >&2
  echo "Архитектурные заслоны не пройдены. Правьте код, а не заслон." >&2
  exit 1
fi
echo "Архитектурные заслоны пройдены."
```

> **Этот скрипт проверен на исполнении** — в отличие от Rust-кода в плане.
> Первая редакция содержала пять дефектов, из которых один был fail-open:
> `grep -qx` в пайплайне под `set -o pipefail` инвертировал результат
> (`grep -q` закрывает пайп → `jq` умирает по SIGPIPE → код пайплайна
> ненулевой → условие ложно), и настоящая утечка эталона читалась бы
> как «заслон пройден». Заслон, молча пропускающий нарушение, хуже
> отсутствующего.
>
> Остальные четыре: мёртвая ветка `grep -qA1` с инвертированной логикой;
> четыре отдельных вызова `cargo metadata`, каждый с `|| true` — четыре
> шанса молча упасть; `|| true`, привязанный ко всему пайплайну вместо
> grep-стадии; неэкранированная точка в `approx\.rs`.

> **Про `cd` в корень.** `cd "$(git rev-parse --show-toplevel)"` не выполняет
> требование «работает из любого каталога»: из не-git каталога подстановка
> пуста и получается `cd ""`. Корень ищется от каталога **самого скрипта**.

> **Заслон №1 запрещает ядру зависимости всех видов, включая
> `[dev-dependencies]`.** Это осознанно: `iaam-oracle` (задача 14) зависит
> от `iaam-core`, а не наоборот. Но помните ограничение — ядро не сможет
> получить dev-зависимость на крейту воркспейса никогда.

- [ ] **Step 2: Сделать исполняемым и запустить**

```bash
chmod +x scripts/check-architecture.sh
nix develop -c ./scripts/check-architecture.sh
```

Ожидается: `Архитектурные заслоны пройдены.`

- [ ] **Step 3: Проверить, что заслон `f64` срабатывает**

Временно добавьте в `crates/iaam-core/src/lib.rs`:

```rust
pub fn deliberately_float() -> f64 {
    1.0
}
```

```bash
nix develop -c ./scripts/check-architecture.sh
```

Ожидается: **выход с кодом 1** и сообщение о двоичной плавающей точке.

Уберите функцию, убедитесь, что скрипт снова зелёный.

- [ ] **Step 4: Проверить, что заслон `async` срабатывает**

Временно добавьте:

```rust
pub async fn deliberately_async() {}
```

```bash
nix develop -c ./scripts/check-architecture.sh
```

Ожидается: **выход с кодом 1** и сообщение про async.

Уберите функцию.

- [ ] **Step 5: Добавить в CI**

В `.github/workflows/ci.yml`, в job `check`, после шага `Clippy`:

```yaml
      - name: Architecture guards
        run: nix develop -c ./scripts/check-architecture.sh
```

- [ ] **Step 6: Коммит**

```bash
git add scripts/check-architecture.sh .github/workflows/ci.yml
git commit -m "ci: архитектурные заслоны направления зависимостей и чистоты ядра (iaam-a4x)"
```

---

### Task 4: Заслоны против подложных тестов

Закрывают режимы отказа из §15.7, специфичные для агентской разработки.

**Files:**
- Create: `scripts/check-diff-lint.sh`
- Create: `scripts/check-fixtures.sh`
- Create: `tests/fixtures/MANIFEST.sha256`
- Create: `deny.toml`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: CI из Task 3
- Produces: заслоны против ослабления проверок, подмены фикстур и небезопасных зависимостей

**Acceptance Criteria:**
- Добавление нового `#[allow(...)]` в диффе роняет CI
- Изменение содержимого файла в `tests/fixtures/` без обновления манифеста роняет CI
- `cargo deny check` проходит
- Каждый заслон продемонстрирован падающим на намеренном нарушении

- [ ] **Step 1: Создать `scripts/check-diff-lint.sh`**

```bash
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
```

> **Проверено на исполнении. Первая редакция была fail-open ровно в том
> пути CI, который план сам и строит.** Хеш пустого дерева проходит
> `git rev-parse --verify` с кодом 0, но `git diff <tree>...HEAD` — фатальная
> ошибка (`object ... is a tree, not a commit`), и `|| true` на хвосте
> пайплайна её глотает. Демонстрация с настоящим `#[allow(dead_code)]`
> в HEAD: старая версия печатала «Diff-lint пройден» с кодом 0, новая
> ловит нарушение с кодом 1. Заслон был бы слеп именно на первом push
> в ветку — то есть там, где ослабление проверки и проезжает.

> **Список файлов политики закрыт по решению владельца.** В него входят
> `flake.nix`, `flake.lock` и `rustfmt.toml` — иначе удаление
> `cargo-mutants` или `diff-cover` из окружения было бы ослаблением
> проверки, которое заслон не помечает. Проверено: заслон ловит правку
> самого себя.

- [ ] **Step 2: Создать `scripts/check-fixtures.sh`**

```bash
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
```

> **`--include` обязан стоять до `--`.** В первой редакции было
> `grep -rqF -- "$name" crates/ --include='*.rs'`: после `--` всё
> трактуется как имя файла, поэтому фильтр по `*.rs` не применялся,
> grep возвращал 2 из-за несуществующего «файла», и исход зависел от
> порядка обхода каталога. Упоминание фикстуры в `NOTES.md` засчитывалось
> как ссылка из теста.
>
> Само `-F` при этом необходимо: без него точка в имени расширяет поиск,
> и `x.json` совпадает с текстом `xQjson` — мёртвый эталон проходит.

- [ ] **Step 3: Создать первую фикстуру и манифест**

Фикстура-заглушка нужна, чтобы заслон был проверяем уже сейчас. Настоящие появятся в Task 19.

```bash
mkdir -p tests/fixtures
cat > tests/fixtures/smoke.json <<'EOF'
{
  "_comment": "Проверочная фикстура для заслона check-fixtures.sh. Заменяется настоящими эталонами в Task 19.",
  "value": 42
}
EOF
sha256sum tests/fixtures/smoke.json > tests/fixtures/MANIFEST.sha256
```

Добавьте в `crates/iaam-core/src/lib.rs` в блок `mod tests`:

```rust
    #[test]
    fn fixture_manifest_is_wired() {
        let raw = include_str!("../../../tests/fixtures/smoke.json");
        assert!(raw.contains("\"value\": 42"));
    }
```

- [ ] **Step 4: Проверить, что заслон фикстур срабатывает**

```bash
chmod +x scripts/check-fixtures.sh
nix develop -c ./scripts/check-fixtures.sh          # ожидается: пройдено
sed -i 's/42/43/' tests/fixtures/smoke.json
nix develop -c ./scripts/check-fixtures.sh          # ожидается: выход 1
sed -i 's/43/42/' tests/fixtures/smoke.json
nix develop -c ./scripts/check-fixtures.sh          # ожидается: пройдено
```

> **Осторожно с пайпами через `nix develop -c`.** `shellHook` в `flake.nix`
> печатает баннер в stdout, поэтому `nix develop -c cargo metadata | jq`
> из командной строки ломается: баннер попадает на вход `jq`. Внутри
> скрипта проблемы нет — баннер выводится один раз до старта скрипта.
> Учитывайте это в задачах, где что-то пайпится.

- [ ] **Step 5: Создать `deny.toml`**

```toml
[advisories]
yanked = "deny"

[bans]
multiple-versions = "warn"
wildcards = "deny"

[licenses]
allow = ["MIT", "Apache-2.0", "BSD-3-Clause", "ISC", "Unicode-3.0", "Zlib"]

[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

```bash
nix develop -c cargo deny check
```

Ожидается: проверка проходит. Если падает на лицензии транзитивной зависимости — добавьте её в `allow` **осознанно**, а не автоматически.

- [ ] **Step 6: Добавить заслоны в CI**

В `.github/workflows/ci.yml`, после `Architecture guards`:

```yaml
      - name: Diff lint
        env:
          # Владелец выставляет переменную в настройках репозитория,
          # когда изменение политики качества действительно одобрено.
          POLICY_CHANGE_APPROVED: ${{ vars.POLICY_CHANGE_APPROVED }}
        run: |
          if [ -n "${{ github.base_ref }}" ]; then
            BASE="origin/${{ github.base_ref }}"
          else
            BASE="${{ github.event.before }}"
            # Первый push в ветку даёт нулевой SHA — сравниваем с пустым деревом.
            if [ "$BASE" = "0000000000000000000000000000000000000000" ]; then
              BASE=$(git hash-object -t tree /dev/null)
            fi
          fi
          nix develop -c ./scripts/check-diff-lint.sh "$BASE"
      - name: Frozen fixtures
        run: nix develop -c ./scripts/check-fixtures.sh
      - name: Dependency policy
        run: nix develop -c cargo deny check
```

- [ ] **Step 7: Коммит**

```bash
chmod +x scripts/check-diff-lint.sh scripts/check-fixtures.sh
git add scripts/ tests/fixtures/ deny.toml .github/workflows/ci.yml crates/iaam-core/src/lib.rs
git commit -m "ci: заслоны против ослабления тестов, подмены фикстур и небезопасных зависимостей (iaam-a4x)"
```

---

### Task 5: Мутационное тестирование и покрытие

**Files:**
- Create: `.cargo/mutants.toml`
- Create: `scripts/check-mutants.sh`
- Create: `.github/workflows/mutants.yml`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: CI из Task 4
- Produces: `cargo mutants` с порогом по каждому критичному модулю; покрытие по диффу

**Acceptance Criteria:**
- Конфигурация `cargo-mutants` **читается** — подтверждено через `--list`
- Порог проверяется по каждому модулю из явного списка, а не общим числом
- Тест, написанный так, чтобы проходить впустую, оставляет выжившего мутанта, и `check-mutants.sh` возвращает ненулевой код
- Покрытие по диффу **проверяется порогом**, а не только публикуется артефактом

- [ ] **Step 1: Создать `.cargo/mutants.toml`**

```toml
# Мутационное тестирование (§15.7).
# Порог задаётся по КАЖДОМУ критичному модулю, а не общий по проекту:
# общий порог позволяет спрятать непокрытый модуль за хорошо покрытыми.

timeout_multiplier = 5.0
minimum_test_timeout = 30

# Мутанты, возвращающие ошибку из функций с Result.
# Без них путь обработки ошибок не мутируется вовсе: по умолчанию
# подставляется только Ok(Default::default()), и ветка «а что если
# операция не удалась» остаётся непроверенной.
#
# Пути обязаны быть вида `crate::…`. С `::iaam_core::…` мутанты
# нежизнеспособны — внутри самой крейты такой путь не резолвится,
# и конфигурация выглядит включённой, не давая ничего.
error_values = [
    "crate::numeric::NumericError::Overflow",
    "crate::money::MoneyError::Overflow",
]

exclude_globs = [
    "crates/iaam-bootstrap/**",
]

# Исключения только для точек сборки. Доменные модули исключать ЗАПРЕЩЕНО:
# исключение модуля из мутационного тестирования — способ спрятать
# подложные тесты, а не способ ускорить сборку.
```

> **Путь файла проверьте.** `cargo-mutants` читает конфигурацию из
> `.cargo/mutants.toml`; в некоторых версиях допустим и `mutants.toml`
> в корне. Файл, положенный не туда, **молча игнорируется** — это худший
> вид отказа заслона. Убедитесь, что конфигурация читается:
>
> ```bash
> nix develop -c cargo mutants --list | head
> ```
>
> затем временно добавьте в `exclude_globs` заведомо покрытый файл и
> проверьте, что он исчез из списка. Верните обратно.

- [ ] **Step 2: Создать `scripts/check-mutants.sh`** — порог по каждому модулю

Общий порог по проекту позволяет спрятать непокрытый модуль за хорошо
покрытыми. Поэтому проверка идёт помодульно, и список модулей задаётся явно.

```bash
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
```

> **Проверено на исполнении. Первая редакция содержала три дефекта,
> из которых два делали заслон бессмысленным.**
>
> **`--output DIR` создаёт `mutants.out` внутри `DIR`.** План читал
> `$out_dir/outcomes.json` — такого файла не существует никогда,
> то есть вся диагностика выживших мутантов была мёртвым кодом.
> Верный путь: `$out_dir/mutants.out/outcomes.json`.
>
> **`--output` не создаёт промежуточные каталоги.** Первый прогон падает
> с `create output parent directory`, и по коду возврата это неотличимо
> от выживших мутантов — план напечатал бы «ВЫЖИВШИЕ МУТАНТЫ» на сбое
> инструмента. Нужен `mkdir -p`, а отсутствие отчёта надо называть сбоем
> прогона, а не результатом.
>
> **Подавление конфигурацией читалось как успех.** Если доменный модуль
> попадал в `exclude_globs` — ровно то, что план запрещает словами, —
> скрипт печатал «выживших нет» и проходил. Проверенная версия отличает
> две причины пустого списка, сравнивая `--list` с `--list --no-config`:
> подавление конфигурацией — отказ, отсутствие мутируемого кода — явное
> сообщение.
>
> Мелочи: `--package iaam-core` был зашит жёстко (критичный модуль
> в другой крейте дал бы ноль мутантов и молчаливый проход) — пакет
> выводится из `cargo metadata`; `--no-shuffle` стал поведением
> по умолчанию; `.scenario.Mutant.name` информативнее, чем
> `.scenario.Mutant.function.function_name`.

- [ ] **Step 3: Создать `.github/workflows/mutants.yml`**

Мутационное тестирование медленное, поэтому вынесено в отдельный workflow.

```yaml
name: Mutants

on:
  pull_request:
  schedule:
    - cron: "0 3 * * 1"

jobs:
  mutants:
    runs-on: ubuntu-latest
    timeout-minutes: 60
    steps:
      - uses: actions/checkout@v4
      - uses: cachix/install-nix-action@v27
        with:
          extra_nix_config: "experimental-features = nix-command flakes"
      - name: Mutation testing (порог по каждому модулю)
        run: nix develop -c ./scripts/check-mutants.sh
      - name: Upload report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: mutants-report
          path: target/mutants/
```

- [ ] **Step 4: Убедиться, что мутационное тестирование ловит подложный тест**

Временно добавьте в `crates/iaam-core/src/lib.rs`:

```rust
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}
```

и **намеренно подложный** тест в `mod tests`:

```rust
    #[test]
    fn add_vacuous() {
        let _ = super::add(2, 2);   // результат не проверяется
    }
```

```bash
nix develop -c cargo mutants --package iaam-core --file crates/iaam-core/src/lib.rs
```

Ожидается: **ненулевой код возврата** и мутант, заменяющий `a + b` на `a - b`, в отчёте как выживший. Ненулевой код здесь — признак успеха демонстрации, а не сбоя шага.

Теперь замените тест на настоящий:

```rust
    #[test]
    fn add_returns_sum() {
        assert_eq!(super::add(2, 2), 4);
        assert_eq!(super::add(-1, 1), 0);
    }
```

```bash
nix develop -c cargo mutants --package iaam-core --file crates/iaam-core/src/lib.rs
```

Ожидается: **конкретный** мутант `a + b -> a - b` убит. Требовать убийства *всех* мутантов игрушечной функции нельзя: часть из них может оказаться эквивалентной, и тогда демонстрация превратится в невыполнимое условие.

Уберите `add` и её тест — они были нужны только для проверки заслона.

- [ ] **Step 5: Добавить покрытие по диффу в CI**

`cargo llvm-cov --lcov` строит полный отчёт, но **не** считает покрытие
добавленных строк и не задаёт порог. Без этого шага критерий «покрытие
по диффу» остался бы декларацией.

Добавьте `diff-cover` в `flake.nix` (`pkgs.python3Packages.diff-cover`),
затем в `.github/workflows/ci.yml` после `Test`:

```yaml
      - name: Coverage
        run: |
          # Флаг --branch НЕ добавлять: на stable он падает с
          # "error: the option 'Z' is only accepted on the nightly compiler" (rc=101).
          # Порог diff-cover от этого не страдает — он считает по строкам.
          nix develop -c cargo llvm-cov --workspace --all-features \
            --lcov --output-path lcov.info
      - name: Diff coverage gate
        if: github.event_name == 'pull_request'
        run: |
          nix develop -c diff-cover lcov.info \
            --compare-branch=origin/${{ github.base_ref }} \
            --fail-under=90
      - name: Upload coverage
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: coverage
          path: lcov.info
```

- [ ] **Step 6: Заменить `cargo test` на `cargo nextest`**

`nextest` изолирует тесты по процессам, что делает видимыми зависимости между ними.

В `.github/workflows/ci.yml` замените шаг `Test` на:

```yaml
      - name: Test
        run: nix develop -c cargo nextest run --workspace --all-features
      - name: Doc tests
        run: nix develop -c cargo test --workspace --doc
```

> `nextest` не выполняет doc-тесты, поэтому они запускаются отдельно.

- [ ] **Step 7: Коммит**

```bash
chmod +x scripts/check-mutants.sh
git add .cargo/mutants.toml scripts/check-mutants.sh .github/workflows/ flake.nix flake.lock
git commit -m "ci: мутационное тестирование помодульно и покрытие по диффу (iaam-a4x)"
```

---

### Task 6: Закрытие эпика E0

**Files:**
- Modify: `README.md`

**Acceptance Criteria:**
- В README перечислены все заслоны с указанием, что каждый ловит
- `bd close iaam-a4x` выполнен

- [ ] **Step 1: Дописать в `README.md`**

```markdown
## Заслоны качества

Каждый заслон проверен падением на намеренно внесённом нарушении.
Настроенный, но не проверенный заслон хуже отсутствующего — он создаёт ложную уверенность.

| Заслон | Команда | Что ловит |
|---|---|---|
| Формат | `cargo fmt --all -- --check` | расхождение стиля |
| Линты | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | деградацию качества |
| `forbid(unsafe_code)` | компилятор | unsafe в крейтах первой стороны |
| Архитектура | `./scripts/check-architecture.sh` | нарушение направления зависимостей, f64 и async в ядре |
| Diff-lint | `./scripts/check-diff-lint.sh` | новые allow/ignore/todo — ослабление проверок |
| Фикстуры | `./scripts/check-fixtures.sh` | правку замороженного эталона и мёртвые фикстуры |
| Зависимости | `cargo deny check` | уязвимости, лицензии, неизвестные источники |
| Тесты | `cargo nextest run --workspace --all-features` | регрессии |
| Покрытие | `cargo llvm-cov --branch` | недостижимый код |
| Мутации | `./scripts/check-mutants.sh` | тесты, проходящие впустую (порог по каждому модулю) |
| Покрытие по диффу | `diff-cover lcov.info --fail-under=90` | непокрытый новый код (построчно; `--branch` требует nightly) |

**Заслон не чинится ослаблением.** Если он мешает — либо код неверен, либо заслон
требует осознанного исключения, вносимого отдельным коммитом с обоснованием.
```

- [ ] **Step 2: Прогнать все заслоны разом**

```bash
nix develop -c bash -c '
  cargo fmt --all -- --check &&
  cargo clippy --workspace --all-targets --all-features -- -D warnings &&
  ./scripts/check-architecture.sh &&
  ./scripts/check-fixtures.sh &&
  cargo deny check &&
  cargo nextest run --workspace --all-features
'
```

Ожидается: все команды успешны.

> `check-diff-lint.sh` и `check-mutants.sh` в этот набор не входят: первый
> требует базу для сравнения, второй долгий. Они запускаются в CI.

- [ ] **Step 3: Коммит и закрытие эпика**

```bash
git add README.md
git commit -m "docs: заслоны качества и их назначение (iaam-a4x)"
```

```bash
bd close iaam-a4x --reason "Каркас собран, все заслоны проверены падением на намеренном нарушении"
```

---

# Эпик E1 — Ядро учёта и первый ответ (`iaam-1fk`)

Задачи 7–14 создают **необратимую схему** (§16.1): то, отсутствие чего заставит позже менять смысл уже записанных фактов. Это требования к дисциплине записи, а не к объёму вычислений — этап не превращается в налоговый движок.

---

### Task 7: Три числовых режима

Спека (§6.6) требует различать точный, денежный и приближённый режимы. Требование «невязка тождества равна нулю» невыполнимо без указания, в какой арифметике оно проверяется: деление в `Decimal` округляется внутри самой операции.

**Files:**
- Create: `crates/iaam-core/src/numeric/mod.rs`
- Create: `crates/iaam-core/src/numeric/exact.rs`
- Create: `crates/iaam-core/src/numeric/decimal.rs`
- Create: `crates/iaam-core/src/numeric/approx.rs`
- Modify: `crates/iaam-core/Cargo.toml`
- Modify: `crates/iaam-core/src/lib.rs`

**Interfaces:**
- Consumes: ничего
- Produces:
  - `numeric::exact::Exact` — рациональное число на `i128`, точное сложение/вычитание/умножение, `Exact::new(num, den) -> Result<Self, NumericError>`, `Exact::from_int(i128)`, `Exact::zero()`, `Exact::is_zero()`
  - `numeric::decimal::Dec` — обёртка над `rust_decimal::Decimal`, `Dec::from_str`, `Dec::to_exact() -> Exact`
  - `numeric::approx::{ApproxValue, SolverPolicy}` — единственное место, где разрешён `f64`
  - `numeric::NumericError`

**Acceptance Criteria:**
- `Exact` складывает `1/3 + 1/3 + 1/3` и даёт ровно `1`
- `Dec::to_exact` не теряет точность для значений с масштабом ≤ `MAX_SCALE` (18) и возвращает `ScaleTooLarge` для 19 и выше
- `ApproxValue` всегда несёт границу погрешности — сконструировать без неё невозможно
- `scripts/check-architecture.sh` зелёный: `f64` встречается только в `approx.rs`

- [ ] **Step 1: Написать падающий тест точной арифметики**

Создать `crates/iaam-core/src/numeric/exact.rs`:

```rust
//! Точный режим (§6.6): рациональная арифметика без потери точности.
//!
//! Используется там, где невязка обязана быть строго нулевой: тождество
//! результата (§6.3), разнесение налоговой стоимости, сверка.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirds_sum_to_exactly_one() {
        let third = Exact::new(1, 3).unwrap();
        let sum = third.add(&third).add(&third);
        assert_eq!(sum, Exact::from_int(1));
    }

    #[test]
    fn decimal_tenths_sum_exactly() {
        // 0.1 + 0.2 == 0.3 — то, чего не даёт двоичная плавающая точка
        let a = Exact::new(1, 10).unwrap();
        let b = Exact::new(2, 10).unwrap();
        assert_eq!(a.add(&b), Exact::new(3, 10).unwrap());
    }

    #[test]
    fn zero_denominator_rejected() {
        assert!(matches!(Exact::new(1, 0), Err(NumericError::ZeroDenominator)));
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что не компилируется**

Файл, на который никто не ссылается, не компилируется вовсе, и `cargo test`
пройдёт успешно — это выглядит как «тест не написан», а не как «тест падает».
Чтобы получить обещанную ошибку, нужна минимальная обвязка: создайте
`numeric/mod.rs` с `pub mod exact;` и добавьте `pub mod numeric;` в `lib.rs`.

```bash
nix develop -c cargo test --package iaam-core
```

Ожидается: `E0433`, `cannot find type Exact`.

- [ ] **Step 3: Реализовать `Exact`**

Дописать в начало `crates/iaam-core/src/numeric/exact.rs` (перед `mod tests`):

```rust
use core::cmp::Ordering;
use core::fmt;

use super::NumericError;

/// Рациональное число: всегда в несократимом виде, знаменатель всегда положителен.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Exact {
    num: i128,
    den: i128,
}

impl Exact {
    pub fn new(num: i128, den: i128) -> Result<Self, NumericError> {
        if den == 0 {
            return Err(NumericError::ZeroDenominator);
        }
        // checked_neg, а не унарный минус: `i128::MIN` не имеет положительного
        // представления, и `-i128::MIN` паникует в debug и заворачивается
        // в release. Тихое заворачивание в финансовой арифметике недопустимо.
        //
        // `is_negative()`, а не `den < 0`: после раннего возврата при `den == 0`
        // мутант `den <= 0` поведенчески тождествен оригиналу, то есть выживает
        // всегда и никаким тестом не убивается. Метод операторным мутантом
        // не заменяется.
        let (num, den) = if den.is_negative() {
            (
                num.checked_neg().ok_or(NumericError::Overflow)?,
                den.checked_neg().ok_or(NumericError::Overflow)?,
            )
        } else {
            (num, den)
        };
        let g = gcd(num.unsigned_abs(), den.unsigned_abs());
        // g != 0: den != 0, поэтому НОД положителен
        let g = g as i128;
        Ok(Self { num: num / g, den: den / g })
    }

    pub const fn from_int(v: i128) -> Self {
        Self { num: v, den: 1 }
    }

    pub const fn zero() -> Self {
        Self { num: 0, den: 1 }
    }

    pub const fn is_zero(&self) -> bool {
        self.num == 0
    }

    pub const fn numerator(&self) -> i128 {
        self.num
    }

    pub const fn denominator(&self) -> i128 {
        self.den
    }

    /// Сложение. Паникует при переполнении `i128` — это ошибка предметной
    /// области (суммы такого порядка невозможны), а не штатный путь.
    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        let num = self
            .num
            .checked_mul(other.den)
            .and_then(|a| other.num.checked_mul(self.den).and_then(|b| a.checked_add(b)))
            .expect("переполнение i128 в точной арифметике");
        let den = self.den.checked_mul(other.den).expect("переполнение i128 в знаменателе");
        Self::new(num, den).expect("знаменатель не может стать нулём")
    }

    pub fn neg(&self) -> Result<Self, NumericError> {
        Ok(Self {
            num: self.num.checked_neg().ok_or(NumericError::Overflow)?,
            den: self.den,
        })
    }

    pub fn sub(&self, other: &Self) -> Result<Self, NumericError> {
        Ok(self.add(&other.neg()?))
    }

    #[must_use]
    pub fn mul(&self, other: &Self) -> Self {
        let num = self.num.checked_mul(other.num).expect("переполнение i128 в умножении");
        let den = self.den.checked_mul(other.den).expect("переполнение i128 в умножении");
        Self::new(num, den).expect("знаменатель не может стать нулём")
    }

    pub fn div(&self, other: &Self) -> Result<Self, NumericError> {
        if other.is_zero() {
            return Err(NumericError::DivisionByZero);
        }
        let num = self.num.checked_mul(other.den).ok_or(NumericError::Overflow)?;
        let den = self.den.checked_mul(other.num).ok_or(NumericError::Overflow)?;
        Self::new(num, den)
    }

    /// Сумма списка. Вынесена отдельно, потому что проверка тождества (§6.3)
    /// суммирует компоненты и обязана давать строгий ноль.
    #[must_use]
    pub fn sum(items: &[Self]) -> Self {
        items.iter().fold(Self::zero(), |acc, x| acc.add(x))
    }
}

impl PartialOrd for Exact {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Exact {
    fn cmp(&self, other: &Self) -> Ordering {
        // a/b <=> c/d при b,d > 0 эквивалентно a*d <=> c*b.
        // Произведение считается в 256 битах, поэтому переполнения нет
        // и сравнение остаётся точным при любых допустимых величинах.
        let lhs = I256::from(self.num) * I256::from(other.den);
        let rhs = I256::from(other.num) * I256::from(self.den);
        lhs.cmp(&rhs)
    }
}

const fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    if a == 0 {
        1
    } else {
        a
    }
}
```

> **Про 256-битное умножение.** В стандартной библиотеке такого типа нет,
> поэтому берётся крейт `ethnum`. Сравнение через приведение к `f64`
> рассматривалось и отвергнуто: оно вносит приближение в тип, существующий
> ради точности, и нарушило бы заслон из задачи 3. Сравнение через
> `checked_mul` с откатом тоже отвергнуто — откат пришлось бы делать
> приближённым.
>
> Добавьте в `crates/iaam-core/Cargo.toml`:
>
> ```toml
> ethnum = "1"
> ```
>
> и в начало `exact.rs`:
>
> ```rust
> use ethnum::I256;
> ```

- [ ] **Step 4: Создать `numeric/mod.rs` с типом ошибки**

```rust
//! Три числовых режима (§6.6 спецификации).
//!
//! | Режим | Где | Тип |
//! |---|---|---|
//! | Точный | тождество результата, разнесение basis, сверка | [`exact::Exact`] |
//! | Денежный | суммы, цены, курсы, НКД | [`decimal::Dec`] |
//! | Приближённый | XIRR, CAGR, DCF — степени, корни, итерации | [`approx`] |
//!
//! Приближённые величины **никогда** не входят в денежное тождество:
//! тождество проверяет суммы, а не ставки.

pub mod approx;
pub mod decimal;
pub mod exact;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NumericError {
    #[error("знаменатель равен нулю")]
    ZeroDenominator,
    #[error("деление на ноль")]
    DivisionByZero,
    #[error("переполнение при точном вычислении")]
    Overflow,
    #[error("масштаб {scale} превышает поддерживаемый максимум {max}")]
    ScaleTooLarge { scale: u32, max: u32 },
}
```

- [ ] **Step 5: Добавить зависимости**

`crates/iaam-core/Cargo.toml`:

```toml
[dependencies]
rust_decimal = { version = "1", default-features = false, features = ["std", "serde"] }
thiserror = "2"
serde = { version = "1", features = ["derive"] }
# 256-битная целая арифметика для точного сравнения рациональных чисел.
ethnum = "1"
```

> Если `thiserror` версии 2 недоступен в закреплённом наборе — используйте `1`. Проверьте `cargo build`, не гадайте.

- [ ] **Step 6: Реализовать денежный режим**

Создать `crates/iaam-core/src/numeric/decimal.rs`:

```rust
//! Денежный режим (§6.6): суммы, цены, курсы, НКД.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::exact::Exact;
use super::NumericError;

/// Максимальный масштаб, который умеет представить `Exact` без переполнения
/// при типичных величинах портфеля.
const MAX_SCALE: u32 = 18;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Dec(Decimal);

impl Dec {
    #[must_use]
    pub const fn new(value: Decimal) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn inner(&self) -> Decimal {
        self.0
    }

    #[must_use]
    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    /// Перевод в точный режим. Возможен без потерь: десятичная дробь
    /// с масштабом `s` — это рациональное число со знаменателем `10^s`.
    pub fn to_exact(&self) -> Result<Exact, NumericError> {
        let scale = self.0.scale();
        if scale > MAX_SCALE {
            return Err(NumericError::ScaleTooLarge { scale, max: MAX_SCALE });
        }
        let mantissa = self.0.mantissa();
        let den = 10_i128
            .checked_pow(scale)
            .ok_or(NumericError::Overflow)?;
        Exact::new(mantissa, den)
    }

    /// Явное преобразование в приближённый режим.
    /// Используется **только** внутри решателей (§6.6).
    pub fn to_approx_raw(&self) -> Option<f64> {
        self.0.to_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;

    #[test]
    fn decimal_to_exact_is_lossless() {
        let d = Dec::new(Decimal::from_str("123.456").unwrap());
        let e = d.to_exact().unwrap();
        assert_eq!(e, Exact::new(123_456, 1_000).unwrap());
    }

    #[test]
    fn tenths_and_hundredths_are_exact_after_conversion() {
        let a = Dec::new(Decimal::from_str("0.1").unwrap()).to_exact().unwrap();
        let b = Dec::new(Decimal::from_str("0.2").unwrap()).to_exact().unwrap();
        let c = Dec::new(Decimal::from_str("0.3").unwrap()).to_exact().unwrap();
        assert_eq!(a.add(&b), c);
    }
}
```

> **`to_approx_raw` и заслон.** Метод возвращает `f64` и находится в `decimal.rs`, поэтому заслон из Task 3 его отклонит. Перенесите его в `approx.rs` в виде свободной функции `pub fn dec_to_f64(d: &Dec) -> Option<f64>` и удалите из `Dec`. Заслон не ослабляйте.

- [ ] **Step 7: Реализовать приближённый режим**

Создать `crates/iaam-core/src/numeric/approx.rs`:

```rust
//! Приближённый режим (§6.6): единственное место в ядре, где разрешена
//! двоичная плавающая точка.
//!
//! Применяется только там, где требуются степени, корни и итерации:
//! XIRR, CAGR, дисконтирование. Результаты этого модуля **никогда**
//! не входят в денежное тождество §6.3 — тождество проверяет суммы,
//! а не ставки.

use rust_decimal::prelude::ToPrimitive;

use super::decimal::Dec;

/// Политика численного метода. Каждый решатель обязан её объявить,
/// и она попадает в результат рядом с числом.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolverPolicy {
    /// Критерий остановки по величине невязки.
    pub tolerance: f64,
    /// Максимум итераций до отказа.
    pub max_iterations: u32,
    /// Нижняя граница локализации корня.
    pub bracket_low: f64,
    /// Верхняя граница локализации корня.
    pub bracket_high: f64,
}

impl SolverPolicy {
    /// Политика по умолчанию для расчёта ставок доходности.
    ///
    /// Локализация от −99,99 % до +10 000 % годовых покрывает любой
    /// реалистичный результат, включая полную потерю капитала.
    #[must_use]
    pub const fn returns_default() -> Self {
        Self {
            tolerance: 1e-9,
            max_iterations: 200,
            bracket_low: -0.9999,
            bracket_high: 100.0,
        }
    }
}

/// Приближённое значение вместе с оценкой погрешности.
///
/// Сконструировать без границы погрешности невозможно: значение,
/// про которое неизвестно, насколько оно точно, бесполезно для отчёта.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ApproxValue {
    value: f64,
    error_bound: f64,
    iterations: u32,
}

impl ApproxValue {
    #[must_use]
    pub const fn new(value: f64, error_bound: f64, iterations: u32) -> Self {
        Self { value, error_bound, iterations }
    }

    #[must_use]
    pub const fn value(&self) -> f64 {
        self.value
    }

    #[must_use]
    pub const fn error_bound(&self) -> f64 {
        self.error_bound
    }

    #[must_use]
    pub const fn iterations(&self) -> u32 {
        self.iterations
    }
}

/// Явный переход из денежного режима в приближённый.
/// Единственная разрешённая точка такого перехода.
#[must_use]
pub fn dec_to_f64(d: &Dec) -> Option<f64> {
    d.inner().to_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approx_value_carries_error_bound() {
        let v = ApproxValue::new(0.1234, 1e-9, 12);
        assert!(v.error_bound() > 0.0);
        assert_eq!(v.iterations(), 12);
    }

    #[test]
    fn returns_policy_brackets_total_loss_and_extreme_gain() {
        let p = SolverPolicy::returns_default();
        assert!(p.bracket_low < -0.99, "должна покрывать полную потерю капитала");
        assert!(p.bracket_high > 10.0, "должна покрывать экстремальный рост");
    }
}
```

- [ ] **Step 8: Подключить модуль**

В `crates/iaam-core/src/lib.rs` заменить содержимое на:

```rust
//! Ядро учёта инвестиций.
//!
//! Чистые синхронные функции над загруженным срезом данных.
//! Ни ввода-вывода, ни `async`, ни `Mutex`, ни зависимостей на другие
//! крейты воркспейса. См. §3.1 спецификации.

pub mod numeric;

#[cfg(test)]
mod tests {
    #[test]
    fn fixture_manifest_is_wired() {
        let raw = include_str!("../../../tests/fixtures/smoke.json");
        assert!(raw.contains("\"value\": 42"));
    }
}
```

- [ ] **Step 9: Прогнать тесты и заслоны**

```bash
nix develop -c cargo nextest run --package iaam-core
nix develop -c ./scripts/check-architecture.sh
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Ожидается: все тесты проходят, заслон архитектуры зелёный (весь `f64` в `approx.rs`).

- [ ] **Step 10: Коммит**

```bash
git add crates/iaam-core/
git commit -m "feat(core): три числовых режима — точный, денежный, приближённый (iaam-1fk)"
```

---

### Task 8: Деньги, количества и валютная типизация

**Files:**
- Create: `crates/iaam-core/src/money.rs`
- Modify: `crates/iaam-core/src/lib.rs`

**Interfaces:**
- Consumes: `numeric::{Dec, Exact, NumericError}`
- Produces:
  - `money::CurrencyCode` — исчерпаемый `enum` валют с `minor_units()`
  - `money::PostedMinor` — проведённая сумма в минимальных единицах, `i64`
  - `money::Money { amount: PostedMinor, currency: CurrencyCode }` с `try_add`, `try_sub`, `negate`, `to_exact`
  - `money::Quantity` — количество бумаг, `Dec`
  - `money::MoneyError`

**Acceptance Criteria:**
- Сложение сумм в разных валютах возвращает `Err(MoneyError::CurrencyMismatch)`, а не молча складывает
- Конверсии `Dec` → `PostedMinor` в этой задаче **нет вовсе**. Когда она понадобится, она обязана требовать явного режима округления. Не записывайте отсутствие конверсии как выполненный критерий защиты
- `Money::to_exact()` даёт точное рациональное представление
- Сумма пустого списка равна нулю в заданной валюте, а не паникует

> **Отклонение от спецификации — зафиксировано осознанно.**
>
> §15.1 требует, чтобы «`Money<Rub>` и `Money<Usd>` не складывались». Буквальная реализация — фантомные типы `Money<C: Currency>` — даёт проверку на этапе компиляции, но **несовместима с журналом фактов**: валюта приходит из данных (отчёт брокера, ввод пользователя), а не известна статически. Обобщённый по валюте тип нельзя ни положить в `enum` события, ни десериализовать без стирания.
>
> Принятое решение: валюта — **рантайм-тег**, а несложение разных валют обеспечивается тем, что `Money` **не реализует `std::ops::Add`**. Единственный способ сложить — `try_add`, возвращающий `Result`. Молча сложить рубли с долларами невозможно: `a + b` не компилируется, а `a.try_add(b)` обязывает обработать ошибку.
>
> Гарантия слабее компиляционной, но сильнее, чем в любой системе с голым `i64`. Альтернативу с фантомными типами имеет смысл добавить позже как надстройку для расчётных путей, где валюта известна статически.

- [ ] **Step 1: Написать падающие тесты**

Создать `crates/iaam-core/src/money.rs`:

```rust
//! Деньги и количества.
//!
//! Разделяются две категории величин (§3.4):
//! - **проведённые суммы** — целые в минимальных единицах, в опубликованной
//!   источником точности; это факты, их нельзя пересчитывать;
//! - **расчётные величины** — [`crate::numeric::decimal::Dec`].

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_currency_adds() {
        let a = Money::new(PostedMinor::new(100_00), CurrencyCode::Rub);
        let b = Money::new(PostedMinor::new(250_50), CurrencyCode::Rub);
        assert_eq!(a.try_add(b).unwrap(), Money::new(PostedMinor::new(350_50), CurrencyCode::Rub));
    }

    #[test]
    fn different_currencies_refuse_to_add() {
        let rub = Money::new(PostedMinor::new(100_00), CurrencyCode::Rub);
        let usd = Money::new(PostedMinor::new(100_00), CurrencyCode::Usd);
        assert!(matches!(
            rub.try_add(usd),
            Err(MoneyError::CurrencyMismatch { left: CurrencyCode::Rub, right: CurrencyCode::Usd })
        ));
    }

    #[test]
    fn sum_of_empty_is_zero_in_requested_currency() {
        let z = Money::sum(&[], CurrencyCode::Rub).unwrap();
        assert_eq!(z, Money::zero(CurrencyCode::Rub));
    }

    #[test]
    fn sum_rejects_mixed_currencies() {
        let items = [
            Money::new(PostedMinor::new(1), CurrencyCode::Rub),
            Money::new(PostedMinor::new(1), CurrencyCode::Usd),
        ];
        assert!(Money::sum(&items, CurrencyCode::Rub).is_err());
    }

    #[test]
    fn to_exact_is_scaled_by_minor_units() {
        // 350,50 ₽ == 35050/100
        let m = Money::new(PostedMinor::new(350_50), CurrencyCode::Rub);
        let e = m.to_exact().unwrap();
        assert_eq!(e, crate::numeric::exact::Exact::new(35_050, 100).unwrap());
    }

    #[test]
    fn negate_flips_sign_and_keeps_currency() {
        let m = Money::new(PostedMinor::new(5_00), CurrencyCode::Rub);
        let n = m.checked_negate().unwrap();
        assert_eq!(n.amount().raw(), -500);
        assert_eq!(n.currency(), CurrencyCode::Rub);
    }

    #[test]
    fn subtracting_from_the_minimum_does_not_panic() {
        // `-i64::MIN` не представим. Реализация через отрицание сделала бы
        // try_sub паническим при обещанном Result.
        let min = Money::new(PostedMinor::new(i64::MIN), CurrencyCode::Rub);
        let one = Money::new(PostedMinor::new(1), CurrencyCode::Rub);
        assert!(matches!(min.try_sub(one), Err(MoneyError::Overflow)));
    }
}
```

- [ ] **Step 2: Запустить, убедиться в провале**

```bash
nix develop -c cargo test --package iaam-core money
```

Ожидается: ошибки компиляции — типы не определены.

- [ ] **Step 3: Реализовать**

Дописать перед `mod tests`:

```rust
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::numeric::decimal::Dec;
use crate::numeric::exact::Exact;
use crate::numeric::NumericError;

/// Валюта. Исчерпаемый `enum`, а не строка (§15.1): добавление валюты
/// обязано сломать сборку везде, где её не обработали.
/// Атрибут `#[non_exhaustive]` намеренно **не** применяется: он запретил бы
/// исчерпывающий `match` внешним крейтам и тем самым отменил бы гарантию
/// «добавление валюты ломает сборку везде, где её не обработали» (§15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CurrencyCode {
    Rub,
    Usd,
    Eur,
    Cny,
    /// Золото в граммах — металлический счёт (§9.5).
    Xau,
}

impl CurrencyCode {
    /// Число знаков после запятой в минимальной единице.
    #[must_use]
    pub const fn minor_units(self) -> u32 {
        match self {
            Self::Rub | Self::Usd | Self::Eur | Self::Cny => 2,
            Self::Xau => 4,
        }
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Rub => "RUB",
            Self::Usd => "USD",
            Self::Eur => "EUR",
            Self::Cny => "CNY",
            Self::Xau => "XAU",
        }
    }
}

/// Проведённая сумма в минимальных единицах валюты.
///
/// Обёртка, а не голый `i64`: смешать её с количеством бумаг или
/// с расчётной величиной невозможно.
/// Поле **приватное**: публичное `pub i64` делало бы тривиальным обход
/// запрета на смешение валют — достаточно было бы сложить сырые `i64`.
/// Доступ к сырому значению даётся только точному арифметическому слою
/// и сериализации.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PostedMinor(i64);

impl PostedMinor {
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Сырое значение. Предназначено для сериализации, форматирования
    /// и перевода в точный режим — не для арифметики над деньгами.
    #[must_use]
    pub const fn raw(self) -> i64 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    pub fn checked_neg(self) -> Option<Self> {
        self.0.checked_neg().map(Self)
    }
}

/// Количество бумаг. Дробное — крипта и дробные остатки после сплитов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Quantity(pub Dec);

impl Quantity {
    #[must_use]
    pub fn zero() -> Self {
        Self(Dec::zero())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MoneyError {
    #[error("нельзя смешивать валюты: {left:?} и {right:?}")]
    CurrencyMismatch { left: CurrencyCode, right: CurrencyCode },
    #[error("переполнение при сложении сумм")]
    Overflow,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Денежная сумма с валютой.
///
/// **Намеренно не реализует `std::ops::Add`.** Сложить можно только через
/// [`Money::try_add`], который обязывает обработать несовпадение валют.
/// Это компенсация за рантайм-тег валюты вместо фантомного типа —
/// обоснование в описании задачи 8 плана.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    amount: PostedMinor,
    currency: CurrencyCode,
}

impl Money {
    #[must_use]
    pub const fn new(amount: PostedMinor, currency: CurrencyCode) -> Self {
        Self { amount, currency }
    }

    #[must_use]
    pub const fn zero(currency: CurrencyCode) -> Self {
        Self { amount: PostedMinor(0), currency }
    }

    #[must_use]
    pub const fn amount(&self) -> PostedMinor {
        self.amount
    }

    #[must_use]
    pub const fn currency(&self) -> CurrencyCode {
        self.currency
    }

    #[must_use]
    pub const fn is_zero(&self) -> bool {
        self.amount.raw() == 0
    }

    fn require_same_currency(self, other: Self) -> Result<(), MoneyError> {
        if self.currency == other.currency {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch { left: self.currency, right: other.currency })
        }
    }

    pub fn try_add(self, other: Self) -> Result<Self, MoneyError> {
        self.require_same_currency(other)?;
        let amount = self.amount.checked_add(other.amount).ok_or(MoneyError::Overflow)?;
        Ok(Self { amount, currency: self.currency })
    }

    /// Вычитание через `checked_sub`, а **не** через отрицание:
    /// `-i64::MIN` не представим, поэтому реализация через `negate`
    /// делала бы метод паническим при обещанном в сигнатуре `Result`.
    pub fn try_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.require_same_currency(other)?;
        let amount = self.amount.checked_sub(other.amount).ok_or(MoneyError::Overflow)?;
        Ok(Self { amount, currency: self.currency })
    }

    pub fn checked_negate(self) -> Result<Self, MoneyError> {
        let amount = self.amount.checked_neg().ok_or(MoneyError::Overflow)?;
        Ok(Self { amount, currency: self.currency })
    }

    /// Сумма списка. Валюта задаётся явно, чтобы пустой список давал
    /// осмысленный ноль, а не паниковал и не угадывал.
    pub fn sum(items: &[Self], currency: CurrencyCode) -> Result<Self, MoneyError> {
        items
            .iter()
            .try_fold(Self::zero(currency), |acc, item| acc.try_add(*item))
    }

    /// Точное представление: `amount / 10^minor_units`.
    pub fn to_exact(&self) -> Result<Exact, MoneyError> {
        let den = 10_i128
            .checked_pow(self.currency.minor_units())
            .ok_or(NumericError::Overflow)?;
        Ok(Exact::new(i128::from(self.amount.raw()), den)?)
    }
}
```

- [ ] **Step 4: Подключить и прогнать**

В `lib.rs` добавить `pub mod money;` после `pub mod numeric;`.

```bash
nix develop -c cargo nextest run --package iaam-core
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c ./scripts/check-architecture.sh
```

Ожидается: шесть тестов модуля `money` проходят, заслоны зелёные.

- [ ] **Step 5: Проверить мутационным тестированием**

```bash
nix develop -c cargo mutants --package iaam-core --file crates/iaam-core/src/money.rs
```

Ожидается: выживших мутантов нет. Если мутант, заменивший `!=` на `==` в проверке валют, выжил — тест `different_currencies_refuse_to_add` написан неверно, чините тест.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/
git commit -m "feat(core): деньги с рантайм-валютой без Add, количества (iaam-1fk)"
```

---

### Task 9: Семантические даты и идентичности

Спека требует шести дат (§4.2). Без раздельных типов их гарантированно перепутают — сделка 30 декабря с расчётами 3 января попадёт не в тот налоговый год.

**Files:**
- Create: `crates/iaam-core/src/dates.rs`
- Create: `crates/iaam-core/src/ids.rs`
- Modify: `crates/iaam-core/Cargo.toml`
- Modify: `crates/iaam-core/src/lib.rs`

**Interfaces:**
- Consumes: ничего
- Produces:
  - `dates::{TradeDate, SettledDate, CashPostedDate, EntitlementDate, PaidDate, TaxPeriod}` — различные типы поверх `time::Date`
  - `dates::EventDates` — набор дат события, все поля кроме одного опциональны
  - `dates::EffectiveOrder` — детерминированный порядок при равных датах
  - `ids::{OwnerId, AccountId, CustodyId, InstrumentId, SourceId, EventId, TransferId}`

**Acceptance Criteria:**
- Передать `SettledDate` туда, где ожидается `TradeDate`, невозможно — не компилируется
- **Каждая публичная функция вызвана хотя бы одним тестом.** Тестов из шага 2 недостаточно: проверено исполнением, что с ними `cargo mutants` даёт трёх выживших — удаление поля в `for_cash`, и подмена `sequence()` на `0` и на `1`. План не вызывает ни `for_cash`, ни `sequence()`, ни `date()`, ни `inner()`
- `TaxPeriod` для сделки 30.12 с расчётами 03.01 выводится из `SettledDate`, а не из `TradeDate`
- `EffectiveOrder` даёт строгий полный порядок для событий с одинаковой датой
- Идентификаторы различных сущностей не взаимозаменяемы

- [ ] **Step 1: Добавить зависимость на календарь**

`crates/iaam-core/Cargo.toml`, в `[dependencies]`:

```toml
time = { version = "0.3", default-features = false, features = ["std", "macros", "serde", "parsing", "formatting"] }
uuid = { version = "1", features = ["serde", "v4"] }
```

> `time` выбран вместо `chrono`: меньше поверхность, нет зависимости на локальную таймзону по умолчанию. Даты в системе — календарные (`Date`), а не моменты времени: операция происходит в дату, а не в момент.

- [ ] **Step 2: Написать падающие тесты дат**

Создать `crates/iaam-core/src/dates.rs`:

```rust
//! Шесть семантических дат (§4.2).
//!
//! Одной даты недостаточно: сделка 30 декабря с расчётами 3 января
//! попадает в другой налоговый год; дивиденд имеет дату отсечки и дату
//! выплаты; налог имеет дату удержания и период, к которому относится.

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn tax_period_follows_settlement_not_trade() {
        let dates = EventDates::for_trade(
            TradeDate(date!(2025 - 12 - 30)),
            Some(SettledDate(date!(2026 - 01 - 03))),
        );
        assert_eq!(dates.tax_period(), Some(TaxPeriod(2026)));
    }

    #[test]
    fn tax_period_falls_back_to_trade_when_settlement_unknown() {
        let dates = EventDates::for_trade(TradeDate(date!(2025 - 12 - 30)), None);
        assert_eq!(dates.tax_period(), Some(TaxPeriod(2025)));
    }

    #[test]
    fn effective_order_is_total_for_same_date() {
        let a = EffectiveOrder::new(date!(2026 - 03 - 01), 0);
        let b = EffectiveOrder::new(date!(2026 - 03 - 01), 1);
        assert!(a < b);
        assert_ne!(a, b);
    }

    #[test]
    fn effective_order_sorts_by_date_first() {
        let earlier_high_seq = EffectiveOrder::new(date!(2026 - 03 - 01), 99);
        let later_low_seq = EffectiveOrder::new(date!(2026 - 03 - 02), 0);
        assert!(earlier_high_seq < later_low_seq);
    }
}
```

- [ ] **Step 3: Реализовать даты**

Дописать перед `mod tests`:

```rust
use serde::{Deserialize, Serialize};
use time::Date;

/// Макрос объявления типизированной даты.
///
/// Каждая дата — отдельный тип, поэтому передать одну вместо другой
/// невозможно. Это первый слой проверки (§15.1).
macro_rules! typed_date {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub Date);

        impl $name {
            #[must_use]
            pub const fn inner(&self) -> Date {
                self.0
            }
        }
    };
}

typed_date!(
    /// Дата заключения сделки.
    TradeDate
);
typed_date!(
    /// Дата расчётов и перехода прав.
    SettledDate
);
typed_date!(
    /// Дата движения денег по счёту.
    CashPostedDate
);
typed_date!(
    /// Дата, определяющая право на выплату (отсечка).
    EntitlementDate
);
typed_date!(
    /// Дата фактической выплаты.
    PaidDate
);

/// Налоговый период — календарный год, к которому относится событие.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaxPeriod(pub i32);

/// Набор дат события. Заполнены не все — это нормально (§4.9),
/// но схема обязана их допускать без переинтерпретации.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDates {
    pub trade: Option<TradeDate>,
    pub settled: Option<SettledDate>,
    pub cash_posted: Option<CashPostedDate>,
    pub entitlement: Option<EntitlementDate>,
    pub paid: Option<PaidDate>,
    /// Явно заданный налоговый период. Если `None` — выводится
    /// правилом [`EventDates::tax_period`].
    pub tax_period_override: Option<TaxPeriod>,
}

impl EventDates {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            trade: None,
            settled: None,
            cash_posted: None,
            entitlement: None,
            paid: None,
            tax_period_override: None,
        }
    }

    #[must_use]
    pub const fn for_trade(trade: TradeDate, settled: Option<SettledDate>) -> Self {
        Self { trade: Some(trade), settled, ..Self::empty() }
    }

    #[must_use]
    pub const fn for_cash(posted: CashPostedDate) -> Self {
        Self { cash_posted: Some(posted), ..Self::empty() }
    }

    /// Дата, по которой событие попадает в отчётный период.
    ///
    /// Приоритет: расчёты → движение денег → выплата → сделка.
    /// Расчёты важнее сделки, потому что права переходят при расчётах.
    #[must_use]
    pub fn effective_date(&self) -> Option<Date> {
        self.settled
            .map(|d| d.0)
            .or_else(|| self.cash_posted.map(|d| d.0))
            .or_else(|| self.paid.map(|d| d.0))
            .or_else(|| self.trade.map(|d| d.0))
    }

    /// Налоговый период события.
    ///
    /// Сделка 30 декабря с расчётами 3 января относится к следующему году —
    /// именно поэтому одной даты недостаточно.
    #[must_use]
    pub fn tax_period(&self) -> Option<TaxPeriod> {
        self.tax_period_override
            .or_else(|| self.effective_date().map(|d| TaxPeriod(d.year())))
    }
}

/// Детерминированный порядок событий.
///
/// При одинаковой дате порядок задаёт `sequence`, а не порядок импорта —
/// иначе проекция зависела бы от того, в каком порядке загрузили файлы,
/// и инвариант детерминизма (§15.3) не выполнялся бы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EffectiveOrder {
    date: Date,
    sequence: u32,
}

impl EffectiveOrder {
    #[must_use]
    pub const fn new(date: Date, sequence: u32) -> Self {
        Self { date, sequence }
    }

    #[must_use]
    pub const fn date(&self) -> Date {
        self.date
    }

    #[must_use]
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }
}
```

> Порядок полей в `derive(PartialOrd, Ord)` значим: `date` объявлена первой, поэтому сортировка идёт сначала по дате, затем по номеру. Тест `effective_order_sorts_by_date_first` это фиксирует — не меняйте порядок полей.

- [ ] **Step 4: Реализовать идентичности**

Создать `crates/iaam-core/src/ids.rs`:

```rust
//! Раздельные идентичности (§4.5).
//!
//! Брокерский счёт не является одновременно владельцем, денежным счётом
//! и местом хранения бумаг: перевод бумаг между депозитариями внутри
//! одного брокера — реальная операция.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            #[must_use]
            pub fn new_random() -> Self {
                Self(Uuid::new_v4())
            }

            #[must_use]
            pub const fn inner(&self) -> Uuid {
                self.0
            }
        }
    };
}

typed_id!(
    /// Владелец портфеля.
    OwnerId
);
typed_id!(
    /// Денежный счёт: брокерский, банковский, вклад, кошелёк.
    AccountId
);
typed_id!(
    /// Место хранения бумаг (депозитарий, субсчёт).
    CustodyId
);
typed_id!(
    /// Инструмент.
    InstrumentId
);
typed_id!(
    /// Источник данных: конкретный отчёт, синхронизация, ручной ввод.
    SourceId
);
typed_id!(
    /// Событие журнала.
    EventId
);
typed_id!(
    /// Перевод денег между счетами. Связывает обе стороны движения:
    /// без него классификатор контура не знает второй счёт (§4.10).
    TransferId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_of_different_kinds_are_distinct_types() {
        // Несовместимость типов проверяется только вручную: строка ниже
        // даёт E0308 (expected AccountId, found OwnerId), но постоянного
        // заслона на это НЕТ — нужен trybuild, которого в этом плане
        // не появляется. Не считайте закомментированную строку проверкой.
        // let _: AccountId = OwnerId::new_random();
        let a = AccountId::new_random();
        let b = AccountId::new_random();
        assert_ne!(a, b, "два случайных идентификатора не совпадают");
    }
}
```

- [ ] **Step 5: Прогнать**

`pub mod dates;` и `pub mod ids;` добавляются в `lib.rs` **на шагах 2 и 4**,
вместе с созданием файлов — иначе шаг «убедитесь, что тест падает» даёт
зелёный прогон ни о чём.

**Допишите тесты на функции, которых нет в шаге 2:** `for_cash`,
`EffectiveOrder::sequence()`, `date()`, `inner()` типизированных дат
и идентификаторов. Без них мутационный заслон даёт выживших.

```bash
nix develop -c cargo nextest run --package iaam-core
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Ожидается: тесты дат и идентичностей проходят.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/
git commit -m "feat(core): шесть семантических дат и раздельные идентичности (iaam-1fk)"
```

---

### Task 10: Ноги движения, происхождение и envelope события

Центральная задача необратимого ядра. Событие не является одной неразложимой суммой (§4.3): без разложения на ноги амортизация, НКД и разнесение комиссии невосстановимы.

**Files:**
- Create: `crates/iaam-core/src/event/mod.rs`
- Create: `crates/iaam-core/src/event/leg.rs`
- Create: `crates/iaam-core/src/event/provenance.rs`
- Create: `crates/iaam-core/src/event/kind.rs`
- Modify: `crates/iaam-core/src/lib.rs`

**Interfaces:**
- Consumes: `money::{Money, Quantity, CurrencyCode}`, `dates::{EventDates, EffectiveOrder}`, `ids::*`, `numeric::exact::Exact`
- Produces:
  - `event::leg::{Leg, LegKind}` — `Leg::cash`, `Leg::security`, `Leg::principal`, `Leg::fee`, `Leg::tax`
  - `event::provenance::{Provenance, RawHash, ParserVersion, RowLocator}`
  - `event::kind::EventKind` — исчерпаемое семейство типов событий этапа 1
  - `event::{Event, Confidence}` — envelope
  - `Event::validate_structure() -> Result<(), EventValidationError>` — проверка формы по типу события
  - `event::kind::FlowEndpoints` — конечные точки денежного движения

**Acceptance Criteria:**
- Событие покупки раскладывается минимум на две ноги: денежную (списание) и бумажную (зачисление)
- Покупка с положительным знаком денежной ноги **отклоняется** — это класс ошибок, который пропускало бы общее правило «сумма ног равна нулю»
- Расчётная сумма сделки учитывает НКД и комиссию с верным знаком для покупки и продажи
- Комиссия с одной фактической ногой **проходит** проверку: контрсчёта расхода в модели нет
- Перевод требует двух встречных ног на объявленных счетах
- `Provenance` невозможно сконструировать без хеша сырья и версии парсера
- Добавление варианта в `EventKind` ломает сборку во всех местах разбора
- `Confidence::Unknown` представимо, нулевой заглушки нет

- [ ] **Step 1: Реализовать ноги движения**

Создать `crates/iaam-core/src/event/leg.rs`:

```rust
//! Типизированные ноги движения (§4.3).
//!
//! Событие раскладывается на ноги, а не хранится одной суммой:
//! иначе амортизация номинала, НКД и разнесение комиссии
//! невосстановимы из записанного факта.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, CustodyId, InstrumentId};
use crate::money::{Money, Quantity};

/// Что именно движется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegKind {
    /// Движение денег по счёту.
    Cash,
    /// Движение количества бумаг.
    SecurityQuantity,
    /// Движение непогашенного номинала (амортизация).
    Principal,
    /// Комиссия.
    Fee,
    /// Налог.
    Tax,
}

/// Одна нога движения.
///
/// Знак задаёт направление: положительный — приход в указанный счёт
/// или custody, отрицательный — расход.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leg {
    pub kind: LegKind,
    pub account: AccountId,
    pub custody: Option<CustodyId>,
    pub instrument: Option<InstrumentId>,
    pub money: Option<Money>,
    pub quantity: Option<Quantity>,
}

impl Leg {
    #[must_use]
    pub fn cash(account: AccountId, money: Money) -> Self {
        Self {
            kind: LegKind::Cash,
            account,
            custody: None,
            instrument: None,
            money: Some(money),
            quantity: None,
        }
    }

    #[must_use]
    pub fn security(
        account: AccountId,
        custody: CustodyId,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> Self {
        Self {
            kind: LegKind::SecurityQuantity,
            account,
            custody: Some(custody),
            instrument: Some(instrument),
            money: None,
            quantity: Some(quantity),
        }
    }

    #[must_use]
    pub fn fee(account: AccountId, money: Money) -> Self {
        Self {
            kind: LegKind::Fee,
            account,
            custody: None,
            instrument: None,
            money: Some(money),
            quantity: None,
        }
    }

    #[must_use]
    pub fn tax(account: AccountId, money: Money) -> Self {
        Self {
            kind: LegKind::Tax,
            account,
            custody: None,
            instrument: None,
            money: Some(money),
            quantity: None,
        }
    }

    #[must_use]
    pub fn principal(
        account: AccountId,
        instrument: InstrumentId,
        money: Money,
    ) -> Self {
        Self {
            kind: LegKind::Principal,
            account,
            custody: None,
            instrument: Some(instrument),
            money: Some(money),
            quantity: None,
        }
    }

    /// Денежное содержание ноги, если оно есть.
    /// Комиссия и налог — тоже деньги: они уменьшают денежный остаток.
    #[must_use]
    pub fn cash_effect(&self) -> Option<Money> {
        match self.kind {
            LegKind::Cash | LegKind::Fee | LegKind::Tax | LegKind::Principal => self.money,
            LegKind::SecurityQuantity => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{CurrencyCode, PostedMinor};

    #[test]
    fn security_leg_has_no_cash_effect() {
        let leg = Leg::security(
            AccountId::new_random(),
            CustodyId::new_random(),
            InstrumentId::new_random(),
            Quantity::zero(),
        );
        assert!(leg.cash_effect().is_none());
    }

    #[test]
    fn fee_leg_counts_as_cash_effect() {
        let m = Money::new(PostedMinor::new(-35), CurrencyCode::Rub);
        let leg = Leg::fee(AccountId::new_random(), m);
        assert_eq!(leg.cash_effect(), Some(m));
    }
}
```

- [ ] **Step 2: Реализовать происхождение**

Создать `crates/iaam-core/src/event/provenance.rs`:

```rust
//! Происхождение факта (§4.1).
//!
//! Восстановить эти данные позже невозможно, поэтому они обязательны
//! с первого коммита (§16.1).

use serde::{Deserialize, Serialize};

use crate::ids::SourceId;

/// Хеш сырой записи источника. Шестнадцатеричная строка SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RawHash(String);

impl RawHash {
    /// Принимает только корректный шестнадцатеричный SHA-256.
    pub fn parse(hex: &str) -> Option<Self> {
        let ok = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
        ok.then(|| Self(hex.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParserVersion(pub String);

/// Указание на конкретную строку исходного документа.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowLocator {
    pub document: String,
    pub sheet: Option<String>,
    pub row: u64,
}

/// Происхождение. Сконструировать без хеша сырья и версии парсера нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    source: SourceId,
    raw_hash: RawHash,
    parser_version: ParserVersion,
    source_operation_id: Option<String>,
    row: Option<RowLocator>,
}

impl Provenance {
    #[must_use]
    pub fn new(source: SourceId, raw_hash: RawHash, parser_version: ParserVersion) -> Self {
        Self { source, raw_hash, parser_version, source_operation_id: None, row: None }
    }

    #[must_use]
    pub fn with_source_operation_id(mut self, id: impl Into<String>) -> Self {
        self.source_operation_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_row(mut self, row: RowLocator) -> Self {
        self.row = Some(row);
        self
    }

    #[must_use]
    pub const fn source(&self) -> SourceId {
        self.source
    }

    #[must_use]
    pub const fn raw_hash(&self) -> &RawHash {
        &self.raw_hash
    }

    #[must_use]
    pub fn source_operation_id(&self) -> Option<&str> {
        self.source_operation_id.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_hash_rejects_malformed_input() {
        assert!(RawHash::parse("не хеш").is_none());
        assert!(RawHash::parse("abc").is_none());
        assert!(RawHash::parse(&"a".repeat(64)).is_some());
    }

    #[test]
    fn raw_hash_is_normalised_to_lowercase() {
        let h = RawHash::parse(&"A".repeat(64)).unwrap();
        assert_eq!(h.as_str(), "a".repeat(64));
    }
}
```

- [ ] **Step 3: Реализовать семейство типов событий**

Создать `crates/iaam-core/src/event/kind.rs`:

```rust
//! Семейство типов событий (§4.6).
//!
//! На этапе 1 реализовано подмножество, достаточное для ручного ввода
//! и расчёта XIRR до налога. Остальные варианты добавляются на своих
//! этапах — добавление варианта обязано сломать сборку везде, где
//! разбор не полон.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, InstrumentId, TransferId};
use crate::money::{Money, Quantity};

/// Направление сделки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide {
    Buy,
    Sell,
}

/// Тип события. Исчерпаемый — `#[non_exhaustive]` намеренно **не**
/// применяется: внешних потребителей у ядра нет, а исчерпаемость даёт
/// проверку полноты разбора (§15.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    /// Покупка или продажа.
    Trade {
        side: TradeSide,
        instrument: InstrumentId,
        quantity: Quantity,
        gross: Money,
        fee: Option<Money>,
        /// НКД, уплаченный продавцу или полученный от покупателя (§7.2).
        accrued_interest: Option<Money>,
    },
    /// Деньги вошли в контур извне (§4.10).
    CashIn { amount: Money },
    /// Деньги вышли из контура.
    CashOut { amount: Money },
    /// Движение денег между счетами.
    ///
    /// **Оба счёта хранятся в самом событии.** Классификация относительно
    /// контура невозможна без второго счёта: перевод с внешнего вклада на
    /// внутренний брокерский счёт — внешний поток, а между двумя внутренними
    /// счетами — нет. Событие необратимо, поэтому недостающая семантика
    /// здесь означала бы миграцию журнала позже (§16.1).
    CashTransfer {
        transfer_id: TransferId,
        from: AccountId,
        to: AccountId,
        amount: Money,
    },
    /// Купон, дивиденд, фактически выплаченные проценты.
    Income { instrument: Option<InstrumentId>, gross: Money },
    /// Комиссия, не привязанная к сделке.
    Fee { amount: Money, origin: FeeOrigin },
    /// Восстановленная позиция для счёта без истории (§10.7).
    OpeningPosition {
        instrument: InstrumentId,
        quantity: Quantity,
        cost_basis: Option<Money>,
    },
    /// Восстановленный денежный остаток.
    OpeningCash { amount: Money },
}

/// Происхождение комиссии. Нужно уже на этапе 1, потому что проценты
/// по марже импортируются как комиссия с пометкой (§11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeeOrigin {
    Brokerage,
    Depositary,
    AccountMaintenance,
    /// Проценты по марже. Позиция вне периметра, но денежный эффект сохраняется.
    MarginInterest,
    Other,
}

impl EventKind {
    /// Короткое машиночитаемое имя. Используется в API и хранилище.
    ///
    /// Реализовано исчерпывающим `match` без ветки `_`: добавление
    /// варианта обязано сломать сборку здесь.
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::Trade { .. } => "trade",
            Self::CashIn { .. } => "cash_in",
            Self::CashOut { .. } => "cash_out",
            Self::CashTransfer { .. } => "cash_transfer",
            Self::Income { .. } => "income",
            Self::Fee { .. } => "fee",
            Self::OpeningPosition { .. } => "opening_position",
            Self::OpeningCash { .. } => "opening_cash",
        }
    }

    /// Куда и откуда движутся деньги.
    ///
    /// Само по себе событие **не знает**, пересекает ли оно границу контура:
    /// это свойство пары «событие + определение контура». Классификацию
    /// делает [`crate::contour::classify`], а здесь описываются только
    /// конечные точки движения.
    #[must_use]
    pub const fn flow_endpoints(&self) -> FlowEndpoints {
        match self {
            Self::CashIn { .. } => FlowEndpoints::InboundFromOutside,
            Self::CashOut { .. } => FlowEndpoints::OutboundToOutside,
            Self::CashTransfer { from, to, .. } => {
                FlowEndpoints::BetweenAccounts { from: *from, to: *to }
            }
            Self::Trade { .. }
            | Self::Income { .. }
            | Self::Fee { .. }
            | Self::OpeningPosition { .. }
            | Self::OpeningCash { .. } => FlowEndpoints::WithinAccount,
        }
    }
}

/// Конечные точки денежного движения события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowEndpoints {
    /// Деньги пришли от контрагента, которого система не наблюдает.
    InboundFromOutside,
    /// Деньги ушли контрагенту, которого система не наблюдает.
    OutboundToOutside,
    /// Движение между двумя известными счетами.
    BetweenAccounts { from: AccountId, to: AccountId },
    /// Движение внутри одного счёта: покупка, купон, комиссия.
    WithinAccount,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{CurrencyCode, PostedMinor};

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    #[test]
    fn external_cash_has_outside_endpoints() {
        assert_eq!(
            EventKind::CashIn { amount: rub(1) }.flow_endpoints(),
            FlowEndpoints::InboundFromOutside
        );
        assert_eq!(
            EventKind::CashOut { amount: rub(1) }.flow_endpoints(),
            FlowEndpoints::OutboundToOutside
        );
    }

    #[test]
    fn transfer_reports_both_accounts() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(100_000_00),
        };
        assert_eq!(kind.flow_endpoints(), FlowEndpoints::BetweenAccounts { from, to });
    }

    #[test]
    fn buying_a_security_stays_within_the_account() {
        let kind = EventKind::Trade {
            side: TradeSide::Buy,
            instrument: InstrumentId::new_random(),
            quantity: Quantity::zero(),
            gross: rub(50_000_00),
            fee: None,
            accrued_interest: None,
        };
        assert_eq!(kind.flow_endpoints(), FlowEndpoints::WithinAccount);
    }

    #[test]
    fn income_stays_within_the_account() {
        assert_eq!(
            EventKind::Income { instrument: None, gross: rub(1_000_00) }.flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
    }
}
```

- [ ] **Step 4: Реализовать envelope**

Создать `crates/iaam-core/src/event/mod.rs`:

```rust
//! Envelope события журнала (§4.1).

pub mod kind;
pub mod leg;
pub mod provenance;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dates::{EffectiveOrder, EventDates};
use crate::ids::{AccountId, EventId, OwnerId};
use crate::money::{CurrencyCode, Money, MoneyError};
use kind::EventKind;
use leg::Leg;
use provenance::Provenance;

/// Уверенность в записанном факте (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    /// Факт подтверждён источником.
    Known,
    /// Значение восстановлено или оценено.
    Estimated,
    /// Значение неизвестно и не должно подставляться нулём.
    Unknown,
}

/// Связь с другим событием (§4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Relation {
    /// Самостоятельное событие.
    None,
    /// Сторнирование указанного события.
    Reversal { target: EventId },
    /// Замена указанного события. Всегда идёт после сторнирования.
    Replacement { target: EventId },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EventValidationError {
    #[error("для {kind} ожидалось: {expected}; найдено ног: {found}")]
    LegCount { kind: &'static str, expected: &'static str, found: usize },
    #[error("для {kind} знак денежной ноги неверен: {amount} в {currency:?}")]
    WrongSign { kind: &'static str, amount: i64, currency: CurrencyCode },
    #[error("сумма ног ({legs}) не совпадает с суммой события ({declared}) для {kind}")]
    AmountMismatch { kind: &'static str, legs: i64, declared: i64 },
    #[error("нога отнесена не к тому счёту: ожидался {expected:?}")]
    WrongAccount { expected: AccountId },
    #[error("две стороны перевода не сходятся: остаток {residual}")]
    TransferResidual { residual: i64 },
    #[error(transparent)]
    Money(#[from] MoneyError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sign {
    Positive,
    Negative,
    Any,
}

/// Факт журнала. Неизменяем после записи.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub schema_version: u32,
    pub owner: OwnerId,
    pub account: AccountId,
    pub kind: EventKind,
    pub dates: EventDates,
    pub order: EffectiveOrder,
    pub legs: Vec<Leg>,
    pub provenance: Provenance,
    pub relation: Relation,
    pub confidence: Confidence,
    /// Ключ идемпотентности от клиента (§10.6).
    pub idempotency_key: Option<String>,
}

/// Текущая версия схемы события.
pub const SCHEMA_VERSION: u32 = 1;

impl Event {
    /// Сумма денежного эффекта всех ног в указанной валюте.
    pub fn cash_effect(&self, currency: CurrencyCode) -> Result<Money, MoneyError> {
        let amounts: Vec<Money> = self
            .legs
            .iter()
            .filter_map(Leg::cash_effect)
            .filter(|m| m.currency() == currency)
            .collect();
        Money::sum(&amounts, currency)
    }

    fn cash_legs(&self) -> Vec<&Leg> {
        self.legs.iter().filter(|l| matches!(l.kind, leg::LegKind::Cash)).collect()
    }

    fn security_legs(&self) -> Vec<&Leg> {
        self.legs
            .iter()
            .filter(|l| matches!(l.kind, leg::LegKind::SecurityQuantity))
            .collect()
    }

    /// Структурная проверка события (§15.2).
    ///
    /// **Не является бухгалтерским балансом.** Ноги события не образуют
    /// двойную запись: контрсчетов капитала, дохода и расхода у них нет.
    /// Поэтому единого правила «сумма ног равна нулю» не существует —
    /// комиссия, записанная одной фактической ногой, никогда не даст ноль,
    /// и это корректно. У каждого типа события своя форма, она и проверяется.
    pub fn validate_structure(&self) -> Result<(), EventValidationError> {
        let name = self.kind.discriminant();
        match &self.kind {
            EventKind::CashIn { amount } => self.expect_single_cash(name, *amount, Sign::Positive),
            EventKind::CashOut { amount } => self.expect_single_cash(name, *amount, Sign::Negative),
            EventKind::OpeningCash { amount } => self.expect_single_cash(name, *amount, Sign::Any),
            EventKind::Income { gross, .. } => {
                self.expect_single_cash(name, *gross, Sign::Positive)
            }
            EventKind::Fee { amount, .. } => {
                let fee_legs: Vec<&Leg> = self
                    .legs
                    .iter()
                    .filter(|l| matches!(l.kind, leg::LegKind::Fee))
                    .collect();
                let money = single_leg_money(name, &fee_legs, "ровно одна нога комиссии")?;
                if money.amount().raw() >= 0 {
                    return Err(EventValidationError::WrongSign {
                        kind: name,
                        amount: money.amount().raw(),
                        currency: money.currency(),
                    });
                }
                require_equal(name, money, *amount)
            }
            EventKind::CashTransfer { from, to, amount, .. } => {
                let legs = self.cash_legs();
                if legs.len() != 2 {
                    return Err(EventValidationError::LegCount {
                        kind: name,
                        expected: "ровно две денежные ноги",
                        found: legs.len(),
                    });
                }
                let out = legs
                    .iter()
                    .find(|l| l.account == *from)
                    .ok_or(EventValidationError::WrongAccount { expected: *from })?;
                let inn = legs
                    .iter()
                    .find(|l| l.account == *to)
                    .ok_or(EventValidationError::WrongAccount { expected: *to })?;
                let out_money = leg_money(name, out)?;
                let in_money = leg_money(name, inn)?;
                let residual = out_money.try_add(in_money)?;
                if !residual.is_zero() {
                    return Err(EventValidationError::TransferResidual {
                        residual: residual.amount().raw(),
                    });
                }
                require_equal(name, in_money, *amount)
            }
            EventKind::Trade { side, gross, fee, accrued_interest, .. } => {
                let cash = self.cash_legs();
                let cash_money = single_leg_money(name, &cash, "ровно одна денежная нога")?;
                let sec = self.security_legs();
                if sec.len() != 1 {
                    return Err(EventValidationError::LegCount {
                        kind: name,
                        expected: "ровно одна бумажная нога",
                        found: sec.len(),
                    });
                }
                // Расчётная сумма: тело плюс НКД, затем комиссия —
                // при покупке прибавляется, при продаже вычитается (§7.2).
                let mut settlement = *gross;
                if let Some(ai) = accrued_interest {
                    settlement = settlement.try_add(*ai)?;
                }
                let expected = match side {
                    TradeSide::Buy => {
                        let with_fee = match fee {
                            Some(f) => settlement.try_add(*f)?,
                            None => settlement,
                        };
                        with_fee.checked_negate()?
                    }
                    TradeSide::Sell => match fee {
                        Some(f) => settlement.try_sub(*f)?,
                        None => settlement,
                    },
                };
                require_equal(name, cash_money, expected)
            }
            EventKind::OpeningPosition { .. } => {
                let cash = self.cash_legs();
                if !cash.is_empty() {
                    return Err(EventValidationError::LegCount {
                        kind: name,
                        expected: "ни одной денежной ноги",
                        found: cash.len(),
                    });
                }
                let sec = self.security_legs();
                if sec.len() != 1 {
                    return Err(EventValidationError::LegCount {
                        kind: name,
                        expected: "ровно одна бумажная нога",
                        found: sec.len(),
                    });
                }
                Ok(())
            }
        }
    }

    fn expect_single_cash(
        &self,
        name: &'static str,
        declared: Money,
        sign: Sign,
    ) -> Result<(), EventValidationError> {
        let legs = self.cash_legs();
        let money = single_leg_money(name, &legs, "ровно одна денежная нога")?;
        let raw = money.amount().raw();
        let ok = match sign {
            Sign::Positive => raw > 0,
            Sign::Negative => raw < 0,
            Sign::Any => true,
        };
        if !ok {
            return Err(EventValidationError::WrongSign {
                kind: name,
                amount: raw,
                currency: money.currency(),
            });
        }
        require_equal(name, money, declared)
    }
}

fn leg_money(name: &'static str, leg: &Leg) -> Result<Money, EventValidationError> {
    leg.money.ok_or(EventValidationError::LegCount {
        kind: name,
        expected: "нога с указанной суммой",
        found: 0,
    })
}

fn single_leg_money(
    name: &'static str,
    legs: &[&Leg],
    expected: &'static str,
) -> Result<Money, EventValidationError> {
    if legs.len() != 1 {
        return Err(EventValidationError::LegCount { kind: name, expected, found: legs.len() });
    }
    leg_money(name, legs[0])
}

fn require_equal(
    name: &'static str,
    leg: Money,
    declared: Money,
) -> Result<(), EventValidationError> {
    if leg.currency() != declared.currency() {
        return Err(EventValidationError::Money(MoneyError::CurrencyMismatch {
            left: leg.currency(),
            right: declared.currency(),
        }));
    }
    if leg.amount().raw() != declared.amount().raw() {
        return Err(EventValidationError::AmountMismatch {
            kind: name,
            legs: leg.amount().raw(),
            declared: declared.amount().raw(),
        });
    }
    Ok(())
}
```

> **Почему не единый баланс ног.** Прежняя редакция освобождала от проверки
> всё, что имеет бумажную ногу или пересекает границу контура. Такое
> освобождение пропускало покупку с неверным знаком денег, продажу без
> денежного прихода, двойную комиссию и НКД, не вошедший в расчётную сумму, —
> и одновременно **отвергало корректную комиссию с одной ногой**. Форма
> события зависит от его типа, поэтому проверяется форма, а не общая сумма.

- [ ] **Step 5: Написать тесты envelope**

Дописать в конец `crates/iaam-core/src/event/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::kind::{FeeOrigin, TradeSide};
    use super::provenance::{ParserVersion, RawHash};
    use super::*;
    use crate::dates::CashPostedDate;
    use crate::ids::{CustodyId, InstrumentId, SourceId, TransferId};
    use crate::money::{PostedMinor, Quantity};
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn event(kind: EventKind, legs: Vec<Leg>, account: AccountId) -> Event {
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 03 - 01))),
            order: EffectiveOrder::new(date!(2026 - 03 - 01), 0),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"a".repeat(64)).unwrap(),
                ParserVersion("manual/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    #[test]
    fn fee_with_a_single_negative_leg_is_valid() {
        // Комиссия — одна фактическая нога. Сумма ног в ноль не сходится,
        // и это корректно: контрсчёта расхода в модели нет.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee { amount: rub(-35_00), origin: FeeOrigin::Brokerage },
            vec![Leg::fee(acc, rub(-35_00))],
            acc,
        );
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn positive_fee_is_rejected() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee { amount: rub(35_00), origin: FeeOrigin::Brokerage },
            vec![Leg::fee(acc, rub(35_00))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));
    }

    #[test]
    fn cash_in_must_be_positive_and_match_the_declared_amount() {
        let acc = AccountId::new_random();
        let ok = event(
            EventKind::CashIn { amount: rub(50_000_00) },
            vec![Leg::cash(acc, rub(50_000_00))],
            acc,
        );
        assert!(ok.validate_structure().is_ok());

        let negative = event(
            EventKind::CashIn { amount: rub(-50_000_00) },
            vec![Leg::cash(acc, rub(-50_000_00))],
            acc,
        );
        assert!(matches!(
            negative.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));

        let mismatched = event(
            EventKind::CashIn { amount: rub(50_000_00) },
            vec![Leg::cash(acc, rub(49_000_00))],
            acc,
        );
        assert!(matches!(
            mismatched.validate_structure(),
            Err(EventValidationError::AmountMismatch { .. })
        ));
    }

    #[test]
    fn transfer_requires_two_matching_sides() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(100_000_00),
        };
        let ok = event(
            kind.clone(),
            vec![Leg::cash(from, rub(-100_000_00)), Leg::cash(to, rub(100_000_00))],
            from,
        );
        assert!(ok.validate_structure().is_ok());

        let lopsided = event(
            kind,
            vec![Leg::cash(from, rub(-100_000_00)), Leg::cash(to, rub(99_000_00))],
            from,
        );
        assert!(matches!(
            lopsided.validate_structure(),
            Err(EventValidationError::TransferResidual { residual: -100_00 })
        ));
    }

    /// Именно этот класс ошибок пропускало прежнее «освобождение
    /// событий с бумажной ногой» от проверки.
    #[test]
    fn buy_with_the_wrong_cash_sign_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let kind = EventKind::Trade {
            side: TradeSide::Buy,
            instrument,
            quantity: Quantity::zero(),
            gross: rub(50_000_00),
            fee: Some(rub(35_00)),
            accrued_interest: None,
        };
        // Покупка обязана списывать деньги: −50 035,00.
        let wrong = event(
            kind.clone(),
            vec![
                Leg::cash(acc, rub(50_035_00)),
                Leg::security(acc, CustodyId::new_random(), instrument, Quantity::zero()),
            ],
            acc,
        );
        assert!(matches!(
            wrong.validate_structure(),
            Err(EventValidationError::AmountMismatch { .. })
        ));

        let right = event(
            kind,
            vec![
                Leg::cash(acc, rub(-50_035_00)),
                Leg::security(acc, CustodyId::new_random(), instrument, Quantity::zero()),
            ],
            acc,
        );
        assert!(right.validate_structure().is_ok());
    }

    #[test]
    fn buy_settlement_includes_accrued_interest() {
        // НКД платится продавцу сверх тела: 50 000 + 1 200 + 35 = 51 235.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(50_000_00),
                fee: Some(rub(35_00)),
                accrued_interest: Some(rub(1_200_00)),
            },
            vec![
                Leg::cash(acc, rub(-51_235_00)),
                Leg::security(acc, CustodyId::new_random(), instrument, Quantity::zero()),
            ],
            acc,
        );
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn sell_settlement_subtracts_the_fee() {
        // Продажа: 50 000 + НКД 1 200 − комиссия 35 = 51 165 приходит.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: Quantity::zero(),
                gross: rub(50_000_00),
                fee: Some(rub(35_00)),
                accrued_interest: Some(rub(1_200_00)),
            },
            vec![
                Leg::cash(acc, rub(51_165_00)),
                Leg::security(acc, CustodyId::new_random(), instrument, Quantity::zero()),
            ],
            acc,
        );
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn trade_without_a_security_leg_is_rejected() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: InstrumentId::new_random(),
                quantity: Quantity::zero(),
                gross: rub(50_000_00),
                fee: None,
                accrued_interest: None,
            },
            vec![Leg::cash(acc, rub(-50_000_00))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
    }
}
```

- [ ] **Step 6: Подключить, прогнать, проверить мутациями**

В `lib.rs` добавить `pub mod event;`.

```bash
nix develop -c cargo nextest run --package iaam-core
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c ./scripts/check-architecture.sh
nix develop -c cargo mutants --package iaam-core --file crates/iaam-core/src/event/mod.rs
nix develop -c cargo mutants --package iaam-core --file crates/iaam-core/src/event/kind.rs
```

Ожидается: тесты зелёные; мутанты в `validate_structure` и `flow_endpoints` не выживают. Особое внимание — мутантам, меняющим знак сравнения в `expect_single_cash` и направление комиссии в расчёте суммы сделки.

- [ ] **Step 7: Коммит**

```bash
git add crates/iaam-core/
git commit -m "feat(core): envelope события, ноги движения, происхождение, семейство типов (iaam-1fk)"
```

---

### Task 11: Семантика исправлений

События неизменяемы (§4.8). Модель — сторнирование плюс замена. Ключевое требование: результат **не зависит от порядка импорта**, иначе инвариант детерминизма (§15.3) не выполняется, а два одинаковых журнала дадут разные цифры.

**Files:**
- Create: `crates/iaam-core/src/event/correction.rs`
- Modify: `crates/iaam-core/src/event/mod.rs`

**Interfaces:**
- Consumes: `event::{Event, Relation}`, `ids::EventId`, `dates::EffectiveOrder`
- Produces:
  - `event::correction::resolve(events: &[Event]) -> Result<Vec<&Event>, CorrectionError>` — отбирает действующие события, отбрасывая сторнированные и заменённые
  - `event::correction::CorrectionError`

**Acceptance Criteria:**
- Событие плюс его сторно дают пустой действующий набор
- Замена вытесняет исходное событие, но обе записи остаются в журнале
- Перестановка входного среза не меняет результат `resolve`
- Две конкурирующие замены одного события — ошибка `ConflictingReplacements`, а не молчаливый выбор
- Ссылка на несуществующее событие — ошибка `DanglingTarget`, а не тихий пропуск

- [ ] **Step 1: Написать падающие тесты**

Создать `crates/iaam-core/src/event/correction.rs` с тестами:

```rust
//! Исправления (§4.8): сторнирование плюс замена.
//!
//! Журнал append-only, поэтому исправление не стирает исходное событие,
//! а добавляет новое со ссылкой. Проекция строится по действующему
//! набору, который вычисляет [`resolve`].

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::test_support::{sample_event, sample_event_with};
    use crate::event::Relation;

    #[test]
    fn plain_event_is_effective() {
        let e = sample_event(0);
        let out = resolve(&[e.clone()]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, e.id);
    }

    #[test]
    fn reversal_cancels_its_target() {
        let original = sample_event(0);
        let reversal = sample_event_with(1, Relation::Reversal { target: original.id });
        let out = resolve(&[original, reversal]).unwrap();
        assert!(out.is_empty(), "сторнированное событие не действует");
    }

    #[test]
    fn replacement_supersedes_its_target() {
        let original = sample_event(0);
        let replacement = sample_event_with(1, Relation::Replacement { target: original.id });
        let out = resolve(&[original.clone(), replacement.clone()]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, replacement.id, "действует замена, не исходное");
    }

    #[test]
    fn result_does_not_depend_on_input_order() {
        let original = sample_event(0);
        let replacement = sample_event_with(1, Relation::Replacement { target: original.id });

        let forward = resolve(&[original.clone(), replacement.clone()]).unwrap();
        let backward = resolve(&[replacement, original]).unwrap();

        let ids_forward: Vec<_> = forward.iter().map(|e| e.id).collect();
        let ids_backward: Vec<_> = backward.iter().map(|e| e.id).collect();
        assert_eq!(ids_forward, ids_backward, "порядок импорта не должен влиять");
    }

    #[test]
    fn conflicting_replacements_are_an_error() {
        let original = sample_event(0);
        let first = sample_event_with(1, Relation::Replacement { target: original.id });
        let second = sample_event_with(2, Relation::Replacement { target: original.id });
        assert!(matches!(
            resolve(&[original, first, second]),
            Err(CorrectionError::ConflictingReplacements { .. })
        ));
    }

    #[test]
    fn dangling_target_is_an_error() {
        let orphan = sample_event_with(
            0,
            Relation::Reversal { target: crate::ids::EventId::new_random() },
        );
        assert!(matches!(resolve(&[orphan]), Err(CorrectionError::DanglingTarget { .. })));
    }
}
```

- [ ] **Step 2: Добавить тестовые вспомогательные функции**

В `crates/iaam-core/src/event/mod.rs` дописать перед `#[cfg(test)] mod tests`:

```rust
/// Конструкторы событий для тестов. Доступны и другим модулям крейты,
/// поэтому вынесены из приватного `mod tests`.
#[cfg(test)]
pub(crate) mod test_support {
    use super::provenance::{ParserVersion, Provenance, RawHash};
    use super::*;
    use crate::dates::CashPostedDate;
    use crate::ids::SourceId;
    use crate::money::PostedMinor;
    use time::macros::date;

    pub(crate) fn sample_event(sequence: u32) -> Event {
        sample_event_with(sequence, Relation::None)
    }

    pub(crate) fn sample_event_with(sequence: u32, relation: Relation) -> Event {
        let account = AccountId::new_random();
        let amount = Money::new(PostedMinor(10_000_00), CurrencyCode::Rub);
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 03 - 01))),
            order: EffectiveOrder::new(date!(2026 - 03 - 01), sequence),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"b".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
}
```

Также добавить `pub mod correction;` в список модулей `event/mod.rs`.

- [ ] **Step 3: Запустить, убедиться в провале**

```bash
nix develop -c cargo test --package iaam-core correction
```

Ожидается: ошибки компиляции — `resolve` и `CorrectionError` не определены.

- [ ] **Step 4: Реализовать `resolve`**

Дописать в `correction.rs` перед `mod tests`:

```rust
use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use super::{Event, Relation};
use crate::ids::EventId;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CorrectionError {
    #[error("событие {source:?} ссылается на несуществующее {target:?}")]
    DanglingTarget { source: EventId, target: EventId },
    #[error("событие {target:?} заменяется более чем одним событием: {first:?} и {second:?}")]
    ConflictingReplacements { target: EventId, first: EventId, second: EventId },
    #[error("событие {id:?} встречается в срезе более одного раза")]
    DuplicateEvent { id: EventId },
}

/// Действующий набор событий.
///
/// Возвращает события, отсортированные по [`crate::dates::EffectiveOrder`],
/// с исключёнными сторнированными и заменёнными. Результат **не зависит**
/// от порядка входного среза: внутри используется упорядоченная карта,
/// а конфликты являются ошибкой, а не поводом выбрать «последний».
pub fn resolve(events: &[Event]) -> Result<Vec<&Event>, CorrectionError> {
    // 1. Индекс по идентификатору, с проверкой на дубликаты.
    let mut by_id: BTreeMap<EventId, &Event> = BTreeMap::new();
    for e in events {
        if by_id.insert(e.id, e).is_some() {
            return Err(CorrectionError::DuplicateEvent { id: e.id });
        }
    }

    // 2. Собираем сторнированные и заменённые цели.
    let mut reversed: BTreeSet<EventId> = BTreeSet::new();
    let mut replaced_by: BTreeMap<EventId, EventId> = BTreeMap::new();

    for e in events {
        match e.relation {
            Relation::None => {}
            Relation::Reversal { target } => {
                if !by_id.contains_key(&target) {
                    return Err(CorrectionError::DanglingTarget { source: e.id, target });
                }
                reversed.insert(target);
            }
            Relation::Replacement { target } => {
                if !by_id.contains_key(&target) {
                    return Err(CorrectionError::DanglingTarget { source: e.id, target });
                }
                if let Some(existing) = replaced_by.insert(target, e.id) {
                    // Детерминированный порядок сообщения: меньший идентификатор первым,
                    // чтобы текст ошибки не зависел от порядка импорта.
                    let (first, second) =
                        if existing < e.id { (existing, e.id) } else { (e.id, existing) };
                    return Err(CorrectionError::ConflictingReplacements {
                        target,
                        first,
                        second,
                    });
                }
            }
        }
    }

    // 3. Действующим является событие, которое не сторнировано,
    //    не заменено, и само не является сторнирующим.
    let mut effective: Vec<&Event> = events
        .iter()
        .filter(|e| !reversed.contains(&e.id))
        .filter(|e| !replaced_by.contains_key(&e.id))
        .filter(|e| !matches!(e.relation, Relation::Reversal { .. }))
        .collect();

    // 4. Устойчивая сортировка: порядок задаётся EffectiveOrder,
    //    ничьи разрешаются идентификатором.
    effective.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));

    Ok(effective)
}
```

- [ ] **Step 5: Прогнать**

```bash
nix develop -c cargo nextest run --package iaam-core
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Ожидается: шесть тестов `correction` проходят.

- [ ] **Step 6: Мутационная проверка**

```bash
nix develop -c cargo mutants --package iaam-core --file crates/iaam-core/src/event/correction.rs
```

Ожидается: выживших нет. Если выжил мутант, убравший проверку `DanglingTarget`, — тест `dangling_target_is_an_error` не покрывает путь, чините тест.

- [ ] **Step 7: Коммит**

```bash
git add crates/iaam-core/
git commit -m "feat(core): сторнирование и замена с детерминированным разрешением (iaam-1fk)"
```

---

### Task 12: Контуры

Контур определяет владелец, а не учреждение (§4.10). Это ключ к правильной XIRR: перевод со вклада на брокерский счёт — не пополнение, потому что оба счёта внутри контура. Определение версионировано, иначе изменение состава задним числом молча меняет исторические цифры.

**Files:**
- Create: `crates/iaam-core/src/contour.rs`
- Modify: `crates/iaam-core/src/lib.rs`

**Interfaces:**
- Consumes: `ids::{AccountId, TransferId}`, `event::{Event, kind::{EventKind, FlowEndpoints}}`
- Produces:
  - `contour::{ContourId, ContourVersion, ContourDefinition}`
  - `ContourDefinition::contains(&self, account: AccountId) -> bool`
  - `contour::classify(def: &ContourDefinition, event: &Event) -> FlowClass`
  - `contour::FlowClass` — `ExternalIn { .. }`, `ExternalOut { .. }`, `Internal`, `Irrelevant`

**Acceptance Criteria:**
- Перевод между двумя счетами внутри контура классифицируется как `Internal`
- **То же самое событие** при узком контуре классифицируется как `ExternalIn` — меняется определение контура, а не событие
- Перевод из контура наружу — `ExternalOut`
- Перевод между двумя внешними счетами — `Irrelevant`
- Версия определения входит в результат классификации

- [ ] **Step 1: Написать падающие тесты**

Создать `crates/iaam-core/src/contour.rs`:

```rust
//! Контуры (§4.10).
//!
//! Брокер считает перевод со вклада пополнением, потому что его контур —
//! только его собственный счёт. Владелец видит всю картину, поэтому
//! границу проводит он.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::EventKind;
    use crate::event::test_support::sample_event;
    use crate::event::Event;
    use crate::ids::{AccountId, TransferId};
    use crate::money::{CurrencyCode, Money, PostedMinor};

    fn transfer(from: AccountId, to: AccountId) -> Event {
        let mut e = sample_event(0);
        e.account = from;
        e.kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: Money::new(PostedMinor::new(100_000_00), CurrencyCode::Rub),
        };
        e
    }

    fn contour(accounts: Vec<AccountId>) -> ContourDefinition {
        ContourDefinition::new(ContourId::new_random(), ContourVersion(1), accounts)
    }

    #[test]
    fn transfer_between_two_inside_accounts_is_internal() {
        // Вклад -> брокерский счёт, оба внутри контура «весь капитал».
        // Это не пополнение: доходность не меняется, меняется аллокация.
        let deposit = AccountId::new_random();
        let broker = AccountId::new_random();
        let def = contour(vec![deposit, broker]);
        assert_eq!(classify(&def, &transfer(deposit, broker)), FlowClass::Internal);
    }

    #[test]
    fn the_same_event_is_external_for_a_narrower_contour() {
        // Событие ОДНО И ТО ЖЕ, меняется только определение контура.
        // Прежняя редакция плана подменяла здесь CashTransfer на CashIn
        // и потому перевод вообще не тестировала.
        let deposit = AccountId::new_random();
        let broker = AccountId::new_random();
        let event = transfer(deposit, broker);

        let wide = contour(vec![deposit, broker]);
        let narrow = contour(vec![broker]);

        assert_eq!(classify(&wide, &event), FlowClass::Internal);
        assert!(
            matches!(classify(&narrow, &event), FlowClass::ExternalIn { .. }),
            "для узкого контура тот же перевод — приход извне"
        );
    }

    #[test]
    fn transfer_out_of_the_contour_is_external_out() {
        let broker = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![broker]);
        assert!(matches!(
            classify(&def, &transfer(broker, outside)),
            FlowClass::ExternalOut { .. }
        ));
    }

    #[test]
    fn transfer_between_two_outside_accounts_is_irrelevant() {
        let a = AccountId::new_random();
        let b = AccountId::new_random();
        let def = contour(vec![AccountId::new_random()]);
        assert_eq!(classify(&def, &transfer(a, b)), FlowClass::Irrelevant);
    }

    #[test]
    fn buying_a_security_is_internal_not_a_contribution() {
        let broker = AccountId::new_random();
        let def = contour(vec![broker]);
        let mut ev = sample_event(0);
        ev.account = broker;
        ev.kind = EventKind::Trade {
            side: crate::event::kind::TradeSide::Buy,
            instrument: crate::ids::InstrumentId::new_random(),
            quantity: crate::money::Quantity::zero(),
            gross: Money::new(PostedMinor::new(50_000_00), CurrencyCode::Rub),
            fee: None,
            accrued_interest: None,
        };
        assert_eq!(classify(&def, &ev), FlowClass::Internal);
    }

    #[test]
    fn cash_in_on_an_outside_account_is_irrelevant() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![inside]);
        let mut ev = sample_event(0);
        ev.account = outside;
        assert_eq!(classify(&def, &ev), FlowClass::Irrelevant);
    }

    #[test]
    fn contour_version_is_carried_into_the_classification() {
        let broker = AccountId::new_random();
        let def =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(7), vec![broker]);
        let mut ev = sample_event(0);
        ev.account = broker;
        match classify(&def, &ev) {
            FlowClass::ExternalIn { version, .. } => assert_eq!(version, ContourVersion(7)),
            other => panic!("ожидался ExternalIn, получено {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Реализовать**

Дописать перед `mod tests`:

```rust
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::kind::FlowEndpoints;
use crate::event::Event;
use crate::ids::AccountId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContourId(pub Uuid);

impl ContourId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Версия определения контура.
///
/// Расчёт доходности ссылается на версию: без этого изменение состава
/// контура задним числом молча меняет исторические цифры.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContourVersion(pub u32);

/// Состав контура на конкретной версии.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContourDefinition {
    id: ContourId,
    version: ContourVersion,
    accounts: BTreeSet<AccountId>,
}

impl ContourDefinition {
    #[must_use]
    pub fn new(
        id: ContourId,
        version: ContourVersion,
        accounts: impl IntoIterator<Item = AccountId>,
    ) -> Self {
        Self { id, version, accounts: accounts.into_iter().collect() }
    }

    #[must_use]
    pub const fn id(&self) -> ContourId {
        self.id
    }

    #[must_use]
    pub const fn version(&self) -> ContourVersion {
        self.version
    }

    #[must_use]
    pub fn contains(&self, account: AccountId) -> bool {
        self.accounts.contains(&account)
    }
}

/// Отношение события к границе контура.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowClass {
    /// Деньги вошли в контур извне. Входит в XIRR со знаком плюс.
    ExternalIn { contour: ContourId, version: ContourVersion },
    /// Деньги вышли из контура. Входит в XIRR со знаком минус.
    ExternalOut { contour: ContourId, version: ContourVersion },
    /// Внутри контура: меняет аллокацию, но не доходность.
    Internal,
    /// Событие к этому контуру не относится.
    Irrelevant,
}

/// Классификация события относительно контура.
///
/// Ключевое место всей системы: именно из-за путаницы здесь сервисы
/// показывают доходность, в которой собственные пополнения выглядят
/// заработком. Для перевода классификация определяется **парой**
/// принадлежностей, поэтому оба счёта обязаны храниться в событии.
#[must_use]
pub fn classify(def: &ContourDefinition, event: &Event) -> FlowClass {
    let inbound = FlowClass::ExternalIn { contour: def.id(), version: def.version() };
    let outbound = FlowClass::ExternalOut { contour: def.id(), version: def.version() };

    match event.kind.flow_endpoints() {
        FlowEndpoints::InboundFromOutside => {
            if def.contains(event.account) { inbound } else { FlowClass::Irrelevant }
        }
        FlowEndpoints::OutboundToOutside => {
            if def.contains(event.account) { outbound } else { FlowClass::Irrelevant }
        }
        FlowEndpoints::BetweenAccounts { from, to } => {
            match (def.contains(from), def.contains(to)) {
                (true, true) => FlowClass::Internal,
                (false, true) => inbound,
                (true, false) => outbound,
                (false, false) => FlowClass::Irrelevant,
            }
        }
        FlowEndpoints::WithinAccount => {
            if def.contains(event.account) { FlowClass::Internal } else { FlowClass::Irrelevant }
        }
    }
}
```

> **Что переносится в E2.** Событие перевода несёт оба счёта, поэтому
> классификация полна уже на этапе 1. В E2 добавляется **сшивание**:
> когда две стороны приходят из разных источников (выписка банка и отчёт
> брокера) отдельными записями, их нужно сопоставить и свести в одно
> событие с общим `transfer_id` (§4.11). Ручной ввод создаёт целое событие
> сразу, поэтому этап 1 в сшивании не нуждается.

- [ ] **Step 3: Подключить и прогнать**

В `lib.rs` добавить `pub mod contour;`.

```bash
nix develop -c cargo nextest run --package iaam-core
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 4: Коммит**

```bash
git add crates/iaam-core/
git commit -m "feat(core): контуры с версионированием и классификация потоков (iaam-1fk)"
```

---

### Task 13: Реестр правил и списание лотов по FIFO

§16.1 пункт 9: **FIFO не зашивается в факт продажи.** Событие хранит проданное количество и сумму; какие лоты списаны — результат версионированной проекции. §3.2: механизм подключения версионированных стратегий обязан существовать, иначе «версионированность» останется декларацией, а в проекциях разрастётся `match year`.

**Files:**
- Create: `crates/iaam-core/src/rules/mod.rs`
- Create: `crates/iaam-core/src/rules/lot_disposal.rs`
- Modify: `crates/iaam-core/src/lib.rs`

**Interfaces:**
- Consumes: `money::{Money, Quantity, PostedMinor}`, `dates::TradeDate`, `ids::InstrumentId`, `numeric::{Dec, Exact}`
- Produces:
  - `rules::lot_disposal::{Lot, LotId, DisposalInput, DisposalResult, DisposedPart, LotDisposalRule, RuleId}`
  - `rules::lot_disposal::FifoV1` — реализация по ст. 214.1
  - `rules::RuleRegistry` с `lot_rules: BTreeMap<LotRuleVersion, Box<dyn LotDisposalRule>>`
  - `RuleRegistry::disposal_rule(&self, version) -> Option<&dyn LotDisposalRule>`

**Acceptance Criteria:**
- Продажа 10 из лотов [10 по 100, 10 по 90] списывает первый лот целиком, а не среднюю цену
- Продажа 15 списывает первый лот целиком и 5 из второго, `basis_released` считается точно
- Продажа большего количества, чем есть, возвращает `Err(InsufficientQuantity)`, а не отрицательный остаток
- Идентификатор применённого правила входит в `DisposalResult`
- Правило подключается через реестр, а не вызывается напрямую из проекции

- [ ] **Step 1: Написать падающие тесты**

Создать `crates/iaam-core/src/rules/lot_disposal.rs`:

```rust
//! Списание лотов (§4.12).
//!
//! FIFO предписан ст. 214.1 НК РФ, но **не является глобальной очередью
//! по портфелю**: область задаётся налогоплательщиком, агентом, базой,
//! инструментом, счётом, режимом и годом. На этапе 1 область — пара
//! «счёт × инструмент»; расширение до полной области — эпик E5.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(n: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(n)))
    }

    /// Два лота: сначала дороже, потом дешевле.
    /// Ровно случай из постановки задачи: «купил 10 яблок дороже,
    /// потом 10 дешевле, брокер показывает среднюю».
    fn two_lots() -> Vec<Lot> {
        vec![
            Lot {
                id: LotId::new_random(),
                instrument: InstrumentId::new_random(),
                acquired: Some(TradeDate(date!(2026 - 01 - 10))),
                quantity: qty(10),
                cost_basis: rub(1_000_00), // 10 шт по 100 ₽
            },
            Lot {
                id: LotId::new_random(),
                instrument: InstrumentId::new_random(),
                acquired: Some(TradeDate(date!(2026 - 02 - 10))),
                quantity: qty(10),
                cost_basis: rub(900_00), // 10 шт по 90 ₽
            },
        ]
    }

    #[test]
    fn selling_ten_takes_the_first_lot_whole_not_the_average() {
        let lots = two_lots();
        let rule = FifoV1;
        let out = rule
            .apply(&DisposalInput { lots: lots.clone(), quantity: qty(10) })
            .unwrap();

        assert_eq!(out.disposed.len(), 1, "списан ровно один лот");
        assert_eq!(out.disposed[0].lot, lots[0].id, "списан первый по времени");
        assert_eq!(out.basis_released, rub(1_000_00), "по цене первого лота, не средней");
        assert_eq!(out.remaining.len(), 1);
        assert_eq!(out.remaining[0].quantity, qty(10));
    }

    #[test]
    fn selling_fifteen_splits_the_second_lot() {
        let lots = two_lots();
        let out = FifoV1
            .apply(&DisposalInput { lots: lots.clone(), quantity: qty(15) })
            .unwrap();

        assert_eq!(out.disposed.len(), 2);
        // 1000,00 за первый лот целиком + половина второго = 450,00
        assert_eq!(out.basis_released, rub(1_450_00));
        assert_eq!(out.remaining.len(), 1);
        assert_eq!(out.remaining[0].quantity, qty(5));
        assert_eq!(out.remaining[0].cost_basis, rub(450_00));
    }

    #[test]
    fn selling_more_than_held_is_an_error() {
        let out = FifoV1.apply(&DisposalInput { lots: two_lots(), quantity: qty(25) });
        assert!(matches!(out, Err(DisposalError::InsufficientQuantity { .. })));
    }

    #[test]
    fn result_records_which_rule_was_applied() {
        let out = FifoV1
            .apply(&DisposalInput { lots: two_lots(), quantity: qty(1) })
            .unwrap();
        assert_eq!(out.rule, RuleId::new(FifoV1::ID));
    }

    #[test]
    fn selling_everything_leaves_nothing() {
        let out = FifoV1
            .apply(&DisposalInput { lots: two_lots(), quantity: qty(20) })
            .unwrap();
        assert!(out.remaining.is_empty());
        assert_eq!(out.basis_released, rub(1_900_00));
    }
}
```

- [ ] **Step 2: Запустить, убедиться в провале**

```bash
nix develop -c cargo test --package iaam-core lot_disposal
```

Ожидается: ошибки компиляции.

- [ ] **Step 3: Реализовать**

Дописать перед `mod tests`:

```rust
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::dates::TradeDate;
use crate::ids::InstrumentId;
use crate::money::{Money, MoneyError, PostedMinor, Quantity};
use crate::numeric::decimal::Dec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LotId(pub Uuid);

impl LotId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Экономический лот: партия приобретения.
///
/// Позиция является проекцией набора лотов, а не самостоятельной сущностью.
/// Без лотов невозможен ЛДВ: три года владения — свойство покупки,
/// у позиции со средней ценой возраста нет.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lot {
    pub id: LotId,
    pub instrument: InstrumentId,
    /// Может быть неизвестна для восстановленной позиции (§10.7).
    pub acquired: Option<TradeDate>,
    pub quantity: Quantity,
    pub cost_basis: Money,
}

/// Идентификатор версии правила. Входит в результат и в след аудита.
///
/// Владеющая `String`, а не `&'static str`: десериализация заимствованной
/// строки с временем жизни `'static` из обычного JSON не является корректным
/// контрактом — входные данные столько не живут.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RuleId(pub String);

impl RuleId {
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self(id.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisposalInput {
    /// Лоты в порядке приобретения. Порядок обеспечивает вызывающий.
    pub lots: Vec<Lot>,
    pub quantity: Quantity,
}

/// Часть лота, списанная при выбытии.
#[derive(Debug, Clone, PartialEq)]
pub struct DisposedPart {
    pub lot: LotId,
    pub quantity: Quantity,
    pub basis_released: Money,
    pub acquired: Option<TradeDate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisposalResult {
    pub rule: RuleId,
    pub disposed: Vec<DisposedPart>,
    pub remaining: Vec<Lot>,
    /// Суммарная списанная стоимость. Компонент тождества §6.5.
    pub basis_released: Money,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DisposalError {
    #[error("недостаточно количества: запрошено {requested}, доступно {available}")]
    InsufficientQuantity { requested: String, available: String },
    #[error("лоты в разных валютах не поддерживаются в одном выбытии")]
    MixedCurrencies,
    #[error("список лотов пуст")]
    NoLots,
    #[error(transparent)]
    Money(#[from] MoneyError),
}

/// Стратегия списания. Доменная стратегия, **не порт ввода-вывода**:
/// передаётся в ядро как неизменяемый вход, чистота сохраняется (§3.2).
pub trait LotDisposalRule: Send + Sync {
    fn id(&self) -> RuleId;
    fn apply(&self, input: &DisposalInput) -> Result<DisposalResult, DisposalError>;
}

/// FIFO по ст. 214.1 НК РФ.
///
/// Specific lot identification в РФ недоступна: продал — списались первые
/// по времени приобретения, независимо от намерения.
#[derive(Debug, Clone, Copy, Default)]
pub struct FifoV1;

impl FifoV1 {
    pub const ID: &'static str = "fifo/214.1/v1";
}

impl LotDisposalRule for FifoV1 {
    fn id(&self) -> RuleId {
        RuleId::new(Self::ID)
    }

    fn apply(&self, input: &DisposalInput) -> Result<DisposalResult, DisposalError> {
        let Some(first) = input.lots.first() else {
            return Err(DisposalError::NoLots);
        };
        let currency = first.cost_basis.currency();
        if input.lots.iter().any(|l| l.cost_basis.currency() != currency) {
            return Err(DisposalError::MixedCurrencies);
        }

        let available: Decimal = input.lots.iter().map(|l| l.quantity.0.inner()).sum();
        let requested = input.quantity.0.inner();
        if requested > available {
            return Err(DisposalError::InsufficientQuantity {
                requested: requested.to_string(),
                available: available.to_string(),
            });
        }

        let mut left = requested;
        let mut disposed = Vec::new();
        let mut remaining = Vec::new();

        for lot in &input.lots {
            let lot_qty = lot.quantity.0.inner();
            if left.is_zero() {
                remaining.push(lot.clone());
                continue;
            }
            if lot_qty <= left {
                // Лот списывается целиком.
                disposed.push(DisposedPart {
                    lot: lot.id,
                    quantity: lot.quantity,
                    basis_released: lot.cost_basis,
                    acquired: lot.acquired,
                });
                left -= lot_qty;
            } else {
                // Лот делится. Стоимость разносится пропорционально количеству.
                let taken_basis = split_basis(lot.cost_basis, left, lot_qty)?;
                let kept_basis = lot.cost_basis.try_sub(taken_basis)?;
                disposed.push(DisposedPart {
                    lot: lot.id,
                    quantity: Quantity(Dec::new(left)),
                    basis_released: taken_basis,
                    acquired: lot.acquired,
                });
                remaining.push(Lot {
                    quantity: Quantity(Dec::new(lot_qty - left)),
                    cost_basis: kept_basis,
                    ..lot.clone()
                });
                left = Decimal::ZERO;
            }
        }

        let released: Vec<Money> = disposed.iter().map(|d| d.basis_released).collect();
        let basis_released = Money::sum(&released, currency)?;

        Ok(DisposalResult { rule: self.id(), disposed, remaining, basis_released })
    }
}

/// Разнесение стоимости лота пропорционально списываемому количеству.
///
/// Округление — половина к чётному, однократно, на границе представления
/// в минимальных единицах (§6.6). Остаток от округления остаётся
/// в невыбывшей части: суммарная стоимость лота сохраняется.
fn split_basis(
    total: Money,
    taken_qty: Decimal,
    lot_qty: Decimal,
) -> Result<Money, DisposalError> {
    debug_assert!(!lot_qty.is_zero(), "количество лота не может быть нулевым");
    let minor = Decimal::from(total.amount().raw());
    let scaled = (minor * taken_qty) / lot_qty;
    let rounded = scaled.round_dp_with_strategy(
        0,
        rust_decimal::RoundingStrategy::MidpointNearestEven,
    );
    let value = i64::try_from(rounded.trunc().mantissa())
        .map_err(|_| DisposalError::Money(MoneyError::Overflow))?;
    Ok(Money::new(PostedMinor::new(value), total.currency()))
}

```

> **Про импорты.** `CurrencyCode` нужен только тестам, поэтому импортируется
> внутри `mod tests`, а не в области модуля: неиспользуемый импорт при
> `-D warnings` становится ошибкой сборки. `Dec` используется и в продакшене
> (в `split_basis` и `Quantity`), поэтому остаётся наверху. Подавлять
> предупреждение через `#[allow(unused_imports)]` запрещено — заслон
> из задачи 4 это отклонит, и правильно: лишний импорт надо убрать,
> а не спрятать.

- [ ] **Step 4: Создать реестр правил**

Создать `crates/iaam-core/src/rules/mod.rs`:

```rust
//! Доменные стратегии и их версии (§3.2).
//!
//! Стратегия — **не порт ввода-вывода**: она передаётся в ядро как
//! неизменяемый вход, поэтому чистота функционального ядра сохраняется.
//! Реестр закрытый: плагины в рантайме не нужны.

pub mod lot_disposal;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use lot_disposal::{FifoV1, LotDisposalRule};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LotRuleVersion(pub u32);

/// Реестр версионированных доменных правил.
///
/// На этапе 1 содержит только списание лотов. Налоговые правила
/// (`TaxRuleSet`, ключ `(TaxYear, TaxBaseKind)`) добавляются в эпике E5
/// по той же схеме.
pub struct RuleRegistry {
    lot_rules: BTreeMap<LotRuleVersion, Box<dyn LotDisposalRule>>,
}

impl RuleRegistry {
    /// Реестр с правилами по умолчанию.
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut lot_rules: BTreeMap<LotRuleVersion, Box<dyn LotDisposalRule>> = BTreeMap::new();
        lot_rules.insert(LotRuleVersion(1), Box::new(FifoV1));
        Self { lot_rules }
    }

    #[must_use]
    pub fn disposal_rule(&self, version: LotRuleVersion) -> Option<&dyn LotDisposalRule> {
        self.lot_rules.get(&version).map(|rule| rule.as_ref())
    }

    /// Наибольшая доступная версия. Используется, когда вызывающий
    /// не указал версию явно.
    #[must_use]
    pub fn latest_disposal_version(&self) -> Option<LotRuleVersion> {
        self.lot_rules.keys().next_back().copied()
    }
}

impl Default for RuleRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_fifo_v1() {
        let reg = RuleRegistry::with_defaults();
        let rule = reg.disposal_rule(LotRuleVersion(1)).expect("FIFO v1 зарегистрирован");
        assert_eq!(rule.id(), lot_disposal::RuleId::new(FifoV1::ID));
    }

    #[test]
    fn unknown_version_is_none_not_a_silent_fallback() {
        let reg = RuleRegistry::with_defaults();
        assert!(
            reg.disposal_rule(LotRuleVersion(99)).is_none(),
            "неизвестная версия не должна молча подменяться доступной"
        );
    }

    #[test]
    fn latest_version_is_reported() {
        let reg = RuleRegistry::with_defaults();
        assert_eq!(reg.latest_disposal_version(), Some(LotRuleVersion(1)));
    }
}
```

- [ ] **Step 5: Подключить и прогнать**

В `lib.rs` добавить `pub mod rules;`.

```bash
nix develop -c cargo nextest run --package iaam-core
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c ./scripts/check-architecture.sh
```

- [ ] **Step 6: Мутационная проверка списания**

```bash
nix develop -c cargo mutants --package iaam-core --file crates/iaam-core/src/rules/lot_disposal.rs
```

Ожидается: выживших нет. Особое внимание — мутантам в `split_basis` и в сравнении `lot_qty <= left`.

- [ ] **Step 7: Коммит**

```bash
git add crates/iaam-core/
git commit -m "feat(core): реестр правил и списание лотов по FIFO ст. 214.1 (iaam-1fk)"
```

---

### Task 14: Независимый эталон и замороженные фикстуры

§15.4: проверка «два способа дают одно и то же» имеет силу **только если способы независимы**. Эталон живёт в отдельной крейте и не делит с продакшн-кодом ничего ниже примитивов.

**Files:**
- Create: `crates/iaam-oracle/Cargo.toml`
- Create: `crates/iaam-oracle/src/lib.rs`
- Create: `crates/iaam-oracle/src/lots_reference.rs`
- Create: `crates/iaam-oracle/tests/fifo_parity.rs`
- Create: `tests/fixtures/fifo_cases.json`
- Modify: `tests/fixtures/MANIFEST.sha256`
- Modify: `Cargo.toml`
- Delete: `tests/fixtures/smoke.json` (заменяется настоящей фикстурой)

**Interfaces:**
- Consumes: `iaam-core::rules::lot_disposal::*` — только как проверяемый объект, не как источник логики
- Produces: `iaam_oracle::lots_reference::dispose_fifo_rational` — списание, реализованное **другим алгоритмом** на целочисленной арифметике

**Acceptance Criteria:**
- Эталон реализован рекурсивным проходом с накоплением, а не тем же циклом, что в продакшене
- Эталон использует целочисленную арифметику, а не `Decimal`
- Фикстура содержит ожидаемые значения, посчитанные **вручную**, и это отражено в комментарии к каждому случаю
- Расхождение продакшена с эталоном роняет тест
- `iaam-oracle` не является зависимостью ни одной не-тестовой крейты

- [ ] **Step 1: Создать крейту эталона**

`crates/iaam-oracle/Cargo.toml`:

```toml
[package]
name = "iaam-oracle"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
# Никаких зависимостей на iaam-core в основной сборке:
# эталон не должен делить с продакшеном ничего, кроме примитивов.

[dev-dependencies]
iaam-core = { path = "../iaam-core" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
# Нужны тесту соответствия: он строит входные данные в типах ядра.
rust_decimal = { version = "1", default-features = false, features = ["std"] }
time = { version = "0.3", default-features = false, features = ["std", "macros"] }

[lints]
workspace = true
```

Добавить `"crates/iaam-oracle"` в `members` корневого `Cargo.toml`.

- [ ] **Step 2: Реализовать эталон другим алгоритмом**

`crates/iaam-oracle/src/lots_reference.rs`:

```rust
//! Эталонная реализация списания лотов (§15.4).
//!
//! **Намеренно другой алгоритм.** Продакшн использует итеративный проход
//! с изменяемым остатком и `Decimal`. Эталон — рекурсию с накоплением
//! и целочисленную арифметику. Общего кода нет, поэтому общая ошибка
//! проявиться не может.
//!
//! Количества здесь целые: эталон покрывает биржевые бумаги, где дробных
//! количеств не бывает. Дробные случаи (крипта) проверяются фикстурами.

/// Лот в эталонном представлении: количество и стоимость в минимальных единицах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefLot {
    pub quantity: i64,
    pub basis_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefDisposal {
    pub basis_released_minor: i64,
    pub remaining: Vec<RefLot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefError {
    InsufficientQuantity,
}

/// Списание по принципу «первые по времени приобретения».
///
/// Реализовано рекурсией: на каждом шаге обрабатывается голова списка,
/// хвост передаётся дальше. Накопитель несёт списанную стоимость.
pub fn dispose_fifo_rational(lots: &[RefLot], quantity: i64) -> Result<RefDisposal, RefError> {
    fn go(lots: &[RefLot], left: i64, released: i64) -> Result<RefDisposal, RefError> {
        match lots.split_first() {
            None if left == 0 => Ok(RefDisposal { basis_released_minor: released, remaining: vec![] }),
            None => Err(RefError::InsufficientQuantity),
            Some((head, tail)) if left == 0 => {
                let mut remaining = vec![*head];
                remaining.extend_from_slice(tail);
                Ok(RefDisposal { basis_released_minor: released, remaining })
            }
            Some((head, tail)) if head.quantity <= left => {
                go(tail, left - head.quantity, released + head.basis_minor)
            }
            Some((head, tail)) => {
                // Пропорциональное разнесение через целочисленную арифметику
                // с округлением половины к чётному — как в продакшене,
                // но выраженное иначе.
                let taken = round_half_to_even(head.basis_minor, left, head.quantity);
                let kept = head.basis_minor - taken;
                let mut remaining = vec![RefLot {
                    quantity: head.quantity - left,
                    basis_minor: kept,
                }];
                remaining.extend_from_slice(tail);
                Ok(RefDisposal { basis_released_minor: released + taken, remaining })
            }
        }
    }
    go(lots, quantity, 0)
}

/// `total * num / den` с округлением половины к чётному, без плавающей точки.
fn round_half_to_even(total: i64, num: i64, den: i64) -> i64 {
    debug_assert!(den > 0);
    let product = i128::from(total) * i128::from(num);
    let den = i128::from(den);
    let quotient = product.div_euclid(den);
    let remainder = product.rem_euclid(den);
    let twice = remainder * 2;
    let result = if twice > den {
        quotient + 1
    } else if twice < den {
        quotient
    } else if quotient % 2 == 0 {
        quotient
    } else {
        quotient + 1
    };
    i64::try_from(result).expect("стоимость лота не выходит за i64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_to_even_rounds_ties_to_even() {
        // 5 * 1 / 2 = 2,5 → 2 (чётное)
        assert_eq!(round_half_to_even(5, 1, 2), 2);
        // 7 * 1 / 2 = 3,5 → 4 (чётное)
        assert_eq!(round_half_to_even(7, 1, 2), 4);
    }

    #[test]
    fn taking_first_lot_whole() {
        let lots = [RefLot { quantity: 10, basis_minor: 100_000 }];
        let out = dispose_fifo_rational(&lots, 10).unwrap();
        assert_eq!(out.basis_released_minor, 100_000);
        assert!(out.remaining.is_empty());
    }
}
```

`crates/iaam-oracle/src/lib.rs`:

```rust
//! Независимые эталонные реализации для тестов (§15.4).
//!
//! Крейта существует, чтобы проверка «два способа дают одно и то же»
//! не вырождалась в тавтологию. Она **не является** зависимостью
//! ни одной продакшн-крейты.

pub mod lots_reference;
```

- [ ] **Step 3: Создать замороженную фикстуру**

Заменить `tests/fixtures/smoke.json` на `tests/fixtures/fifo_cases.json`:

```json
{
  "_comment": "Замороженные эталоны списания по FIFO. Ожидаемые значения посчитаны ВРУЧНУЮ и не правятся ради зелёного теста (§15.7 спецификации).",
  "cases": [
    {
      "name": "первый лот целиком",
      "_manual_calculation": "10 шт по 100,00 = 100000 копеек; продаём 10 → списывается весь первый лот",
      "lots": [
        { "quantity": 10, "basis_minor": 100000 },
        { "quantity": 10, "basis_minor": 90000 }
      ],
      "sell_quantity": 10,
      "expected_basis_released_minor": 100000,
      "expected_remaining": [{ "quantity": 10, "basis_minor": 90000 }]
    },
    {
      "name": "первый целиком плюс половина второго",
      "_manual_calculation": "100000 + 90000*5/10 = 100000 + 45000 = 145000",
      "lots": [
        { "quantity": 10, "basis_minor": 100000 },
        { "quantity": 10, "basis_minor": 90000 }
      ],
      "sell_quantity": 15,
      "expected_basis_released_minor": 145000,
      "expected_remaining": [{ "quantity": 5, "basis_minor": 45000 }]
    },
    {
      "name": "неделимый остаток, округление половины к чётному",
      "_manual_calculation": "лот 3 шт за 1000 копеек; продаём 1 → 1000*1/3 = 333,33... → 333; остаётся 667",
      "lots": [{ "quantity": 3, "basis_minor": 1000 }],
      "sell_quantity": 1,
      "expected_basis_released_minor": 333,
      "expected_remaining": [{ "quantity": 2, "basis_minor": 667 }]
    },
    {
      "name": "ровно половина, ничья округляется к чётному",
      "_manual_calculation": "лот 2 шт за 5 копеек; продаём 1 → 5*1/2 = 2,5 → 2 (чётное); остаётся 3",
      "lots": [{ "quantity": 2, "basis_minor": 5 }],
      "sell_quantity": 1,
      "expected_basis_released_minor": 2,
      "expected_remaining": [{ "quantity": 1, "basis_minor": 3 }]
    },
    {
      "name": "продано всё",
      "_manual_calculation": "100000 + 90000 = 190000; ничего не остаётся",
      "lots": [
        { "quantity": 10, "basis_minor": 100000 },
        { "quantity": 10, "basis_minor": 90000 }
      ],
      "sell_quantity": 20,
      "expected_basis_released_minor": 190000,
      "expected_remaining": []
    }
  ]
}
```

```bash
rm tests/fixtures/smoke.json
sha256sum tests/fixtures/fifo_cases.json > tests/fixtures/MANIFEST.sha256
```

Уберите из `crates/iaam-core/src/lib.rs` тест `fixture_manifest_is_wired` — он ссылался на удалённую фикстуру.

- [ ] **Step 4: Написать тест соответствия продакшена и эталона**

`crates/iaam-oracle/tests/fifo_parity.rs`:

```rust
//! Соответствие продакшн-реализации списания эталонной (§15.4).
//!
//! Оба прогоняются на одних входных данных, и оба сверяются
//! с замороженным ожидаемым значением. Совпадение продакшена
//! с эталоном без сверки с фикстурой недостаточно: обе реализации
//! могли бы ошибаться одинаково по случайности.

use iaam_core::dates::TradeDate;
use iaam_core::ids::InstrumentId;
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::rules::lot_disposal::{DisposalInput, FifoV1, Lot, LotDisposalRule, LotId};
use iaam_oracle::lots_reference::{dispose_fifo_rational, RefLot};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::macros::date;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    lots: Vec<RefLotJson>,
    sell_quantity: i64,
    expected_basis_released_minor: i64,
    expected_remaining: Vec<RefLotJson>,
}

#[derive(Deserialize, Clone, Copy)]
struct RefLotJson {
    quantity: i64,
    basis_minor: i64,
}

fn to_core_lots(items: &[RefLotJson]) -> Vec<Lot> {
    let instrument = InstrumentId::new_random();
    items
        .iter()
        .map(|l| Lot {
            id: LotId::new_random(),
            instrument,
            acquired: Some(TradeDate(date!(2026 - 01 - 01))),
            quantity: Quantity(Dec::new(Decimal::from(l.quantity))),
            cost_basis: Money::new(PostedMinor::new(l.basis_minor), CurrencyCode::Rub),
        })
        .collect()
}

#[test]
fn production_matches_oracle_and_frozen_expectations() {
    let raw = include_str!("../../../tests/fixtures/fifo_cases.json");
    let fixture: Fixture = serde_json::from_str(raw).expect("фикстура разбирается");
    assert!(!fixture.cases.is_empty(), "фикстура не должна быть пустой");

    for case in &fixture.cases {
        // --- Эталон ---
        let ref_lots: Vec<RefLot> = case
            .lots
            .iter()
            .map(|l| RefLot { quantity: l.quantity, basis_minor: l.basis_minor })
            .collect();
        let oracle = dispose_fifo_rational(&ref_lots, case.sell_quantity)
            .unwrap_or_else(|e| panic!("эталон упал на случае «{}»: {e:?}", case.name));

        // --- Продакшн ---
        let input = DisposalInput {
            lots: to_core_lots(&case.lots),
            quantity: Quantity(Dec::new(Decimal::from(case.sell_quantity))),
        };
        let production = FifoV1
            .apply(&input)
            .unwrap_or_else(|e| panic!("продакшн упал на случае «{}»: {e:?}", case.name));

        // --- Оба против замороженного ожидания ---
        assert_eq!(
            oracle.basis_released_minor, case.expected_basis_released_minor,
            "эталон разошёлся с фикстурой на случае «{}»",
            case.name
        );
        assert_eq!(
            production.basis_released.amount().raw(),
            case.expected_basis_released_minor,
            "продакшн разошёлся с фикстурой на случае «{}»",
            case.name
        );

        // --- Остатки ---
        assert_eq!(
            oracle.remaining.len(),
            case.expected_remaining.len(),
            "эталон: неверное число оставшихся лотов на «{}»",
            case.name
        );
        assert_eq!(
            production.remaining.len(),
            case.expected_remaining.len(),
            "продакшн: неверное число оставшихся лотов на «{}»",
            case.name
        );
        for (i, expected) in case.expected_remaining.iter().enumerate() {
            assert_eq!(
                oracle.remaining[i].basis_minor, expected.basis_minor,
                "эталон: остаток {i} на «{}»",
                case.name
            );
            assert_eq!(
                production.remaining[i].cost_basis.amount().raw(),
                expected.basis_minor,
                "продакшн: остаток {i} на «{}»",
                case.name
            );
        }
    }
}
```

- [ ] **Step 5: Добавить `serde_json` и прогнать**

В `crates/iaam-oracle/Cargo.toml` `serde_json` уже в `dev-dependencies`.

```bash
nix develop -c cargo nextest run --workspace
nix develop -c ./scripts/check-fixtures.sh
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Ожидается: тест `production_matches_oracle_and_frozen_expectations` проходит на всех пяти случаях; заслон фикстур зелёный.

- [ ] **Step 6: Проверить, что эталон действительно ловит ошибку**

Временно сломайте продакшн: в `split_basis` замените `MidpointNearestEven` на `MidpointAwayFromZero`.

```bash
nix develop -c cargo nextest run --workspace
```

Ожидается: **падение** на случае «ровно половина, ничья округляется к чётному» — продакшн даст 3 вместо 2.

Верните `MidpointNearestEven`.

> Если тест **не** упал — фикстура не покрывает округление, и слой §15.4 не работает. Добавьте случай, который его покрывает, прежде чем продолжать.

- [ ] **Step 7: Проверить изоляцию эталона**

Проверка уже встроена в `scripts/check-architecture.sh` (пункт 4): она
запрещает `iaam-oracle` в обычных и build-зависимостях любой другой крейты.
Форма `grep && { exit 1; } || echo ok` намеренно не используется — она
способна напечатать «в порядке» после неудачи. Заслон построен на `if`.

```bash
nix develop -c ./scripts/check-architecture.sh
```

Убедитесь, что заслон срабатывает: временно добавьте в
`crates/iaam-core/Cargo.toml` в `[dependencies]` строку
`iaam-oracle = { path = "../iaam-oracle" }`, запустите скрипт — он обязан
упасть с сообщением про §15.4, — затем уберите строку.

- [ ] **Step 8: Коммит**

```bash
git add crates/ tests/fixtures/ Cargo.toml scripts/check-architecture.sh
git commit -m "test: независимый эталон списания лотов и замороженные фикстуры (iaam-1fk)"
```

---

### Task 15: Свойства с указанием области применимости

§15.3: свойство без области — источник ложных падений, на которые агент отвечает ослаблением генератора до тавтологии. Каждое свойство здесь снабжено оговоркой.

**Files:**
- Create: `crates/iaam-core/tests/properties.rs`
- Modify: `crates/iaam-core/Cargo.toml`

**Interfaces:**
- Consumes: весь публичный API `iaam-core`
- Produces: набор свойств, проверяемых `proptest`

**Acceptance Criteria:**
- Перестановка входного среза не меняет результат `resolve` — на случайных журналах
- Событие вместе со сторно не влияет на действующий набор
- Сумма списанной и оставшейся стоимости равна исходной — точно, без потерь на округлении
- Свойства **не включают** склейку периодов для XIRR и масштабирование при налогах: они неверны в общем виде

- [ ] **Step 1: Добавить `proptest`**

`crates/iaam-core/Cargo.toml`:

```toml
[dev-dependencies]
proptest = "1"
```

> `rust_decimal` и `time` уже находятся в `[dependencies]` крейты, поэтому
> интеграционным тестам в `tests/` доступны без отдельного объявления.
> `iaam-oracle` — другая крейта, там они нужны в `[dev-dependencies]` явно.

- [ ] **Step 2: Написать свойства**

`crates/iaam-core/tests/properties.rs`:

```rust
//! Свойства с указанием области применимости (§15.3).
//!
//! Каждое свойство сопровождается оговоркой о том, где оно выполняется.
//! Свойства без области — источник ложных падений, на которые проще
//! всего ответить ослаблением генератора до тавтологии.
//!
//! **Намеренно отсутствуют** и не должны быть добавлены:
//! - склейка периодов для XIRR: IRR не цепляется, свойства нет;
//! - масштабирование всех сумм при включённых налогах: прогрессивная
//!   шкала, пороги и минимальные комиссии его нарушают;
//! - сдвиг дат при налоговых правилах: меняются база начисления дней,
//!   налоговый год и ЛДВ.

use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::rules::lot_disposal::{DisposalInput, FifoV1, Lot, LotDisposalRule, LotId};
use iaam_core::dates::TradeDate;
use iaam_core::ids::InstrumentId;
use proptest::prelude::*;
use rust_decimal::Decimal;
use time::macros::date;

fn lot_strategy() -> impl Strategy<Value = (i64, i64)> {
    // Количество 1..=1000, стоимость 1..=100_000_000 минорных единиц.
    (1_i64..=1_000, 1_i64..=100_000_000)
}

proptest! {
    /// Область: любой набор лотов одной валюты, любое допустимое количество.
    /// Инвариант точный — округление разносится так, что суммарная
    /// стоимость лота сохраняется (§6.6).
    #[test]
    fn released_plus_remaining_equals_original_basis(
        raw_lots in prop::collection::vec(lot_strategy(), 1..8),
        sell_fraction in 0_u32..=100,
    ) {
        let instrument = InstrumentId::new_random();
        let lots: Vec<Lot> = raw_lots
            .iter()
            .map(|(q, b)| Lot {
                id: LotId::new_random(),
                instrument,
                acquired: Some(TradeDate(date!(2026 - 01 - 01))),
                quantity: Quantity(Dec::new(Decimal::from(*q))),
                cost_basis: Money::new(PostedMinor::new(*b), CurrencyCode::Rub),
            })
            .collect();

        let total_qty: i64 = raw_lots.iter().map(|(q, _)| *q).sum();
        let total_basis: i64 = raw_lots.iter().map(|(_, b)| *b).sum();
        let sell_qty = total_qty * i64::from(sell_fraction) / 100;

        let out = FifoV1
            .apply(&DisposalInput {
                lots,
                quantity: Quantity(Dec::new(Decimal::from(sell_qty))),
            })
            .expect("количество в пределах доступного");

        let remaining_basis: i64 =
            out.remaining.iter().map(|l| l.cost_basis.amount().raw()).sum();

        prop_assert_eq!(
            out.basis_released.amount().raw() + remaining_basis,
            total_basis,
            "списанная и оставшаяся стоимость обязаны в сумме давать исходную"
        );
    }

    /// Область: любое допустимое количество. Списанное количество
    /// равно запрошенному — ни больше, ни меньше.
    #[test]
    fn disposed_quantity_equals_requested(
        raw_lots in prop::collection::vec(lot_strategy(), 1..8),
        sell_fraction in 0_u32..=100,
    ) {
        let instrument = InstrumentId::new_random();
        let lots: Vec<Lot> = raw_lots
            .iter()
            .map(|(q, b)| Lot {
                id: LotId::new_random(),
                instrument,
                acquired: None,
                quantity: Quantity(Dec::new(Decimal::from(*q))),
                cost_basis: Money::new(PostedMinor::new(*b), CurrencyCode::Rub),
            })
            .collect();

        let total_qty: i64 = raw_lots.iter().map(|(q, _)| *q).sum();
        let sell_qty = total_qty * i64::from(sell_fraction) / 100;

        let out = FifoV1
            .apply(&DisposalInput {
                lots,
                quantity: Quantity(Dec::new(Decimal::from(sell_qty))),
            })
            .expect("количество в пределах доступного");

        let disposed: Decimal = out.disposed.iter().map(|d| d.quantity.0.inner()).sum();
        prop_assert_eq!(disposed, Decimal::from(sell_qty));
    }

    /// Область: любые количества сверх доступного.
    /// Отказ, а не отрицательный остаток.
    #[test]
    fn overselling_always_errors(
        raw_lots in prop::collection::vec(lot_strategy(), 1..5),
        excess in 1_i64..=1_000,
    ) {
        let instrument = InstrumentId::new_random();
        let total_qty: i64 = raw_lots.iter().map(|(q, _)| *q).sum();
        let lots: Vec<Lot> = raw_lots
            .iter()
            .map(|(q, b)| Lot {
                id: LotId::new_random(),
                instrument,
                acquired: None,
                quantity: Quantity(Dec::new(Decimal::from(*q))),
                cost_basis: Money::new(PostedMinor::new(*b), CurrencyCode::Rub),
            })
            .collect();

        let out = FifoV1.apply(&DisposalInput {
            lots,
            quantity: Quantity(Dec::new(Decimal::from(total_qty + excess))),
        });
        prop_assert!(out.is_err());
    }
}
```

- [ ] **Step 3: Добавить детерминированный регрессионный тест на округление**

Свойство из шага 2 покрывает разнесение только когда `proptest` сгенерирует
частичное списание. При `sell_fraction` 0 или 100 функция `split_basis`
не вызывается вовсе, поэтому «почти наверняка сработает» не является
заслоном. Нужен обычный тест с фиксированным входом.

Дописать в `crates/iaam-core/tests/properties.rs` перед блоком `proptest!`:

```rust
/// Ничья при разнесении округляется к чётному, и стоимость сохраняется.
/// Детерминированный аналог свойства: `proptest` до этого случая
/// может и не добраться.
#[test]
fn tie_rounding_preserves_total_basis() {
    let instrument = InstrumentId::new_random();
    let lots = vec![Lot {
        id: LotId::new_random(),
        instrument,
        acquired: None,
        quantity: Quantity(Dec::new(Decimal::from(2))),
        cost_basis: Money::new(PostedMinor::new(5), CurrencyCode::Rub),
    }];

    let out = FifoV1
        .apply(&DisposalInput { lots, quantity: Quantity(Dec::new(Decimal::from(1))) })
        .expect("одна из двух штук доступна");

    // 5 * 1 / 2 = 2,5 — ничья, округляется к чётному, то есть к 2.
    assert_eq!(out.basis_released.amount().raw(), 2);
    assert_eq!(out.remaining[0].cost_basis.amount().raw(), 3);
    assert_eq!(
        out.basis_released.amount().raw() + out.remaining[0].cost_basis.amount().raw(),
        5,
        "стоимость лота обязана сохраниться"
    );
}
```

- [ ] **Step 4: Прогнать**

```bash
nix develop -c cargo nextest run --package iaam-core --test properties
```

Ожидается: все свойства проходят. Если `released_plus_remaining_equals_original_basis` падает — это **настоящая ошибка разнесения**, а не повод ослабить свойство.

- [ ] **Step 5: Проверить, что проверки ловят ошибку**

Временно замените в `split_basis` возврат `Ok(Money::new(PostedMinor::new(value), total.currency()))` на `Ok(Money::new(PostedMinor(value + 1), total.currency()))`.

```bash
nix develop -c cargo nextest run --package iaam-core --test properties
```

Ожидается: падение **обоих** — детерминированного теста с конкретными числами и свойства с контрпримером. Детерминированный падает всегда, свойство — с высокой вероятностью.

Верните исходный код.

- [ ] **Step 6: Коммит**

```bash
git add crates/iaam-core/
git commit -m "test: свойства с областью применимости для списания лотов (iaam-1fk)"
```

---

### Task 16: Заморозка схемы и закрытие плана

**Files:**
- Create: `docs/irreversible-core.md`
- Modify: `README.md`

**Acceptance Criteria:**
- Каждый из десяти пунктов §16.1 отмечен как реализованный, со ссылкой на модуль
- Все заслоны зелёные на финальном коммите
- Второй план запланирован биддом

- [ ] **Step 1: Написать `docs/irreversible-core.md`**

```markdown
# Необратимое ядро схемы

Изменение любого пункта ниже потребует миграции журнала фактов — то есть
переинтерпретации уже записанных событий. Всё остальное аддитивно (§16.2
спецификации) и добавляется новым типом события или новой проекцией.

| # | Требование §16.1 | Где реализовано |
|---|---|---|
| 1 | Версионированный envelope события | `iaam-core::event::Event`, `SCHEMA_VERSION` |
| 2 | Несколько семантических дат | `iaam-core::dates::EventDates` — шесть типов |
| 3 | Сохранение сырых значений без потерь | `event::kind::EventKind` — gross, fee, НКД раздельно |
| 4 | Раздельные идентичности | `iaam-core::ids` — owner, account, custody, instrument, source |
| 5 | Типизированные ноги движения | `event::leg::{Leg, LegKind}` |
| 6 | Проведённые суммы против расчётных | `money::PostedMinor` против `numeric::decimal::Dec` |
| 7 | Append-only с детерминированным разрешением | `event::correction::resolve` |
| 8 | Provenance | `event::provenance::Provenance` — без хеша не конструируется |
| 9 | FIFO не зашит в факт продажи | `rules::lot_disposal` — версионированная стратегия |
| 10 | `unknown` как значение | `event::Confidence`, `Option<T>` во всех неизвестных полях |

Дополнительно зафиксировано по итогам ревью:

| Требование | Где реализовано |
|---|---|
| Перевод несёт **оба** счёта | `EventKind::CashTransfer { transfer_id, from, to, amount }` |
| Структура события проверяется по его типу, а не общим балансом | `Event::validate_structure` |
| Деньги нельзя сложить в обход валюты | `PostedMinor` с приватным полем, `Money` без `impl Add` |
| Отрицание не паникует на `i64::MIN` / `i128::MIN` | `checked_negate`, `checked_sub` |

## Что менять нельзя без миграции

- Состав и семантику `EventDates` — от них зависит налоговый период.
- Порядок полей `EffectiveOrder` — от него зависит детерминизм сортировки.
- Значения `EventKind::discriminant()` — они попадают в хранилище.
- Семантику `flow_endpoints()` и состав `CashTransfer` — от них зависит вся доходность.
- Требование `Provenance` — восстановить происхождение задним числом невозможно.

## Что можно добавлять свободно

Новые варианты `EventKind` и корпоративных действий; новые версии правил
в `RuleRegistry`; новые проекции; налоговые базы и лоты; NAV и TWR;
разложение результата; рыночные данные.
```

- [ ] **Step 2: Прогнать полный набор заслонов**

```bash
nix develop -c bash -c '
  cargo fmt --all -- --check &&
  cargo clippy --workspace --all-targets --all-features -- -D warnings &&
  ./scripts/check-architecture.sh &&
  ./scripts/check-fixtures.sh &&
  ./scripts/check-diff-lint.sh origin/main &&
  cargo deny check &&
  cargo nextest run --workspace --all-features &&
  cargo test --workspace --doc &&
  cargo mutants --package iaam-core
'
```

Ожидается: всё зелёное, выживших мутантов нет.

- [ ] **Step 3: Коммит**

```bash
git add docs/ README.md
git commit -m "docs: необратимое ядро схемы зафиксировано (iaam-1fk)"
```

- [ ] **Step 4: Завести бид на второй план**

```bash
bd create "Второй план E1: проекции, XIRR, хранилище, REST" \
  -t task -p 1 --parent iaam-1fk \
  --description "Написать .internal/plans/2026-XX-XX-e1-part2-projections-and-api.md, покрывающий: проекции лотов и позиций со снимками project/advance; денежные потоки и классификацию границы контура; XIRR с политикой решателя; инварианты с типизированной ошибкой; iaam-store на SQLite; iaam-app с портами; iaam-server с axum, аутентификацией и OpenAPI через utoipa; ручной ввод и CSV; скилл для внешнего агента. Пишется ПОСЛЕ того, как схема из первого плана существует как работающий код." \
  --acceptance="План написан, прошёл самопроверку и утверждён владельцем"
```

- [ ] **Step 5: Отчитаться владельцу**

Первый план завершён: каркас собран, заслоны проверены падением, необратимая схема зафиксирована и покрыта эталоном, свойствами и мутационным тестированием. Эпик `iaam-a4x` закрыт. Эпик `iaam-1fk` остаётся открытым — его закрывает второй план.

---

## Приложение: соответствие плана спецификации

| Требование спеки | Задача |
|---|---|
| §3.1 функциональное ядро без I/O | 1, 3 (заслон) |
| §3.2 направление зависимостей, запрет shared | 3 (заслон) |
| §3.2 доменные стратегии как реестр | 13 |
| §3.4, §6.6 три числовых режима | 7 |
| §4.1 envelope и provenance | 10 |
| §4.2 шесть семантических дат | 9 |
| §4.3 типизированные ноги | 10 |
| §4.5 раздельные идентичности | 9 |
| §4.6 семейство типов событий (подмножество этапа 1) | 10 |
| §4.8 сторнирование и замена | 11 |
| §4.9 `unknown` как значение | 10 (Confidence), 9 (Option в датах) |
| §4.10 контуры с версиями | 12 |
| §4.12 лоты, FIFO как проекция | 13 |
| §15.1 типы, делающие ошибку непредставимой | 8, 9, 10 |
| §15.2 инварианты как код | 10 (`validate_structure`), 15 (свойства) |
| §15.3 свойства с областью | 15 |
| §15.4 независимый эталон | 14 |
| §15.5 замороженные эталоны | 14 |
| §15.7 заслоны против подложных тестов | 4, 5 |
| §16.1 необратимое ядро, все десять пунктов | 16 (сводка) |
| §17 стек и структура крейт | 1, 14 |

**Осознанные переносы во второй план** (не пропуски):

| Требование | Куда |
|---|---|
| §3.1 снимки `project` / `advance`, наложения сценариев | план 2 |
| §4.11 сшивание переводов | эпик E2 |
| §5, §7, §8 оценка, облигации, вклады | эпик E3 |
| §6.1 XIRR | план 2 |
| §6.2 TWR | эпик E6 |
| §6.3–6.5 тождество результата | эпик E4 |
| §9 налоги | эпик E5 |
| §10 приёмка и сверка | эпик E2 |
| §11 периметр: маржа, РЕПО | эпик E2 (`FeeOrigin::MarginInterest` уже заложен) |
| §12 рыночные данные | эпик E3 |
| §13 REST, OpenAPI, скилл агента | план 2 |
| §14 безопасность, архивный бандл | план 2 (аутентификация), эпик E7 (шифрование токенов) |
