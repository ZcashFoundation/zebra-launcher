use std::{process::Child, sync::Mutex};

use tokio::sync::oneshot;

pub struct ZebradChild {
    handle: Mutex<Option<Child>>,
    read_task_shutdown_sender: Mutex<Option<oneshot::Sender<()>>>,
}

impl ZebradChild {
    pub fn new(zebrad_child: Child, read_task_shutdown_sender: oneshot::Sender<()>) -> Self {
        Self {
            handle: Mutex::new(Some(zebrad_child)),
            read_task_shutdown_sender: Mutex::new(Some(read_task_shutdown_sender)),
        }
    }

    pub fn insert_zebrad_child(&self, new_zebrad_child: Child) {
        let mut zebrad_child_handle = self
            .handle
            .lock()
            .expect("could not get lock on zebrad child mutex");

        *zebrad_child_handle = Some(new_zebrad_child);
    }

    pub fn insert_log_reader_shutdown_sender(&self, new_shutdown_sender: oneshot::Sender<()>) {
        let mut read_task_shutdown_sender = self
            .read_task_shutdown_sender
            .lock()
            .expect("could not get lock on zebrad child mutex");

        if let Some(old_shutdown_sender) = read_task_shutdown_sender.replace(new_shutdown_sender) {
            // It's okay if there's a send error, the task may exit and close the channel before
            // the shutdown signal is sent.
            let _ = old_shutdown_sender.send(());
        };
    }

    pub fn is_running(&self) -> bool {
        self.handle
            .lock()
            .expect("could not get lock on zebrad_child mutex")
            .is_some()
    }

    /// Drops and kills the `zebrad_child` child process, if any, and
    /// sends a shutdown signal to the log reader task, if any
    ///
    /// Returns true if there was a zebrad child process that's been killed and dropped, or
    /// returns false if there was no zebrad child process in the state.
    pub fn kill(&self) -> bool {
        if let Some(mut zebrad_child) = self
            .handle
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
