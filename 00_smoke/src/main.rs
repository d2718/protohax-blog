/*!
Protohackers Problem 0: Smoke Test

Asynchronous TCP Echo implementation.
*/
use std::io::ErrorKind;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream}
};

static ADDR: &str = "0.0.0.0:12321";

/// We'll read a kilobyte at a time.
const BUFFSIZE: usize = 1024;

/// Suck small mouthfuls out of a socket and spit them back until there's
/// no more data coming.
///
/// If we encounter any errors, we'll just give up and close the connection.
async fn handle(mut sock: TcpStream, client_n: u64) {
    let mut buff = [0u8; BUFFSIZE];

    loop {
        // Read no more than will fit in `buff`.
        match sock.read(&mut buff).await {
            // Ok(0) means EOF.
            Ok(0) => break,
            // n bytes have been read into `buff`.
            Ok(n) => {
                log::debug!("Client {} read {} bytes.", client_n, n);
                // Write the first n bytes of `buff` back into the socket.
                if let Err(e) = sock.write_all(&buff[..n]).await {
                    log::error!(
                        "Client {}: error writing to socket: {}",
                        client_n, &e
                    );
                    break;
                }
                log::debug!("Client {} wrote {} bytes.", client_n, n);
            },
            Err(e) => {
                // WouldBlock means just that; we'll try it again in a moment.
                // Any other error, and we'll just give up.
                if e.kind() != ErrorKind::WouldBlock {
                    log::error!(
                        "Client {}: error reading from socket: {}",
                        client_n, &e
                    );
                    break;
                }
            }
        }
    }

    log::debug!("Closing client {}.", client_n);
    if let Err(e) = sock.shutdown().await {
        log::error!(
            "Client {}: error shutting down connection: {}",
            client_n, &e
        );
    }
    log::debug!("Client {} disconnected.", client_n);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    // We'll leave this `.unwrap()` in here instead of "handling" it because
    // if this fails, we'd just have to give up anyway.
    let listener = TcpListener::bind(ADDR).await.unwrap();
    log::info!("Listening on {}", listener.local_addr().unwrap());

    let mut client_n: u64 = 0;

    loop {
        match listener.accept().await {
            Ok((sock, addr)) => {
                log::debug!(
                    "Accepted connection {} from {}", client_n, addr
                );

                // tokio::task::spawn() is the exact async analog to the
                // threaded std::thread::spawn().
                tokio::task::spawn(async move { handle(sock, client_n).await });

                client_n += 1;
            },
            Err(e) => {
                log::error!("Error accepting connection: {}", &e);
            }
        }
    }
}