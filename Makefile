# Wrapper for quality gates and run commands.
#
# The source of truth for the gates is .github/workflows/ci.yml and the table in
# README. The targets below repeat the CI steps command for command: a wrapper that
# checks less strictly can produce a green local run while CI is red, which is
# worse than having no wrapper. There are currently no intentional discrepancies:
# the last one, in target `test`, was removed in iaam-829 by changing CI itself.
#
# Makefile is not included in the list of policy files in scripts/check-diff-lint.sh.
# This means that weakening a guard by changing this wrapper will not be caught by
# the guard. If the wrapper proves useful, `Makefile` should be added to that list —
# but that requires changing the script itself, which is a policy change that must
# be made by the repository owner (POLICY_CHANGE_APPROVED=1), not an agent.

# Inside `nix develop` (including one started by direnv), the prefix is unnecessary;
# outside it, the target enters the environment itself — otherwise the project will
# be built with the wrong toolchain and check the wrong thing.
RUN := $(if $(IN_NIX_SHELL),,nix develop -c)

# Comparison base for diff guards. In CI it is set to the PR target branch.
BASE ?= origin/main

# Defaults for run commands. There is no default environment: tokens differ between
# environments, and recording the wrong environment results in a gateway rejection
# whose message gives no clue that the environment is the problem.
LABEL  ?= laptop
BROKER ?= tinkoff
ENVIRONMENT ?=

.DEFAULT_GOAL := help

.PHONY: help
help: ## List targets
	@grep -hE '^[a-z][a-z0-9_-]*:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  %-14s %s\n", $$1, $$2}'

# --- Quality gates -----------------------------------------------------

.PHONY: check
check: fmt lint arch privacy fixtures deps test doc-test ## Everything that avoids the network and completes within minutes

.PHONY: fmt
fmt: ## Format (check)
	$(RUN) cargo fmt --all -- --check

.PHONY: fmt-fix
fmt-fix: ## Format (fix)
	$(RUN) cargo fmt --all

.PHONY: lint
lint: ## Lints
	$(RUN) cargo clippy --workspace --all-targets --all-features -- -D warnings

.PHONY: arch
arch: ## Dependency direction, f64, and async in the core
	$(RUN) ./scripts/check-architecture.sh

.PHONY: hooks
hooks: ## Install the git hooks, including the privacy guard
	./scripts/install-hooks.sh

.PHONY: privacy
privacy: ## No personal data in the tree (shapes only; needs no configuration)
	$(RUN) ./scripts/check-no-personal-data.sh $(BASE)

.PHONY: fixtures
fixtures: ## Frozen reference data and dead fixtures
	$(RUN) ./scripts/check-fixtures.sh

.PHONY: deps
deps: ## Vulnerabilities, licenses, sources
	$(RUN) cargo deny check

# There is no `--all-features` here, and CI does not use it either (iaam-829):
# the flag would enable the `sandbox` feature of the iaam-broker crate and turn
# the run into an internet request (docs/deployment.md, “Two check modes”).
# The live check has been moved to the separate `sandbox` target. Do not add
# the flag back here.
.PHONY: test
test: ## Tests that do not access the network
	$(RUN) cargo nextest run --workspace

.PHONY: doc-test
doc-test: ## Doc-tests (nextest does not run them)
	$(RUN) cargo test --workspace --doc

.PHONY: diff-lint
diff-lint: ## New allow/ignore/todo! directives and changes to policy files (BASE=...)
	$(RUN) ./scripts/check-diff-lint.sh $(BASE)

.PHONY: coverage
coverage: ## Coverage report in lcov.info
	$(RUN) cargo llvm-cov --workspace --lcov --output-path lcov.info

.PHONY: diff-coverage
diff-coverage: coverage ## 90% threshold for added lines (BASE=...)
	$(RUN) diff-cover lcov.info --compare-branch=$(BASE) --fail-under=90

.PHONY: mutants
mutants: ## Mutation testing with a threshold for each module (slow)
	$(RUN) ./scripts/check-mutants.sh

.PHONY: mutants-diff
mutants-diff: ## Mutants only in changed lines (BASE=...). Fast, but NOT a gate
	$(RUN) env BASE=$(BASE) ./scripts/mutants-in-diff.sh

# Accesses the internet and requires configured access: IAAM_DATABASE and
# IAAM_BROKER_KEY_FILE must be set, otherwise the target fails — the mode was
# requested explicitly, and silently skipping it would be misleading.
.PHONY: sandbox
sandbox: require-database require-broker-key ## Live broker gateway check (network)
	$(RUN) cargo test -p iaam-broker --features sandbox

# --- Running -----------------------------------------------------------

# The database path and key path intentionally have no defaults: a database and
# key “in a known location” are the worst kind of default. They are checked here
# because the program itself reports an unset variable with debug output such as
# `Error: Invalid { name: "IAAM_DATABASE", ... }` (iaam-p28).
.PHONY: require-database
require-database:
	@test -n "$(IAAM_DATABASE)" || { \
		echo "IAAM_DATABASE is not set: specify the path to the database file, for example" >&2; \
		echo "  make $(MAKECMDGOALS) IAAM_DATABASE=/var/lib/iaam/iaam.db" >&2; \
		exit 1; }

.PHONY: require-broker-key
require-broker-key:
	@test -n "$(IAAM_BROKER_KEY_FILE)" || { \
		echo "IAAM_BROKER_KEY_FILE is not set: specify the path to the key file, for example" >&2; \
		echo "  make $(MAKECMDGOALS) IAAM_BROKER_KEY_FILE=/etc/iaam/broker-key" >&2; \
		exit 1; }

.PHONY: run
run: require-database ## Start service (IAAM_DATABASE=...)
	$(RUN) cargo run -p iaam-bootstrap --release -- serve

# Recovery path for a lost owner token: it issues a token to the existing sole
# owner and prints it once — only the hash remains in the database. An empty
# database has no owner to issue to; `iaam claim --label <label>` creates one,
# and it is deliberately not a make target, because claiming an instance is an
# act, not a build step (ADR-0003).
.PHONY: owner-token
owner-token: require-database ## Owner token from the console (IAAM_DATABASE=..., LABEL=...)
	$(RUN) cargo run -p iaam-bootstrap --release -- token issue --label "$(LABEL)"

.PHONY: broker-key
broker-key: require-database require-broker-key ## Create a key for encrypting credentials
	@install -d -m 700 "$(dir $(IAAM_BROKER_KEY_FILE))"
	$(RUN) cargo run -p iaam-bootstrap --release -- broker key generate

# The token is read from standard input: the process list is visible across the
# machine, and shell history outlives the session.
.PHONY: broker-access
broker-access: require-database require-broker-key ## Provision a broker credential locally (BROKER=..., ENVIRONMENT=prod|sandbox)
	@test -n "$(ENVIRONMENT)" || { \
		echo "ENVIRONMENT is not set: prod or sandbox — tokens differ between environments." >&2; \
		echo "  make broker-access BROKER=$(BROKER) ENVIRONMENT=sandbox" >&2; \
		exit 1; }
	$(RUN) cargo run -p iaam-bootstrap --release -- broker access add \
		--broker "$(BROKER)" --environment "$(ENVIRONMENT)"