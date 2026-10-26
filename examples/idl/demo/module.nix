{config, lib, ...}: let
  base = config.rust.workspace.override {target = "sel4";};
  child = base.members.idl-demo-child;
  rootWorkspace = config.rust.workspace.override {
    target = "sel4-roottask";
    crateOverrides = {
      idl-demo-root = old: {
        CHILD_ELF = "${child}/bin/task.elf";
      };
      memchr = old: {
        features = [ "alloc" ];
      };
    };
  };
in {
  root-task.package = lib.mkForce rootWorkspace.members.idl-demo-root;
}
