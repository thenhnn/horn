{
  workspace,
  pkgs,
  probe ? "none",
  driverTest ? "none",
}: let
  mimalloc = import ../../../runtime/mimalloc {inherit pkgs;};
  protocol = old: {
    HORN_IDL = "${../../../../tools/idl}";
    HORN_CLOCK_IDL = "${../../../core/clock/idl}";
    HORN_VM_IDL = "${../../../core/vm/idl}";
    HORN_PROCESS_IDL = "${../../../core/procd/idl}";
    nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.python3];
  };
in
  (workspace.override {
    target = "sel4";
    crateOverrides = {
      horn-virtio-console = old: {
        features = (old.features or []) ++ pkgs.lib.optional (probe != "none") "dma-probe" ++ pkgs.lib.optional (builtins.elem driverTest ["ignore" "fault" "exit" "orderly" "force"]) "lifecycle-test";
        HORN_DRIVER_TEST = driverTest;
        HORN_DMA_PROBE = probe;
      };
      horn-devmgr = protocol;
      horn-console = protocol;
      horn-procd = protocol;
      memchr = old: {features = ["alloc"];};
      horn-supervisor = protocol;
      horn-vm = protocol;
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
    };
  }).members.horn-virtio-console
