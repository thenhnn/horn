{lib, ...}: {
  options.root-task.package = lib.mkOption {
    type = lib.types.package;
    description = "Initial user task, installed as bin/root-task.elf.";
  };
}
