self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.screentime.client;
  usersArg = lib.concatStringsSep "," cfg.managedUsers;
  inWheel = u:
    builtins.elem "wheel" ((config.users.users.${u} or { }).extraGroups or [ ])
    || builtins.elem u (config.users.groups.wheel.members or [ ]);
in
{
  options.services.screentime.client = {
    enable = lib.mkEnableOption "screentime client daemon, PAM module and greeter";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
    };
    serverUrl = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Base URL of the screentime server. Without it the client runs on its cached policy.";
    };
    tokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "File holding this machine's API token for the server.";
    };
    serverFingerprint = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "1F:E7:E4:02:...:3E:6A";
      description = ''
        SHA-256 fingerprint of the server's certificate (printed by `screentime-server gen-cert`,
        `screentime-server fingerprint`, and at server start). When set, only that certificate is
        trusted, so a self-signed server works with no CA to install.
      '';
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
    greeter.enable = lib.mkEnableOption "the screentime greetd greeter";
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !(builtins.elem "root" cfg.managedUsers);
        message = "screentime: root cannot be a managed user.";
      }
      {
        assertion = !(config.boot.loader.systemd-boot.enable && config.boot.loader.systemd-boot.editor);
        message = ''
          screentime: set boot.loader.systemd-boot.editor = false. With the boot menu editor on,
          anyone at the keyboard can boot with init=/bin/sh and skip every limit.
        '';
      }
    ] ++ map (u: {
      assertion = !(inWheel u);
      message = "screentime: managed user ${u} is in wheel and could disable the limits with sudo.";
    }) cfg.managedUsers;

    warnings = lib.optional (config.boot.loader.grub.enable && config.boot.loader.grub.users == { }) ''
      screentime: GRUB has no password, so its command line can be edited at boot to skip the limits.
    '';

    systemd.services.screentime-client = {
      description = "screentime screen time enforcement";
      wantedBy = [ "multi-user.target" ];
      before = [ "display-manager.service" "systemd-user-sessions.service" ];
      after = [ "dbus.service" "systemd-logind.service" ];
      environment = {
        SCREENTIME_USERS = usersArg;
        SCREENTIME_GRACE_SECS = toString cfg.graceSeconds;
        SCREENTIME_ADMIN_GROUP = cfg.adminGroup;
      } // lib.optionalAttrs (cfg.serverUrl != null) {
        SCREENTIME_SERVER = cfg.serverUrl;
      } // lib.optionalAttrs (cfg.tokenFile != null) {
        SCREENTIME_TOKEN_FILE = toString cfg.tokenFile;
      } // lib.optionalAttrs (cfg.caFile != null) {
        SCREENTIME_CA_FILE = toString cfg.caFile;
      } // lib.optionalAttrs (cfg.serverFingerprint != null) {
        SCREENTIME_SERVER_FINGERPRINT = cfg.serverFingerprint;
      };
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/screentime-client";
        RuntimeDirectory = "screentime";
        RuntimeDirectoryPreserve = true;
        StateDirectory = "screentime";
        StateDirectoryMode = "0700";
        # One-time move of usage and settings from before the project was renamed (acceptrequest).
        ExecStartPre = "${pkgs.writeShellScript "screentime-migrate-client-state" ''
          if [ -f /var/lib/acceptrequest/state.json ] && [ ! -e /var/lib/screentime/state.json ]; then
            cp -a /var/lib/acceptrequest/state.json /var/lib/screentime/state.json
          fi
        ''}";
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
      rules.account.screentime = {
        order = 10000;
        control = "required";
        modulePath = "${cfg.package}/lib/security/pam_screentime.so";
        args = [ "users=${usersArg}" ];
      };
    }) // {
      # Password check for "My parent is here". The daemon answers the prompts itself, so no fingerprint.
      screentime = {
        fprintAuth = false;
        unixAuth = true;
      };
    };

    services.greetd = lib.mkIf cfg.greeter.enable {
      enable = true;
      settings.default_session.command = "${cfg.package}/bin/screentime-greeter";
    };
  };
}
