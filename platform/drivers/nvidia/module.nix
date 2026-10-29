# Opt-in Q35/VFIO probe. No host detach, reset or binding operation is performed.
{
  config,
  lib,
  pkgs,
  ...
}: let
  driver = import ./. {
    workspace = config.rust.workspace;
    pkgs = pkgs.pkgsBuildBuild;
  };
  elf = pkgs.pkgsBuildBuild.runCommand "nvidia-probe-runtime.elf" {
    nativeBuildInputs = [pkgs.pkgsBuildBuild.llvmPackages.llvm];
  } ''llvm-objcopy --strip-debug ${driver}/bin/nvidia_probe.elf "$out"'';
in {
  imports = [../../../examples/devmgr/module.nix];
  sel4.settings.KernelRootCNodeSizeBits = 18;
  devmgr-demo.console = lib.mkDefault false;
  devmgr-demo.net = lib.mkDefault false;
  devmgr-demo.shell = lib.mkDefault false;
  services.definitions.devmgr.publish = ["/diagnostics/nvidia"];
  devmgr.drivers = [
    {
      name = "nvidia-probe";
      inherit elf;
      ram = 16 * 1024 * 1024;
      # Explicit future GSP budget, not an allocation or a larger virtio default.
      dmaBytes = 64 * 1024 * 1024;
      dmaObjects = 64;
      pci = {
        vendor = 4318; # 10de
        device = 10243; # 2803
        class = 196608;
        classMask = 16711680;
      };
      publish = {
        namespace = "/diagnostics/nvidia";
        badge = 16;
        rights = 10;
      };
    }
  ];
}
