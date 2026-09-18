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
  ...
}:
let
  cfg = config.services.astIndex;
  package = cfg.package;
  socketDirectory = builtins.dirOf cfg.socket;
  exclusionFlags = lib.concatMapStrings (
    pattern: " --exclude ${lib.escapeShellArg pattern}"
  ) cfg.exclude;
  indexFlag = lib.optionalString cfg.indexOnStart (" --index${exclusionFlags}");
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
      example = "/home/agent/project";
      description = "Directory to index. The index is stored in <root>/.ast-index.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "coding-agent";
      description = ''
        User the service runs as. The user must exist and must own {option}`root`
        (or at least be able to create `{root}/.ast-index`).
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
        Only used when {option}`indexOnStart` is enabled.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ package ];

    systemd.services.ast-index = {
      description = "ast-index AST code index service";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        ExecStart = "${package}/bin/ast-index --root ${cfg.root} serve --socket ${cfg.socket}${indexFlag}";
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
          cfg.root
          "${socketDirectory}"
        ];
        RestrictAddressFamilies = [ "AF_UNIX" ];
      }
      // lib.optionalAttrs (cfg.group != null) { Group = cfg.group; };
    };
  };
}
