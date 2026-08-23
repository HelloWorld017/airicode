{
  description = "workspace for airicode";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils = {
      url = "github:numtide/flake-utils";
      inputs.systems.follows = "systems";
    };
    systems.url = "github:nix-systems/default-linux";
  };

  outputs = { nixpkgs, crane, fenix, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs { inherit system; };
      rustToolchain = fenix.packages.${system}.combine [
        fenix.packages.${system}.stable.toolchain
        fenix.packages.${system}.stable.rust-src
      ];

      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
      packageFor = cargoPackage:
        craneLib.buildPackage {
          pname = cargoPackage;
          version = "0.0.0";
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          cargoExtraArgs = "--package ${cargoPackage}";
          nativeBuildInputs = [ pkgs.git ];
          meta.mainProgram = cargoPackage;
        };
    in {
      packages = {
        default = packageFor "airicode";
      };

      devShells.default = craneLib.devShell {
        packages = with pkgs; [ git lld ];
      };
    });
}
