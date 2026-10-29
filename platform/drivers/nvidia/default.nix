{
  workspace,
  pkgs,
}: let
  mimalloc = import ../../runtime/mimalloc {inherit pkgs;};
in
  (workspace.override {
    target = "sel4";
    crateOverrides = {
      memchr = old: {features = ["alloc"];};
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
    };
  }).members.horn-nvidia-task
