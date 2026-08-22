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
            # Покрытие по диффу: cargo llvm-cov строит полный отчёт,
            # но порог на добавленных строках задаёт diff-cover.
            pkgs.python3Packages.diff-cover
          ];
          # rusqlite с feature "bundled" компилирует SQLite из исходников
          shellHook = ''
            echo "iaam dev shell · $(rustc --version)"
          '';
        };
      });
}
