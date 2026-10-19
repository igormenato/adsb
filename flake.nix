{
  description = "Rust and the Raspberry Pi toolchains for building trck";

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

      # libcc1 and its plugins need a host libstdc++. The tarball only
      # ships the target copies, and the compiler driver never loads them.
      rpiGcc = { triple, linkerName, hash }:
        pkgs.stdenvNoCC.mkDerivation {
          pname = "${triple}-gcc";
          version = "14.4.0";

          src = pkgs.fetchurl {
            url = "https://github.com/tttapa/toolchains/releases/download/1.4.0/x-tools-${triple}-gcc14.tar.xz";
            inherit hash;
          };

          nativeBuildInputs = [ pkgs.autoPatchelfHook ];
          autoPatchelfIgnoreMissingDeps = [ "libstdc++.so.6" "libgcc_s.so.1" ];

          sourceRoot = "x-tools";
          dontConfigure = true;
          dontBuild = true;
          dontFixup = true;

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin"
            cp -a ${triple} "$out/"
            ln -s "$out/${triple}/bin/${triple}-gcc" "$out/bin/${linkerName}"

            prefix="$out/${triple}"
            autoPatchelf "$prefix/bin" "$prefix/lib" "$prefix/libexec" \
              "$prefix/${triple}/bin"
            runHook postInstall
          '';
        };

      zeroGcc = rpiGcc {
        triple = "armv6-rpi-linux-gnueabihf";
        linkerName = "arm-linux-gnueabihf-gcc";
        hash = "sha256-mzVBPS7tqOU4wRl9+/a8QWMrdOypKX/D4BZI30vWgm0=";
      };
      pi3Gcc = rpiGcc {
        triple = "aarch64-rpi3-linux-gnu";
        linkerName = "aarch64-linux-gnu-gcc";
        hash = "sha256-OcQq5fDiYSA/FvaSkvwadEk4FQfF8giJOfuOJYyJLOw=";
      };

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

      crossPackage = target: linkerDrv: linkerName:
        let
          cargoTarget = lib.toUpper (builtins.replaceStrings [ "-" ] [ "_" ] target);
        in
        craneLib.buildPackage {
          inherit src cargoVendorDir;
          strictDeps = true;
          doCheck = false;
          dontStrip = true;
          dontPatchELF = true;
          CARGO_BUILD_TARGET = target;
          "CARGO_TARGET_${cargoTarget}_LINKER" = "${linkerDrv}/bin/${linkerName}";
          nativeBuildInputs = [ linkerDrv ];
        };

      piZero = crossPackage "arm-unknown-linux-gnueabihf" zeroGcc "arm-linux-gnueabihf-gcc";
      pi3 = crossPackage "aarch64-unknown-linux-gnu" pi3Gcc "aarch64-linux-gnu-gcc";

      tests = craneLib.cargoTest {
        inherit src cargoVendorDir;
        cargoArtifacts = hostArtifacts;
      };
    in
    {
      packages.${system} = {
        default = pkgs.linkFarm "trck" [
          { name = "bin/trck-pi-zero-w"; path = "${piZero}/bin/trck"; }
          { name = "bin/trck-pi-3b"; path = "${pi3}/bin/trck"; }
        ];
        pi-zero-w = piZero;
        pi-3b = pi3;
      };

      checks.${system} = {
        inherit piZero pi3 tests;
      };

      devShells.${system}.default = craneLib.devShell {
        packages = [ zeroGcc pi3Gcc ];
        CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${zeroGcc}/bin/arm-linux-gnueabihf-gcc";
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER = "${pi3Gcc}/bin/aarch64-linux-gnu-gcc";
      };
    };
}
