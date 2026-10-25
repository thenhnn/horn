{lib, ...}: {
  imports = [../sel4/profiles/debug.nix];
  nixpkgs.hostPlatform = lib.systems.examples.gnu64;
  rust.release = lib.mkDefault false;
  sel4.platform = "pc";
}
