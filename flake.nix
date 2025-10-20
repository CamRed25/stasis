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
    pkgs = nixpkgs.legacyPackages.${system};

    stasisSrc = builtins.fetchGit {
      url = "https://github.com/saltnpepper97/stasis.git";
      rev = "a1b2c3d4e5f67890123456789abcdef012345678";
    };

    stasisDerivation = pkgs.stdenv.mkDerivation {
      pname = "stasis";
      version = "latest";

      src = stasisSrc;

      buildPhase = ''
        cargo build --release --locked
      '';

      installPhase = ''
        mkdir -p $out/bin
        cp target/release/stasis $out/bin/
      '';

      # Add dependencies if needed
    };
  in {
    packages.x86_64-linux = {
      default = stasisDerivation;
      stasis = stasisDerivation;
    };
  };
}
