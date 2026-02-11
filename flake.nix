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

            # Container runtime (rootless, for testcontainers)
            podman
            slirp4netns      # rootless networking
            fuse-overlayfs   # rootless storage
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

            # === Podman rootless setup for testcontainers ===
            export XDG_RUNTIME_DIR="''${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
            export DOCKER_HOST="unix://$XDG_RUNTIME_DIR/podman/podman.sock"
            # Ryuk (testcontainers cleanup) can be flaky with Podman
            export TESTCONTAINERS_RYUK_DISABLED="true"

            # Configure container registries (Docker Hub as default, like Docker)
            CONTAINERS_DIR="$XDG_RUNTIME_DIR/containers"
            mkdir -p "$CONTAINERS_DIR"

            export CONTAINERS_REGISTRIES_CONF="$CONTAINERS_DIR/registries.conf"
            cat > "$CONTAINERS_REGISTRIES_CONF" << 'EOF'
            unqualified-search-registries = ["docker.io"]

            [[registry]]
            prefix = "docker.io"
            location = "docker.io"
            EOF

            # Image signature policy (accept all - like Docker default)
            # Must be at ~/.config/containers/policy.json (Podman's fixed lookup path)
            POLICY_FILE="$HOME/.config/containers/policy.json"
            if [ ! -f "$POLICY_FILE" ]; then
              mkdir -p "$(dirname "$POLICY_FILE")"
              cat > "$POLICY_FILE" << 'EOF'
            {
              "default": [{ "type": "insecureAcceptAnything" }]
            }
            EOF
              echo "Created $POLICY_FILE"
            fi

            # Start Podman socket via systemd (preferred) or manually
            if command -v systemctl &>/dev/null && systemctl --user is-system-running &>/dev/null; then
              systemctl --user start podman.socket 2>/dev/null && \
                echo "Podman socket: $DOCKER_HOST (systemd)"
            elif [ ! -S "$XDG_RUNTIME_DIR/podman/podman.sock" ]; then
              mkdir -p "$XDG_RUNTIME_DIR/podman"
              podman system service -t 0 "$DOCKER_HOST" &>/dev/null &
              sleep 0.5
              [ -S "$XDG_RUNTIME_DIR/podman/podman.sock" ] && \
                echo "Podman socket: $DOCKER_HOST (manual)"
            else
              echo "Podman socket: $DOCKER_HOST (existing)"
            fi

            echo ""
            echo "Run tests: set -a && source .env && set +a && cargo test"
          '';
        };
      });
}
