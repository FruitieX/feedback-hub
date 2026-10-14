{
  description = "feedback-hub: in-app feedback with annotated screenshots, stored centrally for coding agents";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }: let
    systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
    eachSystem = nixpkgs.lib.genAttrs systems;
    forSystem = system: let
      pkgs = import nixpkgs { inherit system; };
      web = pkgs.buildNpmPackage {
        pname = "feedback-hub-web"; version = "0.1.0";
        src = pkgs.lib.cleanSourceWith { src = ./web; filter = path: type: !(builtins.elem (baseNameOf path) [ "node_modules" "dist" ]); };
        nodejs = pkgs.nodejs_22;
        npmDepsHash = "sha256-Xuw7z+/wKGctZ2LZ2s/x7HBBB3TL5B+1LmCClpBN7P0=";
        env.PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD = "1";
        installPhase = ''mkdir -p $out/share/feedback-hub; cp -r dist/. $out/share/feedback-hub/'';
      };
      server = pkgs.rustPlatform.buildRustPackage {
        pname = "feedback-hub"; version = "0.1.0";
        src = pkgs.lib.cleanSourceWith { src = ./.; filter = path: type: !(builtins.elem (baseNameOf path) [ "target" "web" "deploy" "data" ".github" ]); };
        cargoLock.lockFile = ./Cargo.lock;
        meta.mainProgram = "feedback-hub";
      };
    in { inherit pkgs web server; };
  in {
    packages = eachSystem (system: let build = forSystem system; inherit (build) pkgs web server; in {
      inherit web server;
      default = server;
    } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
      container = pkgs.dockerTools.buildLayeredImage {
        name = "ghcr.io/fruitiex/feedback-hub"; tag = "main";
        contents = [ server web pkgs.cacert ];
        extraCommands = ''mkdir -p data tmp; chmod 1777 tmp'';
        fakeRootCommands = ''chown 10001:10001 data'';
        config = {
          User = "10001:10001"; WorkingDir = "/";
          Entrypoint = [ "${server}/bin/feedback-hub" "serve" ];
          Env = [ "FEEDBACK_HUB_BIND=0.0.0.0:8080" "FEEDBACK_HUB_DATA_DIR=/data" "FEEDBACK_HUB_STATIC_DIR=${web}/share/feedback-hub" ];
          ExposedPorts."8080/tcp" = {}; Volumes."/data" = {};
        };
      };
    });
    devShells = eachSystem (system: let pkgs = (forSystem system).pkgs; in {
      default = pkgs.mkShell {
        packages = with pkgs; [ nodejs_22 cargo rustc rustfmt clippy gcc git ]
          ++ lib.optionals stdenv.hostPlatform.isLinux [ chromium ];
        shellHook = nixpkgs.lib.optionalString (nixpkgs.lib.hasSuffix "linux" system) ''
          export PLAYWRIGHT_CHROMIUM_EXECUTABLE=${pkgs.chromium}/bin/chromium
        '';
      };
    });
  };
}
