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
      ref = "refs/heads/main";
      allRefs = true;
    };

    stasisDerivation = pkgs.rustPlatform.buildRustPackage rec {
      pname = "stasis";
      version = "latest";
      src = stasisSrc;
      # Remove this line if the Cargo.lock is inside the fetched source
      cargoLock = ./Cargo.lock;

      nativeBuildInputs = [pkgs.openssl]; # Add any dependencies here
    };
  in {
    packages.x86_64-linux = {
      default = stasisDerivation;
      stasis = stasisDerivation;
    };
  };
}
