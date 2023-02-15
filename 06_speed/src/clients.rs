/*!
Bookkeeping and behavior of the types of clients.
*/
use std::{
    io::ErrorKind,
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
    sync::mpsc::{Receiver, UnboundedSender},
    time::{Instant, Interval, interval_at},
};
use tracing::{event, Level, span};

use crate::{
    error::Error,
    events::Event,
    message::*,
    obs::{Infraction, Obs},
};

const IO_BUFF_SIZE: usize = 1024;

/// Wraps a TcpStream so the read half is buffered.
struct IOPair {
    reader: ReadHalf<TcpStream>,
    writer: WriteHalf<TcpStream>,
    buffer: [u8; IO_BUFF_SIZE],
    idx: usize,
}

impl From<TcpStream> for IOPair {
    fn from(socket: TcpStream) -> Self {
        let (reader, writer) = tokio::io::split(socket);
        IOPair {
            reader, writer,
            buffer: [0u8; IO_BUFF_SIZE],
            idx: 0
        }
    }
}

impl IOPair {
    /// Read a ClientMessage from the socket.
    pub async fn read(&mut self) -> Result<ClientMessage, Error> {
        loop {
            {
                let mut curs = BCursor::new(
                    &self.buffer[..self.idx]
                );
                let res = ClientMessage::read(&mut curs);
                match res {
                    Ok(msg) => {
                        let cpos = curs.position();
                        let span = self.idx - cpos;
                        let mut buff = [0u8; IO_BUFF_SIZE];
                        (&mut buff[..span]).copy_from_slice(&self.buffer[cpos..self.idx]);
                        (&mut self.buffer[..span]).copy_from_slice(&buff[..span]);
                        self.idx = span;
                        return Ok(msg);
                    },
                    Err(Error::Disconnected) => {
                        // No or only partial ClientMessage in buffer.
                    },
                    Err(e) => { return Err(e); }
                }
            }

            let res = self.reader.read(&mut self.buffer[self.idx..]).await;
            match res? {
                0 => { return Err(Error::Disconnected); },
                n => {
                    self.idx += n;
                    if self.idx > 1000 {
                        return Err(Error::ClientError(
                            "filled read buffer w/o completing a message".into()
                        ));
                    }
                },
            }
        }
    }

    /// Write a ServerMessage to the socket.
    pub async fn write(&mut self, msg: ServerMessage) -> Result<(), Error> {
        msg.write(&mut self.writer).await.map_err(|e| e.into())
    }

    /// Shut the underlying TcpStream down so it can be dropped cleanly.
    pub async fn shutdown(self) -> Result<(), Error> {
        let mut socket = self.reader.unsplit(self.writer);
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
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message: {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::IAmCamera{ road, mile, limit } => {
                            let evt = Event::Camera{ id: self.id };
                            self.tx.send(evt)?;
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
                            self.tx.send(evt)?;
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

     pub async fn run(mut self) {
        span!(Level::TRACE, "run", client = &self.id);

        if let Err(e) = self.wrapped_run().await {
            match e {
                Error::Disconnected => {
                    event!(Level::TRACE,
                        "client {} disconnected", &self.id
                    );
                },
                Error::IOError(_) |
                Error::ServerError(_) => {
                    event!(Level::ERROR, "client {}: {:?}", &self.id, &e);
                    let msg = ServerMessage::Error{
                        msg: LPString::from(
                            &"the server encountered an error"
                        )
                    };
                    let _ = self.socket.write(msg).await;
                },
                Error::ClientError(cerr) => {
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
                            self.tx.send(evt)?;
                        },

                        ClientMessage::WantHeartbeat{ interval } => {
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

                        msg => {
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
                                return Err(Error::ClientError(
                                    "multiple heartbeat requests".into()
                                ));
                            } else {
                                self.heart.set_decisecs(interval);
                            }
                        },

                        msg => {
                            return Err(Error::ClientError(
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