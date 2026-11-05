{
  description = "Terminal UI for Linear issues across workspaces, with a Rex session per issue";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      forAllSystems = nixpkgs.lib.genAttrs systems;

      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          lib = pkgs.lib;
          cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          source = lib.cleanSourceWith {
            src = ./.;
            filter =
              path: _type:
              !(builtins.elem (baseNameOf path) [
                ".git"
                ".jj"
                "result"
                "target"
              ]);
          };
        in
        rec {
          default = linrex;

          linrex = pkgs.rustPlatform.buildRustPackage {
            pname = cargoToml.package.name;
            version = cargoToml.package.version;

            src = source;
            cargoLock.lockFile = ./Cargo.lock;

            nativeCheckInputs = [
              pkgs.git
              pkgs.jujutsu
            ];

            meta = {
              description = cargoToml.package.description;
              homepage = cargoToml.package.repository;
              license = lib.licenses.mit;
              mainProgram = "linrex";
            };
          };
        }
      );

      checks = forAllSystems (system: {
        default = self.packages.${system}.default;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              git
              jujutsu
              nixfmt
              rust-analyzer
              rustc
              rustfmt
            ];
            RUST_BACKTRACE = "1";
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
