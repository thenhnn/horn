default:
    @just --list

fmt:
    treefmt

build-sel4 profile="debug" platform="pc":
    nix-build nix -A sel4 --argstr profile '{{ profile }}' --argstr platform '{{ platform }}' --out-link work/sel4

npins-bump:
    npins -d nix/npins update
