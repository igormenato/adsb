{
  description = "Rust for building trck";

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

      # Bundled with the Nix rustc. It links the musl objects shipped in
      # rust-std, so the release does not need a C sysroot.
      rustLld = "${rust}/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld";
      staticMusl = "-C target-feature=+crt-static -C linker-flavor=ld.lld -C link-self-contained=yes";

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

      board = { target, rustflags }:
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
          "CARGO_TARGET_${cargoTarget}_LINKER" = rustLld;
          "CARGO_TARGET_${cargoTarget}_RUSTFLAGS" = rustflags;
        };

      piZero = board {
        target = "arm-unknown-linux-musleabihf";
        rustflags = staticMusl;
      };
      # rustc adds the Cortex-A53 workaround only when the linker is gcc.
      pi3 = board {
        target = "aarch64-unknown-linux-musl";
        rustflags = "${staticMusl} -C link-arg=--fix-cortex-a53-843419";
      };

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
        CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_LINKER = rustLld;
        CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS = staticMusl;
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = rustLld;
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = "${staticMusl} -C link-arg=--fix-cortex-a53-843419";
      };
    };
}
