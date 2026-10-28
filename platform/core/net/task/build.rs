fn main() {
    for key in [
        "HORN_TCP_BUFFER_BYTES",
        "HORN_TCP_CONNECTIONS",
        "HORN_TCP_BACKLOG",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
}
