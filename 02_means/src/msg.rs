/*!
Reading the 9-byte message format.
*/
use tokio::io::AsyncReadExt;

/// Represents the types of messages expected from clients.
#[derive(Debug, Clone, Copy)]
pub enum Msg {
    Insert{ timestamp: i32, price: i32, },
    Query{ begin: i32, end: i32, }
}

pub trait MsgReader: AsyncReadExt + Unpin {
    async fn read_message(&mut self) -> Result<Msg, String> {
        let mut buff = [0u8; 9];
        self.read_exact(&mut buff).await.map_err(|e| format!(
            "read error: {}", &e
        ))?;

        let mut quad = [0u8; 4];
        quad.clone_from_slice(&buff[1..5]);
        let a = i32::from_be_bytes(quad.clone());
        quad.clone_from_slice(&buff[5..9]);
        let b = i32::from_be_bytes(quad.clone());

        match buff[0] {
            b'I' => Ok(Msg::Insert{ timestamp: a, price: b }),
            b'Q' => Ok(Msg::Query{ begin: a, end: b }),
            x    => Err(format!("unrecognized type: {}", x)),
        }
    }
}