{
  workspace,
  pkgs,
  probe ? "none",
  driverTest ? "none",
}: let
  mimalloc = import ../../../runtime/mimalloc {inherit pkgs;};
in
  (workspace.override {
    target = "sel4";
    crateOverrides = {
      horn-virtio-console = old: {
        features = (old.features or []) ++ pkgs.lib.optional (probe != "none") "dma-probe" ++ pkgs.lib.optional (builtins.elem driverTest ["ignore" "fault" "exit" "orderly" "force"]) "lifecycle-test";
        HORN_DRIVER_TEST = driverTest;
        HORN_DMA_PROBE = probe;
      };
      memchr = old: {features = ["alloc"];};
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
    };
  }).members.horn-virtio-console
