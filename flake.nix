{
  description = "Nix flake for bpb_enhance";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        guiRuntimeLibs = with pkgs; [
          libGL
          vulkan-loader
          wayland
          libxkbcommon
          libx11
          libxcursor
          libxi
          libxrandr
          libxcb
          libxext
          freetype
          fontconfig
          expat
        ];

        commonArgs = {
          pname = "bpb_enhance";
          version = "0.6.2";
          src = ./.;
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
          doCheck = false;
        };

        bpb_enhance-cli = pkgs.rustPlatform.buildRustPackage (
          commonArgs
          // {
            buildNoDefaultFeatures = true;
            buildFeatures = [ "cli" ];
            checkNoDefaultFeatures = true;
            checkFeatures = [ "cli" ];
            nativeBuildInputs = with pkgs; [ pkg-config ];
          }
        );

        bpb_enhance-gui = pkgs.rustPlatform.buildRustPackage (
          commonArgs
          // {
            buildNoDefaultFeatures = true;
            buildFeatures = [ "gui" ];
            checkNoDefaultFeatures = true;
            checkFeatures = [ "gui" ];

            nativeBuildInputs = with pkgs; [
              makeWrapper
              pkg-config
            ];

            buildInputs = guiRuntimeLibs;

            postInstall = ''
              wrapProgram "$out/bin/bpb_enhance" \
                --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath guiRuntimeLibs}"
            '';
          }
        );
      in
      {
        packages = {
          default = bpb_enhance-cli;
          cli = bpb_enhance-cli;
          gui = bpb_enhance-gui;
        };

        apps = {
          default = flake-utils.lib.mkApp { drv = bpb_enhance-cli; };
          cli = flake-utils.lib.mkApp { drv = bpb_enhance-cli; };
          gui = flake-utils.lib.mkApp { drv = bpb_enhance-gui; };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            pkg-config
          ] ++ guiRuntimeLibs;

          LD_LIBRARY_PATH = lib.makeLibraryPath guiRuntimeLibs;
        };
      }
    );
}
