{
  description = "Rust and the ARMv6 hard-float toolchain for building trck for the Pi Zero W";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  inputs.rust-overlay = {
    url = "github:oxalica/rust-overlay";
    inputs.nixpkgs.follows = "nixpkgs";
  };
  inputs.crane.url = "github:ipetkov/crane/v0.24.0";

  outputs = { nixpkgs, rust-overlay, crane, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      lib = pkgs.lib;
      rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      craneLib = (crane.mkLib pkgs).overrideToolchain rust;

      target = "arm-unknown-linux-gnueabihf";
      rpiGcc = pkgs.stdenvNoCC.mkDerivation {
        pname = "armv6-rpi-linux-gnueabihf-gcc";
        version = "14.4.0";

        src = pkgs.fetchurl {
          url = "https://github.com/tttapa/toolchains/releases/download/1.4.0/x-tools-armv6-rpi-linux-gnueabihf-gcc14.tar.xz";
          hash = "sha256-mzVBPS7tqOU4wRl9+/a8QWMrdOypKX/D4BZI30vWgm0=";
        };

        nativeBuildInputs = [ pkgs.autoPatchelfHook ];

        # libcc1 and its plugins need a host libstdc++. The tarball only
        # ships the ARM copies, and the compiler driver never loads them.
        autoPatchelfIgnoreMissingDeps = [ "libstdc++.so.6" "libgcc_s.so.1" ];

        sourceRoot = "x-tools";
        dontConfigure = true;
        dontBuild = true;
        dontFixup = true;

        installPhase = ''
          runHook preInstall
          mkdir -p "$out/bin"
          cp -a armv6-rpi-linux-gnueabihf "$out/"
          ln -s "$out/armv6-rpi-linux-gnueabihf/bin/armv6-rpi-linux-gnueabihf-gcc" \
            "$out/bin/arm-linux-gnueabihf-gcc"

          prefix="$out/armv6-rpi-linux-gnueabihf"
          autoPatchelf "$prefix/bin" "$prefix/lib" "$prefix/libexec" \
            "$prefix/armv6-rpi-linux-gnueabihf/bin"
          runHook postInstall
        '';
      };
      linker = "${rpiGcc}/bin/arm-linux-gnueabihf-gcc";

      src = lib.cleanSourceWith {
        src = lib.cleanSource ./.;
        filter = path: type:
          (craneLib.filterCargoSources path type)
          || lib.hasSuffix "/sample" path
          || lib.hasInfix "/sample/" path;
      };

      cargoVendorDir = craneLib.vendorCargoDeps { inherit src; };

      hostArtifacts = craneLib.buildDepsOnly {
        inherit src cargoVendorDir;
      };

      trck = craneLib.buildPackage {
        inherit src cargoVendorDir;
        strictDeps = true;
        doCheck = false;
        dontStrip = true;
        dontPatchELF = true;
        CARGO_BUILD_TARGET = target;
        CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER = linker;
        nativeBuildInputs = [ rpiGcc ];
      };

      tests = craneLib.cargoTest {
        inherit src cargoVendorDir;
        cargoArtifacts = hostArtifacts;
      };
    in
    {
      packages.${system}.default = trck;

      checks.${system} = {
        inherit trck tests;
      };

      devShells.${system}.default = craneLib.devShell {
        packages = [ rpiGcc ];
        CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER = linker;
      };
    };
}
