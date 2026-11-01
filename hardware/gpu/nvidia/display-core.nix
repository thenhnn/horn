# Opt-in reference experiment; no host GPU state changes.
{
  imports = [./module.nix];
  nvidia.display-core-only = true;
}
