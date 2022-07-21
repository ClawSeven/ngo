use crate::prelude::*;
#[cfg(feature = "sgx")]
use std::thread::SgxThread as Thread;
#[cfg(not(feature = "sgx"))]
use std::thread::Thread;

/// A primitive for putting a thread to sleep and waking it up.
pub struct Parker {
    thread: Mutex<Option<Thread>>,
}

impl Parker {
    pub fn new() -> Self {
        let thread = Mutex::new(None);
        Self { thread }
    }

    pub fn register(&self) {
        let mut thread = self.thread.lock();
        thread.replace(std::thread::current());
    }

    pub fn unregister(&self) {
        let mut thread = self.thread.lock();
        thread.take();
    }

    /// Park, i.e., put the current thread to sleep.
    //
    /// This method must not be called concurrently.
    /// Doing so does not lead any memory safety issues, but
    /// may cause a parked thread to sleep forever.
    pub fn park(&self) {
        std::thread::park();
    }

    /// Unpark, i.e., wake up a thread put to sleep by the parker.
    ///
    /// This method can be called concurrently.
    pub fn unpark(&self) {
        let inner_thread = self.thread.lock();
        let thread = inner_thread.clone();
        drop(inner_thread);
        if thread.is_some() {
            thread.unwrap().unpark();
        }
    }
}
