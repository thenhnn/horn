{
  workspace,
  pkgs,
  firmware ? null,
  displayCoreOnly ? false,
}: let
  mimalloc = import ../../../platform/runtime/mimalloc {inherit pkgs;};
in
  (workspace.override {
    target = "sel4";
    crateOverrides = {
      memchr = old: {features = ["alloc"];};
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
      horn-nvidia-task = old:
        {
          NVIDIA_DISPLAY_CORE_ONLY =
            if displayCoreOnly
            then "1"
            else "0";
        }
        // pkgs.lib.optionalAttrs (firmware != null) {NVIDIA_FIRMWARE_DIR = "${firmware}/nvidia/ad106/gsp";};
    };
  }).members.horn-nvidia-task
