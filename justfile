default:
    @just --list

fmt:
    treefmt

build-sel4 profile="default":
    nix-build nix -A sel4.out --argstr profile '{{ profile }}' --out-link work/sel4

rust-generate:
    python3 tools/rust-generate.py

npins-bump:
    npins -d nix/npins update
