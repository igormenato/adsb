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

        nativeBuildInputs = [ pkgs.binutils pkgs.patchelf ];

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
          interp="${glibc}/lib/ld-linux-x86-64.so.2"
          rpath="${glibc}/lib"
          find "$prefix/bin" "$prefix/lib" "$prefix/libexec" \
              "$prefix/armv6-rpi-linux-gnueabihf/bin" -type f -print0 |
            while IFS= read -r -d $'\0' bin; do
              readelf -h "$bin" 2>/dev/null | grep -q 'Machine:[[:space:]]*Advanced Micro Devices X86-64' || continue
              if readelf -l "$bin" 2>/dev/null | grep -q 'Requesting program interpreter'; then
                patchelf --set-interpreter "$interp" --set-rpath "$rpath" "$bin"
              else
                patchelf --set-rpath "$rpath" "$bin"
              fi
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
