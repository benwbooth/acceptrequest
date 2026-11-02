self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.acceptrequest.client;
  usersArg = lib.concatStringsSep "," cfg.managedUsers;
  inWheel = u:
    builtins.elem "wheel" ((config.users.users.${u} or { }).extraGroups or [ ])
    || builtins.elem u (config.users.groups.wheel.members or [ ]);
in
{
  options.services.acceptrequest.client = {
    enable = lib.mkEnableOption "acceptrequest client daemon, PAM module and greeter";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
    };
    serverUrl = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Base URL of the acceptrequest server. Without it the client runs on its cached policy.";
    };
    tokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "File holding this machine's API token for the server.";
    };
    caFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Extra CA certificate to trust for the server, for a certificate from your own CA.";
    };
    managedUsers = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Accounts whose screen time is limited. Everyone else is never affected.";
    };
    graceSeconds = lib.mkOption {
      type = lib.types.ints.positive;
      default = 120;
      description = "How long a session stays locked after time runs out before it is ended.";
    };
    pamServices = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "login" "greetd" "sshd" "su" "sudo" "systemd-user" ];
      description = "PAM services that check screen time before letting a managed user in.";
    };
    adminGroup = lib.mkOption {
      type = lib.types.str;
      default = "wheel";
      description = ''Members of this group can approve time on the spot with "My parent is here".'';
    };
    greeter.enable = lib.mkEnableOption "the acceptrequest greetd greeter";
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !(builtins.elem "root" cfg.managedUsers);
        message = "acceptrequest: root cannot be a managed user.";
      }
      {
        assertion = !(config.boot.loader.systemd-boot.enable && config.boot.loader.systemd-boot.editor);
        message = ''
          acceptrequest: set boot.loader.systemd-boot.editor = false. With the boot menu editor on,
          anyone at the keyboard can boot with init=/bin/sh and skip every limit.
        '';
      }
    ] ++ map (u: {
      assertion = !(inWheel u);
      message = "acceptrequest: managed user ${u} is in wheel and could disable the limits with sudo.";
    }) cfg.managedUsers;

    warnings = lib.optional (config.boot.loader.grub.enable && config.boot.loader.grub.users == { }) ''
      acceptrequest: GRUB has no password, so its command line can be edited at boot to skip the limits.
    '';

    systemd.services.acceptrequest-client = {
      description = "acceptrequest screen time enforcement";
      wantedBy = [ "multi-user.target" ];
      before = [ "display-manager.service" "systemd-user-sessions.service" ];
      after = [ "dbus.service" "systemd-logind.service" ];
      environment = {
        ACCEPTREQUEST_USERS = usersArg;
        ACCEPTREQUEST_GRACE_SECS = toString cfg.graceSeconds;
        ACCEPTREQUEST_ADMIN_GROUP = cfg.adminGroup;
      } // lib.optionalAttrs (cfg.serverUrl != null) {
        ACCEPTREQUEST_SERVER = cfg.serverUrl;
      } // lib.optionalAttrs (cfg.tokenFile != null) {
        ACCEPTREQUEST_TOKEN_FILE = toString cfg.tokenFile;
      } // lib.optionalAttrs (cfg.caFile != null) {
        ACCEPTREQUEST_CA_FILE = toString cfg.caFile;
      };
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/acceptrequest-client";
        RuntimeDirectory = "acceptrequest";
        RuntimeDirectoryPreserve = true;
        StateDirectory = "acceptrequest";
        StateDirectoryMode = "0700";
        Restart = "always";
        RestartSec = 2;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;
      };
    };

    environment.systemPackages = [ cfg.package ];

    security.pam.services = lib.genAttrs cfg.pamServices (_: {
      rules.account.acceptrequest = {
        order = 10000;
        control = "required";
        modulePath = "${cfg.package}/lib/security/pam_acceptrequest.so";
        args = [ "users=${usersArg}" ];
      };
    }) // {
      # Password check for "My parent is here". The daemon answers the prompts itself, so no fingerprint.
      acceptrequest = {
        fprintAuth = false;
        unixAuth = true;
      };
    };

    services.greetd = lib.mkIf cfg.greeter.enable {
      enable = true;
      settings.default_session.command = "${cfg.package}/bin/acceptrequest-greeter";
    };
  };
}
