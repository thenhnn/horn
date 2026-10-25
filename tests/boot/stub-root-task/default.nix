{
  pkgs,
  sel4,
}: let
  inherit (pkgs) lib;
  build = pkgs.pkgsBuildBuild;
  llvm = build.llvmPackages;
  binutils = pkgs.buildPackages.binutils-unwrapped;
in
  assert lib.assertMsg pkgs.stdenv.hostPlatform.isx86_64 "The test stub currently supports x86-64 only.";
    pkgs.stdenvNoCC.mkDerivation {
      name = "stub-root-task";
      src = ./src;
      nativeBuildInputs = [llvm.clang-unwrapped];
      dontConfigure = true;
      hardeningDisable = ["all"];
      dontStrip = true;
      dontPatchELF = true;
      buildPhase = ''
        runHook preBuild
        clang --target=${pkgs.stdenv.hostPlatform.config} \
          -std=gnu11 -O2 -g -Wall -Wextra -Werror \
          -ffreestanding -fno-stack-protector -fno-pic -fno-pie \
          -mno-red-zone \
          -I${sel4}/libsel4/include \
          -nostdlib -static --ld-path=${binutils}/bin/${binutils.targetPrefix}ld \
          -Wl,-T,link.ld -Wl,--build-id=none -Wl,-z,noexecstack \
          start.S main.c -o root-task.elf
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 root-task.elf "$out/bin/root-task.elf"
        runHook postInstall
      '';
    }
