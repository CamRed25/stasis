{
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
        # Pure Nix build using buildRustPackage. This is hermetic and CI-friendly.
        packages.stasis = pkgs.rustPlatform.buildRustPackage rec {
          pname = "stasis";
          version = "0.1.0";
          src = ./.;

          # Use the repository Cargo.lock to avoid querying crates.io during the
          # derivation evaluation step.
          cargoLock = {lockFile = ./Cargo.lock;};

          # Dependencies required at build/runtime
          nativeBuildInputs = [pkgs.pkg-config];
          buildInputs = [pkgs.openssl pkgs.zlib pkgs.udev pkgs.dbus pkgs.libinput];

          # Optionally set RUSTFLAGS or other env vars
          RUSTFLAGS = "-C target-cpu=native";
        };
        # not much testing done here, feel free to change if needed.
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
