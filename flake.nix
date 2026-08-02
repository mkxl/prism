{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix/monthly";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      crane,
      fenix,
      flake-utils,
      nixpkgs,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        rust-toolchain = fenix.packages.${system}.stable.withComponents [
          "cargo"
          "clippy"
          "rust-docs"
          "rust-src"
          "rust-std"
          "rustc"
          "rustfmt"
        ];
        crane-lib = (crane.mkLib pkgs).overrideToolchain rust-toolchain;
      in
      {
        packages.default = crane-lib.buildPackage {
          src = crane-lib.cleanCargoSource ./.;
        };

        devShells.default = crane-lib.devShell { };

        formatter = pkgs.nixfmt;
      }
    );
}
