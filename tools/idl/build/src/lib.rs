//! Native build-script glue. Protocol layout and source resolution stay in Python.
use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
};

/// Generate a registered schema into OUT_DIR, tracking its transitive inputs.
/// Nix supplies a store-only source catalog; local Cargo uses the repository one.
pub fn rust(schema: &str, output: &str) {
    for variable in ["HORN_IDL", "HORN_IDL_SOURCES", "PYTHON"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let compiler = env::var_os("HORN_IDL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
    let python = env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let result = Command::new(python)
        .args(["-B", "-m", "horn_idl", schema, "--rust"])
        .arg(PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join(output))
        .arg("--cargo-deps")
        .env("PYTHONPATH", compiler)
        .stderr(Stdio::inherit())
        .output()
        .expect("run Horn IDL compiler (requires Python 3)");
    assert!(
        result.status.success(),
        "Horn IDL compilation failed: {schema}"
    );
    print!(
        "{}",
        String::from_utf8(result.stdout).expect("IDL dependency paths must be UTF-8")
    );
}
