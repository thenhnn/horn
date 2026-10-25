{lib, ...}: {
  sel4.settings = {
    KernelVerificationBuild = lib.mkDefault false;
    KernelOptimisation = lib.mkDefault "-O2";
    KernelIsMCS = lib.mkDefault true;
    KernelRootCNodeSizeBits = lib.mkDefault 14;
  };
}
