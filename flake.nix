{
  description = "wayluck: a Wayland compositor that leaves placement to a window manager";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
      # Loaded at run time (dlopen) by winit and EGL, not linked: they have
      # to be on the library path for `cargo run` to open a window.
      runtimeLibs =
        pkgs: with pkgs; [
          libGL
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
        ];
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "wayluck";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [
            pkg-config
            makeWrapper
          ];
          buildInputs = with pkgs; [
            libxkbcommon
            wayland
          ];
          # The libraries winit and EGL open at run time.
          postFixup = ''
            wrapProgram $out/bin/wayluck \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)}
          '';
        };
      });
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            pkg-config
            wayland-utils
          ];
          buildInputs = with pkgs; [
            libxkbcommon
            wayland
          ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });
    };
}
