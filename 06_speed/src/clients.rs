/*!
Bookkeeping and behavior of the types of clients.
*/
use std::time::Duration;

use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, UnboundedSender},
    time::{interval_at, Instant, Interval},
};
use tracing::{event, span, Level};

use crate::{
    bio::*,
    events::Event,
    obs::{Infraction, Obs},
};

/// Error message with which we'll panic if any of the client tasks find the
/// channel to the main task closed. This should never happen, and if it does,
/// the program can't do any more work anyway, so it's totally reasonable
/// to just die.
static CHANNEL_CLOSED: &str = "main channel closed";

/// A struct to provide an optional heartbeat.
///
/// Can be set to either resolve at regular intervals, or never, so it can
/// be used in a join! either way.
struct Heartbeat(Option<Interval>);

impl Heartbeat {
    fn new() -> Heartbeat {
        Heartbeat(None)
    }

    /// Set this heart to not beat.
    fn stop(&mut self) {
        self.0 = None;
    }

    /// Set this heart to beat every `n` deciseconds.
    fn set_decisecs(&mut self, n: u32) {
        if n == 0 {
            self.stop();
            return;
        }
        let millis: u64 = (n as u64) * 100;
        let period = Duration::from_millis(millis);
        self.0 = Some(interval_at(
            // Set it to start exactly one period from now.
            Instant::now().checked_add(period).unwrap(),
            period,
        ));
    }

    /// Either resolves or doesn't, depending how this Heartbeat is set.
    async fn beat(&mut self) {
        if let Some(ref mut i) = &mut self.0 {
            i.tick().await;
        } else {
            std::future::pending::<()>().await;
        }
    }

    /// Return whether this heartbeat is beating.
    fn is_beating(&self) -> bool {
        self.0.is_some()
    }
}

/// When a client first connects, it is unidentified.
///
/// Once an IAmCamera or IAmDispatcher message is received, this will then
/// run as the appropriate type.
pub struct Client {
    id: usize,
    heart: Heartbeat,
    socket: IOPair,
    /// Channel to the main task.
    tx: UnboundedSender<Event>,
    /// Channel from the main task.
    rx: Receiver<Infraction>,
}

impl Client {
    pub fn new(
        id: usize,
        socket: TcpStream,
        tx: UnboundedSender<Event>,
        rx: Receiver<Infraction>,
    ) -> Client {
        Client {
            id,
            tx,
            rx,
            heart: Heartbeat::new(),
            socket: IOPair::from(socket),
        }
    }

    async fn try_start_heart(&mut self, interval: u32) -> Result<(), Error> {
        if self.heart.is_beating() {
            return Err(Error::ProtocolError(
                "multiple heartbeat requests".into()
            ));
        } else {
            self.heart.set_decisecs(interval);
            Ok(())
        }
    }

    async fn wrapped_run(&mut self) -> Result<(), Error> {
        loop {
            tokio::select! {
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message: {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::IAmCamera{ road, mile, limit } => {
                            let evt = Event::Camera{ id: self.id };
                            self.tx.send(evt).expect(CHANNEL_CLOSED);
                            // Server reports limit in mi/hr, but all of our
                            // calculations (and our dispatching) are done in
                            // hundredths of mi/hr.
                            let limit = limit * 100;
                            return self.run_as_camera(
                                road, mile, limit
                            ).await;
                        },

                        ClientMessage::IAmDispatcher{ roads } => {
                            let evt = Event::Dispatcher{
                                id: self.id, roads
                            };
                            self.tx.send(evt).expect(CHANNEL_CLOSED);
                            return self.run_as_dispatcher().await;

                        },

                        ClientMessage::WantHeartbeat{ interval } => {
                            self.try_start_heart(interval).await?
                        },

                        msg => {
                            return Err(Error::ProtocolError(format!(
                                "illegal unident msg: {:?} ", &msg
                            )));
                        },
                    }
                },

                _ = self.heart.beat() => {
                    self.socket.write(ServerMessage::Heartbeat).await?;
                },
            }
        }
    }

    async fn run_as_camera(&mut self, road: u16, mile: u16, limit: u16) -> Result<(), Error> {
        // Don't need this anymore.
        self.rx.close();

        span!(Level::TRACE, "running as Camera", client = self.id);
        loop {
            tokio::select! {
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::Plate{ plate, timestamp } => {
                            let pos = Obs{ mile, timestamp};
                            let evt = Event::Observation{
                                plate, road, limit, pos
                            };
                            self.tx.send(evt).expect(CHANNEL_CLOSED);
                        },

                        ClientMessage::WantHeartbeat{ interval } => {
                            self.try_start_heart(interval).await?
                        },

                        msg => {
                            return Err(Error::ProtocolError(
                                format!("illegal Camera msg: {:?}", &msg)
                            ));
                        },
                    }
                },

                _ = self.heart.beat() => {
                    self.socket.write(ServerMessage::Heartbeat).await?;
                }
            }
        }
    }

    async fn run_as_dispatcher(&mut self) -> Result<(), Error> {
        span!(Level::TRACE, "running as Dispatcher", client = self.id);
        loop {
            tokio::select! {
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::WantHeartbeat{ interval } => {
                            self.try_start_heart(interval).await?
                        },

                        msg => {
                            return Err(Error::ProtocolError(
                                format!("illegal Dispatcher msg: {:?}", &msg)
                            ));
                        },
                    }
                },

                msg = self.rx.recv() => {
                    event!(Level::TRACE,
                        "client {} recv'd: {:?}", &self.id, &msg
                    );
                    match msg {
                        Some(Infraction{ plate, road, start, end, speed }) => {
                            let msg = ServerMessage::Ticket{
                                plate, road, speed,
                                mile1: start.mile,
                                timestamp1: start.timestamp,
                                mile2: end.mile,
                                timestamp2: end.timestamp,
                            };
                            self.socket.write(msg).await?;
                            event!(Level::DEBUG,
                                "client {} wrote Infraction for {:?}", &self.id, &plate
                            );
                        },

                        None => {
                            event!(Level::ERROR, "Dispatcher {} channel closed", &self.id);
                            let msg = ServerMessage::Error {
                                msg: LPString::from(
                                    &"server error, sorry"
                                )
                            };
                            self.socket.write(msg).await?;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    pub async fn run(mut self) {
        span!(Level::TRACE, "run", client = &self.id);

        if let Err(e) = self.wrapped_run().await {
            match e {
                Error::Eof => { /* clean */ }
                Error::IOError(_) => {
                    event!(Level::ERROR, "client {}: {:?}", &self.id, &e);
                    let msg = ServerMessage::Error {
                        msg: LPString::from(&"the server encountered an error"),
                    };
                    let _ = self.socket.write(msg).await;
                }
                Error::ProtocolError(cerr) => {
                    event!(Level::ERROR, "client {} error: {}", &self.id, &cerr);
                    let msg = ServerMessage::Error {
                        msg: LPString::from(&cerr),
                    };
                    let _ = self.socket.write(msg).await;
                }
            }
        }

        if let Err(e) = self.socket.shutdown().await {
            event!(
                Level::ERROR,
                "client {}: error shutting down socket: {:?}",
                &self.id,
                &e
            );
        }
        if let Err(e) = self.tx.send(Event::Gone { id: self.id }) {
            event!(Level::ERROR, "error sending Gone({}): {:?}", &self.id, &e);
        }
        event!(Level::TRACE, "client {} disconnects", &self.id);
    }
}
