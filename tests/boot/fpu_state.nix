{
  config,
  lib,
  pkgs,
  ...
}: {
  sel4.settings = {
    KernelMaxNumNodes = lib.mkForce 2;
    KernelXSaveFeatureSet = lib.mkForce 7;
    KernelXSaveOptionalFeatureSet = 224;
    KernelXSaveSize = lib.mkForce 4096;
  };
  root-task.package = pkgs.stdenv.mkDerivation {
    pname = "horn-fpu-state-probe";
    version = "1";
    src = ./fpu-state;
    dontConfigure = true;
    hardeningDisable = ["all"];
    dontStrip = true;
    dontPatchELF = true;
    buildPhase = ''
      $CC -nostdlib -static -no-pie -ffreestanding -fno-builtin \
        -fno-stack-protector -fno-asynchronous-unwind-tables -mgeneral-regs-only \
        -D__thread= -DNDEBUG -O2 -Wall -Wextra -Werror \
        -I${config.sel4.out}/libsel4/include \
        -Wl,-e,_start -Wl,-Ttext=0x400000 main.c entry.S -o task.elf
    '';
    installPhase = ''
      mkdir -p "$out/bin"
      cp task.elf "$out/bin/task.elf"
    '';
  };
}
