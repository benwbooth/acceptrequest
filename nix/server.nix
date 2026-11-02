self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.acceptrequest.server;
in
{
  options.services.acceptrequest.server = {
    enable = lib.mkEnableOption "acceptrequest policy server";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
    };
    listen = lib.mkOption {
      type = lib.types.str;
      default = "0.0.0.0:8443";
    };
    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Secrets (admin password hash, ntfy token, TLS paths).";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.acceptrequest-server = {
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment = {
        ACCEPTREQUEST_LISTEN = cfg.listen;
        ACCEPTREQUEST_DB = "sqlite:///var/lib/acceptrequest/acceptrequest.db";
      };
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/acceptrequest-server";
        DynamicUser = true;
        StateDirectory = "acceptrequest";
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
      };
    };
  };
}
