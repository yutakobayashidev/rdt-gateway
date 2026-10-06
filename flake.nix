{
  description = "rdt-gateway: headless Reddit transport with independent CLI and MCP clients";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/a7868a727837f3c09cee2ce0ca671c76b1589fed";
    crane.url = "github:ipetkov/crane/47b6b27ed9a3a9181415e4367d0c30ab2a0e0250";
  };

  outputs = { self, nixpkgs, crane }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      craneLib = crane.mkLib pkgs;
      src = craneLib.cleanCargoSource ./.;
      native = with pkgs; [ cmake clang pkg-config gnumake git ];
      common = {
        inherit src;
        pname = "rdt-gateway-workspace";
        strictDeps = true;
        nativeBuildInputs = native;
        LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
        CARGO_BUILD_JOBS = "2";
      };
      cargoArtifacts = craneLib.buildDepsOnly common;
      package = name:
        let
          args = {
            inherit src;
            pname = name;
            strictDeps = true;
            CARGO_BUILD_JOBS = "2";
            cargoExtraArgs = "--locked -p ${name}";
          } // pkgs.lib.optionalAttrs (builtins.elem name [ "rdt-gateway" "rdt-mcp" ]) {
            nativeBuildInputs = native;
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
          };
        in craneLib.buildPackage (args // {
          cargoArtifacts = craneLib.buildDepsOnly args;
        });
      gateway = package "rdt-gateway";
    in {
      packages.${system} = {
        default = gateway;
        rdt-gateway = gateway;
        rdt-cli = package "rdt-cli";
        rdt-mcp = package "rdt-mcp";
      };
      apps.${system} = builtins.mapAttrs (name: value: {
        type = "app";
        meta.description = "${name} executable";
        program = "${value}/bin/${if name == "default" then "rdt-gateway" else if name == "rdt-cli" then "rdt" else name}";
      }) self.packages.${system};
      devShells.${system}.default = craneLib.devShell {
        packages = native ++ [ pkgs.libclang pkgs.rustfmt pkgs.clippy pkgs.bash pkgs.curl pkgs.ripgrep pkgs.coreutils pkgs.python3 ];
        LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
        CARGO_BUILD_JOBS = "2";
      };
      checks.${system} = {
        tests = craneLib.cargoTest (common // { inherit cargoArtifacts; });
        nixos = pkgs.testers.runNixOSTest {
          name = "rdt-gateway";
          nodes.machine = { ... }: {
            imports = [ self.nixosModules.default ];
            services.rdt-gateway.enable = true;
            virtualisation.memorySize = 768;
          };
          testScript = ''
            machine.start()
            machine.wait_for_unit("rdt-gateway.service")
            machine.wait_for_open_port(8787)
            machine.succeed("curl -fsS http://127.0.0.1:8787/health/live | grep true")
            machine.succeed("test $(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8787/health/ready) = 503")
            machine.succeed("ss -ltn | grep '127.0.0.1:8787'")
            machine.succeed("test $(systemctl show rdt-gateway.service -p DynamicUser --value) = yes")
            machine.succeed("systemctl restart rdt-gateway.service")
            machine.wait_for_unit("rdt-gateway.service")
            machine.wait_for_open_port(8787)
            machine.succeed("curl -fsS http://127.0.0.1:8787/health/live | grep true")
          '';
        };
      };
      nixosModules.default = import ./nix/module.nix self;
    };
}
