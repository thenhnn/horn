{config, ...}: {
  root-task.package = import ./rust-root-task {
    workspace = config.rust.workspace.override {target = "sel4-roottask";};
  };
}
