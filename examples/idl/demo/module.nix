{config, lib, pkgs, ...}: let
  schema = "${./schema.py}";
  idl = "${../../../tools/idl}";

  demoOverride = old: {
    HORN_SCHEMA = schema;
    HORN_IDL = idl;
    HORN_MEMORY_IDL = "${../../../platform/core/memory/idl}";
    nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.python3];
  };

  childWorkspace = config.rust.workspace.override {
    target = "sel4";
    crateOverrides = {
      idl-demo-child = demoOverride;
    };
  };
  child = childWorkspace.members.idl-demo-child;

  rootWorkspace = config.rust.workspace.override {
    target = "sel4-roottask";
    crateOverrides = {
      idl-demo-root = old: (demoOverride old) // {
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
