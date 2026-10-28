//! Tiny native console shell. No POSIX, login, process-launch or device control.
#![no_std]
#![no_main]
use core::fmt::{self, Write};
use horn_console::protocol as c;
use horn_devmgr::protocol as d;
use horn_ipc::*;
use horn_procd::protocol as p;
use horn_shell::protocol as a;
const SLOTS: TaskSlots = TaskSlots::new(a::SHELL_CAPS_FIRST_FREE_SLOT, a::SHELL_CAPS_SIZE_BITS);
horn_runtime::task!(main,vm:horn_runtime::VmBootstrap{
    cnode:a::SHELL_CAPS_SELF_SLOT,factory:a::SHELL_CAPS_VM_SLOT,
    address_space:a::SHELL_CAPS_ADDRESS_SPACE_SLOT,slots:SLOTS,
});
fn cap<T: CapType>(slot: u32) -> Cap<T> {
    horn_runtime::bootstrap_cap(
        sel4::cap::CNode::from_bits(a::SHELL_CAPS_SELF_SLOT as _),
        slot,
    )
}
struct Shell {
    io: Sel4Transport<Sel4CapSpace>,
    output: [u8; 256],
    output_len: usize,
}
impl Shell {
    fn wait(&mut self, events: c::ConsoleEvents) {
        let mut client =
            c::ConsoleInterfaceClient::new(&mut self.io, cap(a::SHELL_CAPS_SERVICE_SLOT));
        let status = client.status().expect("console status");
        assert_eq!(
            status.events.0 & c::ConsoleEvents::CLOSED.0,
            0,
            "console closed"
        );
        client
            .watch(
                Some(cap(a::SHELL_CAPS_SIGNAL_SLOT)),
                events,
                status.size_generation,
            )
            .expect("console watch");
        sel4::cap::Notification::from_bits(a::SHELL_CAPS_EVENTS_SLOT as _).wait();
    }
    fn bytes(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let count = data.len().min(self.output.len() - self.output_len);
            self.output[self.output_len..self.output_len + count].copy_from_slice(&data[..count]);
            self.output_len += count;
            data = &data[count..];
            if self.output_len == self.output.len() {
                self.flush();
            }
        }
    }
    fn prompt(&mut self) {
        self.bytes(b"$ ");
        self.flush();
    }
    fn flush(&mut self) {
        let mut offset = 0;
        while offset < self.output_len {
            let count = self.output_len - offset;
            let mut bytes = List::default();
            bytes.len = count as _;
            bytes.data[..count].copy_from_slice(&self.output[offset..self.output_len]);
            match c::ConsoleInterfaceClient::new(&mut self.io, cap(a::SHELL_CAPS_SERVICE_SLOT))
                .write(bytes)
            {
                Ok(n) => {
                    assert!(n > 0 && n as usize <= count);
                    offset += n as usize;
                }
                Err(CallError::Remote(c::ConsoleError::WouldBlock)) => {
                    self.wait(c::ConsoleEvents::WRITABLE)
                }
                error => panic!("console write: {:?}", error),
            }
        }
        self.output_len = 0;
    }
    fn read(&mut self) -> List<u8, 256> {
        // Also flush input echo before blocking. Writable waits in flush() must
        // not recursively flush the same pending buffer.
        self.flush();
        loop {
            match c::ConsoleInterfaceClient::new(&mut self.io, cap(a::SHELL_CAPS_SERVICE_SLOT))
                .read(256)
            {
                Ok(bytes) => return bytes,
                Err(CallError::Remote(c::ConsoleError::WouldBlock)) => {
                    self.wait(c::ConsoleEvents::READABLE)
                }
                error => panic!("console read: {:?}", error),
            }
        }
    }
    fn command(&mut self, line: &[u8]) {
        let command = core::str::from_utf8(line).unwrap().trim();
        match command {
            ""=>{},
            "help"=>self.bytes(b"help  show commands\r\npci   list PCIe devices\r\nps    list supervised processes\r\n"),
            "pci"=>self.pci(),
            "ps"=>self.processes(),
            _=>self.bytes(b"unknown command; try help\r\n"),
        }
    }
    fn pci(&mut self) {
        self.bytes(b"BDF          VENDOR:DEVICE CLASS\r\n");
        let mut cursor = 0;
        loop {
            let page = match d::DeviceObserverClient::new(&mut self.io, cap(a::SHELL_CAPS_PCI_SLOT))
                .list(cursor)
            {
                Ok(page) => page,
                Err(error) => {
                    writeln!(self, "PCI observer: {:?}\r", error).unwrap();
                    return;
                }
            };
            for device in &page.entries.data[..page.entries.len as usize] {
                let bdf = device.address;
                write!(
                    self,
                    "{:04x}:{:02x}:{:02x}.{:x} {:04x}:{:04x} {:02x}:{:02x}:{:02x}\r\n",
                    bdf.segment,
                    bdf.bus,
                    bdf.device,
                    bdf.function,
                    device.vendor_id,
                    device.device_id,
                    device.class_code,
                    device.subclass,
                    device.programming_interface
                )
                .unwrap();
            }
            if page.next == 0 {
                break;
            }
            assert!(page.next > cursor);
            cursor = page.next;
        }
    }
    fn processes(&mut self) {
        // The observer grant is scoped to the supervisor's children, not a global PID registry.
        self.bytes(b"Processes (supervisor scope)\r\nPID STATE RAM NAME\r\n");
        let mut cursor = 0;
        loop {
            let page = match p::ProcessObserverClient::new(
                &mut self.io,
                cap(a::SHELL_CAPS_PROCESSES_SLOT),
            )
            .list(cursor)
            {
                Ok(page) => page,
                Err(error) => {
                    writeln!(self, "Process observer: {:?}\r", error).unwrap();
                    return;
                }
            };
            for process in &page.entries.data[..page.entries.len as usize] {
                write!(
                    self,
                    "{} {:?} {} ",
                    process.id, process.state, process.ram.charged_bytes
                )
                .unwrap();
                // Names are display data, not trusted terminal escape sequences.
                for &byte in &process.name.data[..process.name.len as usize] {
                    self.bytes(&[if (32..=126).contains(&byte) {
                        byte
                    } else {
                        b'?'
                    }]);
                }
                self.bytes(b"\r\n");
            }
            if page.next == 0 {
                break;
            }
            assert!(page.next > cursor);
            cursor = page.next;
        }
    }
}
impl Write for Shell {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.bytes(text.as_bytes());
        Ok(())
    }
}
fn main() -> ! {
    let root = sel4::cap::CNode::from_bits(a::SHELL_CAPS_SELF_SLOT as _);
    let mut shell = Shell {
        io: horn_runtime::transport(root, SLOTS.application()).unwrap(),
        output: [0; 256],
        output_len: 0,
    };
    shell.bytes(b"\x1b[1;36mHorn v0.1 started\x1b[0m\r\n$ ");
    shell.flush();
    horn_runtime::notify_ready(a::SHELL_CAPS_READY_SLOT);
    let mut line = [0u8; 256];
    let mut length = 0;
    let mut overflow = false;
    let mut previous_cr = false;
    let mut escape = 0;
    loop {
        let packet = shell.read();
        for &byte in &packet.data[..packet.len as usize] {
            if previous_cr && byte == b'\n' {
                previous_cr = false;
                continue;
            }
            previous_cr = false;
            if byte == 3 {
                length = 0;
                overflow = false;
                escape = 0;
                shell.bytes(b"^C\r\n");
                shell.prompt();
                continue;
            }
            if escape == 1 {
                escape = if byte == b'[' { 2 } else { 0 };
                continue;
            }
            if escape == 2 {
                if (0x40..=0x7e).contains(&byte) {
                    escape = 0;
                }
                continue;
            }
            match byte {
                27 => escape = 1,
                b'\r' | b'\n' => {
                    previous_cr = byte == b'\r';
                    shell.bytes(b"\r\n");
                    if overflow {
                        shell.bytes(b"line too long (maximum 256 bytes)\r\n");
                    } else {
                        shell.command(&line[..length]);
                    }
                    length = 0;
                    overflow = false;
                    shell.prompt();
                }
                8 | 127 => {
                    if !overflow && length != 0 {
                        length -= 1;
                        shell.bytes(b"\x08 \x08");
                    }
                }
                21 => {
                    length = 0;
                    overflow = false;
                    shell.bytes(b"\r\x1b[2K$ ");
                }
                b' '..=b'~' | b'\t' => {
                    if length == line.len() {
                        if !overflow {
                            shell.bytes(b"\x07");
                        }
                        overflow = true;
                    } else if !overflow {
                        let byte = if byte == b'\t' { b' ' } else { byte };
                        line[length] = byte;
                        length += 1;
                        shell.bytes(&[byte]);
                    }
                }
                _ => {}
            }
        }
    }
}
