{
  description = "Wrapper for Stasis non-flake repo";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
  };

  outputs = {
    self,
    nixpkgs,
  }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs {
      system = system;
      overlays = [
        (import (builtins.fetchTarball {
          url = "https://github.com/oxalica/rust-overlay/archive/refs/heads/master.tar.gz";
          sha256 = "sha256:16d5wlabz1fydrh2hsh4vabidysh7ja9agx4d5sf79811j7fwf7r";
        }))
      ];
    };

    stasisSrc = pkgs.fetchFromGitHub {
      owner = "saltnpepper97";
      repo = "stasis";
      rev = "main";
      sha256 = "sha256-MOb56PJS5gBITScJMvou/Z6IGN/Xfw+f114v5Fxctf0=";
    };
  in {
    packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
      pname = "stasis";
      version = "latest";
      src = stasisSrc;
      cargoHash = "sha256-M5L6kcx/FY+cusYhVSDoKCyuH0LpaPXzBo3wJZsLQak=";
      nativeBuildInputs = [
        pkgs.pkg-config
        pkgs.openssl
        pkgs.systemd
        pkgs.dbus.dev
        pkgs.libinput
        pkgs.rust-bin.stable.latest.default
      ];
      buildInputs = [
        pkgs.openssl
        pkgs.systemd
        pkgs.dbus.dev
        pkgs.libinput
      ];
    };
  };
}
