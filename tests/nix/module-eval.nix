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
  escaped = mkConfig {
    root = "/var/lib/ast index%root";
    socket = "/run/ast-index/my socket%";
    exclude = [
      "private notes/*"
      "literal%$"
    ];
  };
  explicitService = explicit.systemd.services.ast-index;
  implicitService = implicit.systemd.services.ast-index;
  escapedExec = escaped.systemd.services.ast-index.serviceConfig.ExecStart;
in
if
  explicit.services.astIndex.enable
  && explicit.services.astIndex.package.system == system
  && explicit.services.astIndex.package == self.packages.${system}.default
  && builtins.elem explicit.services.astIndex.package explicit.environment.systemPackages
  && explicitService.serviceConfig.User == "root"
  && explicitService.serviceConfig.Group == "root"
  && !(implicitService.serviceConfig ? Group)
  && nixpkgs.lib.hasInfix ''"--root" "/var/lib/ast index%%root"'' escapedExec
  && nixpkgs.lib.hasInfix ''"--socket" "/run/ast-index/my socket%%"'' escapedExec
  && nixpkgs.lib.hasInfix ''"--exclude" "private notes/*"'' escapedExec
  && nixpkgs.lib.hasInfix ''"--exclude" "literal%%$$"'' escapedExec
then
  pkgs.runCommand "ast-index-module-eval" { } ''
    touch "$out"
  ''
else
  throw "services.astIndex did not evaluate as expected"
