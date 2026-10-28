{
  workspace,
  pkgs,
  probe ? "none",
}: let
  mimalloc = import ../../../runtime/mimalloc {inherit pkgs;};
  protocol = old: {
    HORN_IDL = "${../../../../tools/idl}";
    HORN_VM_IDL = "${../../../core/vm/idl}";
    nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.python3];
  };
in
  (workspace.override {
    target = "sel4";
    crateOverrides = {
      horn-virtio-console = old: {
        features = (old.features or []) ++ pkgs.lib.optional (probe != "none") "dma-probe";
        HORN_DMA_PROBE = probe;
      };
      horn-devmgr = protocol;
      horn-vm = protocol;
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
    };
  }).members.horn-virtio-console
