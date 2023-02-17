/*!
Bookkeeping and behavior of the types of clients.
*/
use std::time::Duration;

use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, UnboundedSender},
    time::{Instant, Interval, interval_at},
};
use tracing::{event, Level, span};

use crate::{
    bio::*,
    events::Event,
    obs::{Infraction, Obs},
};

static CHANNEL_CLOSED: &str = "main channel closed";

/// A struct to provide an optional heartbeat.
/// 
/// Can be set to either resolve at regular intervals, or never, so it can
/// be used in a join! either way.
struct Heartbeat(Option<Interval>);

impl Heartbeat {
    fn new() -> Heartbeat { Heartbeat(None) }

    /// Set this heart to not beat.
    fn stop(&mut self) { self.0 = None; }

    /// Set this heart to beat every `n` deciseconds.
    fn set_decisecs(&mut self, n: u32) {
        if n == 0 {
            self.stop();
            return;
        }
        let millis: u64 = (n as u64) * 100;
        let period = Duration::from_millis(millis);
        self.0 = Some(interval_at(
            Instant::now().checked_add(period).unwrap(),
            period
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
    fn is_beating(&self) -> bool { self.0.is_some() }
}

/// When a client first connects, it is unidentified.
///
/// Once an IAmCamera or IAmDispatcher message is received, this will then
/// run as the appropriate type.
pub struct Client {
    id: usize,
    heart: Heartbeat,
    socket: IOPair,
    tx: UnboundedSender<Event>,
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
            id, tx, rx,
            heart: Heartbeat::new(),
            socket: IOPair::from(socket)
        }
     }

     async fn wrapped_run(&mut self) -> Result<(), Error> {
        loop {
            tokio::select!{
                biased;

                _ = self.heart.beat() => {
                    self.socket.write(ServerMessage::Heartbeat).await?;
                },

                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message: {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::IAmCamera{ road, mile, limit } => {
                            let evt = Event::Camera{ id: self.id };
                            self.tx.send(evt).expect(CHANNEL_CLOSED);
                            // Server reports limit in mi/hr, but all of our
                            // calculations (and our dispatching) is done in
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
                            if self.heart.is_beating() {
                                let msg = ServerMessage::Error{
                                    msg: LPString::from(
                                        &"multiple heartbeat requests"
                                    )
                                };
                                self.socket.write(msg).await?;
                                return Err(Error::ProtocolError(
                                    "multiple heartbeat requests".into()
                                ));
                            } else {
                                self.heart.set_decisecs(interval);
                            }
                        },

                        msg => {
                            let errmsg = ServerMessage::Error{
                                msg: LPString::from(
                                    &"client not identified yet"
                                )
                            };
                            self.socket.write(errmsg).await?;
                            return Err(Error::ProtocolError(format!(
                                "sent {:?} without identifying", &msg
                            )));
                        },
                    }
                },
            }
        }
     }

     pub async fn run(mut self) {
        span!(Level::TRACE, "run", client = &self.id);

        if let Err(e) = self.wrapped_run().await {
            match e {
                Error::Eof => { /* clean */ },
                Error::IOError(_) => {
                    event!(Level::ERROR, "client {}: {:?}", &self.id, &e);
                    let msg = ServerMessage::Error{
                        msg: LPString::from(
                            &"the server encountered an error"
                        )
                    };
                    let _ = self.socket.write(msg).await;
                },
                Error::ProtocolError(cerr) => {
                    event!(Level::ERROR,
                        "client {} error: {}", &self.id, &cerr
                    );
                    let msg = ServerMessage::Error{
                        msg: LPString::from(
                            &cerr
                        )
                    };
                    let _ = self.socket.write(msg).await;
                },
            }
        }

        if let Err(e) = self.socket.shutdown().await {
            event!(Level::ERROR,
                "client {}: error shutting down socket: {:?}", &self.id, &e
            );
        }
        if let Err(e) = self.tx.send(Event::Gone{ id: self.id }) {
            event!(Level::ERROR, "error sending Gone({}): {:?}", &self.id, &e);
        }
        event!(Level::TRACE, "client {} disconnects", &self.id);
     }

     async fn run_as_camera(
        &mut self,
        road: u16,
        mile: u16,
        limit: u16
    ) -> Result<(), Error> {
        // Don't need this anymore.
        self.rx.close();

        span!(Level::TRACE, "running as Camera", client = self.id);
        loop {
            tokio::select!{
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::Plate{ plate, timestamp } => {
                            let pos = Obs::new(mile, timestamp);
                            let evt = Event::Observation{
                                plate, road, limit, pos
                            };
                            self.tx.send(evt).expect(CHANNEL_CLOSED);
                        },

                        ClientMessage::WantHeartbeat{ interval } => {
                            if self.heart.is_beating() {
                                let errmsg = ServerMessage::Error{
                                    msg: LPString::from(
                                        &"multiple heartbeat requests"
                                    )
                                };
                                self.socket.write(errmsg).await?;
                                return Err(Error::ProtocolError(
                                    "multiple heartbeat requests".into()
                                ));
                            } else {
                                self.heart.set_decisecs(interval);
                            }
                        },

                        msg => {
                            let errmsg = ServerMessage::Error{
                                msg: LPString::from(
                                    &"illegal Camera message"
                                )
                            };
                            self.socket.write(errmsg).await?;
                            return Err(Error::ProtocolError(
                                format!("Camera sent {:?}", &msg)
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
            tokio::select!{
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::WantHeartbeat{ interval } => {
                            if self.heart.is_beating() {
                                let errmsg = ServerMessage::Error{
                                    msg: LPString::from(
                                        &"multiple heartbeat requests"
                                    )
                                };
                                self.socket.write(errmsg).await?;
                                return Err(Error::ProtocolError(
                                    "multiple heartbeat requests".into()
                                ));
                            } else {
                                self.heart.set_decisecs(interval);
                            }
                        },

                        msg => {
                            return Err(Error::ProtocolError(
                                format!("illegal msg: {:?}", &msg)
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
}