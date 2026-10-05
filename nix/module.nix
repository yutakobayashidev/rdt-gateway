self: { config, lib, pkgs, ... }:
let
  cfg = config.services.rdt-gateway;
  address = if lib.hasInfix ":" cfg.listenAddress then "[${cfg.listenAddress}]" else cfg.listenAddress;
in {
  options.services.rdt-gateway = {
    enable = lib.mkEnableOption "the headless Reddit gateway";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.rdt-gateway;
      description = "Gateway daemon package.";
    };
    listenAddress = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      description = "Address to bind. No firewall ports are opened.";
    };
    port = lib.mkOption {
      type = lib.types.port;
      default = 8787;
      description = "Gateway HTTP port.";
    };
  };
  config = lib.mkIf cfg.enable {
    systemd.services.rdt-gateway = {
      description = "Headless Reddit gateway";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      environment.RDT_GATEWAY_LISTEN = "${address}:${toString cfg.port}";
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/rdt-gateway";
        DynamicUser = true;
        Restart = "on-failure";
        RestartSec = 5;
        NoNewPrivileges = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
      };
    };
  };
}
