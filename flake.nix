{
  description = "acceptrequest: screen time limits, downtime schedules and time requests for Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "acceptrequest";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          doCheck = false;
          postInstall = ''
            mkdir -p $out/lib/security
            mv $out/lib/libpam_acceptrequest.so $out/lib/security/pam_acceptrequest.so
          '';
        };
      });

      nixosModules = {
        server = import ./nix/server.nix self;
        client = import ./nix/client.nix self;
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ rustc cargo clippy rustfmt rust-analyzer sqlx-cli cargo-leptos pam ];
        };
      });
    };
}
