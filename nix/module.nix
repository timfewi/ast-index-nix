# NixOS module for the ast-index service.
#
# The service indexes one root directory and serves the MCP surface on a Unix
# socket. Harnesses connect through `ast-index mcp --socket <path>`, which is a
# byte-level proxy to this service, so every harness uses the same warm index.
{
  self,
}:
{
  config,
  lib,
  pkgs,
  utils,
  ...
}:
let
  cfg = config.services.astIndex;
  inherit (cfg) package;
  socketDirectory = builtins.dirOf cfg.socket;
  stateDirectory = lib.removePrefix "/var/lib/" (toString cfg.stateDir);
  semanticState = "${toString cfg.stateDir}/semantic";
  indexArgs = lib.optionals cfg.indexOnStart (
    [ "--index" ]
    ++ lib.concatMap (pattern: [
      "--exclude"
      pattern
    ]) cfg.exclude
  );
in
{
  options.services.astIndex = {
    enable = lib.mkEnableOption "the ast-index code index service";

    package = lib.mkOption {
      type = lib.types.package;
      default =
        if self ? packages && self.packages ? ${pkgs.stdenv.hostPlatform.system} then
          self.packages.${pkgs.stdenv.hostPlatform.system}.default
        else
          throw "services.astIndex.package must be set when the flake package is unavailable";
      defaultText = lib.literalExpression "self.packages.\${system}.default";
      description = "The ast-index package to run.";
    };

    root = lib.mkOption {
      type = lib.types.path;
      example = "/srv/ast-index/project";
      description = "Directory to index. Derived state defaults to <root>/.ast-index.";
    };

    stateDir = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = "/var/lib/ast-index/instance-a";
      description = ''
        Dedicated derived-state directory, or null for <root>/.ast-index.
        Must be a normalized path below /var/lib using letters, digits, dots,
        underscores, hyphens and slashes. Systemd creates and owns this directory
        as the service user/group with mode 0700. The source root stays read-only.
      '';
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "coding-agent";
      description = ''
        User the service runs as. The user must exist and be able to read {option}`root`.
        Without {option}`stateDir`, it must also be able to create `{root}/.ast-index`.
      '';
    };

    group = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "users";
      description = ''
        Group of the service process and of the socket directory. Null lets
        systemd use the user's primary group from the account database, which is
        what NixOS normal users need: they are members of `users`, and a group
        named after the user does not necessarily exist.
      '';
    };

    socket = lib.mkOption {
      type = lib.types.path;
      default = "/run/ast-index/socket";
      description = "Unix socket the service listens on.";
    };

    indexOnStart = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Run one indexing pass before serving requests.";
    };

    exclude = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "secrets/" ];
      description = ''
        gitignore-style path patterns excluded from the index, relative to
        {option}`root`. A pattern without a leading slash matches at any depth.
        Used for startup indexing when {option}`indexOnStart` is enabled, and
        always for enabled semantic snapshots, refreshes and query admission.
      '';
    };

    semantic = {
      enable = lib.mkEnableOption "explicit private project/profile semantic refresh";
      projectsFile = lib.mkOption {
        type = lib.types.nullOr (lib.types.strMatching "/.+");
        default = null;
        example = "/var/lib/ast-index/projects.json";
        description = ''
          Owner-maintained private runtime JSON file (mode 0600), never store
          content: {"version":1,"projects":[{"project":"relative/project",
          "profile":"rust","dependencies":false}]}. At most eight selections.
          No projects are discovered or enrolled automatically. Missing file
          skips the runner; a missing or stale profile never runs a compiler
          during a query. Dependencies must already be staged locally.
        '';
      };
      package = lib.mkOption {
        type = lib.types.package;
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.semantic-runner;
        defaultText = lib.literalExpression "self.packages.\${system}.semantic-runner";
        description = "Pinned offline Rust/TypeScript producer closure.";
      };
      intervalSeconds = lib.mkOption {
        type = lib.types.ints.positive;
        default = 300;
        description = "Delay after a completed refresh, at least 60 seconds. Fresh inputs skip the compiler.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion =
          !cfg.semantic.enable
          || (
            cfg.stateDir != null
            && cfg.semantic.projectsFile != null
            && !(lib.hasPrefix "/nix/store/" (toString cfg.semantic.projectsFile))
            && cfg.semantic.intervalSeconds >= 60
          );
        message = "services.astIndex.semantic needs a dedicated stateDir, non-store runtime projectsFile and intervalSeconds >= 60.";
      }
      {
        assertion =
          cfg.stateDir == null
          || (
            builtins.match "/var/lib/[a-zA-Z0-9_./-]+" (toString cfg.stateDir) != null
            && lib.all (part: part != "" && part != "." && part != "..") (lib.splitString "/" stateDirectory)
          );
        message = "services.astIndex.stateDir must be a normalized directory below /var/lib using letters, digits, dots, underscores, hyphens and slashes.";
      }
    ];

    environment.systemPackages = [ package ] ++ lib.optional cfg.semantic.enable cfg.semantic.package;

    systemd = {
      services = {
        ast-index = {
          description = "ast-index AST code index service";
          wantedBy = [ "multi-user.target" ];
          after = [ "network.target" ];
          serviceConfig = {
            Type = "simple";
            User = cfg.user;
            ExecStart = utils.escapeSystemdExecArgs (
              [
                "${package}/bin/ast-index"
                "--root"
                cfg.root
              ]
              ++ lib.optionals (cfg.stateDir != null) [
                "--state-dir"
                cfg.stateDir
              ]
              ++ lib.optionals cfg.semantic.enable (
                [
                  "--semantic-state-dir"
                  semanticState
                ]
                ++ lib.concatMap (pattern: [
                  "--semantic-exclude"
                  pattern
                ]) cfg.exclude
              )
              ++ [
                "serve"
                "--socket"
                cfg.socket
              ]
              ++ indexArgs
            );
            RuntimeDirectory = "ast-index";
            RuntimeDirectoryMode = "0750";
            UMask = "0007";
            Restart = "on-failure";
            RestartSec = 2;
            # Hardening that does not break file access to the indexed root.
            NoNewPrivileges = true;
            PrivateTmp = true;
            ProtectSystem = "strict";
            ProtectHome = "read-only";
            ReadWritePaths = [
              (if cfg.stateDir == null then cfg.root else cfg.stateDir)
              "${socketDirectory}"
            ];
            RestrictAddressFamilies = [ "AF_UNIX" ];
          }
          // lib.optionalAttrs (cfg.group != null) { Group = cfg.group; }
          // lib.optionalAttrs (cfg.stateDir != null) {
            StateDirectory = stateDirectory;
            StateDirectoryMode = "0700";
          };
        };

        ast-index-semantic-refresh = lib.mkIf cfg.semantic.enable {
          description = "Refresh explicitly selected private ast-index semantic profiles";
          unitConfig = {
            ConditionPathExists = cfg.semantic.projectsFile;
            ConditionPathIsDirectory = cfg.root;
          };
          serviceConfig = {
            Type = "oneshot";
            User = cfg.user;
            UMask = "0077";
            TimeoutStartSec = 600;
            ExecStart = utils.escapeSystemdExecArgs (
              [
                "${cfg.semantic.package}/bin/ast-index-semantic"
                "refresh"
                "--root"
                cfg.root
                "--state-dir"
                semanticState
                "--projects-file"
                cfg.semantic.projectsFile
              ]
              ++ lib.concatMap (pattern: [
                "--exclude"
                pattern
              ]) cfg.exclude
            );
            StateDirectory = "${stateDirectory}/semantic";
            StateDirectoryMode = "0700";
            NoNewPrivileges = true;
            PrivateTmp = true;
            PrivateNetwork = true;
            ProtectSystem = "strict";
            ProtectHome = "read-only";
            ReadWritePaths = [ semanticState ];
            RestrictAddressFamilies = [ "AF_UNIX" ];
          }
          // lib.optionalAttrs (cfg.group != null) { Group = cfg.group; };
        };
      };
      timers.ast-index-semantic-refresh = lib.mkIf cfg.semantic.enable {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnBootSec = "3min";
          OnUnitInactiveSec = "${toString cfg.semantic.intervalSeconds}s";
          AccuracySec = "10s";
        };
      };
    };
  };
}
