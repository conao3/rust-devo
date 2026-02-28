{
  description = "devo development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs =
    inputs@{ flake-parts
    , treefmt-nix
    , ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" "aarch64-darwin" ];

      perSystem =
        { system, ... }:
        let
          overlay = _: prev: {
            devoRust = prev.rust-bin.stable."1.84.1".minimal;
          };
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [
              inputs.rust-overlay.overlays.default
              overlay
            ];
          };
          treefmtEval = treefmt-nix.lib.evalModule pkgs {
            projectRootFile = "flake.nix";
            programs.nixpkgs-fmt.enable = true;
            programs.rustfmt.enable = true;
          };
        in
        {
          formatter = treefmtEval.config.build.wrapper;

          packages.default = pkgs.rustPlatform.buildRustPackage {
            pname = "devo";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
          };

          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              devoRust
            ];
          };
        };
    };
}
