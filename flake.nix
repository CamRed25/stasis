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
      rev = "<commit-sha-or-branch>"; # specify if needed
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
      stasis = stasisDerivation;
    };
  };
}
