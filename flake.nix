{
  description = "BAML Agent Platform development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        # Rust edition 2024 requires nightly
        rustToolchain = pkgs.rust-bin.nightly.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            # Rust toolchain
            rustToolchain

            # C compiler for quickjs_runtime (compiles QuickJS from source)
            clang
            llvmPackages.bintools

            # Build essentials
            pkg-config
            cmake

            # For linking
            libiconv

            # Cargo tools used in pre-commit
            cargo-deny
            cargo-machete
            cargo-outdated
            typos

            # Testing tools
            cargo-insta

            # Pre-commit
            pre-commit
          ] ++ lib.optionals stdenv.isDarwin [
            darwin.apple_sdk.frameworks.Security
            darwin.apple_sdk.frameworks.SystemConfiguration
          ];

          # Environment variables for compilation
          env = {
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            # Ensure cargo can find the linker
            CC = "clang";
          };

          shellHook = ''
            echo "BAML Agent Platform development environment"
            echo "Rust: $(rustc --version)"
            echo ""
            echo "Run tests with: cargo test"
            echo "For API-key-dependent tests: set -a && source .env && set +a && cargo test"
          '';
        };
      });
}
