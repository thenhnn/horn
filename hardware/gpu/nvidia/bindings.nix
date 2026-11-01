# Build-host tools and immutable headers shared by Cargo and crate2nix builds.
{pkgs}: {
  NVIDIA_OPEN_GPU_SOURCE = (import ../../../nix/npins).nvidia-open-gpu-r570144;
  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
}
