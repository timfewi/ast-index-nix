{
  description = "Lightweight AST code index and query service for local agent harnesses";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/7a0f122f5090cf4c2ade2a13a0e229d4e19ba71f";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      mkPackage =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "ast-index";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./tests/integration.rs
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          # The Rust test suite is the documented fast gate (`scripts/check fast`).
          # It spawns Unix-socket services, so it stays out of the hermetic build.
          doCheck = false;
          strictDeps = true;
          meta = {
            description = "AST code index with a single-tool MCP surface and a Unix socket service";
            license = pkgs.lib.licenses.mit;
            platforms = systems;
            mainProgram = "ast-index";
          };
        };
    in
    {
      nixosModules.default = import ./nix/module.nix { inherit self; };

      packages = forAllSystems (
        system:
        let
          package = mkPackage nixpkgs.legacyPackages.${system};
        in
        {
          default = package;
          ast-index = package;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rust-analyzer
              rustfmt
              clippy
              sqlite
              nix
              deadnix
              nixfmt
              statix
              shellcheck
            ];
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);

      checks = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          package = self.packages.${system}.default;
          module-eval = import ./tests/nix/module-eval.nix {
            inherit pkgs nixpkgs self;
          };
        }
      );
    };
}
