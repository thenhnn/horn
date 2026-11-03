//! Compile-only ownership/lifetime fixtures, using examples/idl/tagged.py.
#![no_std]
pub mod protocol;
use horn_ipc::*;
use protocol::*;

// The receiver borrows only the owner, not io; views may coexist.
pub fn use_views<T: Transport>(owner: OwnedCap<Endpoint>, io: &mut T) {
    let first = owner.interface::<Probe>();
    let second = owner.interface::<Probe>();
    let _ = first.check(io);
    let _ = second.check(io);
    drop(owner);
}

// Capability-bearing replies arrive owning, including application-error caps.
// No adoption, unsafe block, or raw handoff is needed in the consumer.
pub fn receive<T: Transport>(
    receiver: Probe,
    io: &mut T,
) -> Result<OwnedCap<Endpoint>, CallError<OwnedCap<Notification>>> {
    receiver.handoff(io)
}

// The consumed owner is dropped for both Ok and Err; no remote close in Drop.
pub fn finish<T: Transport>(
    owner: OwnedCap<Endpoint>,
    io: &mut T,
) -> Result<(), CallError<EventOwned>> {
    owner.interface::<Probe>().check(io)
}

#[cfg(feature = "reject_outlive")]
pub fn reject_outlive(owner: OwnedCap<Endpoint>) {
    let view = owner.interface::<Probe>();
    drop(owner);
    let _ = view;
    // A use, not merely a discarded Copy value, keeps the borrow live.
    use_view(view);
}
#[cfg(feature = "reject_outlive")]
fn use_view(_: BorrowedCap<'_, ProbeTag>) {}

#[cfg(feature = "reject_copy")]
pub fn reject_copy(owner: OwnedCap<Endpoint>) {
    let moved = owner;
    drop(owner);
    drop(moved);
}

#[cfg(feature = "reject_send")]
pub fn reject_send(owner: OwnedCap<Endpoint>) {
    fn require_send<T: Send>(_: T) {}
    require_send(owner);
}

#[cfg(feature = "reject_borrowed_send")]
pub fn reject_borrowed_send(owner: OwnedCap<Endpoint>) {
    fn require_send<T: Send>(_: T) {}
    require_send(owner.interface::<Probe>());
}
