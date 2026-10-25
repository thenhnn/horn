{lib, ...}: {
  sel4.settings = {
    KernelDebugBuild = lib.mkDefault false;
    KernelPrinting = lib.mkDefault false;
  };
}
