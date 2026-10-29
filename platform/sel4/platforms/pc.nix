{
  KernelPlatform = "pc99";
  KernelMaxNumNodes = 64;
  KernelX86MicroArch = "generic";
  KernelVTX = false;
  KernelIOMMU = false; # we handle IOMMU/interrupt remapping in userspace.
  KernelSupportPCID = false;
  KernelFPU = "XSAVE";
  KernelXSave = "XSAVE";
  KernelXSaveFeatureSet = 7; # x87/SSE/AVX: OS state required for x86-64-v3.
  KernelXSaveOptionalFeatureSet = 224; # AVX-512 state, as one optional group.
  KernelXSaveSize = 4096; # bounded capacity; active layout comes from CPUID.
}
