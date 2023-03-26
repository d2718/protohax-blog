/*!
Structs that hold a single UDP packet's worth of data.
*/

use std::{
    fmt::{Debug, Display, Formatter, Write},
    net::SocketAddr,
};

pub const BUFFER_SIZE: usize = 1024;

/// A buffer for storing session IDs.
#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct IdField([u8; 10]);

impl IdField {
    pub fn as_bytes(&self) -> &[u8] { self.0.as_slice() }
}

impl From<&[u8]> for IdField {
    fn from(bytes: &[u8]) -> Self {
        let mut inner = [0u8; 10];
        let length = bytes.len();
        if length > 10 {
            // Only copy the first ten bytes.
            inner[..].copy_from_slice(&bytes[..10]);
        } else {
            inner[..length].copy_from_slice(bytes)
        }

        IdField(inner)
    }
}

impl Display for IdField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "[")?;
        for &n in self.0.iter().take_while(|d| **d > 0) {
            write!(f, "{}", n.saturating_sub(b'0'))?;
        }
        write!(f, "]")
    }
}

impl Debug for IdField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "IdField({})", self)
    }
}

/// A buffer for reading/writing packets.
///
/// Messages can't be more than 1000 bytes long, so we will store all
/// messages in 1000 byte arrays.
type Buffer = [u8; BUFFER_SIZE];

/// Convenience function for returning an empty buffer;
pub fn buffer() -> Buffer { [0u8; BUFFER_SIZE] }

/// Type of message contained in a packet.
#[derive(Debug)]
pub enum Msg {
    Connect,
    Data,
    Ack,
    Close,
}

/// Data packet received.
pub struct Gram {
    bytes: Buffer,
    id: IdField,
    msg: Msg,
    start: usize,
    end: usize,
    addr: SocketAddr,
}

impl Gram {
    pub fn new(bytes: Buffer, end: usize, addr: SocketAddr) -> Option<Gram> {
        if bytes[0] != b'/' { return None; }

        let mut n: Option<usize> = None;
        let mut m: Option<usize> = None;

        for (i, &b) in bytes.as_slice().iter().enumerate().skip(1) {
            if b == b'/' {
                if n.is_none() {
                    n = Some(i);
                } else if m.is_none() {
                    m = Some(i);
                    break;
                }
            }
        }

        let n = n?;
        let m = m?;

        let msg = match &bytes[1..n] {
            b"data" => Msg::Data,
            b"ack" => Msg::Ack,
            b"connect" => Msg::Connect,
            b"close" => Msg::Close,
            _ => { return None; },
        };

        let id = IdField::from(&bytes[(n+1..m)]);
        let start = m + 1;

        Some(Gram { bytes, id, msg, start, end, addr })
    }

    pub fn data(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }
}