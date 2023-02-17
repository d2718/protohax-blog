/*!
Structs and methods for dealing with reading and writing the binary
protocol used in this exercise.

We go to a great deal of effort to make reading a cancellation-safe
process, but in hindsight (having looked at other solutions), this
turns out to not really be necessary.
*/

use std::{
    convert::TryInto,
    fmt::{Debug, Display, Formatter},
    io::{self, Cursor, ErrorKind, Read, Write},
};

use tokio::{
    io::{ReadHalf, WriteHalf},
    net::TcpStream,
};

/// Maximum length of the array of roads a Dispatcher will announce it's
/// covering. According to the spec, this should be 255, but in practice
/// it's never more than about seven. This should be more than long enougn.
const LPU16ARR_LEN: usize = 32;
/// Length of buffer used to read/write messages. Based on the above
/// limitation on the length of LPU16Arrays, the maximum message
/// length should be somewhat less than this.
const IO_BUFF_SIZE: usize = 300;

/// A unifying error type to make error handling a little easier.
#[derive(Debug)]
pub enum Error {
    /// The connected client has disconnected cleanly.
    Eof,
    /// The connected client has written some sort of message that doesn't conform
    /// to the protocol.
    ProtocolError(String),
    /// There was an actual OS-level read/write error.
    IOError(std::io::Error),
}

/// Class to read, write, and represent the length-prefixed string.
///
/// As they are length-prefixed by a single u8, a 256-byte backing array
/// should be long enough to hold any possible string.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct LPString {
    bytes: [u8; 256],
    length: usize
}

impl LPString {
    /// Expose the bytes that actually make up the message.
    pub fn as_slice(&self) -> &[u8] { &self.bytes[..self.length] }
}

/// This is essentially the constructor.
impl<A> From<A> for LPString
where A: AsRef<[u8]> + Sized
{
    fn from(a: A) -> Self {
        let a = a.as_ref();
        let mut bytes = [0u8; 256];
        let mut length = a.len();
        if length > 255 {
            length = 255;
            bytes.copy_from_slice(&a[..length]);
        } else {
            (&mut bytes[..length]).copy_from_slice(a);
        }

        LPString{ bytes, length }
    }
}

impl Display for LPString {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &String::from_utf8_lossy(self.as_slice()))
    }
}

impl Debug for LPString {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "LPString({:?})", &String::from_utf8_lossy(self.as_slice()))
    }
}

/// The IAmDispatcher message sends a length-prefixed array of u16s.
///
/// We have to read it, but we never have to write one. It functions
/// much like the LPString above.
#[derive(Clone, Eq, PartialEq)]
pub struct LPU16Array {
    data: [u16; LPU16ARR_LEN],
    length: usize,
}

impl LPU16Array {
    /// Expose the set values in the array.
    pub fn as_slice(&self) -> &[u16] { &self.data[..self.length] }
}

impl Debug for LPU16Array {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "LPU16Array({:?})", &self.as_slice())
    }
}

/// Messages sent from the connected devices to the server.
#[derive(Debug, Eq, PartialEq)]
pub enum ClientMessage {
    // 0x20
    Plate{ plate: LPString, timestamp: u32 },
    // 0x40
    WantHeartbeat{ interval: u32 },
    // 0x80
    IAmCamera{ road: u16, mile: u16, limit: u16 },
    // 0x81
    IAmDispatcher{ roads: LPU16Array },
}

/// Messages sent back from the server to the connected devices.
#[derive(Debug, Eq, PartialEq)]
pub enum ServerMessage {
    // 0x10
    Error{ msg: LPString },
    // 0x21
    Ticket{
        plate: LPString,
        road: u16,
        mile1: u16,
        timestamp1: u32,
        mile2: u16,
        timestamp2: u32,
        speed: u16,
    },
    // 0x41
    Heartbeat,
}

/// Methods to (synchronously) read the types of values used in the
/// protocol. These are based on similar functions from the AsyncReadExt
/// trait that we'd like to be synchronous. We'll add in ones to read our
/// two complex types (string, array), too.
trait SpeedRead: Read {
    fn read_u8(&mut self) -> io::Result<u8> {
        let mut buff = [0u8; 1];
        self.read_exact(&mut buff)?;
        Ok(unsafe { *buff.get_unchecked(0) })
    }

    fn read_u16(&mut self) -> io::Result<u16> {
        let mut buff = [0u8; 2];
        self.read_exact(&mut buff)?;
        Ok(u16::from_be_bytes(buff))
    }

    fn read_u32(&mut self) -> io::Result<u32> {
        let mut buff = [0u8; 4];
        self.read_exact(&mut buff)?;
        Ok(u32::from_be_bytes(buff))
    }

    fn read_lpstring(&mut self) -> io::Result<LPString> {
        let mut bytes = [0u8; 256];
        let length = self.read_u8()? as usize;
        self.read_exact(&mut bytes[..length])?;
        Ok(LPString{ bytes, length })
    }

    fn read_lpu16arr(&mut self) -> io::Result<LPU16Array> {
        let mut data = [0u16; LPU16ARR_LEN];
        let length = self.read_u8()? as usize;
        if length > LPU16ARR_LEN {
            panic!("max LPU16Array size is {}", &LPU16ARR_LEN);
        }
        for n in 0..length {
            data[n] = self.read_u16()?;
        }
        Ok(LPU16Array{ data, length })
    }
}

/// And we'll implement it for the Cursor, because that's what we're going
/// to use to read from our buffer.
impl<T: AsRef<[u8]>> SpeedRead for std::io::Cursor<T> {}

pub struct IOPair {
    reader: ReadHalf<TcpStream>,
    writer: WriteHalf<TcpStream>,
    buffer: [u8; IO_BUFF_SIZE],
    write_idx: usize,
}

impl From<TcpStream> for IOPair {
    fn from(socket: TcpStream) -> Self {
        let (reader, writer) = tokio::io::split(socket);
        IOPair {
            reader, writer,
            buffer: [0u8; IO_BUFF_SIZE],
            write_idx: 0,
        }
    }
}

impl IOPair {
    fn inner_read(&mut self) -> Result<(ClientMessage, u64), io::Error> {
        let mut c = Cursor::new(&self.buffer[..self.write_idx]);

        let msg_type = c.read_u8()?;

        match msg_type {
            0x20 => {
                let plate = c.read_lpstring()?;
                let timestamp = c.read_u32()?;
                Ok((ClientMessage::Plate{ plate, timestamp }, c.position()))
            },

            0x40 => {
                let interval = c.read_u32()?;
                Ok((ClientMessage::WantHeartbeat{ interval }, c.position()))
            },

            0x80 => {
                let road = c.read_u16()?;
                let mile = c.read_u16()?;
                let limit = c.read_u16()?;
                Ok((ClientMessage::IAmCamera{ road, mile, limit }, c.position()))
            },

            0x81 => {
                let roads = c.read_lpu16arr()?;
                Ok((ClientMessage::IAmDispatcher{ roads }, c.position()))
            },

            b => {
                Err(io::Error::new(
                    ErrorKind::Other,
                    format!("illegal message type: {:x}", &b)
                ))
            }
        }
    }

    /// Attempt to read a ClientMessage.
    pub async fn read(&mut self) -> Result<ClientMessage, Error> {
        use tokio::io::AsyncReadExt;

        loop {
            match self.inner_read() {
                Ok((msg, cpos)) => {
                    let cpos = cpos as usize;
                    let extra = self.write_idx - cpos;
                    if extra > 0 {
                        let mut buff = [0u8; IO_BUFF_SIZE];
                        (&mut buff[..extra]).copy_from_slice(&self.buffer[cpos..self.write_idx]);
                        (&mut self.buffer[..extra]).copy_from_slice(&buff[..extra]);
                    }
                    self.write_idx -= cpos;
                    return Ok(msg);
                } ,
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                    /* No message or partial message in buffer. */
                },
                Err(e) if e.kind() == ErrorKind::Other => {
                    return Err(Error::ProtocolError(
                        format!("{}", &e)
                    ));
                },
                Err(e) => { return Err(Error::IOError(e)); }
            }

            match self.reader.read(&mut self.buffer[self.write_idx..]).await {
                Ok(0) => { return Err(Error::Eof); },
                Ok(n) => {
                    self.write_idx += n;
                    if self.write_idx == IO_BUFF_SIZE {
                        return Err(Error::ProtocolError(
                            "buffer overrun".into()
                        ));
                    }
                },
                Err(e) if e.kind() == ErrorKind::Interrupted ||
                          e.kind() == ErrorKind::WouldBlock =>
                {
                    tokio::task::yield_now().await;
                },
                Err(e) => { return Err(Error::IOError(e)); },
            }
        }
    }

    async fn inner_write(&mut self, smesg: ServerMessage) -> Result<(), io::Error> {
        use tokio::io::AsyncWriteExt;

        match smesg {
            ServerMessage::Heartbeat => {
                self.writer.write_all(&[0x41]).await?;
            },

            ServerMessage::Error { msg } => {
                let len_byte: u8 = msg.length.try_into().unwrap();
                self.writer.write_all(&[0x10, len_byte]).await?;
                self.writer.write_all(msg.as_slice()).await?;
            },

            ServerMessage::Ticket {
                plate, road, mile1, timestamp1, mile2, timestamp2, speed
             } => {
                let mut c = Cursor::new([0u8; 273]);
                let len_byte: u8 = plate.length.try_into().unwrap();

                c.write_all(&[0x21, len_byte])?;
                c.write_all(plate.as_slice())?;
                c.write_all(&road.to_be_bytes())?;
                c.write_all(&mile1.to_be_bytes())?;
                c.write_all(&timestamp1.to_be_bytes())?;
                c.write_all(&mile2.to_be_bytes())?;
                c.write_all(&timestamp2.to_be_bytes())?;
                c.write_all(&speed.to_be_bytes())?;

                let length: usize = c.position().try_into().unwrap();
                let buff = c.into_inner();
                self.writer.write_all(&buff[..length]).await?;
             },
        }

        self.writer.flush().await
    }

    pub async fn write(&mut self, smesg: ServerMessage) -> Result<(), Error> {
        self.inner_write(smesg).await.map_err(|e| Error::IOError(e))
    }

    pub async fn shutdown(self) -> Result<(), Error> {
        use tokio::io::AsyncWriteExt;

        let mut sock = self.reader.unsplit(self.writer);
        sock.shutdown().await.map_err(|e| Error::IOError(e))
    }
}