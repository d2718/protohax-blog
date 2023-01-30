/*!
Protohackers Problem 4: Unusual Database Program

Implement a
[key/value store that's accessed via UDP.](https://protohackers.com/problem/4)
*/

use std::{
    collections::HashMap,
    net::SocketAddr,
};

use tokio::{
    net::UdpSocket,
    sync::mpsc::{channel, Receiver, Sender},
};

static LOCAL_ADDR: &str = "0.0.0.0:12321";
/// Size of buffer to hold requests. Per the specification, requests will
/// be shorter than 1000 bytes.
const BUFFSIZE: usize = 1000;
/// Number of messages each channel will hold before making calls to
/// `.send()` wait. Honestly, 1 should be big enough.
const CHANSIZE: usize = 256;
/// Our response to a "version" request.
static VERSION: &[u8] = b"version=Mr Ken's Wild UDP Ride, v 0.1";

/// Array large enough to hold any legal request.
type Buffer = [u8; BUFFSIZE];
/// Contructor for the `Buffer` type that creates zeroed Buffers.
fn buffer() -> Buffer { [0u8; BUFFSIZE] }

/// This struct will carry both requests to the store task from the socket
/// task, and responses back in the other direction.
#[derive(Debug)]
struct Msg {
    /// Contains the request or response data.
    bytes: Buffer,
    /// Contains the _length_ of the data in the `bytes` buffer.
    length: usize,
    /// The address from which the request came, and hence the address to
    /// which to send the response.
    addr: SocketAddr,
}

async fn manage_socket(
    to_store: Sender<Msg>,
    mut from_store: Receiver<Msg>,
) {
    log::debug!("manage_socket() started.");

    // If we can't bind our socket, there's no point in continuing.
    let socket = UdpSocket::bind(LOCAL_ADDR).await
        .expect("Unable to bind to socket address.");
    log::info!("Listening on {:?}", socket.local_addr().unwrap());

    let mut buff = buffer();

    loop {
        tokio::select!{
            res = socket.recv_from(&mut buff) => match res {
                Ok((length, addr)) => {
                    log::debug!(
                        "rec'd request from {}: {:?}",
                        &addr, &String::from_utf8_lossy(&buff[..length])
                    );
                    let msg = Msg {
                        bytes: buff.clone(),
                        length,
                        addr
                    };

                    if let Err(e) = to_store.send(msg).await {
                        log::error!("unable to send to store task: {}", &e);
                    }
                },
                Err(e) => {
                    log::error!("error reading from socket: {}", &e)
                },
            },

            opt = from_store.recv() => {
                // If the channel from the store task is closed, there's no
                // way we can do any more useful work.
                let msg = opt.expect("channel from store task closed!");
                if let Err(e) = socket.send_to(
                    &msg.bytes[..msg.length], msg.addr
                ).await {
                    log::error!(
                        "error writing {:?} to socket: {}",
                        &String::from_utf8_lossy(&msg.bytes[..msg.length]), &e
                    );
                }
            },
        }
    }
}

async fn manage_store(
    to_socket: Sender<Msg>,
    mut from_socket: Receiver<Msg>
) {
    log::debug!("manage_store() started.");

    let mut store: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();

    while let Some(msg) = from_socket.recv().await {
        let request = &msg.bytes[..msg.length];

        if request == b"version" {
            let mut bytes = buffer();
            (&mut bytes[..VERSION.len()]).clone_from_slice(&VERSION);
            let msg = Msg {
                bytes,
                length: VERSION.len(),
                addr: msg.addr
            };

            if let Err(e) = to_socket.send(msg).await {
                log::error!("unable to send to socket task: {}", &e);
            }
            continue;
        }

        if let Some(n) = request.iter().position(|&b| b == b'=') {
            if &request[..n] == b"version" {
                // Let this request hit the flooooooooor.
                continue;
            }

            let key = Vec::from(&request[..n]);
            let val = Vec::from(&request[(n+1)..]);

            log::debug!(
                "inserting {:?}={:?}",
                &String::from_utf8_lossy(&key),
                &String::from_utf8_lossy(&val)
            );

            store.insert(key, val);

        } else {
            if let Some(val) = store.get(request) {
                // The length of the response.
                let length = val.len() + request.len() + 1;

                let mut bytes = buffer();
                (&mut bytes[..request.len()]).clone_from_slice(request);
                bytes[request.len()] = b'=';
                (&mut bytes[(request.len()+1)..length]).clone_from_slice(val);

                let msg = Msg {
                    bytes, length,
                    addr: msg.addr,
                };

                if let Err(e) = to_socket.send(msg).await {
                    log::error!("unable to send to socket task: {}", &e);
                }
            }
            // If our `store` doesn't contain a value associated with the
            // requested key, we just ignore the request.
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    let (to_store, from_socket) = channel(CHANSIZE);
    let (to_socket, from_store) = channel(CHANSIZE);

    tokio::join!(
        manage_socket(to_store, from_store),
        manage_store(to_socket, from_socket),
    );
}
