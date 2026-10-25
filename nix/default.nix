{
  profile ? "debug",
  platform ? "pc",
}: let
  sources = import ./npins;
  pkgs = import sources.nixpkgs {};
  target = (import ./sel4/platforms {inherit (pkgs) lib;}).${platform};
  targetPkgs = import sources.nixpkgs {crossSystem = target.crossSystem;};
in {
  sel4 = import ./sel4 {
    pkgs = targetPkgs;
    platform = target;
    inherit profile;
  };
  shell = import ./shell.nix {inherit pkgs;};
}
