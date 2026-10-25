{
  pkgs,
  settings,
}: let
  inherit (pkgs) lib;
  inherit (pkgs.stdenv) hostPlatform;
  build = pkgs.pkgsBuildBuild;
  llvm = build.llvmPackages;
  binutils = pkgs.buildPackages.binutils-unwrapped;
  upstream = builtins.fromJSON (builtins.readFile ../../vendor/sel4/upstream.json);
  profile =
    if settings.KernelDebugBuild
    then "debug"
    else "release";
in
  pkgs.stdenvNoCC.mkDerivation {
    pname = "horn-sel4-${settings.KernelPlatform}-${profile}";
    version = builtins.substring 0 12 upstream.revision;
    src = ../../vendor/sel4/src;

    nativeBuildInputs =
      [
        build.cmake
        build.ninja
        build.libxml2
        llvm.clang-unwrapped
        llvm.llvm
        (build.python3.withPackages (p: [p.jinja2 p.ply p.typing-extensions p.pyyaml p.lxml p.pyfdt]))
      ]
      ++ lib.optionals (!hostPlatform.isx86_64) [build.dtc build.qemu]; # qemu is needed to dump device tree for device.
    cmakeFlags =
      lib.mapAttrsToList (name: value:
        if builtins.isBool value
        then lib.cmakeBool name value
        else lib.cmakeFeature name (toString value))
      settings
      ++ [
        (lib.cmakeFeature "TRIPLE" hostPlatform.config)
        # FIXME: LLVM linker doesn't support some stuff, use binutils for now.
        (lib.cmakeFeature "CMAKE_EXE_LINKER_FLAGS" "--ld-path=${binutils}/bin/${binutils.targetPrefix}ld")
        (lib.cmakeFeature "CMAKE_AR" "${llvm.llvm}/bin/llvm-ar")
        (lib.cmakeFeature "CMAKE_RANLIB" "${llvm.llvm}/bin/llvm-ranlib")
        (lib.cmakeFeature "CMAKE_OBJCOPY" "${llvm.llvm}/bin/llvm-objcopy")
      ];
    preConfigure = ''
      cmakeFlagsArray+=("-DCMAKE_TOOLCHAIN_FILE=$PWD/llvm.cmake")
    '';
    postPatch = ''
      patchShebangs tools libsel4/tools
    '';

    # we're doing bare metal
    hardeningDisable = ["all"];
    dontStrip = true;
    dontPatchELF = true;
    dontFixCmake = true;

    postInstall = ''
      mkdir -p "$out/share/sel4"
      cp ${../../vendor/sel4/upstream.json} "$out/share/sel4/upstream.json"
      cp ${build.writeText "sel4-config.json" (builtins.toJSON settings)} "$out/share/sel4/config.json"
      cp CMakeCache.txt "$out/share/sel4/CMakeCache.txt"
      test -s "$out/bin/kernel.elf"
      test -s "$out/libsel4/include/sel4/sel4.h"
      test -s "$out/libsel4/include/sel4/gen_config.h"
      test -s "$out/libsel4/include/kernel/gen_config.json"
    '';
    passthru = {inherit profile settings;};
    meta = {
      description = "seL4 ${hostPlatform.parsed.cpu.name} kernel and libsel4 (${profile})";
      platforms = lib.platforms.linux;
      license = [lib.licenses.gpl2Only lib.licenses.bsd2];
    };
  }
