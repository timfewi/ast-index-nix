# Evaluate the NixOS module without building a system closure.
#
# Covers the two group modes: an explicit group is passed through, and an unset
# group must not emit Group= (systemd then uses the user's primary group, which
# is what NixOS normal users need).
{
  pkgs,
  nixpkgs,
  self,
}:
let
  system = pkgs.stdenv.hostPlatform.system;
  mkConfig =
    extra:
    (nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        self.nixosModules.default
        {
          services.astIndex = {
            enable = true;
            user = "root";
            root = "/var/lib/ast-index-fixture";
          }
          // extra;
        }
      ];
    }).config;
  explicit = mkConfig { group = "root"; };
  implicit = mkConfig { };
  explicitService = explicit.systemd.services.ast-index;
  implicitService = implicit.systemd.services.ast-index;
in
if
  explicit.services.astIndex.enable
  && explicitService.serviceConfig.User == "root"
  && explicitService.serviceConfig.Group == "root"
  && !(implicitService.serviceConfig ? Group)
then
  pkgs.runCommand "ast-index-module-eval" { } ''
    touch "$out"
  ''
else
  throw "services.astIndex did not evaluate as expected"
