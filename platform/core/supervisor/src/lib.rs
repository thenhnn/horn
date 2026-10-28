//! Generic supervision state machine. No device policy or synchronous cleanup callback.
#![no_std]
pub const CORE_READY: u64 = 1 << 59;
pub const CHILD_EXIT: u64 = 1 << 60;
pub const OWNER_CHANGED: u64 = 1 << 62;
pub const STOP: u64 = 2;
pub const IRQ: u64 = 1;
pub const FACTORY_BADGE: u64 = 64;
pub const MAX_JOBS: usize = 8;
#[allow(dead_code, unused_imports, non_camel_case_types)]
pub mod protocol { include!(concat!(env!("OUT_DIR"), "/protocol.rs")); }
use protocol::*;
#[cfg(feature="client")]
pub mod client;
/// No transition can reclaim resources until CPU stop and explicit owner ACK.
pub struct Lifecycle { pub state: ServiceStatus, pending: Option<StopMode> }
impl Lifecycle {
    pub fn new() -> Self { Self {state:ServiceStatus::Created,pending:None} }
    pub fn start(&mut self)->Result<(),SupervisorError> {
        if self.state!=ServiceStatus::Created {return Err(SupervisorError::Busy);}
        self.state=ServiceStatus::Starting;Ok(())
    }
    pub fn ready(&mut self)->bool {
        if self.state!=ServiceStatus::Starting {return false;}
        self.state=ServiceStatus::Active;true
    }
    pub fn request_stop(&mut self, mode:StopMode)->Result<(),SupervisorError> {
        if !matches!(self.state,ServiceStatus::Starting|ServiceStatus::Active) || self.pending.is_some() {return Err(SupervisorError::Busy);}
        self.pending=Some(mode);Ok(())
    }
    pub fn take_stop(&mut self)->Option<StopMode> {self.pending.take()}
    pub fn stopping(&mut self) {self.state=ServiceStatus::Stopping;}
    pub fn cpu_stopped(&mut self) { self.state=ServiceStatus::AwaitingCleanup;self.pending=None; }
    pub fn can_release(&self)->bool {self.state==ServiceStatus::AwaitingCleanup}
    pub fn released(&mut self) {assert!(self.can_release());self.state=ServiceStatus::Exited;}
}
impl Default for Lifecycle {fn default()->Self{Self::new()}}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn readiness_is_explicit_and_cannot_revive_dead_instance() {
        let mut s=Lifecycle::new();assert!(!s.ready());s.start().unwrap();assert!(s.ready());assert!(!s.ready());
        s.cpu_stopped();assert!(!s.ready());s.released();assert!(!s.ready());assert!(s.start().is_err());
    }
    #[test] fn cleanup_ack_requires_stopped_execution() {
        let mut s=Lifecycle::new();assert!(!s.can_release());s.start().unwrap();assert!(!s.can_release());
        s.ready();assert!(!s.can_release());s.stopping();assert!(!s.can_release());s.cpu_stopped();assert!(s.can_release());
        s.released();assert!(!s.can_release());
    }
    #[test] fn one_stop_intent_and_no_restart() {
        let mut s=Lifecycle::new();assert!(s.request_stop(StopMode::Force).is_err());s.start().unwrap();
        s.request_stop(StopMode::Graceful).unwrap();assert!(s.request_stop(StopMode::Force).is_err());
        assert_eq!(s.take_stop(),Some(StopMode::Graceful));s.cpu_stopped();assert!(s.request_stop(StopMode::Force).is_err());
    }
}
