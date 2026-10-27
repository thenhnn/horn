{pkgs}:
pkgs.llvmPackages.stdenv.mkDerivation {
  pname = "horn-mimalloc";
  version = "3.6.0";
  src = ../../vendor/mimalloc/src;
  dontConfigure = true;
  hardeningDisable = ["all"];
  buildPhase = ''
    runHook preBuild
    $CC -std=gnu11 -O2 -DNDEBUG -DMI_DEBUG=0 -DMI_HORN=1 \
      -DMI_DEFAULT_VIRTUAL_ADDRESS_BITS=32 -DMI_FREE_USE_PAGEMAP=1 -DMI_PROFILE=0 \
      -DMI_PROFILE_USE_BUILTIN=0 -DMI_GUARDED=0 -DMI_SECURE=0 \
      -DMI_STAT=0 -U__linux__ -ffreestanding -fno-stack-protector \
      -fno-pie -fno-builtin -mno-red-zone -ftls-model=local-exec \
      -ffunction-sections -fdata-sections \
      -I include -I ${./.} -c src/static.c -o mimalloc.o
    $CC -std=gnu11 -O2 -ffreestanding -fno-pie -mno-red-zone -I include \
      -c ${./init.c} -o init.o
    $AR rcs libhorn-mimalloc.a mimalloc.o init.o
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    mkdir -p $out/lib $out/share/licenses/mimalloc
    cp libhorn-mimalloc.a $out/lib/
    cp LICENSE $out/share/licenses/mimalloc/
    runHook postInstall
  '';
}
