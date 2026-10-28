{
  config,
  lib,
  pkgs,
  ...
}: let
  # Embedded launch images need ELF/TLS/.horn metadata, not nested DWARF.
  # Keep original packages for symbolization; strip only the embedded copies.
  runtimeElf = package: name:
    pkgs.pkgsBuildBuild.runCommand "${name}-runtime.elf" {
      nativeBuildInputs = [pkgs.pkgsBuildBuild.llvmPackages.llvm];
    } ''llvm-objcopy --strip-debug ${package}/bin/${name}.elf "$out"'';
  mimalloc = import ../../platform/runtime/mimalloc {pkgs = pkgs.pkgsBuildBuild;};
  protocol = old: {
    HORN_IDL = "${../../tools/idl}";
    HORN_VM_IDL = "${../../platform/core/vm/idl}";
    HORN_PROCESS_IDL = "${../../platform/core/procd/idl}";
    HORN_DEVICE_IDL = "${../../platform/core/devmgr/idl}";
    nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.python3];
  };
  console = import ../../platform/drivers/virtio/console {
    workspace = config.rust.workspace;
    pkgs = pkgs.pkgsBuildBuild;
    probe = config.devmgr-demo.dma-probe;
    driverTest =
      if config.devmgr-demo.stop-mode == "timeout"
      then "ignore"
      else config.devmgr-demo.stop-mode;
  };
  childWorkspace = config.rust.workspace.override {
    target = "sel4";
    crateOverrides = {
      horn-devmgr = protocol;
      horn-procd = protocol;
      horn-supervisor = protocol;
      horn-vm = protocol;
      memchr = old: {features = ["alloc"];};
      horn-devmgr-probe = protocol;
      horn-devmgr-task = old:
        (protocol old)
        // {
          features = (old.features or []) ++ lib.optional config.devmgr-demo.boot-tests "boot-tests";
          HORN_DEVICE_TEST_IDL =
            if config.devmgr-demo.boot-tests
            then "${../../tests/boot/devmgr-probe/schema.py}"
            else "";
          DEVMGR_TEST_FAULT =
            if config.devmgr-demo.inject-fault
            then "1"
            else "0";
          DEVMGR_TEST_EARLY_FAULT =
            if config.devmgr-demo.early-fault
            then "1"
            else "0";
          DEVMGR_TEST_NO_READY =
            if config.devmgr-demo.withhold-ready
            then "1"
            else "0";
          DEVMGR_DRIVER_ELF =
            if config.devmgr-demo.console
            then "${runtimeElf console "virtio_console"}"
            else "";
          DEVMGR_DRIVER_DEVICE = "33";
          DEVMGR_DRIVER_NAME = "virtio-console";
          DEVMGR_DRIVER_RAM = "33554432";
          DEVMGR_PROBE_ELF =
            if config.devmgr-demo.boot-tests
            then "${runtimeElf childWorkspace.members.horn-devmgr-probe "devmgr_probe"}"
            else "";
        };
      horn-supervisor-task = old: {
        features = (old.features or []) ++ lib.optional config.devmgr-demo.boot-tests "boot-tests";
        DEVMGR_ELF = "${runtimeElf childWorkspace.members.horn-devmgr-task "devmgr"}";
        SUPERVISOR_STALL =
          if config.devmgr-demo.stall-bootstrap
          then "1"
          else "0";
      };
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
    };
  };
  rootWorkspace = config.rust.workspace.override {
    target = "sel4-roottask";
    crateOverrides = {
      horn-devmgr = protocol;
      horn-procd = protocol;
      horn-supervisor = protocol;
      horn-vm = protocol;
      devmgr-demo-root = old: {
        features = (old.features or []) ++ lib.optional config.devmgr-demo.boot-tests "boot-tests";
        SUPERVISOR_ELF = "${runtimeElf childWorkspace.members.horn-supervisor-task "supervisor"}";
        DEVMGR_TEST_VTD =
          if config.devmgr-demo.console
          then "1"
          else "0";
        DEVMGR_TEST_STALL =
          if config.devmgr-demo.stall-bootstrap
          then "1"
          else "0";
        ROOT_STARTUP_TIMEOUT_US = toString config.devmgr-demo.startup-timeout-us;
      };
      memchr = old: {features = ["alloc"];};
    };
  };
in {
  options.devmgr-demo.boot-tests = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = "Compile boot-test fixtures and self-checks; never enabled for ordinary boots.";
  };
  options.devmgr-demo.early-fault = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  options.devmgr-demo.withhold-ready = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  options.devmgr-demo.startup-timeout-us = lib.mkOption {
    type = lib.types.ints.positive;
    default = 30000000;
  };
  options.devmgr-demo.stop-mode = lib.mkOption {
    type = lib.types.enum ["none" "orderly" "force" "timeout" "fault" "exit"];
    default = "none";
    description = "Isolated supervisor/driver lifecycle test mode; never enables reassignment.";
  };
  options.devmgr-demo.dma-probe = lib.mkOption {
    type = lib.types.enum ["none" "outside" "stale"];
    default = "none";
  };
  options.devmgr-demo.console = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  options.devmgr-demo.inject-fault = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  options.devmgr-demo.stall-bootstrap = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  config = {
    sel4.settings.KernelRootCNodeSizeBits = lib.mkForce 16;
    root-task.package = lib.mkForce rootWorkspace.members.devmgr-demo-root;
  };
}
