//! Cooperative cancellation shared by the catalog, the readers and the UI.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::{Error, Result};

/// A cancellation flag. Clones share the flag; a linked token also stops
/// when any of its parents stops.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Option<Arc<Flag>>);

#[derive(Debug, Default)]
struct Flag {
    raised: AtomicBool,
    parents: Vec<Cancel>,
}

impl Cancel {
    /// A token that can be canceled.
    pub fn new() -> Self {
        Self(Some(Arc::default()))
    }

    /// A token that never stops.
    pub fn none() -> Self {
        Self(None)
    }

    /// A token that stops when canceled directly or when a parent stops.
    pub fn linked(parents: &[&Cancel]) -> Self {
        let flag = Flag { raised: AtomicBool::new(false), parents: parents.iter().map(|&p| p.clone()).collect() };
        Self(Some(Arc::new(flag)))
    }

    pub fn cancel(&self) {
        if let Some(flag) = &self.0 {
            flag.raised.store(true, Ordering::SeqCst);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        match &self.0 {
            None => false,
            Some(flag) => flag.raised.load(Ordering::SeqCst) || flag.parents.iter().any(Cancel::is_cancelled),
        }
    }

    pub fn can_be_cancelled(&self) -> bool {
        self.0.is_some()
    }

    /// `Err(Error::Cancelled)` once the token has stopped.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_tokens_follow_parents() {
        let (a, b) = (Cancel::new(), Cancel::new());
        let child = Cancel::linked(&[&a, &b]);
        assert!(!child.is_cancelled());
        b.cancel();
        assert!(child.is_cancelled() && !a.is_cancelled());
        assert!(Cancel::none().check().is_ok());
    }
}
