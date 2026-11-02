self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.acceptrequest.client;
in
{
  options.services.acceptrequest.client = {
    enable = lib.mkEnableOption "acceptrequest client daemon, PAM module and greeter";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
    };
    serverUrl = lib.mkOption { type = lib.types.str; };
    managedUsers = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
    };
    pamServices = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "login" "greetd" "sshd" "su" "sudo" "systemd-user" ];
    };
    greeter.enable = lib.mkEnableOption "the acceptrequest greetd greeter";
  };

  config = lib.mkIf cfg.enable {
    systemd.services.acceptrequest-client = {
      wantedBy = [ "multi-user.target" ];
      environment.ACCEPTREQUEST_SERVER = cfg.serverUrl;
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/acceptrequest-client";
        RuntimeDirectory = "acceptrequest";
        StateDirectory = "acceptrequest";
        Restart = "always";
      };
    };

    security.pam.services = lib.genAttrs cfg.pamServices (_: {
      rules.account.acceptrequest = {
        order = 10000;
        control = "required";
        modulePath = "${cfg.package}/lib/security/pam_acceptrequest.so";
      };
    });

    services.greetd = lib.mkIf cfg.greeter.enable {
      enable = true;
      settings.default_session.command = "${cfg.package}/bin/acceptrequest-greeter";
    };
  };
}
