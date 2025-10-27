{
  description = "Nix flake for stasis (developer shell + simple build)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {inherit system;};
      in {
        # Simple package you can build with: nix build .#stasis
        packages.stasis = pkgs.stdenv.mkDerivation {
          pname = "stasis";
          version = "0.1.0";
          src = ./.;

          buildInputs = [pkgs.rustc pkgs.cargo pkgs.openssl pkgs.pkg-config pkgs.zlib];

          # Use Cargo.lock for reproducible builds when available
          buildPhase = ''
            export CARGO_HOME=$PWD/.cargo
            cargo build --release --locked
          '';

          installPhase = ''
            mkdir -p $out/bin
            cp target/release/stasis $out/bin/ || true
          '';
        };

        # Developer shell: rustc, cargo, openssl, pkg-config and git
        devShell = pkgs.mkShell {
          name = "stasis-devshell";
          buildInputs = [pkgs.rustc pkgs.cargo pkgs.openssl pkgs.pkg-config pkgs.git pkgs.zlib];
          RUSTFLAGS = "-C target-cpu=native";
          shellHook = ''
            echo "Entering stasis dev shell — run: cargo build, cargo run, or nix build .#stasis"
          '';
        };
      }
    );
}
