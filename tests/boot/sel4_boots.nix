{
  config,
  pkgs,
  ...
}: {
  root-task.package = pkgs.callPackage ./stub-root-task {
    sel4 = config.sel4.out;
  };
}
