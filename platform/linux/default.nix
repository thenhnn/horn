{
  pkgs,
  sources,
  sel4,
  workspace,
}: let
  idl = import ../../tools/idl {inherit pkgs;};
  startup = workspace.members.horn-startup-c;
  startupHeaders = import ../libhorn {inherit pkgs;};
  muslDemo = pkgs.pkgsStatic.stdenv.mkDerivation {
    pname = "linux-horn-musl-demo";
    version = "1";
    src = ./init;
    dontConfigure = true;
    hardeningDisable = ["pic"];
    buildPhase = ''
      $CC -std=c11 -O2 -Wall -Wextra -Werror -static -no-pie demo.c -o init
      $CC -nostdlib -static -no-pie -Wl,-e,_start -Wl,-Ttext=0x400000 init.S -o fp-probe
    '';
    installPhase = ''
      mkdir -p "$out/bin"
      cp init fp-probe "$out/bin/"
    '';
  };
  initramfs = pkgs.runCommand "linux-horn-initramfs" {} ''
    mkdir -p "$out"
    printf 'Hello from the Horn initramfs!\n' > "$out/demo.txt"
    printf 'dir /dev 0755 0 0\nnod /dev/console 0600 0 0 c 5 1\nfile /init ${muslDemo}/bin/init 0755 0 0\nfile /fp-probe ${muslDemo}/bin/fp-probe 0755 0 0\nfile /demo.txt %s/demo.txt 0644 0 0\n' "$out" > "$out/list"
  '';
  host = pkgs.stdenv.mkDerivation ((idl.crate {})
    // {
      pname = "linux-horn-host";
      version = "1";
      src = ./host;
      hardeningDisable = ["all"];
      dontConfigure = true;
      buildPhase = ''
        python3 -B -m horn_idl horn_linux --header protocol.h --c protocol.c
        flags="-std=gnu11 -O2 -ffreestanding -fno-builtin -fno-stack-protector -fno-pie -m64 -mcmodel=large -mno-red-zone -mgeneral-regs-only -D__thread= -DNDEBUG"
        cp ${../runtime/c/horn-ipc/src/horn_ipc.c} horn_ipc.c
        for source in transport.c protocol.c horn_ipc.c; do
          $CC $flags -I. -I${./arch/include/shared} -I${sel4}/libsel4/include \
            -I${../runtime/c/horn-ipc/include} -I${startupHeaders}/include -c "$source" -o "$(basename "$source" .c).o"
        done
        ar -M <<EOF
        CREATE host.a
        ADDLIB $(echo ${startup.lib}/lib/libhorn_startup_c-*.a)
        ADDMOD transport.o protocol.o horn_ipc.o
        SAVE
        END
        EOF
      '';
      installPhase = ''
        mkdir -p "$out/lib"
        cp host.a "$out/lib/"
      '';
      dontFixup = true;
    });
in
  pkgs.stdenv.mkDerivation {
    pname = "linux-horn";
    version = "6.18";
    src = sources.linux;
    nativeBuildInputs = with pkgs; [gnumake flex bison bc python3 perl pkg-config];
    buildInputs = with pkgs; [libelf openssl];
    hardeningDisable = ["all"];
    enableParallelBuilding = true;
    KBUILD_BUILD_TIMESTAMP = "1970-01-01 00:00:01 +0000";
    KBUILD_BUILD_USER = "horn";
    KBUILD_BUILD_HOST = "horn";
    KBUILD_BUILD_VERSION = "1";
    SOURCE_DATE_EPOCH = "1";
    postUnpack = ''
      cp -r ${./arch} "$sourceRoot/arch/horn"
      chmod -R u+w "$sourceRoot/arch/horn"
    '';
    makeFlags = ["ARCH=horn" "HOSTCC=cc" "HORN_HOST_ARCHIVE=${host}/lib/host.a"];
    configurePhase = ''
      runHook preConfigure
      make ARCH=horn defconfig
      bash scripts/config --set-str INITRAMFS_SOURCE "${initramfs}/list"
      make ARCH=horn olddefconfig
      runHook postConfigure
    '';
    buildFlags = ["vmlinux"];
    installPhase = ''
      mkdir -p "$out/bin"
      ${pkgs.stdenv.cc.bintools.targetPrefix}objcopy --strip-debug vmlinux "$out/bin/task.elf"
      cp .config "$out/config"
    '';
    dontFixup = true;
    passthru = {inherit host initramfs muslDemo;};
  }
