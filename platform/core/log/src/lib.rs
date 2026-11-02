//! Synchronous, bounded whole-record logs. No allocation in formatting/rendering.
#![no_std]
use core::fmt::{self, Write};
use horn_ipc::List;
pub use log::{debug, error, info, trace, warn};
use owo_colors::OwoColorize;

#[allow(dead_code, unused_imports, non_camel_case_types)]
pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
use protocol::{LogError, LogLevel, LogRecord};
const _: () = {
    assert!(<protocol::LogsinkWriteArgs as horn_ipc::Wire>::SIZE <= horn_ipc::MAX_MESSAGE_BYTES);
    assert!(<protocol::LogsinkWriteArgs as horn_ipc::Wire>::CAPS == 0);
};

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "client")]
pub use client::{emergency, emit, install, kernel_debug_sink};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TargetTooLong,
    MessageTooLong,
    Formatting,
    Rpc(horn_ipc::CallError<LogError>),
    Unavailable,
    Reentrant,
    CriticalContext,
}
/// Default facade filter; explicit emit() remains an unfiltered Result API.
pub fn enabled(level: log::Level, target: &str) -> bool {
    let rust_sel4 = target == "sel4" || target.starts_with("sel4::") || target.starts_with("sel4_");
    level
        <= if target == "virtio_drivers" || target.starts_with("virtio_drivers::") {
            log::Level::Warn
        } else if rust_sel4 {
            log::Level::Info
        } else {
            log::Level::Debug
        }
}

struct Buffer<'a, const N: usize>(&'a mut List<u8, N>);
impl<const N: usize> Write for Buffer<'_, N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let start = self.0.len as usize;
        let end = start.checked_add(text.len()).ok_or(fmt::Error)?;
        let out = self.0.data.get_mut(start..end).ok_or(fmt::Error)?;
        out.copy_from_slice(text.as_bytes());
        self.0.len = end as u32;
        Ok(())
    }
}
pub fn record(
    level: log::Level,
    target: &str,
    args: fmt::Arguments<'_>,
) -> Result<LogRecord, Error> {
    let mut record = LogRecord::default();
    record.level = match level {
        log::Level::Error => LogLevel::Error,
        log::Level::Warn => LogLevel::Warn,
        log::Level::Info => LogLevel::Info,
        log::Level::Debug => LogLevel::Debug,
        log::Level::Trace => LogLevel::Trace,
    };
    Buffer(&mut record.target)
        .write_str(target)
        .map_err(|_| Error::TargetTooLong)?;
    // Distinguish buffer overflow from a user Display implementation's error.
    let mut overflow = false;
    struct Message<'a> {
        record: &'a mut LogRecord,
        overflow: &'a mut bool,
    }
    impl Write for Message<'_> {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            if Buffer(&mut self.record.message).write_str(text).is_err() {
                *self.overflow = true;
                return Err(fmt::Error);
            }
            Ok(())
        }
    }
    fmt::write(
        &mut Message {
            record: &mut record,
            overflow: &mut overflow,
        },
        args,
    )
    .map_err(|_| {
        if overflow {
            Error::MessageTooLong
        } else {
            Error::Formatting
        }
    })?;
    Ok(record)
}
struct Escaped<'a>(&'a str);
impl fmt::Display for Escaped<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            match c {
                '\n' => out.write_str("\n       │ ")?,
                '\t' => out.write_str("    ")?,
                c if c.is_control() => write!(out, "{}", c.escape_default())?,
                c => out.write_char(c)?,
            }
        }
        Ok(())
    }
}
/// Always colored. UTF-8/control bytes validated before emitting any output.
/// Caller must serialize the entire call and reply only after it returns.
pub fn render(out: &mut impl Write, process: &str, record: &LogRecord) -> Result<(), LogError> {
    let target = record
        .target
        .data
        .get(..record.target.len as usize)
        .ok_or(LogError::Invalid)?;
    let message = record
        .message
        .data
        .get(..record.message.len as usize)
        .ok_or(LogError::Invalid)?;
    let target = core::str::from_utf8(target).map_err(|_| LogError::Invalid)?;
    let message = core::str::from_utf8(message).map_err(|_| LogError::Invalid)?;
    let result = (|| {
        match record.level {
            LogLevel::Error => write!(out, "{} ", "ERROR".red().bold())?,
            LogLevel::Warn => write!(out, "{} ", " WARN".yellow().bold())?,
            LogLevel::Info => write!(out, "{} ", " INFO".green().bold())?,
            LogLevel::Debug => write!(out, "{} ", "DEBUG".blue().bold())?,
            LogLevel::Trace => write!(out, "{} ", "TRACE".magenta().bold())?,
        }
        writeln!(
            out,
            "{} {} {}",
            Escaped(process).cyan().bold(),
            Escaped(target).dimmed(),
            Escaped(message)
        )
    })();
    result.map_err(|_| LogError::Output)
}

#[cfg(test)]
mod tests;
