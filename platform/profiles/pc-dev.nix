{lib, ...}: {
  imports = [../sel4/profiles/debug.nix];
  nixpkgs.hostPlatform = lib.systems.examples.gnu64;
  sel4.platform = "pc";
}
