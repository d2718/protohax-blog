/*!
Bookkeeping and behavior of the types of clients.
*/
use std::time::Duration;

use tokio::{
    io::{AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    net::TcpStream,
    sync::mpsc::{Receiver, UnboundedSender},
    time::{Instant, Interval, interval_at},
};
use tracing::{event, Level, span};

use crate::{
    event::Event,
    message::*,
    obs::Obs,
};

/// Wraps a TcpStream so the read half is buffered.
struct IOPair {
    reader: BufReader<ReadHalf<TcpStream>>,
    writer: WriteHalf<TcpStream>,
}

impl From<TcpStream> for IOPair {
    fn from(socket: TcpStream) -> Self {
        let (reader, writer) = tokio::io::split(socket);
        IOPair {
            reader: BufReader::new(reader),
            writer
        }
    }
}

impl IOPair {
    /// Read a ClientMessage from the socket.
    pub async fn read(&mut self) -> Result<ClientMessage, Error> {
        ClientMessage::read(&mut self.reader).await.map_err(|e| e.into())
    }

    /// Write a ServerMessage to the socket.
    pub async fn write(&mut self, msg: ServerMessage) -> Result<(), Error> {
        msg.write(&mut self.writer).await.map_err(|e| e.into())
    }

    /// Shut the underlying TcpStream down so it can be dropped cleanly.
    pub async fn shutdown(self) -> Result<(), Error> {
        let mut socket = self.reader.into_inner().unsplit(self.writer);
        socket.shutdown().await.map_err(|e| e.into())
    }
}

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
/// Once an IAmCamera or IAmDispatcher message is received, this can be
/// converted to the appropriate type.
pub struct Unidentified {
    id: usize,
    heart: Heartbeat,
    socket: IOPair,
    tx: UnboundedSender<Event>,
    rx: Receiver<Event>,
}

impl Unidentified {
    pub fn new(
        id: usize,
        socket: TcpStream,
        tx: UnboundedSender<Event>,
        rx: Receiver<Event>,
     ) -> Unidentified {
        Unidentified {
            id, tx, rx,
            heart: Heartbeat::new(),
            socket: IOPair::from(socket)
        }
     }

     pub fn into_camera(self, road: u16, mile: u16, limit: u16) -> Camera {
        Camera {
            id: self.id,
            road, mile, limit,
            heart: self.heart,
            socket: self.socket,
            tx: self.tx
        }
     }

     pub fn into_dispatcher(self, roads: LPU16Array) -> Dispatcher {
        Dispatcher {
            id: self.id,
            roads,
            heart: self.heart,
            socket: self.socket,
            tx: self.tx,
            rx: self.rx,
        }
     }

     async fn wrapped_run(mut self) -> Result<(), Error> {
        loop {
            tokio::select!{
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message: {:?}", &self.id, &msg);
                    let msg = msg?;
                    match msg {
                        ClientMessage::IAmCamera{ road, mile, limit } => {
                            let evt = Event::Camera{ id: self.id };
                            self.tx.send(evt);
                            return self.into_camera(
                                road, mile, limit
                            ).run().await;
                        },
                        ClientMessage::IAmDispatcher{ roads } => {
                            let evt = Event::Dispatcher{
                                id: self.id,
                                roads: roads.clone()
                            };
                            self.tx.send(evt);
                            return self.into_dispatcher(roads).run().await;
                            
                        },
                        ClientMessage::WantHeartbeat{ interval } => {
                            if self.heart.is_beating() {
                                let msg = ServerMessage::Error{
                                    msg: LPString::from(
                                        &"multiple heartbeat requests"
                                    )
                                };
                                self.socket.write(msg).await?;
                                return Err(Error::ClientError(
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
                            return Err(Error::ClientError(format!(
                                "sent {:?} without identifying", &msg
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

     pub async fn run(self) {
        span!(Level::TRACE, "run", client = &self.id);

        // `self` is about to get moved; this is required so that we can
        // print log events later on.
        let id = self.id;
        // This is required so that we can send the `Gone` event at the end.
        let tx = self.tx.clone();

        if let Err(e) = self.wrapped_run().await {
            event!(Level::ERROR, "client {} error: {:?}", &id, &e);
        }

        tx.send(Event::Gone{ id });
        event!(Level::TRACE, "client {} disconnects", &id);
     }
}

/// When a client sends an IAmCamera message, it becomes one of these.
pub struct Camera {
    id: usize,
    road: u16,
    mile: u16,
    limit: u16,
    heart: Heartbeat,
    socket: IOPair,
    tx: UnboundedSender<Event>,
}

impl Camera {
    async fn run(mut self) -> Result<(), Error> {
        span!(Level::TRACE, "running as Camera", client = self.id);
        loop {
            tokio::select!{
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message {:?}", &self.id, &msg);
                    match msg {
                        Ok(ClientMessage::Plate{ plate, timestamp }) => {
                            let pos = Obs::new(self.mile, timestamp);
                            let evt = Event::Observation{
                                plate, pos,
                                road: self.road,
                                limit: self.limit,
                            };
                            self.tx.send(evt);
                        },

                        Ok(ClientMessage::WantHeartbeat{ interval }) => {
                            if self.heart.is_beating() {
                                let errmsg = ServerMessage::Error{
                                    msg: LPString::from(
                                        &"multiple heartbeat requests"
                                    )
                                };
                                self.socket.write(errmsg).await?;
                                return Err(Error::ClientError(
                                    "multiple heartbeat requests".into()
                                ));
                            } else {
                                self.heart.set_decisecs(interval);
                            }
                        },

                        Ok(msg) => {
                            let errmsg = ServerMessage::Error{
                                msg: LPString::from(
                                    &"illegal Camera message"
                                )
                            };
                            self.socket.write(errmsg).await?;
                            return Err(Error::ClientError(
                                format!("Camera sent {:?}", &msg)
                            ));
                        },

                        Err(e) => {
                            let errmsg = ServerMessage::Error{
                                msg: LPString::from(
                                    &"error reading message"
                                )
                            };
                            self.socket.write(errmsg).await?;
                            return Err(e);
                        },
                    }
                },

                _ = self.heart.beat() => {
                    self.socket.write(ServerMessage::Heartbeat).await?;
                }
            }
        }
    }
}

/// When a client sends an IAmDispatcher message, it becomes one of these.
pub struct Dispatcher {
    id: usize,
    roads: LPU16Array,
    heart: Heartbeat,
    socket: IOPair,
    tx: UnboundedSender<Event>,
    rx: Receiver<Event>
}

impl Dispatcher {
    async fn run(mut self) -> Result<(), Error> {
        span!(Level::TRACE, "running as Dispatcher", client = self.id);
        loop {
            tokio::select!{
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message {:?}", &self.id, &msg);
                    match msg {
                        Ok(ClientMessage::WantHeartbeat{ interval }) => {
                            if self.heart.is_beating() {
                                let errmsg = ServerMessage::Error{
                                    msg: LPString::from(
                                        &"multiple heartbeat requests"
                                    )
                                };
                                self.socket.write(errmsg).await?;
                                return Err(Error::ClientError(
                                    "multiple heartbeat requests".into()
                                ));
                            } else {
                                self.heart.set_decisecs(interval);
                            }
                        },

                        Ok(msg) => {
                            let errmsg = ServerMessage::Error{
                                msg: LPString::from(
                                    &"illegal Dispatcher message"
                                )
                            };
                            self.socket.write(errmsg).await?;
                            return Err(Error::ClientError(
                                format!("Dispatcher sent {:?}", &msg)
                            ));
                        },

                        Err(e) => {
                            let errmsg = ServerMessage::Error{
                                msg: LPString::from(
                                    &"error reading message"
                                )
                            };
                            self.socket.write(errmsg).await?;
                            return Err(e);
                        },
                    }
                },

                evt = self.rx.recv() => {
                    match evt {
                        Some(Event::Ticket{ plate, info }) => {
                            let msg = ServerMessage::Ticket{
                                plate,
                                road: info.road,
                                mile1: info.start.mile,
                                timestamp1: info.start.timestamp,
                                mile2: info.end.mile,
                                timestamp2: info.end.timestamp,
                                speed: info.speed,
                            };
                            self.socket.write(msg).await?;
                        },
                        
                        Some(evt) => {
                            event!(Level::ERROR, "Dispatcher {} rec'd {:?}", &self.id, &evt);
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