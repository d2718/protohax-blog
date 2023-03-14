use std::{
    sync::Arc,
    collections::BTreeMap,
    net::SocketAddr,
};

use tokio::{
    net::UdpSocket,
    sync::mpsc::{channel, unbounded_channel, Sender},
};
use tracing::{event, Level};
use tracing_subscriber::{filter::EnvFilter, fmt::layer, prelude::*};

mod lrl;
mod types;

use crate::{
    types::{MsgBlock, Pkt, PktType, Response, SessionId},
};

static LOCAL_ADDR: &str = "0.0.0.0:12321";
const CHAN_SIZE: usize = 4;

async fn read_pkt(sock: &UdpSocket) -> (Pkt, SocketAddr) {
    loop {
        let mut data = MsgBlock::new();
        let (length, addr) = match sock.recv_from(&mut data.as_mut()).await {
            Err(e) => {
                event!(Level::ERROR, "error reading from socket: {}", &e);
                continue;
            },
            Ok((length, addr)) => (length, addr),
        };
        match Pkt::new(data, length) {
            Ok(p) => return (p, addr),
            Err(e) => {
                event!(Level::ERROR, "error creating Pkt: {}", &e);
            },
        };
    }
}

async fn respond(sock: &UdpSocket, r: Arc<Response>, addr: SocketAddr) {
    match sock.send_to(r.bytes(), addr).await {
        Ok(n) => if n != r.bytes().len() {
            event!(Level::ERROR,
                "only sent {} bytes of {} byte Response",
                &n, &r.bytes().len()
            );
        },
        Err(e) => {
            event!(Level::ERROR,
                "error sending {:?}: {}", &r, &e
            );
        },
    }
}

struct Client {
    tx: Sender<Pkt>,
    addr: SocketAddr,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::registry()
        .with(layer())
        .with(EnvFilter::from_default_env())
        .init();
    
    // Receives Responses from individual tasks.
    let (tx, mut rx) = unbounded_channel::<Arc<Response>>();
    // Holds channels to individual tasks.
    let mut clients: BTreeMap<SessionId, Client> = BTreeMap::new();

    let sock = UdpSocket::bind(LOCAL_ADDR).await.expect("unable to bind socket");
    event!(Level::INFO,
        "listening on {:?}",
        sock.local_addr().expect("socket has no local address")
    );

    loop {
        tokio::select!{
            tup = read_pkt(&sock) => {
                let (pkt, addr) = tup;
                if let Some(conn) = clients.get(pkt.id()) {
                    if let Err(e) = conn.tx.send(pkt).await {
                        // FIX: This needs to shut down this connection.
                        event!(Level::ERROR,"error sending to client: {}", &e)
                    }
                } else {
                    let id = SessionId::from(pkt.id());
                    let (task_tx, task_rx) = channel::<Pkt>(CHAN_SIZE);
                    let client = Client {
                        tx: task_tx, addr,
                    };
                    clients.insert(id.clone(), client);
                    let main_tx = tx.clone();
                    tokio::spawn(async move {
                        lrl::run(id, main_tx, task_rx).await;
                    });
                }
            },
            resp = rx.recv() => {
                let resp = resp.expect("main channel closed");
                if &resp.rtype == &PktType::Close {
                    if let Some(client) = clients.remove(resp.id()) {
                        respond(&sock, resp, client.addr).await;
                    } else {
                        event!(Level::ERROR,
                            "no Client with ID {:?}", resp.id()
                        );
                    }
                } else {
                    if let Some(client) = clients.get(resp.id()) {
                        respond(&sock, resp, client.addr).await;
                    } else {
                        event!(Level::ERROR,
                            "no Client with ID {:?}", resp.id()
                        );
                    }
                }
            }
        }
    }
}