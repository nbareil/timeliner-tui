{
  description = "Timeliner TUI: an indexed forensic timeline workbench";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.flake-utils.url = "github:numtide/flake-utils";

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        source = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml ./Cargo.lock ./src ./tests ./LICENSE
          ];
        };
        app = pkgs.rustPlatform.buildRustPackage {
          pname = "timeliner-tui";
          version = "0.1.0";
          src = source;
          cargoLock.lockFile = ./Cargo.lock;
          doCheck = true;
          meta = {
            description = "Indexed terminal workbench for forensic bodyfile timelines";
            mainProgram = "timeliner-tui";
            license = pkgs.lib.licenses.mit;
            platforms = [ "x86_64-linux" "aarch64-linux" ];
          };
        };
      in {
        packages.default = app;
        packages.timeliner-tui = app;
        apps.default = (flake-utils.lib.mkApp { drv = app; }) // {
          meta.description = "Indexed terminal workbench for forensic bodyfiles";
        };
        checks.default = app;
        checks.style = app.overrideAttrs (old: {
          pname = "timeliner-tui-style";
          nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.rustfmt pkgs.clippy ];
          buildPhase = ''
            cargo fmt --all -- --check
            cargo clippy --locked --offline --all-targets -- -D warnings
          '';
          doCheck = false;
          installPhase = "mkdir -p $out";
        });
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer ];
        };
        formatter = pkgs.nixfmt;
      });
}
