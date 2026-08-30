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
          '';
        };
      });
}