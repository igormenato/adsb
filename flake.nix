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

      piZero = craneLib.buildPackage {
        inherit src cargoVendorDir;
        strictDeps = true;
        doCheck = false;
        dontStrip = true;
        CARGO_BUILD_TARGET = "arm-unknown-linux-musleabihf";
        CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_LINKER = rustLld;
        CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS = staticMusl;
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
        ];
      };

      checks.${system} = {
        inherit piZero tests;
      };

      devShells.${system}.default = craneLib.devShell {
        CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_LINKER = rustLld;
        CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS = staticMusl;
      };
    };
}
