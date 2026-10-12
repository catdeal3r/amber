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
      ];
    in
    {
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
            ])
          }:$LD_LIBRARY_PATH"
        '';
      };
    };
}
