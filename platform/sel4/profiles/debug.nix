{lib, ...}: {
  sel4.settings = {
    KernelDebugBuild = lib.mkDefault true;
    KernelPrinting = lib.mkDefault true;
  };
}
