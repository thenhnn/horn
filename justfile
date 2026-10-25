default:
    @just --list

fmt:
    treefmt

npins-bump:
    npins -d nix/npins update
