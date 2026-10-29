{pkgs, sources, sel4}: let
  idl = import ../../tools/idl {inherit pkgs;};
  initramfs = pkgs.runCommand "linux-horn-initramfs" {nativeBuildInputs = [pkgs.stdenv.cc];} ''
    mkdir -p "$out"
    $CC -nostdlib -static -no-pie -Wl,-e,_start -Wl,-Ttext=0x400000 ${./init/init.S} -o "$out/init"
    printf 'dir /dev 0755 0 0\nnod /dev/console 0600 0 0 c 5 1\nfile /init %s/init 0755 0 0\n' "$out" > "$out/list"
  '';
  host = pkgs.stdenv.mkDerivation ((idl.crate {}) // {
    pname = "linux-horn-host";
    version = "1";
    src = ./host;
    hardeningDisable = ["all"];
    dontConfigure = true;
    buildPhase = ''
      python3 -B -m horn_idl horn_linux --header protocol.h --c protocol.c --capsets caps.json
      flags="-std=gnu11 -O2 -ffreestanding -fno-builtin -fno-stack-protector -fno-pie -m64 -mcmodel=large -mno-red-zone -mgeneral-regs-only -D__thread= -DNDEBUG"
      cp ${../runtime/c/horn-ipc/src/horn_ipc.c} horn_ipc.c
      for source in transport.c protocol.c horn_ipc.c; do
        $CC $flags -I. -I${./arch/include/shared} -I${sel4}/libsel4/include \
          -I${../runtime/c/horn-ipc/include} -c "$source" -o "$(basename "$source" .c).o"
      done
      ar rcs host.a transport.o protocol.o horn_ipc.o
    '';
    installPhase = ''
      mkdir -p "$out/lib"
      cp host.a "$out/lib/"
      cp caps.json "$out/caps.json"
    '';
    dontFixup = true;
  });
in pkgs.stdenv.mkDerivation {
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
    cp ${host}/caps.json "$out/caps.json"
  '';
  dontFixup = true;
  passthru = {inherit host initramfs;};
}
