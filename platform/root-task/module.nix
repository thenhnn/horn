{lib, ...}: {
  options.root-task.package = lib.mkOption {
    type = lib.types.package;
    description = "Initial seL4 task installed in bin/task.elf";
  };
}
