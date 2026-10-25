{lib, ...}: {
  imports = [../sel4/profiles/debug.nix];
  nixpkgs.hostPlatform = lib.systems.examples.riscv64;
  rust.release = lib.mkDefault false;
  sel4.platform = "qemu-riscv64";
}
