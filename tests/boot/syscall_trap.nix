{
  config,
  pkgs,
  ...
}: {
  root-task.package = pkgs.stdenv.mkDerivation {
    pname = "horn-syscall-trap-probe";
    version = "1";
    src = ./syscall-trap;
    dontConfigure = true;
    hardeningDisable = ["all"];
    dontStrip = true;
    dontPatchELF = true;
    buildPhase = ''
      runHook preBuild
      $CC -nostdlib -static -no-pie -ffreestanding -fno-builtin \
        -fno-stack-protector -fno-asynchronous-unwind-tables -mgeneral-regs-only \
        -D__thread= -DNDEBUG -O2 -Wall -Wextra -Werror \
        -I${config.sel4.out}/libsel4/include \
        -Wl,-e,_start -Wl,-Ttext=0x400000 main.c entry.S -o task.elf
      runHook postBuild
    '';
    installPhase = ''
      mkdir -p "$out/bin"
      cp task.elf "$out/bin/task.elf"
    '';
  };
}
