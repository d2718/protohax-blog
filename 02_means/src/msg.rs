/*!
Reading the 9-byte message format.
*/
use std::io::ErrorKind;

use tokio::io::AsyncReadExt;

/// Represents the types of messages expected from clients.
#[derive(Debug, Clone, Copy)]
pub enum Msg {
    Insert{ timestamp: i32, price: i32, },
    Query{ begin: i32, end: i32, }
}

impl Msg {

    /// Suck a `Msg` out of the provided async reader.
    ///
    /// Returns Ok(None) upon reaching EOF.
    pub async fn read_from<R>(reader: &mut R) -> Result<Option<Msg>, String>
    where
        R: AsyncReadExt + Unpin
    {
        let mut buff = [0u8; 9];
        if let Err(e) = reader.read_exact(&mut buff).await {
            if e.kind() == ErrorKind::UnexpectedEof {
                return Ok(None);
            } else {
                return Err(format!("read error: {}", &e));
            }
        }

        // This stanza is mildly hinky. We're going to copy the two 32-bit
        // chunks into this four-byte array, then convert each one
        // big-endianly into an i32.
        let mut quad = [0u8; 4];
        quad.clone_from_slice(&buff[1..5]);
        let a = i32::from_be_bytes(quad.clone());
        quad.clone_from_slice(&buff[5..9]);
        let b = i32::from_be_bytes(quad.clone());

        match buff[0] {
            b'I' => Ok(Some(Msg::Insert{ timestamp: a, price: b })),
            b'Q' => Ok(Some(Msg::Query{ begin: a, end: b })),
            x    => Err(format!("unrecognized type: {}", x)),
        }
    }
}
