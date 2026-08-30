#!/usr/bin/env bash
# Mutation testing with a threshold for EVERY critical module (§15.7).
# A project-wide threshold allows an uncovered module to hide behind a well-
# covered one, so the module list is defined explicitly and each is checked separately.
set -euo pipefail

# The root is found from the script directory, not the caller cwd: otherwise
# running from a non-git directory yields an empty string, `cd ""` (in bash this
# is a successful no-op), and the guard checks the wrong directory. Failure to
# determine the root means the guard must refuse to proceed, not succeed.
if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "MUTANTS: could not determine the repository root from $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

# Critical absolute-value modules. The list grows with the core; deleting lines
# from here is caught by the policy guard in check-diff-lint.sh (the scripts/
# directory is included in its list of policy files).
MODULES=(
  "crates/iaam-core/src/numeric/exact.rs"
  "crates/iaam-core/src/money.rs"
  "crates/iaam-core/src/dates.rs"
  "crates/iaam-core/src/event/kind.rs"
  "crates/iaam-core/src/event/mod.rs"
  "crates/iaam-core/src/event/correction.rs"
  "crates/iaam-core/src/contour.rs"
  "crates/iaam-core/src/rules/lot_disposal.rs"
  # Dec arithmetic is the numerical basis of all monetary calculations.
  # It was absent from the first revision of the plan; it was added during
  # execution task 9 (iaam-1fk.22).
  "crates/iaam-core/src/numeric/decimal.rs"
  "crates/iaam-core/src/projection/balances.rs"
  "crates/iaam-core/src/projection/lots.rs"
  "crates/iaam-core/src/projection/flows.rs"
  "crates/iaam-core/src/projection/invariants.rs"
  "crates/iaam-core/src/projection/state.rs"
  "crates/iaam-core/src/projection/mod.rs"
  "crates/iaam-core/src/numeric/xirr.rs"
  "crates/iaam-core/src/returns/xirr.rs"
  # Report contract: this is where the system decides whether to trust a figure.
  "crates/iaam-core/src/returns/mod.rs"
  "crates/iaam-core/src/valuation.rs"
  # The price-selection rule determines WHICH number the report shows: a mutant
  # that shifts an age-band boundary or the order of criteria does not break
  # a single total—it silently moves the value of positions to another equally
  # plausible figure. Added during execution E3.3 (iaam-d8b.1).
  "crates/iaam-core/src/rules/valuation.rs"
  # Legacy boundary: a mutant returning a candidate instead of LegacyDerived
  # launders an old carried-forward price as a fresh observation. Numerically,
  # this is indistinguishable from an honest recalculation (decision 0002).
  "crates/iaam-core/src/valuation/candidate.rs"
  # The half-open validity interval of an alias determines which instrument
  # stands behind an external code on a given date. An error here does not
  # distort a single total—it substitutes a security, which cannot be detected
  # from the numbers alone. A mutant changing `<` to `<=` in
  # `AliasInterval::covers` makes the ISIN changeover day belong to two issues
  # at once. Added during execution E3.1 (iaam-30v) with owner permission.
  "crates/iaam-core/src/instrument.rs"
  # Reconciliation determines whether a figure can be trusted. An error here
  # does not distort a single total—it declares unchecked data verified,
  # which cannot be detected from the numbers alone (§10.3). Added in
  # plan E2, task 9.
  "crates/iaam-core/src/reconciliation/claim.rs"
  "crates/iaam-core/src/reconciliation/observed.rs"
  "crates/iaam-core/src/reconciliation/check.rs"
  "crates/iaam-core/src/reconciliation/evidence.rs"
  "crates/iaam-core/src/reconciliation/mod.rs"
  # Ownership and the calculation date determine who was entitled to a payment.
  # A mutant here does not change a single total—it changes what the amount
  # represents, exactly as in instrument.rs and reconciliation.
  # The first candidate is `latest < day` in settlement.rs: shifting the
  # boundary to `<=` makes the unprovable appear proven.
  "crates/iaam-core/src/settlement.rs"
  "crates/iaam-core/src/projection/ownership.rs"
  "crates/iaam-core/src/rules/posting_match.rs"
  # The perimeter determines where the system refuses to calculate (§11).
  # A mutant that removes the refusal presents the economics of unsupported
  # financing as calculated.
  "crates/iaam-core/src/perimeter.rs"
  # Storage owns the trust boundary and the append-only journal: these are
  # safety properties, not conveniences. The plan did not include them in the
  # guard; they were added during execution task 11, where the run produced ten
  # surviving mutants.
  "crates/iaam-store/src/events.rs"
  "crates/iaam-store/src/snapshots.rs"
  "crates/iaam-store/src/reference.rs"
  "crates/iaam-store/src/tokens.rs"
  "crates/iaam-store/src/bundle.rs"
  # Ingestion constructs the signs and legs of an event: an error here records
  # a fact in the append-only journal that never occurred.
  "crates/iaam-ingest/src/operation.rs"
  "crates/iaam-ingest/src/csv_source.rs"
  "crates/iaam-ingest/src/verdict.rs"
  # Application and transport: token scope, the rate limiter, and verdict
  # numbering live here.
  #
  # Two shell files are NOT included in the list, and this is a deliberate
  # decision rather than an omission (§15.7 requires written justification;
  # the full analysis is in the description of bead iaam-1fk.18):
  #
  #   adapters/sqlite.rs — replacing the `load_snapshot` snapshot read with
  #   “no snapshot” has no observable consequence. This is intentional:
  #   the snapshot is a cache, and the identity “advance equals full
  #   recalculation” is the central projection invariant. The mutant changes
  #   the amount of work, not the answer. The file's other methods delegate to
  #   iaam-store (listed above) and are covered by contract tests.
  #
  #   scenarios/reports.rs — replacing the condition “an invariant violation
  #   is not a reason to recalculate” with “always recalculate”: a full
  #   recalculation produces exactly the same violation and the same answer.
  #   This also changes the amount of work, not the answer. The predicates
  #   themselves (snapshot_may_be_saved, recompute_is_worth_it) are extracted
  #   into separate functions and tested directly by unit tests.
  "crates/iaam-app/src/ports.rs"
  "crates/iaam-app/src/error.rs"
  "crates/iaam-app/src/scenarios/ingest.rs"
  "crates/iaam-server/src/routes.rs"
  "crates/iaam-server/src/auth.rs"
  "crates/iaam-server/src/rate_limit.rs"
  "crates/iaam-server/src/dto.rs"
  # The reference implementation is mutated alongside production: an error in
  # the reference masks an error in production just as effectively as the
  # reverse (§15.4).
  "crates/iaam-oracle/src/lots_reference.rs"
  # Outbound HTTP. Added during execution E3.2 part 1 (iaam-faf)
  # with owner permission. The first run also produced 13 survivors out of 57,
  # and four of them were not cosmetic:
  #
  #   client_for -> Ok(Default::default()) survived—it replaces the entire
  #   client build with a default client, without the embedded root and without
  #   tls_certs_only, and nothing caught it. The anchor table was tested,
  #   which is the INTENT, but whether the anchor was actually applied was not.
  #
  #   Secret::expose -> "" and HttpRequest::bearer -> None survived:
  #   nothing checked which token reached the request. An empty token would
  #   cause an authorization refusal indistinguishable from a gateway refusal.
  #
  #   Debug for Secret -> Ok(Default::default()) survived: the test checked
  #   only a negative assertion (“the secret is absent from the output”), and
  #   empty output satisfied it. The same class had already been caught for
  #   IssuedToken.
  #
  # destination.rs is included because it contains addresses, and an error
  # in them does not distort a single total—it sends the request to another
  # endpoint or environment, where a plausible answer may arrive. This cannot
  # be detected from the numbers alone. The mutants here are less subtle than
  # the danger itself: cargo-mutants replaces the returned base_url with an
  # empty string or garbage, but does not swap match branches. The guard catches
  # “addresses are not tested at all,” not “production was confused with the
  # sandbox”; the latter is guarded by the test
  # the_sandbox_is_a_different_host_not_a_different_path.
  "crates/iaam-http/src/trust.rs"
  "crates/iaam-http/src/destination.rs"
  "crates/iaam-http/src/request.rs"
  "crates/iaam-http/src/response.rs"
  "crates/iaam-http/src/resilience.rs"
  "crates/iaam-http/src/client.rs"
  # Parsing source responses. Added during execution E3.2 part 2
  # (iaam-tv2) with owner permission. The first run produced 16 survivors
  # out of 139, and all sixteen were in the central bank module:
  #
  #   parse_daily -> Ok(vec![]) survived because the test asserted
  #   “there are more raw records than observations”—which is also true when
  #   there are ZERO observations. The test guarding the skipping of unknown
  #   currencies passed when all records were skipped. This is the same class
  #   described in ADR-0002: checking only negative assertions.
  #
  #   currency_from_iso -> None and deletion of every mapping branch
  #   survived: nothing checked that USD, EUR, and CNY are accepted.
  #
  #   dotted -> String::new() survived: nothing asserted the date format in
  #   the request to the central bank. An empty string causes no error, and a
  #   response for a different period is silently incorrect data.
  #
  # A parsing error here does not distort an amount; it substitutes the
  # observation from which the amount is then calculated. This cannot be
  # detected from the numbers alone—exactly the same reason that instrument.rs
  # and reconciliation are included in the list.
  "crates/iaam-market/src/cbr/fx.rs"
  "crates/iaam-market/src/cbr/key_rate.rs"
  "crates/iaam-market/src/cbr/mod.rs"
  "crates/iaam-market/src/moex/parse.rs"
  "crates/iaam-market/src/moex/mod.rs"
  # The completeness boundary determines whether a partial export is presented
  # as complete (iaam-023.5). The run produced six survivors: nobody checked
  # the count of written lines, and two replacements of `||` with `&&` in the
  # lease condition allowed two runs to write to one series interleaved and
  # both advance the boundary. None of the six changes a single number—they
  # change what that number represents.
  "crates/iaam-store/src/market.rs"
  # New event and amortisation dispatchers. Added during execution
  # E3.4.1 (iaam-8mv) with owner permission; the run produced 64 mutants,
  # with no survivors. legs.rs determines whether an event with an extraneous
  # movement is accepted; amortisation.rs determines how much value is returned
  # together with principal; offers.rs maintains the offer-chain invariant.
  # No mutant here changes a total directly—they change what the amount
  # represents. This is the same class as instrument.rs and reconciliation.
  "crates/iaam-core/src/event/corporate_action.rs"
  "crates/iaam-core/src/event/offer.rs"
  "crates/iaam-core/src/event/legs.rs"
  "crates/iaam-core/src/rules/amortisation.rs"
  # These rules do not add money, but they determine the meaning of an already
  # calculated total: returned_share retains the return share, and allocation
  # selects which part of the tax basis is returned to the owner with
  # amortisation. An error in the boundary or allocation leaves the numbers
  # plausible but substitutes the economic calculation basis—the same risk as
  # in instrument.rs and reconciliation.
  "crates/iaam-core/src/rules/returned_share.rs"
  "crates/iaam-core/src/rules/allocation.rs"
  # Accrued-interest rule and schedule-derived values. Added during execution
  # E3.4.4 (iaam-pa0m). accrued_interest.rs maintains the half-open period
  # boundaries and rounding strategy: both change the amount with the same
  # inputs_hash, and a mutant shifting `<` to `<=` would produce a full coupon
  # instead of zero at the end of the period. finality.rs determines whether
  # the principal has been fully returned; posting.rs determines which field
  # supplies the payment date—payment_date versus accrual_end. None of these
  # mutants changes a number directly; they change what the number represents.
  "crates/iaam-core/src/rules/accrued_interest.rs"
  "crates/iaam-core/src/bond/finality.rs"
  "crates/iaam-core/src/bond/posting.rs"
  # principal.rs derives the outstanding balance from a sequence of repayments.
  # Shifting the boundaries of that sequence does not change an amount directly,
  # but it silently changes the basis of all subsequent calculations for the
  # security; therefore the module is guarded separately alongside finality.rs
  # and posting.rs.
  "crates/iaam-core/src/bond/principal.rs"
  "crates/iaam-core/src/projection/offers.rs"
  # Payment schedule (E3.4 part 2). An error in completeness invariants does not
  # change a single total—it changes what the system treats as a complete
  # schedule, and a truncated series silently shortens W_T.
  "crates/iaam-market/src/schedule/completeness.rs"
  "crates/iaam-market/src/moex/bondization.rs"
  "crates/iaam-app/src/scenarios/schedule.rs"
  # observation.rs is intentionally NOT included in the list: it contains only
  # type declarations, there is nothing to mutate, and `cargo mutants --list`
  # returns zero. The guard treats such a module as a refusal—and rightly so:
  # zero mutants are indistinguishable from a typo in the path. The guarantee
  # that “time axes cannot be confused” comes from the compiler, not a mutation
  # run.
  #
  # rules/quotation.rs is intentionally NOT included in the list (§15.7
  # requires written justification—here it is; analysis is in bead iaam-rjvb).
  # `cargo mutants --list` returns three mutants, and all three are unviable:
  # money_per_unit returns Result<(Dec, CurrencyCode), QuotationError>,
  # and there is no replacement value to substitute. Neither Dec nor
  # CurrencyCode implements Default—the absence of a default currency is
  # intentional; Err(NumericError::Overflow) and Err(MoneyError::Overflow) are
  # not QuotationError, and no conversion exists. The three-branch match on
  # QuotationBasis is not covered by the mutation guard at all, and including
  # the line would make the guard either red (zero mutants—a refusal) or
  # falsely green. The branches are fixed by six unit tests in the file itself
  # and by the property of value linearity with respect to residual principal
  # (tests/properties.rs).
  "crates/iaam-core/src/rules/cashflow.rs"
  "crates/iaam-core/src/returns/zero_reinvestment.rs"
  "crates/iaam-core/src/bond/offer.rs"
)

# An empty list is a guard that always passes. Emptying the array must cause
# a refusal, not “verified zero modules, no violations.”
if [ "${#MODULES[@]}" -eq 0 ]; then
  echo "MUTANTS: critical module list is empty — the guard checks nothing." >&2
  exit 1
fi

# The run is single-threaded by design, not by default. Parallel cargo-mutants
# jobs do not share a build directory: each job creates its own temporary
# directory and recompiles package dependencies—from 25 seconds on iaam-core
# to 134 seconds on iaam-store. Measurement on 20 iaam-core mutants with three
# jobs: 5 minutes versus 2.7 minutes for a per-file run, meaning the triple cold
# rebuild consumes the entire gain. On six cores with 12 GB of memory, 3 GB of
# which is already swapped, there is no capacity for more jobs. Speed is sought
# by reducing the number of checks, not by running them concurrently.

# Tools are checked in advance: `command not found` in the middle of a pipeline
# is harder to understand than an explicit message, and under `|| true` it
# would pass as a success altogether.
for tool in cargo jq awk; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "MUTANTS: $tool is unavailable — the guard cannot be checked." >&2
    exit 1
  fi
done
if ! cargo mutants --version >/dev/null 2>&1; then
  echo "MUTANTS: cargo-mutants is unavailable — the guard cannot be checked." >&2
  exit 1
fi

ERR_FILE=$(mktemp)
trap 'rm -f "$ERR_FILE"' EXIT

# cargo metadata is read ONCE: calling it in the loop creates N chances for one
# call to fail silently. Failure of cargo metadata itself means guard refusal.
if ! META=$(cargo metadata --no-deps --format-version 1 2>"$ERR_FILE"); then
  echo "MUTANTS: cargo metadata did not run — guard cannot be checked." >&2
  cat "$ERR_FILE" >&2
  exit 1
fi

# The package name is obtained from cargo metadata using the crate manifest,
# not from the directory name: they match by convention, but the guard must not
# rely on that convention. Failure to find a package is a refusal, not a skip.
package_of() {
  local module="$1" crate_dir manifest name
  crate_dir=$(printf '%s\n' "$module" | cut -d/ -f1-2)
  manifest="$REPO_ROOT/$crate_dir/Cargo.toml"
  name=$(printf '%s' "$META" | jq -r --arg m "$manifest" \
    '.packages[] | select(.manifest_path == $m) | .name')
  printf '%s' "$name"
}

# Count how many lines of `--list` output belong to the file. Compare the prefix
# using `index`, not grep with a regular expression: a dot in a path matches any
# character in a regular expression, which would mix counters between adjacent
# modules. Empty output means zero lines, not one: `printf '%s\n' ""` would
# produce `wc -l` = 1 and hide an empty list.
count_for_file() {
  local listing="$1" file="$2"
  if [ -z "$listing" ]; then
    printf '0'
    return
  fi
  printf '%s\n' "$listing" |
    awk -v prefix="$file:" 'index($0, prefix) == 1' | wc -l | tr -d ' '
}

# `cargo mutants --list` for a package and all its modules at once. The return
# code is checked explicitly: `|| true` on the pipeline would turn a tool
# failure into “no mutants.”
list_mutants() {
  local package="$1"
  shift
  local out
  if ! out=$(cargo mutants --list --package "$package" "$@" 2>"$ERR_FILE"); then
    echo "MUTANTS: cargo mutants --list did not run for package $package" >&2
    cat "$ERR_FILE" >&2
    return 1
  fi
  printf '%s' "$out"
}

fail=0
checked=0
skipped=0
inert=0

# Modules are grouped by package, and `cargo mutants` is invoked ONCE for a
# package with all its files at once.
#
# Previously the invocation was per file, and each one rebuilt the baseline:
# according to the full-run log, from 25 seconds on iaam-core to 134 seconds on
# iaam-store, amounting to more than an hour of pure rebuilds across 66 modules.
# The guard still answers for each module: listing, thresholding, and parsing
# survivors below are done per file, and each mutant's file is read from
# outcomes.json. This reduces the cost of executing the guard, not its rigor.
declare -A PACKAGE_FILES=()
PACKAGES=()

for module in "${MODULES[@]}"; do
  if [ ! -f "$module" ]; then
    echo "skip (still not created): $module"
    skipped=$((skipped + 1))
    continue
  fi

  package=$(package_of "$module")
  if [ -z "$package" ]; then
    echo "  REFUSAL: could not determine the package for $module by cargo metadata" >&2
    fail=1
    continue
  fi

  if [ -z "${PACKAGE_FILES[$package]+present}" ]; then
    PACKAGES+=("$package")
    PACKAGE_FILES[$package]="$module"
  else
    PACKAGE_FILES[$package]+=$'\n'"$module"
  fi
done

# Error values for mutants in functions returning Result. Previously these
# lived in .cargo/mutants.toml and applied to ALL packages, although
# `crate::numeric::NumericError` and `crate::money::MoneyError` are declared
# only in iaam-core: in every other package such a mutant never compiles, yet
# the full build cost is still paid to establish that fact. According to the
# reports, all 70 of 70 such mutants in an iaam-store run were unviable—half
# the package's mutants. In iaam-core, tests killed 31 of 206 such mutants, so
# they remain enabled here.
#
# The flags are passed both to `--list` and to the run: if they diverged, the
# declared “mutants to check” would no longer match what was actually checked.
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
  echo "=== package $package: modules ${#files[@]} ==="

  mapfile -t error_args < <(error_values_for "$package")

  list_args=()
  for file in "${files[@]}"; do
    list_args+=(--file "$file")
  done

  # --- Guard against a “configured but nonfunctional” check ---
  # `cargo mutants` exits with code 0 when there are ZERO mutants: both when a
  # file is excluded through exclude_globs/exclude_re in .cargo/mutants.toml
  # and when the module list contains a path typo. Verified by running
  # cargo-mutants 27.1.0: “Found 0 mutants to test,” return code 0.
  # Without this check, the run would print “no survivors” for a module that
  # was never tested at all—making exclusion of a domain module through the
  # configuration appear to be a passing guard.
  #
  # Distinguish two causes of an empty list by comparing it with --no-config:
  #   configuration suppresses mutants -> refusal; the domain must not be hidden;
  #   no mutants even without configuration -> the file has no mutable code.
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
      echo "  REFUSAL: configuration suppresses mutants in $file" >&2
      echo "  mutants without configuration: $n_without, with configuration: 0." >&2
      echo "  Excluding a domain module from mutation testing is a way to" >&2
      echo "  hide false tests. Remove the module from .cargo/mutants.toml." >&2
      fail=1
      continue
    fi

    if [ "$n_with" -eq 0 ]; then
      # The file exists and is not suppressed, but it contains no mutable code
      # (for example, only type declarations). This must not be silent: from
      # outside, silence is indistinguishable from a passing check.
      echo "  NO MUTANTS: $file contains no mutable code — nothing to check."
      inert=$((inert + 1))
      continue
    fi

    echo "  $file: mutants to check $n_with"
    run_files+=("$file")
    run_args+=(--file "$file")
  done

  if [ "${#run_files[@]}" -eq 0 ]; then
    continue
  fi

  # Which tests check the package.
  #
  # By default, cargo-mutants with `--package X` runs tests ONLY for package X.
  # This is correct for most modules: their tests live alongside them.
  # But application scenarios are checked by contract tests that live in
  # iaam-server, and without an explicit specification the guard would print
  # “no survivors” for code that nobody tested. Verified by execution:
  # 46 survivors versus 35 on the very same code.
  #
  # Specify only the required packages, not `--test-workspace true`: running
  # the entire test suite for every mutant raises the cost from one and a half
  # seconds to thirteen, approximately ninefold.
  extra_test_packages=()
  case "$package" in
    iaam-app)
      extra_test_packages=(--test-package iaam-app --test-package iaam-server)
      ;;
  esac

  out_dir="target/mutants/$package"
  # `--output DIR` does not create intermediate directories: without mkdir the
  # run fails with “create output parent directory,” and its return code is
  # indistinguishable from surviving mutants.
  rm -rf "$out_dir"
  mkdir -p "$out_dir"

  # `--output DIR` creates mutants.out INSIDE DIR—the report is stored in
  # "$out_dir/mutants.out/", not "$out_dir/".
  report="$out_dir/mutants.out"

  # `--profile mutant` is a separate profile with reduced debug information
  # (justification in Cargo.toml). Each mutant requires linking all package test
  # targets again, and with `debug = 2`, three quarters of a test binary is
  # debug information.
  if cargo mutants --package "$package" "${run_args[@]}" \
      "${extra_test_packages[@]}" "${error_args[@]}" \
      --profile mutant --jobs 1 --output "$out_dir"; then
    for file in "${run_files[@]}"; do
      echo "  no survivors: $file"
      checked=$((checked + 1))
    done
    continue
  fi

  fail=1

  # A non-zero exit code does not necessarily mean surviving mutants: build
  # failures, timeouts, and unviable mutants also end this way. The cause is
  # taken from the report, not guessed from the return code. No report means
  # the run itself failed and must not be described as “survivors.”
  if [ ! -f "$report/outcomes.json" ]; then
    echo "  REFUSAL: package $package run failed and left no report" >&2
    echo "  ($report/outcomes.json is absent). This is a tool failure," >&2
    echo "  not a check result." >&2
    continue
  fi

  if ! counters=$(jq -r '[.missed, .timeout, .unviable, .total_mutants] | @tsv' \
      "$report/outcomes.json" 2>"$ERR_FILE"); then
    echo "  REFUSAL: could not parse $report/outcomes.json" >&2
    cat "$ERR_FILE" >&2
    continue
  fi
  IFS=$'\t' read -r n_missed n_timeout n_unviable n_total <<<"$counters"
  echo "  package $package: total $n_total, survived $n_missed, timeout $n_timeout," \
    "unviable $n_unviable" >&2

  if [ "${n_missed:-0}" -eq 0 ] && [ "${n_timeout:-0}" -eq 0 ]; then
    # The run failed for a reason other than mutants: the build, environment,
    # or tool itself. This must not be attributed to modules, and they must not
    # be marked “verified” either.
    echo "  Package $package run did not pass despite having no surviving mutants —" >&2
    echo "  see $report/" >&2
    continue
  fi

  # Analysis is per file: a package-level run is cheaper than per-file runs,
  # but the guard must answer for each module. Otherwise one survivor taints
  # the whole package, with no indication where to find it.
  for file in "${run_files[@]}"; do
    if ! survivors=$(jq -r --arg f "$file" \
        '.outcomes[]
         | select(.summary == "MissedMutant" or .summary == "Timeout")
         | select(.scenario.Mutant.file == $f)
         | "    " + .summary + ": " + .scenario.Mutant.name' \
        "$report/outcomes.json" 2>"$ERR_FILE"); then
      echo "  REFUSAL: could not parse survivors for $file" >&2
      cat "$ERR_FILE" >&2
      continue
    fi

    if [ -n "$survivors" ]; then
      echo "  SURVIVING MUTANTS in $file:" >&2
      printf '%s\n' "$survivors" >&2
    else
      checked=$((checked + 1))
    fi
  done
done

echo ""
echo "Modules: verified $checked, without mutable code $inert, skipped (not created) $skipped."

if [ "$fail" -ne 0 ]; then
  echo "" >&2
  echo "A surviving mutant means that some test checks nothing." >&2
  echo "A mutant may be declared equivalent only with written" >&2
  echo "justification in the bead description (§15.7)." >&2
  exit 1
fi
echo "Mutation testing passed for all existing modules."