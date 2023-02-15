/*!
Crate for doing some binary I/O.
*/
use std::{
    fmt::{Debug, Display, Formatter},
    io::{self, Cursor, Write},
};

use crate::error::Error;

const LPU16ARR_LEN: usize = 32;

pub struct LPString {
    bytes: [u8; 256],
    length: usize
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
impl LPString {
    pub fn as_slice(&self) -> &[u8] { &self.bytes[..self.length] }

    pub fn write<W>(&self, w: &mut W) -> Result<(), Error> {
        let length: u8 = self.length.try_into().unwrap();
        w.write_all(&[length])?;
        w.write_all(self.as_slice())?;
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

#[derive(Clone)]
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

pub struct BCursor<T> {
    cursor: Cursor<T>,
}

impl<T: AsRef<[u8]> BCursor<T> {
    pub fn new<T>(t: T) -> BCursor {
        BCursor{ cursor: Cursor::new(t) }
    }

    pub fn position(&self) -> usize { self.cursor.position() }

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
        let length = self.read_u8()?;
        self.cursor.read_exact(&mut bytes[..length])?
        Ok(LPString { bytes, length })
    }

    pub fn read_lpu16arr(&mut self) -> io::Result<LPU16Array> {
        let mut data = [0u16; LPU16ARR_LEN];
        let length = self.read_u8()? as usize;
        if length > LPU16ARR_LEN {
            panic!(&format!("max LPU16Array size is {}", &LPU16ARR_LEN));
        }
        for n in 0..length {
            data[n] = self.read_u16()?;
        }
        Ok(LPU16Array{ data, length })
    }
}