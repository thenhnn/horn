{
  workspace,
  pkgs,
  firmware ? null,
}: let
  mimalloc = import ../../runtime/mimalloc {inherit pkgs;};
in
  (workspace.override {
    target = "sel4";
    crateOverrides = {
      memchr = old: {features = ["alloc"];};
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
      horn-nvidia-task = old: pkgs.lib.optionalAttrs (firmware != null) {NVIDIA_FIRMWARE_DIR = "${firmware}/nvidia/ad106/gsp";};
    };
  }).members.horn-nvidia-task
