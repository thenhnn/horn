use super::*;
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};
use horn_ipc::*;
use protocol::{LogSink, LogSinkClient, LogSinkTag};

struct Client {
    io: Sel4Transport<Sel4CapSpace>,
    sink: Cap<LogSinkTag>,
}
struct Logger {
    client: UnsafeCell<Option<Client>>,
    busy: AtomicBool,
}
// All cell accesses, including installation, are serialized by busy.
// The supported runtime is single-threaded: contention indicates reentrancy.
unsafe impl Sync for Logger {}
static LOGGER: Logger = Logger {
    client: UnsafeCell::new(None),
    busy: AtomicBool::new(false),
};
struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        LOGGER.busy.store(false, Ordering::Release);
    }
}
fn lock() -> Result<Guard, Error> {
    LOGGER
        .busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .map_err(|_| Error::Reentrant)?;
    Ok(Guard)
}
/// Install once, after IPC/TLS setup. Dedicated transport prevents application
/// receive/import state from being overwritten. Never install in the sink itself.
/// # Safety
/// Call from the sole task thread before application code. This transport's
/// slots must be exclusively reserved for the process lifetime.
pub unsafe fn install(io: Sel4Transport<Sel4CapSpace>, sink: Cap<LogSinkTag>) -> Result<(), Error> {
    let _guard = lock()?;
    let client = unsafe { &mut *LOGGER.client.get() };
    if client.is_some() {
        return Err(Error::Unavailable);
    }
    // Publish the facade only once its backing state is initialized.
    *client = Some(Client { io, sink });
    if log::set_logger(&LOGGER).is_err() {
        *client = None;
        return Err(Error::Unavailable);
    }
    log::set_max_level(log::LevelFilter::Debug);
    Ok(())
}
/// One blocking Call; no retries, ring, queue or silent truncation.
pub fn emit(level: log::Level, target: &str, args: fmt::Arguments<'_>) -> Result<(), Error> {
    if sel4::try_with_ipc_buffer_slot_mut(|buffer| buffer.is_err()) {
        return Err(Error::CriticalContext);
    }
    let _guard = lock()?;
    let client = unsafe { &mut *LOGGER.client.get() }
        .as_mut()
        .ok_or(Error::Unavailable)?;
    let record = record(level, target, args)?;
    LogSink::from_ref(client.sink.as_ref())
        .write(&mut client.io, record)
        .map_err(Error::Rpc)
}
/// Direct output for bootstrap, panic, allocator/pager critical paths and sink
/// failures. Never performs RPC or allocates. Not serialized against other
/// processes; this is deliberately outside the normal logging contract.
pub fn emergency(args: fmt::Arguments<'_>) {
    let _ = writeln!(sel4::DebugWrite, "{} {args}", "EMERGENCY".red().bold());
}

/// Backend for logd itself, never a call through the installed facade.
pub fn kernel_debug_sink(process: &str, record: &LogRecord) -> Result<(), LogError> {
    render(&mut sel4::DebugWrite, process, record)
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        super::enabled(metadata.level(), metadata.target())
    }
    fn log(&self, record: &log::Record<'_>) {
        // log! does not necessarily call Log::enabled before Log::log.
        if !self.enabled(record.metadata()) {
            return;
        }
        if let Err(error) = emit(record.level(), record.target(), *record.args()) {
            // Facade cannot return errors. Make failed logging explicit using
            // the allocation-free emergency sink; do not recurse into log.
            if error == Error::CriticalContext {
                // rust-sel4 invocation tracing can run while its IPC buffer is
                // borrowed. RPC there would recursively borrow it and panic.
                match super::record(record.level(), record.target(), *record.args()) {
                    Ok(record) => {
                        let _ = kernel_debug_sink("<emergency>", &record);
                    }
                    Err(error) => emergency(format_args!("logging failed: {:?}", error)),
                }
            } else {
                emergency(format_args!("logging failed: {:?}", error));
            }
        }
    }
    // Each successful call already waited for output; no queued data to flush.
    fn flush(&self) {}
}
