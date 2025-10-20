{
  description = "Wrapper for Stasis non-flake repo";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    stasis.url = "github:CamRed25/stasis";
  };

  outputs = {
    self,
    nixpkgs,
  }: let
    system = "x86_64-linux";
    pkgs = nixpkgs.legacyPackages.${system};

    stasisSrc = builtins.fetchGit {
      url = "https://github.com/CamRed25/stasis.git";
      ref = "refs/heads/main";
      allRefs = true;
    };

    stasisDerivation = pkgs.rustPlatform.buildRustPackage rec {
      pname = "stasis";
      version = "latest";
      src = stasisSrc;
      # Remove this line if the Cargo.lock is inside the fetched source
      cargoLock = {
        lockFile = ./Cargo.lock;
        # outputHashes = { "dependency-name" = "<hash>"; };  # fill in if needed
      };

      nativeBuildInputs = [pkgs.openssl]; # Add any dependencies here
    };
  in {
    packages.x86_64-linux = {
      default = stasisDerivation;
      stasis = stasisDerivation;
    };
  };
}
