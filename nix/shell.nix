{pkgs}:
pkgs.mkShellNoCC {
  packages = [
    pkgs.npins
    (pkgs.python3.withPackages (p: [p.pytest]))
    pkgs.qemu
    pkgs.just
    (import ./fmt.nix {inherit pkgs;})
  ];
}
