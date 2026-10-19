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
      glibc = pkgs.glibc;
      rpiGcc = pkgs.stdenvNoCC.mkDerivation {
        pname = "armv6-rpi-linux-gnueabihf-gcc";
        version = "14.4.0";

        src = pkgs.fetchurl {
          url = "https://github.com/tttapa/toolchains/releases/download/1.4.0/x-tools-armv6-rpi-linux-gnueabihf-gcc14.tar.xz";
          hash = "sha256-mzVBPS7tqOU4wRl9+/a8QWMrdOypKX/D4BZI30vWgm0=";
        };

        nativeBuildInputs = [ pkgs.file pkgs.patchelf ];

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

          # Host tools need Nix's libc. Leave the ARMv6 sysroot alone.
          interp="${glibc}/lib/ld-linux-x86-64.so.2"
          find "$out/armv6-rpi-linux-gnueabihf" -path '*/sysroot/*' -prune -o -type f -print0 |
            while IFS= read -r -d $'\0' bin; do
              kind=$(file -b "$bin")
              case "$kind" in
                "ELF 64-bit LSB executable, x86-64,"*"dynamically linked"*)
                  patchelf --set-interpreter "$interp" --set-rpath "${glibc}/lib" "$bin"
                  ;;
                "ELF 64-bit LSB shared object, x86-64,"*)
                  patchelf --set-rpath "${glibc}/lib" "$bin"
                  ;;
              esac
            done
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
