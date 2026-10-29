{
  config,
  lib,
  pkgs,
  ...
}: let
  mimalloc = import ../../../platform/runtime/mimalloc/default.nix {pkgs = pkgs.pkgsBuildBuild;};
  childWorkspace = config.rust.workspace.override {
    target = "sel4";
    crateOverrides = {
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
      memchr = old: {features = ["alloc"];};
    };
  };
  child = childWorkspace.members.idl-demo-child;
  rootWorkspace = config.rust.workspace.override {
    target = "sel4-roottask";
    crateOverrides = {
      idl-demo-root = old: {
        CHILD_ELF = "${child}/bin/task.elf";
        LAUNCH_ELF = "${child}/bin/launched.elf";
        SMP_ELF = "${childWorkspace.members.horn-smp-probe.override {features = ["worker"];}}/bin/smp_worker.elf";
      };
      memchr = old: {features = ["alloc"];};
    };
  };
in {
  sel4.settings.KernelRootCNodeSizeBits = lib.mkForce 16;
  root-task.package = lib.mkForce rootWorkspace.members.idl-demo-root;
}
