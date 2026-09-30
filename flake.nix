{
  description = "IAAM — investment tracking";

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
            # Differential coverage: cargo llvm-cov builds the full report,
            # but diff-cover sets the threshold for added lines.
            pkgs.python3Packages.diff-cover
          ];
          # rusqlite with the "bundled" feature compiles SQLite from source
          shellHook = ''
            # Write to stderr, not stdout: a greeting on stdout ends up in
            # any redirected output and corrupts it. The fixture generator
            # writes JSON to stdout, and the banner made it impossible to parse.
            echo "iaam dev shell · $(rustc --version)" >&2

            # One build directory per repository (iaam-eoji9): every checkout,
            # the main one and each git worktree under ~/.herdr/worktrees or
            # .claude/worktrees, builds into the main checkout's target/, found
            # through git's common directory. A per-worktree target/ was 11-25 GB
            # each and filled the disk during parallel runs. A value the caller
            # set is kept. Outside a git checkout nothing is set.
            if [ -z "''${CARGO_TARGET_DIR:-}" ]; then
              if iaam_common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null); then
                export CARGO_TARGET_DIR="$(dirname "$iaam_common")/target"
              fi
              unset iaam_common
            fi
            # Incremental caches are kept per branch and were most of the shared
            # directory's growth (70 GB measured on 2026-09-28); a worker builds
            # its branch a few times, so they buy it little. A value the caller
            # set is kept.
            export CARGO_INCREMENTAL="''${CARGO_INCREMENTAL:-0}"
          '';
        };
      });
}