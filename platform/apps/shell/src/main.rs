//! Tiny native console shell. No POSIX, login, process-launch or device control.
#![no_std]
#![no_main]

use core::fmt::{self, Write};
use horn_console::protocol as c;
use horn_devmgr::protocol as d;
use horn_ipc::*;
use horn_procd::protocol as p;
use owo_colors::OwoColorize;

horn_runtime::task!(main);
struct Shell {
    ctx: horn_runtime::Context,
    io: Sel4Transport<Sel4CapSpace>,
    output: [u8; 256],
    output_len: usize,
}
impl Shell {
    fn wait(&mut self, events: c::ConsoleEvents) {
        let client = self
            .ctx
            .interface::<c::ConsoleInterfaceReceiver>("console")
            .unwrap();
        let status = client.status(&mut self.io).expect("console status");
        assert_eq!(
            status.events.0 & c::ConsoleEvents::CLOSED.0,
            0,
            "console closed"
        );
        client
            .watch(
                &mut self.io,
                Some(self.ctx.cap("signal").unwrap()),
                events,
                status.size_generation,
            )
            .expect("console watch");
        self.ctx.wait("events").unwrap();
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
            match self
                .ctx
                .interface::<c::ConsoleInterfaceReceiver>("console")
                .unwrap()
                .write(&mut self.io, bytes)
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
            match self
                .ctx
                .interface::<c::ConsoleInterfaceReceiver>("console")
                .unwrap()
                .read(&mut self.io, 256)
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
            "help"=>self.bytes(b"help  show commands\r\npci   list PCIe devices\r\nps    list supervised processes\r\nregistry  list published endpoints\r\nconsoles  list consoles\r\n"),
            "pci"=>self.pci(),
            "ps"=>self.processes(),
            "registry"=>self.namespace("registry","/"),
            "consoles"=>self.namespace("consoles","/consoles/"),
            _=>self.bytes(b"unknown command; try help\r\n"),
        }
    }
    fn namespace(&mut self, name: &str, prefix: &str) {
        let mut cursor = 0;
        loop {
            let page = match self
                .ctx
                .interface::<horn_registry::protocol::RegistryDirectoryReceiver>(name)
                .unwrap()
                .list(&mut self.io, cursor, true)
            {
                Ok(page) => page,
                Err(error) => {
                    writeln!(self, "Registry: {:?}\r", error).unwrap();
                    return;
                }
            };
            for entry in &page.entries.data[..page.entries.len as usize] {
                let name =
                    core::str::from_utf8(&entry.name.data[..entry.name.len as usize]).unwrap();
                write!(
                    self,
                    "{}{} {}\r\n",
                    prefix,
                    name,
                    if entry.available {
                        "ready"
                    } else {
                        "unavailable"
                    }
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
    fn pci(&mut self) {
        self.bytes(b"BDF          VENDOR:DEVICE CLASS\r\n");
        let mut cursor = 0;
        loop {
            let page = match self
                .ctx
                .interface::<d::DeviceObserverReceiver>("pci")
                .unwrap()
                .list(&mut self.io, cursor)
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
            let page = match self
                .ctx
                .interface::<p::ProcessObserverReceiver>("processes")
                .unwrap()
                .list(&mut self.io, cursor)
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
fn main(mut ctx: horn_runtime::Context) -> ! {
    let mut shell = Shell {
        io: ctx.transport().unwrap(),
        ctx,
        output: [0; 256],
        output_len: 0,
    };
    write!(shell, "{}", "Horn development shell\r\n".blue());
    shell.bytes(b"$ ");
    shell.flush();
    shell.ctx.notify_ready().unwrap();
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
