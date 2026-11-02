extern crate std;
use super::*;
use std::string::String;
#[test]
fn defaults_keep_application_debug_but_not_rust_sel4_debug_or_trace() {
    for target in ["timer", "horn_clock::alarm", "app"] {
        assert!(enabled(log::Level::Debug, target));
        assert!(!enabled(log::Level::Trace, target));
    }
    for target in [
        "sel4",
        "sel4::state",
        "sel4_sys::invocations",
        "sel4_runtime_common",
    ] {
        assert!(enabled(log::Level::Info, target));
        assert!(enabled(log::Level::Warn, target));
        assert!(enabled(log::Level::Error, target));
        assert!(!enabled(log::Level::Debug, target));
        assert!(!enabled(log::Level::Trace, target));
    }
}

#[test]
fn formatting_and_colored_rendering_are_whole_record_operations() {
    let record = record(log::Level::Info, "timer", format_args!("ready: {}", 42)).unwrap();
    let mut text = String::new();
    render(&mut text, "timerd", &record).unwrap();
    assert!(text.contains("\x1b["));
    assert!(text.contains("timerd"));
    assert!(text.contains("timer"));
    assert!(text.contains("ready: 42"));
    assert!(text.ends_with('\n'));
}
#[test]
fn overflow_is_an_error_not_a_successfully_truncated_record() {
    let text = "x".repeat(769);
    assert!(matches!(
        record(log::Level::Trace, "timer", format_args!("{text}")),
        Err(Error::MessageTooLong)
    ));
    let target = "x".repeat(97);
    assert!(matches!(
        record(log::Level::Info, &target, format_args!("x")),
        Err(Error::TargetTooLong)
    ));
}
#[test]
fn invalid_utf8_is_rejected_before_output_and_control_bytes_are_escaped() {
    let mut record = record(
        log::Level::Warn,
        "timer",
        format_args!("escape:\x1b[31m\nnext"),
    )
    .unwrap();
    let mut text = String::new();
    render(&mut text, "timerd", &record).unwrap();
    assert!(text.contains("\\u{1b}[31m"));
    assert!(text.contains("\n       │ next"));
    text.clear();
    record.message.data[0] = 0xff;
    assert!(matches!(
        render(&mut text, "timerd", &record),
        Err(LogError::Invalid)
    ));
    assert!(text.is_empty());
}
#[test]
fn output_failure_is_not_acknowledged_as_success() {
    struct Failed;
    impl Write for Failed {
        fn write_str(&mut self, _: &str) -> fmt::Result {
            Err(fmt::Error)
        }
    }
    let record = record(log::Level::Error, "timer", format_args!("failure")).unwrap();
    assert!(matches!(
        render(&mut Failed, "timerd", &record),
        Err(LogError::Output)
    ));
    assert!(<protocol::LogsinkWriteArgs as horn_ipc::Wire>::SIZE <= horn_ipc::MAX_MESSAGE_BYTES);
    assert_eq!(<protocol::LogsinkWriteArgs as horn_ipc::Wire>::CAPS, 0);
}
