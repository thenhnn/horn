{
  config,
  lib,
  pkgs,
  ...
}: let
  idl = "${../../../tools/idl}";
  mimalloc = import ../../../platform/runtime/mimalloc/default.nix {pkgs = pkgs.pkgsBuildBuild;};
  protocolOverride = old: {
    HORN_IDL = idl;
    HORN_VM_IDL = "${../../../platform/core/vm/idl}";
    nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.python3];
  };
  demoOverride = old: (protocolOverride old) // {HORN_SCHEMA = "${./schema.py}";};
  childWorkspace = config.rust.workspace.override {
    target = "sel4";
    crateOverrides = {
      horn-vm = protocolOverride;
      horn-procd = protocolOverride;
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
      idl-demo-child = demoOverride;
      memchr = old: {features = ["alloc"];};
    };
  };
  child = childWorkspace.members.idl-demo-child;
  rootWorkspace = config.rust.workspace.override {
    target = "sel4-roottask";
    crateOverrides = {
      horn-vm = protocolOverride;
      horn-procd = protocolOverride;
      idl-demo-root = old:
        (demoOverride old)
        // {
          CHILD_ELF = "${child}/bin/task.elf";
          LAUNCH_ELF = "${child}/bin/launched.elf";
        };
      memchr = old: {features = ["alloc"];};
    };
  };
in {
  sel4.settings.KernelRootCNodeSizeBits = lib.mkForce 16;
  root-task.package = lib.mkForce rootWorkspace.members.idl-demo-root;
}
