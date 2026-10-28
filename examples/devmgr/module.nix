{
  config,
  lib,
  pkgs,
  ...
}: let
  protocol = old: {
    HORN_IDL = "${../../tools/idl}";
    HORN_VM_IDL = "${../../platform/core/vm/idl}";
    nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.python3];
  };
  childWorkspace = config.rust.workspace.override {
    target = "sel4";
    crateOverrides.horn-devmgr = protocol;
  };
  task = childWorkspace.members.horn-devmgr-task;
  console = import ../../platform/drivers/virtio/console {
    workspace = config.rust.workspace;
    pkgs = pkgs.pkgsBuildBuild;
    probe = config.devmgr-demo.dma-probe;
  };
  rootWorkspace = config.rust.workspace.override {
    target = "sel4-roottask";
    crateOverrides = {
      horn-devmgr = protocol;
      horn-procd = protocol;
      horn-vm = protocol;
      devmgr-demo-root = old:
        {
          DEVMGR_TEST_VTD =
            if config.devmgr-demo.console
            then "1"
            else "0";
          DEVMGR_ELF = "${task}/bin/devmgr.elf";
          DEVMGR_TEST_STALL =
            if config.devmgr-demo.stall-bootstrap
            then "1"
            else "0";
          DEVMGR_TEST_FAULT =
            if config.devmgr-demo.inject-fault
            then "1"
            else "0";
        }
        // lib.optionalAttrs config.devmgr-demo.console {CONSOLE_ELF = "${console}/bin/virtio_console.elf";};
      memchr = old: {features = ["alloc"];};
    };
  };
in {
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
