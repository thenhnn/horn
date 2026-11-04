{
  pkgs,
  rust,
}:
pkgs.mkShellNoCC ((import ../hardware/gpu/nvidia/bindings.nix {inherit pkgs;})
  // {
    packages = [
      rust
      pkgs.crate2nix
      pkgs.npins
      (pkgs.python3.withPackages (p: [p.pytest]))
      pkgs.qemu
      pkgs.just
      (import ./fmt.nix {inherit pkgs;})
    ];
  })
