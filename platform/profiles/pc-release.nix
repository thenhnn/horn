{lib, ...}: {
  imports = [../sel4/profiles/release.nix];
  nixpkgs.hostPlatform = lib.systems.examples.gnu64;
  sel4.platform = "pc";
}
