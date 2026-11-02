self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.acceptrequest.server;
  port = lib.toInt (lib.last (lib.splitString ":" cfg.listen));
  # Secrets reach the dynamic service user as systemd credentials, which the server finds by name.
  credentials =
    lib.optional (cfg.tls != null) "tls-cert:${cfg.tls.certFile}"
    ++ lib.optional (cfg.tls != null) "tls-key:${cfg.tls.keyFile}"
    ++ lib.optional (cfg.adminPasswordHashFile != null) "admin-hash:${cfg.adminPasswordHashFile}"
    ++ lib.optional (cfg.ntfy.tokenFile != null) "ntfy-token:${cfg.ntfy.tokenFile}";
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
    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
    };
    publicUrl = lib.mkOption {
      type = lib.types.str;
      example = "https://screentime.example.net:8443";
      description = "Address your phone uses to reach the server; links in notifications point here.";
    };
    tls = lib.mkOption {
      default = null;
      description = "Certificate and key in PEM. Leave unset only behind a TLS-terminating reverse proxy.";
      type = lib.types.nullOr (lib.types.submodule {
        options = {
          certFile = lib.mkOption { type = lib.types.path; };
          keyFile = lib.mkOption { type = lib.types.path; };
        };
      });
    };
    adminPasswordHashFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "File with the admin password hash from `acceptrequest-server hash-password`.";
    };
    ntfy = {
      url = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://ntfy.sh/some-long-random-topic";
        description = "Full ntfy topic URL. Use a long random topic name on ntfy.sh, or your own server.";
      };
      tokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "ntfy access token, if the topic needs one.";
      };
    };
    defaultAllowanceMinutes = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 120;
      description = "Daily minutes for an account the server hasn't seen before.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [ port ];

    systemd.services.acceptrequest-server = {
      description = "acceptrequest screen time server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment = {
        ACCEPTREQUEST_LISTEN = cfg.listen;
        ACCEPTREQUEST_DB = "sqlite:///var/lib/acceptrequest-server/acceptrequest.db";
        ACCEPTREQUEST_PUBLIC_URL = cfg.publicUrl;
        ACCEPTREQUEST_DEFAULT_ALLOWANCE = toString cfg.defaultAllowanceMinutes;
      } // lib.optionalAttrs (cfg.ntfy.url != null) {
        ACCEPTREQUEST_NTFY_URL = cfg.ntfy.url;
      };
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/acceptrequest-server serve";
        DynamicUser = true;
        StateDirectory = "acceptrequest-server";
        StateDirectoryMode = "0700";
        LoadCredential = credentials;
        Restart = "on-failure";
        AmbientCapabilities = lib.mkIf (port < 1024) [ "CAP_NET_BIND_SERVICE" ];
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        NoNewPrivileges = true;
      };
    };
  };
}
