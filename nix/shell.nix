{
  pkgs,
  rust,
}:
pkgs.mkShellNoCC {
  packages = [
    rust
    pkgs.crate2nix
    pkgs.npins
    (pkgs.python3.withPackages (p: [p.pytest]))
    pkgs.qemu
    pkgs.just
    (import ./fmt.nix {inherit pkgs;})
  ];
}
