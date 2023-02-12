/*!
The protocol spoken by camera and dispatcher clients.
*/
use std::convert::TryInto;

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader}
};

const READ_ERROR: &str = "read error";
const WRITE_ERROR: &str = "write error";

/// For easier error handling, we will define our own error type that
/// will implement From<std::io::Error>
#[derive(Debug)]
pub enum Error {
    IOError(std::io::Error),
    ClientError(String),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::IOError(e)
    }
}

/// Class to read, write, and represent the length-prefixed string.
///
/// As they are length-prefixed by a single u8, a 256-byte backing array
/// should be long enough to hold any possible string.
#[derive(Clone, Copy, Debug)]
pub struct LPString {
    bytes: [u8; 256],
    length: usize,
}

impl LPString {
    /// Read an LPString from the given AsyncReader.
    pub async fn read<R>(r: &mut R) -> Result<LPString, Error>
    where R: AsyncReadExt
    {
        let mut bytes = [0u8; 256];
        let length = r.read_u8().await? as usize;
        for n in 0..length {
            bytes[n] = r.read_u8().await?;
        }

        Ok(LPString { bytes, length })
    }

    /// Write an LPString to the given AsyncWriter.
    pub async fn write<W>(&self, w: &mut W) -> Result<(), Error>
    where W: AsyncWriteExt
    {
        // self.length should have originally been cast _from_ a u8 on
        // construction, so this should always succeed.
        let length: u8 = self.length.try_into().unwrap();
        w.write_u8(length)?;
        w.write_all(self.as_slice())?;

        Ok(())
    }

    /// Return the byte slice of string data.
    pub fn as_slice(&self) -> &[u8] -> { &self.bytes[..self.length] }
}

/// The IAmDispatcher message sends a length-prefixed array of u16s.
///
/// We have to read it, but we never have to write one. It functions
/// much like the LPString above.
put struct LPU16Array {
    data: [u16; 256],
    length: usize,
}

impl LPU16Array {
    pub async fn read<R>(r: &mut R) -> Result<LPu16Array, Error>
    where R: AsyncReadExt
    {
        let mut data = [0u16; 256],
        let length = r.read_u8().await? as usize;
        for n in 0..length {
            data[n] = r.read_u16()?;
        }

        Ok(LPU16Array{ data, length })
    }

    pub fn as_slice(&self) -> &[u16] { &self.data[..self.length] }
}

#[derive(Debug)]
/// Messages that the server might receive.
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
    pub async fn read<R>(r: &mut R) -> Result<ClientMessage, Error>
    where R: AsyncReadExt
    {
        use Self::*;

        let type = r.read_u8().await?;

        match type {
            0x20 => {
                let plate = LPString::read(r).await?;
                let timestamp = r.read_u32().await?;

                Ok(ClientMessage::Plate{ plate, timestamp })
            },

            0x40 => {
                let interval = r.read_u32().await?;

                Ok(WantHeartbeat{ interval: u32 })
            },

            0x80 => {
                let road = r.read_u16().await?;
                let mile = r.read_u16().await?;
                let limit = r.read_u16().await?;

                Ok(IAmCamera{ road, mile, limit })
            },

            0x81 => {
                let roads = LPU16Array::read(r).await?;

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

