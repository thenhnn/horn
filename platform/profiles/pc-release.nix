{lib, ...}: {
  imports = [../sel4/profiles/release.nix];
  nixpkgs.hostPlatform = lib.systems.examples.gnu64;
  rust.release = lib.mkDefault true;
  sel4.platform = "pc";
}
