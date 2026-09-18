{
  description = "Lightweight AST code index and query service for local agent harnesses";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/eaad089433ca2bb662274377d33df3d0e51ef28b";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};

      package = pkgs.rustPlatform.buildRustPackage {
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
          platforms = [ system ];
          mainProgram = "ast-index";
        };
      };
    in
    {
      nixosModules.default = import ./nix/module.nix { inherit self; };

      packages.${system} = {
        default = package;
        ast-index = package;
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          cargo
          rustc
          rust-analyzer
          rustfmt
          clippy
          sqlite
          nixfmt
          shellcheck
        ];
      };

      formatter.${system} = pkgs.nixfmt;

      checks.${system} = {
        inherit package;
        module-eval = import ./tests/nix/module-eval.nix {
          inherit pkgs nixpkgs self;
        };
      };
    };
}
