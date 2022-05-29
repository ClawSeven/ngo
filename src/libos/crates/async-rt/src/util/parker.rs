/// A primitive for putting a thread to sleep and waking it up.
pub struct Parker;

impl Parker {
    pub fn new() -> Self {
        Self
    }

    /// Park, i.e., put the current thread to sleep.
    //
    /// This method must not be called concurrently.
    /// Doing so does not lead any memory safety issues, but
    /// may cause a parked thread to sleep forever.
    pub fn park(&self) {
        // std::thread::park();
    }

    /// Unpark, i.e., wake up a thread put to sleep by the parker.
    ///
    /// This method can be called concurrently.
    pub fn unpark(&self) {}
}
