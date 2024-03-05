use std::{process::Child, sync::Mutex};

pub struct AppState {
    zebrad_child: Mutex<Option<Child>>,
}

impl AppState {
    pub fn new(zebrad_child: Child) -> Self {
        Self {
            zebrad_child: Mutex::new(Some(zebrad_child)),
        }
    }

    pub fn insert_zebrad_child(&self, new_zebrad_child: Child) {
        let mut zebrad_child_handle = self
            .zebrad_child
            .lock()
            .expect("could not get lock on zebrad child mutex");

        *zebrad_child_handle = Some(new_zebrad_child);
    }

    /// Drops and kills the `zebrad_child` child process, if any.
    ///
    /// Returns true if there was a zebrad child process that's been killed and dropped, or
    /// returns false if there was no zebrad child process in the state.
    pub fn kill_zebrad_child(&self) -> bool {
        if let Some(mut zebrad_child) = self
            .zebrad_child
            .lock()
            .expect("could not get lock on zebrad_child mutex")
            .take()
        {
            zebrad_child
                .kill()
                .expect("could not kill zebrad child process");
            true
        } else {
            false
        }
    }
}
