{lib, ...}: {
  imports = [../sel4/profiles/debug.nix];
  nixpkgs.hostPlatform = lib.systems.examples.riscv64;
  sel4.platform = "qemu-riscv64";
}
