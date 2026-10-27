//! RAM ownership, shared by managed objects and delegated Untyped.
//! Charges are released only after successful kernel cleanup.
use alloc::rc::Rc;
use core::cell::RefCell;

use crate::Error;

#[derive(Clone)]
pub struct Account(Rc<RefCell<State>>);
struct State {
    parent: Option<Account>,
    limit: usize,
    charged: usize,
    quarantined: usize,
    closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub limit: usize,
    pub charged: usize,
    pub quarantined: usize,
}

impl Account {
    pub fn new(limit: usize) -> Self {
        Self(Rc::new(RefCell::new(State {
            parent: None,
            limit,
            charged: 0,
            quarantined: 0,
            closed: false,
        })))
    }
    /// A limit is not a reservation. Child charges also consume the parent budget.
    pub fn child(&self, limit: usize) -> Result<Self, Error> {
        let state = self.0.borrow();
        if state.closed || limit > state.limit {
            return Err(Error::QuotaExceeded);
        }
        let result = Self::new(limit);
        result.0.borrow_mut().parent = Some(self.clone());
        Ok(result)
    }
    pub fn usage(&self) -> Usage {
        let state = self.0.borrow();
        Usage {
            limit: state.limit,
            charged: state.charged,
            quarantined: state.quarantined,
        }
    }
    pub fn set_limit(&self, limit: usize) -> Result<(), Error> {
        let mut state = self.0.borrow_mut();
        if state.closed || limit < state.charged {
            return Err(Error::QuotaExceeded);
        }
        if state
            .parent
            .as_ref()
            .is_some_and(|p| limit > p.usage().limit)
        {
            return Err(Error::QuotaExceeded);
        }
        state.limit = limit;
        Ok(())
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
    pub fn close(&self) {
        self.0.borrow_mut().closed = true;
    }
    pub(crate) fn charge(&self, bytes: usize) -> Result<(), Error> {
        let mut state = self.0.borrow_mut();
        if state.closed || bytes > state.limit - state.charged {
            return Err(Error::QuotaExceeded);
        }
        if let Some(parent) = &state.parent {
            parent.charge(bytes)?;
        }
        state.charged += bytes;
        Ok(())
    }
    pub(crate) fn release(&self, bytes: usize) {
        let mut state = self.0.borrow_mut();
        state.charged = state
            .charged
            .checked_sub(bytes)
            .expect("unbalanced RAM charge");
        if let Some(parent) = &state.parent {
            parent.release(bytes);
        }
    }
    pub(crate) fn quarantine(&self, bytes: usize) {
        let mut state = self.0.borrow_mut();
        state.quarantined += bytes;
        if let Some(parent) = &state.parent {
            parent.quarantine(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn children_share_budget_and_failed_charge_is_atomic() {
        let root = Account::new(8192);
        let a = root.child(8192).unwrap();
        let b = root.child(8192).unwrap();
        a.charge(4096).unwrap(); // managed backing
        b.charge(4096).unwrap(); // a whole delegated Untyped
        assert_eq!(a.charge(1), Err(Error::QuotaExceeded));
        assert_eq!(a.usage().charged, 4096);
        assert_eq!(root.usage().charged, 8192);
        a.release(4096);
        b.charge(4096).unwrap();
        assert_eq!(b.set_limit(4096), Err(Error::QuotaExceeded));
        b.close();
        assert_eq!(b.charge(1), Err(Error::QuotaExceeded));
        b.quarantine(4096);
        assert_eq!(root.usage().quarantined, 4096);
        assert_eq!(root.usage().charged, 8192);
    }
}
