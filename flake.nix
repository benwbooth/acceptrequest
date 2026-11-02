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
      packages = forAllSystems (pkgs: rec {
        # The Leptos UI, compiled to WebAssembly.
        web = pkgs.rustPlatform.buildRustPackage {
          pname = "acceptrequest-web";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.wasm-bindgen-cli pkgs.lld ];
          buildPhase = ''
            runHook preBuild
            cargo build --frozen --offline --profile wasm-release --target wasm32-unknown-unknown -p acceptrequest-web
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            mkdir -p $out
            wasm-bindgen --target web --no-typescript --out-dir $out \
              target/wasm32-unknown-unknown/wasm-release/acceptrequest_web.wasm
            cp crates/web/static/* $out/
            runHook postInstall
          '';
          doCheck = false;
        };

        default = pkgs.rustPlatform.buildRustPackage {
          pname = "acceptrequest";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          buildInputs = [ pkgs.pam ];
          doCheck = false;
          cargoBuildFlags = [ "--workspace" "--exclude" "acceptrequest-web" ];
          postInstall = ''
            mkdir -p $out/lib/security $out/share/acceptrequest
            mv $out/lib/libpam_acceptrequest.so $out/lib/security/pam_acceptrequest.so
            rm -f $out/lib/libacceptrequest_web.so
            ln -s ${web} $out/share/acceptrequest/web
          '';
        };
      });

      nixosModules = {
        server = import ./nix/server.nix self;
        client = import ./nix/client.nix self;
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ rustc cargo clippy rustfmt rust-analyzer sqlx-cli wasm-bindgen-cli pam ];
        };
      });
    };
}
