{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix/monthly";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane/2510f2c";  # TODO: [https://github.com/crossterm-rs/crossterm/pull/1099]
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
        rust-toolchain-config = (builtins.fromTOML (builtins.readFile ./rust-toolchain.toml)).toolchain;
        rust-toolchain =
          fenix.packages.${system}.${rust-toolchain-config.channel}.withComponents
            rust-toolchain-config.components;
        crane-lib = (crane.mkLib pkgs).overrideToolchain rust-toolchain;
      in
      {
        packages.default = crane-lib.buildPackage {
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              (crane-lib.fileset.commonCargoSources ./.)
              ./src/default-config.yaml
            ];
          };
        };
        devShells.default = crane-lib.devShell { };
        formatter = pkgs.nixfmt;
      }
    );
}
