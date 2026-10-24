{
  description = "Zig for building trck";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      zig = pkgs.zig_0_17;

      # The hashes match build.zig.zon. A sandbox build cannot fetch them itself.
      clapUrl = "git+https://github.com/Hejsil/zig-clap#05faf3905e8548f5cc269a8836e154065e70128d";
      serialUrl = "git+https://github.com/jcollie/zig-serial#87bb86c2965ba913429ebe470adf33a0d4d3692f";

      zigDeps = pkgs.runCommand "trck-zig-deps" {
        nativeBuildInputs = [ zig pkgs.cacert ];
        outputHashMode = "recursive";
        outputHashAlgo = "sha256";
        outputHash = "sha256-/D5JuioAMPruYAIQkKfGBimd0WeNtpLaHNlW3REFTDE=";
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
      } ''
        export ZIG_GLOBAL_CACHE_DIR=$TMPDIR/zig-fetch
        mkdir -p "$ZIG_GLOBAL_CACHE_DIR"
        zig fetch ${clapUrl}
        zig fetch ${serialUrl}
        mkdir -p "$out/p"
        cp -a "$ZIG_GLOBAL_CACHE_DIR/p/." "$out/p/"
      '';

      src = pkgs.lib.fileset.toSource {
        root = ./.;
        fileset = pkgs.lib.fileset.unions [
          ./src
          ./build.zig
          ./build.zig.zon
        ];
      };

      # The build has no writable $HOME for Zig's cache. The package tarballs
      # are already in zigDeps, so this step does not fetch.
      prepareCache = ''
        export ZIG_GLOBAL_CACHE_DIR=$TMPDIR/zig-cache
        mkdir -p "$ZIG_GLOBAL_CACHE_DIR/p"
        cp -a ${zigDeps}/p/. "$ZIG_GLOBAL_CACHE_DIR/p/"
      '';

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
          ${prepareCache}
          zig build -Dtarget=arm-linux-musleabihf -Dcpu=arm1176jzf_s -Doptimize=ReleaseSafe
          runHook postBuild
        '';
        installPhase = ''
          runHook preInstall
          install -Dm755 zig-out/bin/trck $out/bin/trck-pi-zero-w
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
          ${prepareCache}
          zig fmt --check src build.zig
          zig build test -Dtarget=native-linux-musl
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
