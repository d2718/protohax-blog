/*!
The protocol spoken by camera and dispatcher clients.
*/
use std::{
    convert::TryInto,
    fmt::{Debug, Display, Formatter},
    io::{self, Cursor, Read, Write},
};

use tokio::io::AsyncWriteExt;
use tracing::{event, Level};

use crate::error::Error;

const LPU16ARR_LEN: usize = 256;


/*
/// Class to read, write, and represent the length-prefixed string.
///
/// As they are length-prefixed by a single u8, a 256-byte backing array
/// should be long enough to hold any possible string.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct LPString {
    bytes: [u8; 256],
    length: usize,
}

impl LPString {
    pub fn from<A>(a: &A) -> LPString
    where A: AsRef<[u8]> + Sized
    {
        let a = a.as_ref();
        let mut bytes = [0u8; 256];
        let mut length = a.len();
        if length > 255 {
            length = 255;
            bytes.copy_from_slice(&a[..length]);
        } else {
            (&mut bytes[..length]).copy_from_slice(a);
        }

        LPString { bytes, length }
    }
    /// Read an LPString from the given AsyncReader.
    pub async fn read<R>(r: &mut R) -> Result<LPString, Error>
    where R: AsyncReadExt + Unpin
    {
        let mut bytes = [0u8; 256];
        let length = r.read_u8().await? as usize;
        for n in 0..length {
            bytes[n] = r.read_u8().await?;
        }

        Ok(LPString { bytes, length })
    }

    /// Write an LPString to the given AsyncWriter.
    pub fn write<W>(&self, w: &mut W) -> Result<(), Error>
    where W: Write
    {
        // self.length should have originally been cast _from_ a u8 on
        // construction, so this should always succeed.
        let length: u8 = self.length.try_into().unwrap();
        w.write_all(&[length])?;
        w.write_all(self.as_slice())?;

        Ok(())
    }

    /// Return the byte slice of string data.
    pub fn as_slice(&self) -> &[u8] { &self.bytes[..self.length] }
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LPU16Array {
    data: [u16; 32],
    length: usize,
}

impl LPU16Array {
    pub async fn read<R>(r: &mut R) -> Result<LPU16Array, Error>
    where R: AsyncReadExt + Unpin
    {
        let mut data = [0u16; 32];
        let length = r.read_u8().await? as usize;
        if length > 32 {
            panic!("max LPU16Array size is 32");
        }
        for n in 0..length {
            data[n] = r.read_u16().await?;
        }

        Ok(LPU16Array{ data, length })
    }

    pub fn as_slice(&self) -> &[u16] { &self.data[..self.length] }
}

*/

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct LPString {
    bytes: [u8; 256],
    length: usize
}

impl LPString {
    pub fn as_slice(&self) -> &[u8] { &self.bytes[..self.length] }

    pub fn write<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let length: u8 = self.length.try_into().unwrap();
        w.write_all(&[length])?;
        w.write_all(self.as_slice())
    }
}

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

#[derive(Clone, Eq, PartialEq)]
pub struct LPU16Array {
    data: [u16; LPU16ARR_LEN],
    length: usize,
}

impl LPU16Array {
    pub fn as_slice(&self) -> &[u16] { &self.data[..self.length] }
}

impl Debug for LPU16Array {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "LPU16Array({:?})", &self.as_slice())
    }
}

pub struct BCursor<T: AsRef<[u8]>> {
    cursor: Cursor<T>,
}

impl<T: AsRef<[u8]>> BCursor<T> {
    pub fn new(t: T) -> BCursor<T> {
        BCursor{ cursor: Cursor::new(t) }
    }

    pub fn position(&self) -> usize { self.cursor.position() as usize }

    pub fn read_u8(&mut self) -> io::Result<u8> {
        let mut buff = [0u8; 1];
        self.cursor.read_exact(&mut buff)?;
        Ok(buff[0])
    }

    pub fn read_u16(&mut self) -> io::Result<u16> {
        let mut buff = [0u8; 2];
        self.cursor.read_exact(&mut buff)?;
        Ok(u16::from_be_bytes(buff))
    }

    pub fn read_u32(&mut self) -> io::Result<u32> {
        let mut buff = [0u8; 4];
        self.cursor.read_exact(&mut buff)?;
        Ok(u32::from_be_bytes(buff))
    }

    pub fn read_lpstring(&mut self) -> io::Result<LPString> {
        let mut bytes = [0u8; 256];
        let length = self.read_u8()? as usize;
        self.cursor.read_exact(&mut bytes[..length])?;
        Ok(LPString { bytes, length })
    }

    pub fn read_lpu16arr(&mut self) -> io::Result<LPU16Array> {
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

/// Messages that the server might receive.
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

impl ClientMessage {
    /// Read a ClientMessage from the given AsyncReader
    pub fn read<T: AsRef<[u8]>>(r: &mut BCursor<T>) -> Result<ClientMessage, Error> {
        use ClientMessage::*;

        let msg_type = r.read_u8()?;

        match msg_type {
            0x20 => {
                let plate = r.read_lpstring()?;
                let timestamp = r.read_u32()?;

                Ok(ClientMessage::Plate{ plate, timestamp })
            },

            0x40 => {
                let interval = r.read_u32()?;

                Ok(WantHeartbeat{ interval })
            },

            0x80 => {
                let road = r.read_u16()?;
                let mile = r.read_u16()?;
                let limit = r.read_u16()?;

                Ok(IAmCamera{ road, mile, limit })
            },

            0x81 => {
                let roads = r.read_lpu16arr()?;

                Ok(IAmDispatcher{ roads })
            },

            b => {
                Err(
                    Error::ClientError(
                        format!("illegal type: {:x}", &b)
                    )
                )
            }
        }
    }
}


/// Messages the server might send.
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

impl ServerMessage {
    pub async fn write<W>(&self, w: &mut W) -> Result<(), Error>
    where W: AsyncWriteExt + Unpin
    {
        // So that we can write to non-buffered writers without worrying
        // about explicitly making multiple syscalls, we'll write to a
        // buffer on the stack first, then write the whole thing at once.

        // Buffer large enough to fit any possible message.
        let buff = [0u8; 273];
        let mut cursor = Cursor::new(buff);

        match self {
            &ServerMessage::Error{ ref msg } => {
                cursor.write_all(&[0x10])?;
                msg.write(&mut cursor)?;
            },

            &ServerMessage::Ticket {
                ref plate,
                road, mile1, timestamp1, mile2, timestamp2, speed
            } => {
                cursor.write_all(&[0x21])?;
                plate.write(&mut cursor)?;
                cursor.write_all(&road.to_be_bytes())?;
                cursor.write_all(&mile1.to_be_bytes())?;
                cursor.write_all(&timestamp1.to_be_bytes())?;
                cursor.write_all(&mile2.to_be_bytes())?;
                cursor.write_all(&timestamp2.to_be_bytes())?;
                cursor.write_all(&speed.to_be_bytes())?;
            },

            &ServerMessage::Heartbeat => {
                cursor.write_all(&[0x41])?;
            },
        }

        // Given the maximum length of the buffer here, there's no possible
        // way this could fail.
        let length: usize = cursor.position().try_into().unwrap();
        let buff = cursor.into_inner();
        w.write_all(&buff[..length]).await?;
        w.flush().await?;
        event!(Level::DEBUG, "wrote message {:x?}", &buff[..length]);

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn write_server() -> Result<(), Error> {
        let mut buff: Cursor<Vec<u8>> = Cursor::new(Vec::new());

        let msg = ServerMessage::Error{ msg: LPString::from(&b"testing!") };
        msg.write(&mut buff).await?;

        let msg = ServerMessage::Ticket{
            plate: LPString::from(&b"A550RGY"),
            road: 0x1,
            mile1: 0x2,
            timestamp1: 0x3,
            mile2: 0x4,
            timestamp2: 0x5,
            speed: 0x6
        };
        msg.write(&mut buff).await?;

        let msg =ServerMessage::Heartbeat;
        msg.write(&mut buff).await?;

        let buff = buff.into_inner();
        assert_eq!(
            buff,
            [
                0x10, 0x08, b't', b'e', b's', b't', b'i', b'n', b'g', b'!',
                0x21, 0x07, b'A', b'5', b'5', b'0', b'R', b'G', b'Y',
                    0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x03,
                    0x00, 0x04, 0x00, 0x00, 0x00, 0x05, 0x00, 0x06,
                0x41
            ]
        );

        Ok(())
    }

    #[tokio::test]
    async fn read_client() -> Result<(), Error> {

        let buff: Vec<u8> = vec![
            0x20, 0x07, b'A', b'5', b'5', b'0', b'R', b'G', b'Y',
                0x00, 0x00, 0x00, 0x01,
            0x40, 0x00, 0x00, 0x00, 0x02,
            0x80, 0x00, 0x03, 0x00, 0x04, 0x00, 0x05,
            0x81, 0x06, 0x00, 0x07, 0x00, 0x08, 0x00, 0x09,
                        0x00, 0x0a, 0x00, 0x0b, 0x00, 0x0c
        ];
        let mut buff: Cursor<Vec<u8>> = Cursor::new(buff);

        let mut msgs: Vec<ClientMessage> = Vec::with_capacity(3);
        while let Ok(msg) = ClientMessage::read(&mut buff).await {
            msgs.push(msg);
        }

        assert_eq!(
            msgs,
            vec![
                ClientMessage::Plate{
                    plate: LPString::from(&b"A550RGY"),
                    timestamp: 1,
                },
                ClientMessage::WantHeartbeat{ interval: 2 },
                ClientMessage::IAmCamera{
                    road: 3, mile: 4, limit: 5,
                },
                ClientMessage::IAmDispatcher{
                    roads: LPU16Array::from(&[7, 8, 9, 10, 11, 12])
                },
            ]
        );

        Ok(())
    }
}