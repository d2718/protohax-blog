/*!
Types to be sent between tasks.
*/
#![allow(dead_code)]

use std::{
    borrow::Borrow,
    convert::AsRef,
    fmt::{Debug, Display, Formatter},
    io::Write,
};

use smallvec::SmallVec;

pub const BLOCK_SIZE: usize = 1024;
pub const MAX_PACKET_SIZE: usize = 998;
pub const NUM_LENGTH: usize = 10;

static ARR_WRITE_ERR: &str = "error writing to array";

pub struct MsgBlock([u8; BLOCK_SIZE]);

impl MsgBlock {
    pub fn new() -> MsgBlock { MsgBlock([0u8; BLOCK_SIZE])}
}

impl AsRef<[u8]> for MsgBlock {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl AsMut<[u8]> for MsgBlock {
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.as_mut()
    }
}

impl std::ops::Index<usize> for MsgBlock {
    type Output = u8;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl std::ops::IndexMut<usize> for MsgBlock {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

/// For use as keys in a mapping from session ids to channels.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct SessionId(SmallVec<[u8; NUM_LENGTH]>);

impl SessionId {
    pub fn as_slice(&self) -> &[u8] {
        &self.0.as_slice()
    }
}

impl AsRef<[u8]> for SessionId {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Borrow<[u8]> for SessionId {
    fn borrow(&self) -> &[u8] {
        self.as_slice()
    }
}

impl From<&[u8]> for SessionId {
    fn from(bytes: &[u8]) -> SessionId {
        let v = if bytes.len() > NUM_LENGTH {
            SmallVec::from_slice(&bytes[..NUM_LENGTH])
        } else {
            SmallVec::from_slice(bytes)
        };
        SessionId(v)
    }
}

impl Display for SessionId {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "[")?;
        for &b in self.as_slice().iter() {
            write!(f, "{}", b as char)?;
        }
        write!(f, "]")
    }
}

impl Debug for SessionId {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "SessionId([{}])", self)
    }
}

/// The four types of messages in the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PktType {
    Ack,
    Close,
    Connect,
    Data,
}

/// The partially-parsed data from a single UDP packet.
///
/// Enough of the data is parsed to determine its type and the session ID.
pub struct Pkt {
    pub ptype: PktType,
    pub data: MsgBlock,
    pub id_start: usize,
    pub id_end: usize,
    pub length: usize,
}

impl Pkt {
    /// Given a 1KB block containing the raw data from a UDP packet, parse
    /// enough of it to give us a `Pkt`.
    pub fn new(data: MsgBlock, length: usize) -> Result<Pkt, &'static str> {
        if length == 0 {
            return Err("no data");
        } else if data[0] != b'/' {
            return Err("no initial /");
        } else if data[length-1] != b'/' {
            return Err("no final /");
        }
        
        let second_slash = data.as_ref()[2..length].iter()
            .position(|&b| b == b'/')
            .ok_or("no second /")?;
        let ptype = match &data.as_ref()[1..second_slash] {
            b"ack" => PktType::Ack,
            b"close" => PktType::Close,
            b"connect" => PktType::Connect,
            b"data" => PktType::Data,
            _ => return Err("unrecognized MsgType"),
        };
        
        let id_start = second_slash + 1;
        let id_end = id_start + data.as_ref()[id_start..length].iter()
            .position(|&b| b == b'/' )
            .ok_or("no third /")?;
        
        Ok(Pkt { ptype, data, id_start, id_end, length})
    }

    /// Expose the bytes of the session ID portion of the message.    
    pub fn id(&self) -> &[u8] {
        &self.data.as_ref()[self.id_start..self.id_end]
    }
}

impl Debug for Pkt {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.debug_struct("Pkt")
            .field("ptype", &self.ptype)
            .field("id", &String::from_utf8_lossy(self.id()))
            .field("data", &String::from_utf8_lossy(&self.data.as_ref()[..self.length]))
            .finish()
    }
    
}

/// A response to be sent back to the client.
pub struct Response {
    data: MsgBlock,
    pub rtype: PktType,    
    length: usize,
    id_start: usize,
    id_end: usize,
}

impl Response {
    /// Generate an "ack" response.
    pub fn ack(id: &[u8], count: usize) -> Response {
        let rtype = PktType::Ack;
        let mut data = MsgBlock::new();
        let mut wptr = &mut data.as_mut()[..];
        wptr.write(b"/ack/").expect(ARR_WRITE_ERR);
        
        let id_start = BLOCK_SIZE - wptr.len();
        wptr.write(id).expect(ARR_WRITE_ERR);
        let id_end = BLOCK_SIZE - wptr.len();
        
        write!(wptr, "/{}/", &count).expect(ARR_WRITE_ERR);
        let length = BLOCK_SIZE - wptr.len();

        Response { rtype, data, length, id_start, id_end }
    }

    /// Generate and write data to a "data" response.
    ///
    /// The provided byte iterator should spit out the data; it may not
    /// be entirely used up if the message is long.
    pub fn data<I>(id: &[u8], count: usize, source: &mut I) -> Response
    where
        I: Iterator<Item = u8>
    {
        let rtype = PktType::Data;
        let mut data = MsgBlock::new();
        let id_start;
        let id_end;
        let mut length = {
            let mut wptr = &mut data.as_mut()[..];
            wptr.write(b"/data/").expect(ARR_WRITE_ERR);

            id_start = BLOCK_SIZE - wptr.len();
            wptr.write(id).expect(ARR_WRITE_ERR);
            id_end = BLOCK_SIZE - wptr.len();

            write!(wptr, "/{}/", &count).expect(ARR_WRITE_ERR);
            BLOCK_SIZE - wptr.len()
        };

        while length < MAX_PACKET_SIZE {
            match source.next() {
                None => break,
                Some(b'/') => {
                    data[length] = b'\\';
                    length += 1;
                    data[length] = b'/';
                },
                Some(b'\\') => {
                    data[length] = b'\\';
                    length += 1;
                    data[length] = b'\\';
                },
                Some(b) => data[length] = b,
            };
            length += 1;
        }

        Response{ rtype, data, length, id_start, id_end }
    }

    /// Generate a "close" message.
    pub fn close(id: &[u8]) -> Response {
        let rtype = PktType::Close;
        let mut data = MsgBlock::new();
        let id_start: usize = 7;
        let (id_end, length) = {
            let mut wptr = &mut data.as_mut()[..];
            write!(wptr, "/close/").expect(ARR_WRITE_ERR);
            wptr.write(id).expect(ARR_WRITE_ERR);
            wptr.write(b"/").expect(ARR_WRITE_ERR);
            let length = BLOCK_SIZE - wptr.len();
            (length - 1, length)
        };

        Response{ rtype,data, length, id_start, id_end }
    }        

    /// Expose the bytes of the session ID.
    pub fn id(&self) -> &[u8] {
        &self.data.as_ref()[self.id_start..self.id_end]
    }

    /// Expose the slice containing the whole message.
    pub fn bytes (&self) -> &[u8] {
        &self.data.as_ref()[..self.length]
    }
}

impl Debug for Response {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("data", &String::from_utf8_lossy(&self.data.as_ref()[..self.length]))
            .finish()
    }
}