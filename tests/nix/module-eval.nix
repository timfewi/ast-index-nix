# Evaluate the NixOS module without building a system closure.
{
  pkgs,
  nixpkgs,
  self,
}:
let
  system = pkgs.stdenv.hostPlatform.system;
  evaluated = nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [
      self.nixosModules.default
      {
        services.astIndex = {
          enable = true;
          user = "root";
          group = "root";
          root = "/var/lib/ast-index-fixture";
        };
      }
    ];
  };
  service = evaluated.config.systemd.services.ast-index;
in
if evaluated.config.services.astIndex.enable && service.serviceConfig.User == "root" then
  pkgs.runCommand "ast-index-module-eval" { } ''
    touch "$out"
  ''
else
  throw "services.astIndex did not evaluate as expected"
