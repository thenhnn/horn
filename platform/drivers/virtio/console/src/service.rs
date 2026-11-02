//! Console RPC state, independent of virtio and never synchronously calling clients.
use crate::hal;
use horn_console::{Console, protocol as c};
use horn_ipc::*;
pub struct Service {
    pub console: Console,
    note: Option<Cap<Notification>>,
}
impl Service {
    pub fn new() -> Self {
        Self {
            console: Console::new(),
            note: None,
        }
    }
    pub fn notify(&mut self) {
        if self.console.take_wake() {
            if let Some(note) = self.note {
                hal::with(|s| {
                    s.session
                        .transport()
                        .space
                        .cap::<sel4::cap_type::Notification>(note.as_ref())
                        .unwrap()
                        .signal()
                });
            }
        }
    }
}
impl c::ConsoleInterface for Service {
    fn read(&mut self, maximum: u32) -> Result<List<u8, 256>, c::ConsoleError> {
        self.console.read(maximum)
    }
    fn write(&mut self, data: List<u8, 256>) -> Result<u32, c::ConsoleError> {
        let length = data.len as usize;
        if length > data.data.len() {
            return Err(c::ConsoleError::Invalid);
        }
        self.console.write(&data.data[..length])
    }
    fn size(&mut self) -> Result<c::ConsoleGeometry, c::ConsoleError> {
        Ok(self.console.size())
    }
    fn status(&mut self) -> Result<c::ConsoleStatus, c::ConsoleError> {
        Ok(self.console.status())
    }
    fn watch(
        &mut self,
        notification: Option<Cap<Notification>>,
        interests: c::ConsoleEvents,
        generation: u64,
    ) -> Result<c::ConsoleStatus, c::ConsoleError> {
        if interests.0 & !15 != 0 || generation > self.console.status().size_generation {
            return Err(c::ConsoleError::Invalid);
        }
        let next = if let Some(note) = notification {
            if note.as_ref().unwrapped_badge().is_some() {
                return Err(c::ConsoleError::Denied);
            }
            Some(hal::with(|s| {
                let space = &mut s.session.transport().space;
                let source = space
                    .cap::<sel4::cap_type::Notification>(note.as_ref())
                    .map_err(|_| c::ConsoleError::Denied)?;
                let slot = space.reserve_slot().map_err(|_| c::ConsoleError::Busy)?;
                let root = space.root;
                if slot
                    .copy(
                        &root.absolute_cptr(sel4::CPtr::from_bits(source.bits())),
                        sel4::CapRights::new(false, false, false, true),
                    )
                    .is_err()
                {
                    let empty = space
                        .register(sel4::cap::Unspecified::from_bits(slot.path().bits()))
                        .unwrap();
                    space.delete(empty).unwrap();
                    return Err(c::ConsoleError::Denied);
                }
                Ok(Cap::from_ref(
                    space
                        .register(sel4::cap::Unspecified::from_bits(slot.path().bits()))
                        .unwrap(),
                ))
            })?)
        } else {
            None
        };
        if let Some(old) = self.note.take() {
            hal::with(|s| s.session.transport().space.delete(old.as_ref()).unwrap());
        }
        self.note = next;
        self.console.arm(
            if next.is_some() {
                interests
            } else {
                c::ConsoleEvents(0)
            },
            generation,
        )
    }
}
