//! Runs the front door on a tokio runtime and thread of its own, so a flood of handshakes
//! cannot starve the app's commands. Dropping the [`FrontDoor`] stops it.

use std::sync::Arc;

use launcher_gateway::{
    CertifiedKey, Challenge, Command, Config, Event, GatewayError, Inputs, Mode, Outputs, Stats,
};
use tokio::sync::{mpsc, oneshot, watch};

/// A running front door.
#[derive(Debug)]
pub struct FrontDoor {
    commands: mpsc::Sender<Command>,
    pub events: mpsc::Receiver<Event>,
    pub stats: watch::Receiver<Stats>,
    /// The backend it forwards to, fixed at start.
    pub upstream: std::net::SocketAddr,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FrontDoor {
    /// Starts the front door and waits until it listens, or fails with its error.
    pub async fn start(
        config: Config,
        cert: watch::Receiver<Option<Arc<CertifiedKey>>>,
        challenge: watch::Receiver<Option<Arc<Challenge>>>,
        mode: watch::Receiver<Mode>,
    ) -> Result<FrontDoor, GatewayError> {
        let (commands, commands_rx) = mpsc::channel(8);
        let (events_tx, mut events) = mpsc::channel(256);
        let (stats_tx, stats) = watch::channel(Stats::default());
        let (failed_tx, failed) = oneshot::channel();
        let upstream = config.upstream;
        let inputs = Inputs {
            cert,
            challenge,
            mode,
            commands: commands_rx,
        };
        let outputs = Outputs {
            stats: stats_tx,
            events: events_tx,
        };
        let thread = std::thread::Builder::new()
            .name("zeal-front-door".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("zeal-front-door")
                    .enable_all()
                    .build();
                let result = match runtime {
                    Ok(runtime) => {
                        runtime.block_on(launcher_gateway::serve(config, inputs, outputs))
                    }
                    // The runtime could not start: no port was tried.
                    Err(err) => Err(GatewayError::Listen {
                        port: 0,
                        source: err,
                    }),
                };
                if let Err(err) = result {
                    let _ = failed_tx.send(err);
                }
            })
            .map_err(|source| GatewayError::Listen { port: 0, source })?;
        let mut door = FrontDoor {
            commands,
            events: mpsc::channel(1).1,
            stats,
            upstream,
            thread: Some(thread),
        };
        tokio::select! {
            event = events.recv() => match event {
                Some(Event::Ready { .. }) => {
                    door.events = events;
                    Ok(door)
                }
                _ => Err(GatewayError::Listen { port: 0, source: std::io::Error::other("the front door stopped") }),
            },
            Ok(err) = failed => Err(err),
        }
    }

    /// Closes the port and every connection now.
    pub fn stop(&self) {
        let _ = self.commands.try_send(Command::Stop);
    }

    /// New limits, at once.
    pub fn limits(&self, limits: launcher_gateway::Limits) {
        let _ = self.commands.try_send(Command::Limits(limits));
    }
}

impl Drop for FrontDoor {
    fn drop(&mut self) {
        self.stop();
        // The thread ends with the front door; joining here would block the caller.
        let _ = self.thread.take();
    }
}
