# Compile the UP entry path, not merely boot an SMP kernel with one CPU.
{lib, ...}: {
  sel4.settings.KernelMaxNumNodes = lib.mkForce 1;
}
