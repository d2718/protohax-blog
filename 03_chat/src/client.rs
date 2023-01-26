/*!
Interface between a TcpStream and the rest of the program.
*/
use std::rc::Rc;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    net::TcpStream,
    sync::{broadcast::Receiver, mpsc::Sender},
};

use crate::message::{Event, Message};

static WELCOME_TEXT: &[u8] = b"Welcome. Please enter the name you'd like to use.\n";
static BAD_NAME_TEXT: &[u8] = b"Your name must consist of at least one ASCII alphanumeric character.\n";

pub struct Client {
    id: usize,
    from_user: BufReader<ReadHalf<TcpStream>>,
    to_user: WriteHalf<TcpStream>,
    from_room: Receiver<Rc<Message>>,
    to_room: Sender<Event>,
}

/// Ensure name consists of more than zero ASCII alphanumerics.
fn name_is_ok(name: &str) -> bool {
    if name.len() < 1 { return false; }
    
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() { return false; }
    }

    true
}

impl Client {
    pub fn new(
        id: usize,
        socket: TcpStream,
        from_room: Receiver<Rc<Message>>,
        to_room: Sender<Event>,
    ) -> Client {
        let (from_user, to_user) = tokio::io::split(socket);
        let from_user = BufReader::new(from_user);

        Client { id, from_user, to_user, from_room, to_room }
    }

    /// Send welcome message, get name from client, and validate it.
    async fn get_name(&mut self) -> Result<String, String> {
        self.to_user.write_all(WELCOME_TEXT).await.map_err(|e| format!(
            "write error: {}", &e
        ))?;

        let mut name = String::new();
        if let Err(e) = self.from_user.read_line(&mut name).await {
            return Err(format!("read error: {}", &e));
        }
        // If the read happened properly, this should end with an `\n'
        // we want to remove.
        let _ = name.pop();

        if !name_is_ok(&name) {
            // We don't really care if this fails.
            let _ = self.to_user.write_all(BAD_NAME_TEXT).await;
            return Err(format!("invalid name: {:?}", &name));
        }

        Ok(name)
    }

    async fn run(&mut self) -> Result<(), String> {
        use tokio::sync::broadcast::error::RecvError;
        log::debug!("Client {} is running.", self.id);

        let name = self.get_name().await?;
        log::debug!("Client {} is {}", self.id, &name);

        //self.from_room = self.from_room.resubscribe();

        let joinevt = Event::Join{ id: self.id, name };
        self.to_room.send(joinevt).await.map_err(|e| format!(
            "error sending Join event: {}", &e
        ))?;

        let mut line_buff: String = String::new();

        loop {
            tokio::select!{
                res = self.from_user.read_line(&mut line_buff) => match res {
                    Ok(0) => {
                        log::debug!(
                            "Client {} read 0 bytes; closing connection.",
                            self.id
                        );
                        return Ok(());
                    },
                    Ok(n) => {
                        log::debug!(
                            "Client {} rec'd {} bytes: {}",
                            self.id, n, &line_buff
                        );
                        // Every line has to end with '\n`. If we encountered
                        // EOF during this read, it might be missing.
                        if !line_buff.ends_with('\n') {
                            line_buff.push('\n');
                        }

                        let mut new_buff = String::new();
                        std::mem::swap(&mut line_buff, &mut new_buff);
                        // Now new_buff holds the line we just read, and
                        // line_buff is a new empty string, ready to be read
                        // into next time this branch completes.

                        let evt = Event::Text{ id: self.id, text: new_buff };
                        self.to_room.send(evt).await.map_err(|e| format!(
                            "unable to send event: {}", &e
                        ))?;
                    },
                    Err(e) => {
                        return Err(format!("read error: {}", &e));
                    }
                },

                res = self.from_room.recv() => match res {
                    // We can't match directly on an `Rc`; we have to
                    // dereference it to match "through" it; hence the
                    // ugly nested match here.
                    Ok(msg_ptr) => match *msg_ptr {
                        Message::All{ id, ref text } => {
                            if self.id != id {
                                self.to_user.write_all(text.as_bytes()).await
                                    .map_err(|e| format!(
                                        "write error: {}", &e
                                    ))?;
                            }
                        },
                        Message::One{ id, ref text } => {
                            if self.id == id {
                                self.to_user.write_all(text.as_bytes()).await
                                    .map_err(|e| format!(
                                        "write error: {}", &e
                                    ))?;
                            }
                        },
                    },
                    Err(RecvError::Lagged(n)) => {
                        log::warn!(
                            "Client {} dropped {} Message(s)",
                            self.id, n
                        );
                        let text = format!(
                            "Your connection has lagged and dropped {} message(s).", n
                        );
                        self.to_user.write_all(text.as_bytes()).await
                            .map_err(|e| format!(
                                "write error: {}", &e
                            ))?;
                    }
                    // We shouldn't ever encounter this error, but we have to
                    // match exhaustively, and it's the only other kind of
                    // RecvError.
                    Err(RecvError::Closed) => {
                        return Err("broadcast channel closed".into());
                    },
                }
            }
        }
    }

    pub async fn start(mut self) {
        log::debug!("Client {} started.", self.id);

        if let Err(e) = self.run().await {
            log::error!("Client {}: {}", self.id, &e);
        }
        let leave = Event::Leave{ id: self.id };
        if let Err(e) = self.to_room.send(leave).await {
            log::error!(
                "Client {}: error sending Leave Event: {}",
                self.id, &e
            );
        }

        // Recombine our ReadHalf and WriteHalf into the original TcpStream
        // and attempt to shut it down.
        if let Err(e) = self.from_user.into_inner()
            .unsplit(self.to_user)
            .shutdown()
            .await
        {
            log::error!(
                "Client {}: error shutting down connection: {}",
                self.id, e
            );
        }

        log::debug!("Client {} disconnects.", self.id)
    }
}