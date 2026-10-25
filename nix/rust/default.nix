{
  pkgs,
  rust,
  sel4,
  release ? false,
  cargoNix ? ../../Cargo.nix,
  target ? "sel4",
}: let
  inherit (pkgs) lib;
  build = pkgs.pkgsBuildBuild;
  cpu = pkgs.stdenv.hostPlatform.parsed.cpu.name;
  arch = {riscv64 = "riscv64imac";}.${cpu} or cpu;
  targetName = "${arch}-${target}";
  targetFile = ../../vendor/rust-sel4/src/support/targets + "/${targetName}.json";
  generated = builtins.fromJSON (builtins.readFile ./cfg.json);
  targetData = generated.targets.${targetName};
  targetPlatform = lib.systems.elaborate {
    inherit (pkgs.stdenv.hostPlatform) config extensions;
    rust = {
      rustcTarget = targetName;
      rustcTargetSpec = "${targetFile}";
      platform = (builtins.fromJSON (builtins.readFile targetFile)) // {env = "";};
    };
  };
  cfgFor = platform: optimized:
    generated.targets.${
      if platform.isNone
      then targetName
      else platform.rust.rustcTarget
    }.${
      if optimized
      then "release"
      else "dev"
    };
  makeTarget = optimized: platform: let
    cfg = cfgFor platform optimized;
  in
    {
      name =
        if platform.isNone
        then targetName
        else platform.rust.rustcTarget;
      __flags = map (lib.removePrefix "target_") cfg.flags;
      family = [];
      feature = [];
    }
    // lib.mapAttrs' (name: value: {
      name = lib.removePrefix "target_" name;
      inherit value;
    })
    cfg.values;

  # buildRustCrate hardcodes Unix cfg variables. Replace them with rustc's
  # real target cfg before executing build.rs (which itself runs natively).
  configureCfg = platform: optimized: old: let
    cfg = cfgFor platform optimized;
    name =
      if platform.isNone
      then targetName
      else platform.rust.rustcTarget;
    marker = "# Compile and run the build script, when available.";
    variables = (lib.genAttrs cfg.flags (_: [])) // cfg.values;
    environment = lib.concatStringsSep "\n" (lib.mapAttrsToList (name: value:
      "export CARGO_CFG_${lib.toUpper name}="
      + lib.escapeShellArg (lib.concatStringsSep "," value))
    variables);
  in {
    configurePhase = assert lib.assertMsg (lib.hasInfix marker old.configurePhase) "buildRustCrate configure hook changed; update nix/rust/default.nix";
      builtins.replaceStrings [marker] [
        ''
          for variable in ''${!CARGO_CFG_@}; do unset "$variable"; done
          ${environment}
          export TARGET=${lib.escapeShellArg name}
          export DEBUG=${lib.boolToString (!optimized)}
          ${marker}
        ''
      ]
      old.configurePhase;
  };
  base = cratePkgs: optimized: args: let
    stdenv =
      if cratePkgs.stdenv.hostPlatform.isNone
      then
        cratePkgs.stdenvNoCC.override {
          hostPlatform = targetPlatform;
          inherit targetPlatform;
        }
      else cratePkgs.stdenvNoCC;
    builder = cratePkgs.buildRustCrate.override {
      inherit stdenv;
      rustc = rust;
      cargo = rust;
    };
  in
    (builder args).overrideAttrs (configureCfg stdenv.hostPlatform optimized);

  stdlibBuilder = cratePkgs: args:
    assert builtins.elem args.crateName ["core" "alloc" "compiler_builtins"];
      base cratePkgs true (args
        // {
          # core also includes sibling stdarch/portable-simd source files.
          src = "${rust}/lib/rustlib/src/rust/library";
          workspace_member =
            if args.crateName == "compiler_builtins"
            then "compiler-builtins/compiler-builtins"
            else args.crateName;
          dependencies = map (d: (lib.getLib d) // {stdlib = true;}) args.dependencies;
          extraRustcOpts = ["-Zunstable-options"];
          codegenUnits =
            if args.crateName == "compiler_builtins"
            then 10000
            else 1;
        });
  sysrootCargo = import ./sysroot.nix {
    inherit pkgs;
    rustLibrary = "${rust}/lib/rustlib/src/rust/library";
    crateConfig = {};
    buildRustCrateForPkgs = stdlibBuilder;
  };
  sysroot = sysrootCargo.internal.builtRustCratesWithFeatures {
    packageId = "horn-sysroot";
    features = ["default"];
    buildRustCrateForPkgsFunc = stdlibBuilder;
    makeTarget = makeTarget true;
    runTests = false;
  };
  stdlibs = map (name: sysroot.crates.${name}.lib // {stdlib = true;}) ["core" "alloc" "compiler_builtins"];
  builder = cratePkgs: args:
    base cratePkgs release (args
      // lib.optionalAttrs cratePkgs.stdenv.hostPlatform.isNone {
        dependencies = args.dependencies ++ stdlibs;
      }
      // lib.optionalAttrs (builtins.elem args.crateName ["sel4-config-data" "sel4-sys"]) {
        SEL4_PREFIX = sel4;
      }
      // lib.optionalAttrs (args.crateName == "sel4-sys") {
        LIBCLANG_PATH = "${build.llvmPackages.libclang.lib}/lib";
      });
  cargo = import cargoNix {
    inherit pkgs release;
    crateConfig = {};
    buildRustCrateForPkgs = builder;
  };
  member = packageId:
    lib.makeOverridable ({features ? ["default"]}: let
      graph = cargo.internal.builtRustCratesWithFeatures {
        inherit packageId features;
        buildRustCrateForPkgsFunc = builder;
        makeTarget = makeTarget release;
        runTests = false;
      };
    in
      graph.crates.${packageId}) {};
in
  assert lib.assertMsg (generated.toolchainVersion == rust.version) "Rust toolchain changed; run just rust-generate inside nix-shell";
  assert lib.assertMsg (targetData.specHash == builtins.hashString "sha256" (builtins.readFile targetFile)) "Rust target changed; run just rust-generate inside nix-shell"; {
    members = lib.mapAttrs (_: value: member value.packageId) cargo.workspaceMembers;
  }
