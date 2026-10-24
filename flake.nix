{
  description = "Zig for building trck";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      zig = pkgs.zig_0_17;

      src = pkgs.lib.fileset.toSource {
        root = ./.;
        fileset = ./src;
      };

      # The build has no writable $HOME for Zig's cache.
      zigCache = "export ZIG_GLOBAL_CACHE_DIR=$TMPDIR/zig-cache";

      # arm1176jzf-s is the Pi Zero W core: ARMv6 with VFPv2. musleabihf passes floats in
      # VFP registers, and Zig links its bundled musl statically.
      piZero = pkgs.stdenvNoCC.mkDerivation {
        name = "trck-pi-zero-w";
        inherit src;
        nativeBuildInputs = [ zig ];
        dontConfigure = true;
        dontStrip = true;
        buildPhase = ''
          runHook preBuild
          ${zigCache}
          zig build-exe src/main.zig -target arm-linux-musleabihf -mcpu=arm1176jzf_s -lc -O ReleaseSafe --name trck
          runHook postBuild
        '';
        installPhase = ''
          runHook preInstall
          install -Dm755 trck $out/bin/trck-pi-zero-w
          runHook postInstall
        '';
      };

      tests = pkgs.stdenvNoCC.mkDerivation {
        name = "trck-tests";
        inherit src;
        nativeBuildInputs = [ zig ];
        dontConfigure = true;
        buildPhase = ''
          runHook preBuild
          ${zigCache}
          zig fmt --check src
          zig test src/main.zig -target native-linux-musl -lc
          runHook postBuild
        '';
        installPhase = "touch $out";
      };
    in
    {
      packages.${system}.default = piZero;

      checks.${system} = {
        inherit piZero tests;
      };

      devShells.${system}.default = pkgs.mkShellNoCC {
        packages = [ zig ];
      };
    };
}
