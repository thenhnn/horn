# WHENCE: ad106 -> ad102; ad102/gsp/gsp.bin -> ga102/gsp/gsp.bin.
# Exact LFS objects, not GitHub tarballs containing tiny LFS pointer files.
{
  pkgs,
  sources,
  workspace,
}: let
  checker = (workspace.override {inherit pkgs;}).members.horn-nvidia-gsp.override {features = ["host"];};
in
  pkgs.runCommand "horn-nvidia-ad106-firmware-570.144" {
    nativeBuildInputs = [checker];
  } ''
    dir="$out/nvidia/ad106/gsp"
    mkdir -p "$dir" "$out/share/licenses/nvidia"
    cp ${sources.nvidia-r570144-gsp} "$dir/gsp.bin"
    cp ${sources.nvidia-r570144-ad106-meta} "$dir/gsp.tlv"
    cp ${sources.nvidia-r570144-ad106-bootloader} "$dir/gsp_bootloader.tlv"
    cp ${sources.nvidia-r570144-ad106-load} "$dir/booter_load.tlv"
    cp ${sources.nvidia-r570144-ad106-unload} "$dir/booter_unload.tlv"
    cp ${sources.nvidia-r570144-license} "$out/share/licenses/nvidia/LICENCE.nvidia"
    cp ${sources.nvidia-r570144-whence} "$out/share/licenses/nvidia/WHENCE"
    nvidia-fwcheck "$dir"
  ''
