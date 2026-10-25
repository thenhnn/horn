{lib, ...}: {
  imports = [../sel4/profiles/release.nix];
  nixpkgs.hostPlatform = lib.systems.examples.aarch64-multiplatform;
  rust.release = lib.mkDefault true;
  sel4.platform = "qemu-aarch64";
}
