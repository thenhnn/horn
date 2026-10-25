{lib, ...}: {
  imports = [../sel4/profiles/debug.nix];
  nixpkgs.hostPlatform = lib.systems.examples.aarch64-multiplatform;
  sel4.platform = "qemu-aarch64";
}
