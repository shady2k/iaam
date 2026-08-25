# Обёртка над заслонами качества и командами запуска.
#
# Источник правды по заслонам — .github/workflows/ci.yml и таблица в
# README. Цели ниже повторяют шаги CI команда в команду: обёртка, которая
# проверяет мягче, даёт зелёный локальный прогон при красном CI, а это
# хуже отсутствия обёртки. Единственное намеренное расхождение отмечено
# у цели `test`.
#
# Makefile не входит в список файлов политики scripts/check-diff-lint.sh.
# Значит, ослабление заслона правкой этой обёртки заслон не поймает.
# Если обёртка приживётся, `Makefile` стоит добавить в тот список — но
# это правка самого скрипта, то есть изменение политики, которое вносит
# владелец репозитория (POLICY_CHANGE_APPROVED=1), а не агент.

# Внутри `nix develop` (в том числе поднятого direnv) префикс не нужен;
# снаружи цель заходит в окружение сама — иначе соберётся не тем
# тулчейном и проверит не то.
RUN := $(if $(IN_NIX_SHELL),,nix develop -c)

# База сравнения для diff-заслонов. В CI её задаёт целевая ветка PR.
BASE ?= origin/main

# Умолчания для команд запуска. У среды умолчания нет: токены у сред
# разные, и записанная не та среда оборачивается отказом шлюза, по
# тексту которого о среде не догадаться.
LABEL  ?= ноутбук
BROKER ?= tinkoff
ENVIRONMENT ?=

.DEFAULT_GOAL := help

.PHONY: help
help: ## Список целей
	@grep -hE '^[a-z][a-z0-9_-]*:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  %-14s %s\n", $$1, $$2}'

# --- Заслоны -----------------------------------------------------------

.PHONY: check
check: fmt lint arch fixtures deps test doc-test ## Всё, что не ходит в сеть и укладывается в минуты

.PHONY: fmt
fmt: ## Формат (проверка)
	$(RUN) cargo fmt --all -- --check

.PHONY: fmt-fix
fmt-fix: ## Формат (исправление)
	$(RUN) cargo fmt --all

.PHONY: lint
lint: ## Линты
	$(RUN) cargo clippy --workspace --all-targets --all-features -- -D warnings

.PHONY: arch
arch: ## Направление зависимостей, f64 и async в ядре
	$(RUN) ./scripts/check-architecture.sh

.PHONY: fixtures
fixtures: ## Замороженный эталон и мёртвые фикстуры
	$(RUN) ./scripts/check-fixtures.sh

.PHONY: deps
deps: ## Уязвимости, лицензии, источники
	$(RUN) cargo deny check

# Расхождение с CI: там шаг тестов идёт с `--all-features`, что включает
# фичу `sandbox` крейта iaam-broker и превращает прогон в поход в
# интернет (docs/deployment.md, «Два режима проверки»). Живая проверка
# вынесена в отдельную цель `sandbox`. Флаг сюда не возвращайте.
.PHONY: test
test: ## Тесты, сети не касаются
	$(RUN) cargo nextest run --workspace

.PHONY: doc-test
doc-test: ## Doc-тесты (nextest их не выполняет)
	$(RUN) cargo test --workspace --doc

.PHONY: diff-lint
diff-lint: ## Новые allow/ignore/todo! и правка файлов политики (BASE=...)
	$(RUN) ./scripts/check-diff-lint.sh $(BASE)

.PHONY: coverage
coverage: ## Отчёт покрытия в lcov.info
	$(RUN) cargo llvm-cov --workspace --lcov --output-path lcov.info

.PHONY: diff-coverage
diff-coverage: coverage ## Порог 90% на добавленных строках (BASE=...)
	$(RUN) diff-cover lcov.info --compare-branch=$(BASE) --fail-under=90

.PHONY: mutants
mutants: ## Мутационное тестирование, порог по каждому модулю (долго)
	$(RUN) ./scripts/check-mutants.sh

# Ходит в интернет и требует заведённого доступа: IAAM_DATABASE и
# IAAM_BROKER_KEY_FILE должны быть выставлены, иначе цель падает —
# режим запрошен явно, и молчаливый пропуск был бы враньём.
.PHONY: sandbox
sandbox: require-database require-broker-key ## Живая проверка шлюза брокера (сеть)
	$(RUN) cargo test -p iaam-broker --features sandbox

# --- Запуск ------------------------------------------------------------

# Путь к базе и путь к ключу умолчаний не имеют намеренно: база и ключ
# «в известном месте» — худший вид умолчания. Проверяются здесь, потому
# что сама программа сообщает о незаданной переменной отладочным выводом
# вида `Error: Invalid { name: "IAAM_DATABASE", ... }` (iaam-p28).
.PHONY: require-database
require-database:
	@test -n "$(IAAM_DATABASE)" || { \
		echo "IAAM_DATABASE не задана: укажите путь к файлу базы, например" >&2; \
		echo "  make $(MAKECMDGOALS) IAAM_DATABASE=/var/lib/iaam/iaam.db" >&2; \
		exit 1; }

.PHONY: require-broker-key
require-broker-key:
	@test -n "$(IAAM_BROKER_KEY_FILE)" || { \
		echo "IAAM_BROKER_KEY_FILE не задана: укажите файл ключа, например" >&2; \
		echo "  make $(MAKECMDGOALS) IAAM_BROKER_KEY_FILE=/etc/iaam/broker-key" >&2; \
		exit 1; }

.PHONY: run
run: require-database ## Поднять сервис (IAAM_DATABASE=...)
	$(RUN) cargo run -p iaam-bootstrap --release

# Дверь восстановления при потерянном токене владельца. В пустой базе
# заводит владельца, в непустой выпускает токен существующему. Печатает
# токен один раз — в базе остаётся только хеш.
.PHONY: owner-token
owner-token: require-database ## Токен владельца с консоли (IAAM_DATABASE=..., LABEL=...)
	IAAM_ISSUE_OWNER_TOKEN="$(LABEL)" $(RUN) cargo run -p iaam-bootstrap --release

.PHONY: broker-key
broker-key: require-database require-broker-key ## Завести ключ шифрования доступов
	@install -d -m 700 "$(dir $(IAAM_BROKER_KEY_FILE))"
	IAAM_GENERATE_BROKER_KEY=1 $(RUN) cargo run -p iaam-bootstrap --release

# Токен читается со стандартного ввода: список процессов виден всей
# машине, а история оболочки переживает сессию.
.PHONY: broker-access
broker-access: require-database require-broker-key ## Запасной путь к POST /v1/broker-access (BROKER=..., ENVIRONMENT=prod|sandbox)
	@test -n "$(ENVIRONMENT)" || { \
		echo "ENVIRONMENT не задана: prod или sandbox — токены у сред разные." >&2; \
		echo "  make broker-access BROKER=$(BROKER) ENVIRONMENT=sandbox" >&2; \
		exit 1; }
	IAAM_ADD_BROKER_ACCESS="$(BROKER)" IAAM_BROKER_ENVIRONMENT="$(ENVIRONMENT)" \
		$(RUN) cargo run -p iaam-bootstrap --release
