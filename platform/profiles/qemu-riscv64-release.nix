{lib, ...}: {
  imports = [../sel4/profiles/release.nix];
  nixpkgs.hostPlatform = lib.systems.examples.riscv64;
  sel4.platform = "qemu-riscv64";
}
