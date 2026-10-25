{pkgs}:
pkgs.mkShellNoCC {
  packages = [
    pkgs.npins
    pkgs.python3
    pkgs.just
    (import ./fmt.nix {inherit pkgs;})
  ];
}
