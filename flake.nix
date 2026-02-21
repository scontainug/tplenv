{
  description = "Deterministic Nix build for tplenv";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f:
        builtins.listToAttrs (map (system: {
          name = system;
          value = f system;
        }) systems);
    in {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "tplenv";
            version = "0.3.0";
            src = ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
            };
            doCheck = true;
          };

          tplenv = self.packages.${system}.default;
        });

      devShells = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
            ];
          };
        });
    };
}
