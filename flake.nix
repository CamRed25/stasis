{
  description = "Wrapper for Stasis non-flake repo";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
  }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs {
      inherit system;
      overlays = [rust-overlay.overlays.default];
    };

    stasisSrc = pkgs.fetchFromGitHub {
      owner = "saltnpepper97";
      repo = "stasis";
      rev = "58876355050247a7bbab4c1f0bf50a15ccb81c3b";
      sha256 = "sha256-MOb56PJS5gBITScJMvou/Z6IGN/Xfw+f114v5Fxctf0=";
    };

    myPkg = pkgs.rustPlatform.buildRustPackage {
      pname = "stasis";
      version = "0.4.12";
      src = stasisSrc;
      cargoHash = "sha256-M5L6kcx/FY+cusYhVSDoKCyuH0LpaPXzBo3wJZsLQak=";
      nativeBuildInputs = with pkgs; [pkg-config rust-bin.stable."1.89.0".default];
      buildInputs = with pkgs; [openssl systemd dbus.dev libinput];
    };
  in {
    packages.${system} = {
      stasis = myPkg;
      default = myPkg;
    };

    devShells.${system}.default = pkgs.mkShell {
      buildInputs = with pkgs; [
        rust-bin.stable."1.89.0".default
        pkg-config
        openssl
        systemd
        dbus.dev
        libinput
      ];
    };
  };
}
