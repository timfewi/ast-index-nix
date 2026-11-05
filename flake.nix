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
      # The pinned SCIP CLI hardcodes both of these independently of the LSP
      # settings. Fail at build time if upstream changes the reviewed contract.
      mkRustProducer =
        pkgs:
        pkgs.rust-analyzer.override {
          pname = "ast-index-rust-analyzer";
          rust-analyzer-unwrapped = pkgs.rust-analyzer-unwrapped.overrideAttrs (old: {
            postPatch = (old.postPatch or "") + ''
              substituteInPlace crates/rust-analyzer/src/cli/scip.rs \
                --replace-fail 'load_out_dirs_from_check: true,' 'load_out_dirs_from_check: false,' \
                --replace-fail 'with_proc_macro_server: ProcMacroServerChoice::Sysroot,' 'with_proc_macro_server: ProcMacroServerChoice::None,'
            '';
          });
        };
      mkSemanticEnvironment =
        pkgs:
        let
          analyzer = mkRustProducer pkgs;
          closure = pkgs.closureInfo {
            rootPaths = [
              analyzer
              pkgs.cargo
              pkgs.rustc
            ];
          };
        in
        {
          AST_INDEX_SEMANTIC_BWRAP = "${pkgs.bubblewrap}/bin/bwrap";
          AST_INDEX_SEMANTIC_TIMEOUT = "${pkgs.coreutils}/bin/timeout";
          AST_INDEX_SEMANTIC_RUST_ANALYZER = "${analyzer}/bin/rust-analyzer";
          AST_INDEX_SEMANTIC_CARGO = "${pkgs.cargo}/bin/cargo";
          AST_INDEX_SEMANTIC_RUSTC = "${pkgs.rustc}/bin/rustc";
          AST_INDEX_SEMANTIC_CLOSURE = "${closure}/store-paths";
        };
      semanticExports =
        pkgs:
        pkgs.lib.concatStringsSep "\n" (
          pkgs.lib.mapAttrsToList (name: value: "export ${name}=${pkgs.lib.escapeShellArg value}") (
            mkSemanticEnvironment pkgs
          )
        );
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
              ./tests/quality.rs
              ./tests/producer.rs
              ./tests/quality
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
          pkgs = nixpkgs.legacyPackages.${system};
          package = mkPackage pkgs;
        in
        {
          default = package;
          ast-index = package;
          rust-semantic-producer = mkRustProducer pkgs;
          semantic-runner = pkgs.writeShellApplication {
            name = "ast-index-semantic";
            text = ''
              ${semanticExports pkgs}
              exec ${package}/bin/ast-index-semantic "$@"
            '';
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            shellHook = semanticExports pkgs;
            packages = with pkgs; [
              cargo
              rustc
              (mkRustProducer pkgs)
              rustfmt
              clippy
              sqlite
              nix
              deadnix
              nixfmt
              statix
              shellcheck
              typescript
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
