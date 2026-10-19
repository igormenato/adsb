{
  description = "Rust and the ARMv6 hard-float toolchain for building trck for the Pi Zero W";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
  inputs.rust-overlay = {
    url = "github:oxalica/rust-overlay";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
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
    in
    {
      packages.${system}.default = rpiGcc;

      devShells.${system}.default = pkgs.mkShell {
        packages = [ rust rpiGcc ];
        env.CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER = "${rpiGcc}/bin/arm-linux-gnueabihf-gcc";
      };
    };
}
