{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};

      # Everything iced/wgpu + the wayland client stack need to link.
      nativeBuildInputs = with pkgs; [
        pkg-config
        cmake
      ];

      buildInputs = with pkgs; [
        wayland
        wayland-protocols
        wayland-scanner
        libxkbcommon
        libGL
        vulkan-loader
        vulkan-headers
        fontconfig
        freetype
        linux-pam
      ];
    in
    {
      packages.${system} =
        let
          # The bar, wrapped so it can find the Wayland/GL/fontconfig
          # libraries at runtime (a bare Nix binary has no RPATH for them).
          amber-panel = pkgs.rustPlatform.buildRustPackage {
            pname = "amber-panel";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            buildAndTestSubdir = "crates/panel";
            inherit buildInputs;
            nativeBuildInputs = nativeBuildInputs ++ [ pkgs.makeWrapper ];
            postInstall = ''
              wrapProgram $out/bin/amber-panel \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath buildInputs}
            '';
          };
        in
        {
          inherit amber-panel;
          default = amber-panel;
        };

      devShells.${system}.default = pkgs.mkShell {
        inherit nativeBuildInputs buildInputs;

        shellHook = ''
          # wayland-client, the GL/Vulkan loaders and fontconfig are loaded at
          # runtime, so make sure the linker can find them here.
          export LD_LIBRARY_PATH="${
            pkgs.lib.makeLibraryPath (with pkgs; [
              wayland
              libxkbcommon
              libGL
              vulkan-loader
              fontconfig
              freetype
              linux-pam
            ])
          }:$LD_LIBRARY_PATH"
        '';
      };
    };
}
