{lib}: {
  crossSystem = lib.systems.examples.gnu64;
  settings = {
    KernelPlatform = "pc99";
    KernelMaxNumNodes = 64;
    KernelX86MicroArch = "generic";
    KernelVTX = false;
    KernelIOMMU = false; # we handle IOMMU/interrupt remapping in userspace.
    KernelSupportPCID = false;
    KernelFPU = "XSAVE";
    KernelXSave = "XSAVE";
    KernelXSaveFeatureSet = 3;
    KernelXSaveSize = 576;
  };
}
