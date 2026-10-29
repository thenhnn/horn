# Exercise the alternative entry scratch path without changing normal profiles.
{lib, ...}: {
  sel4.settings.KernelSkimWindow = lib.mkForce false;
}
