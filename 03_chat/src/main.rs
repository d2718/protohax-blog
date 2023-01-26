/*!
Protohackers Problem 3: Budget Chat

This is essentially a line-based chat protocol.
The [full spec is here.](https://protohackers.com/problem/3)
*/

use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc},
};

mod client;
mod message;
mod room;

use crate::{
    client::Client,
    room::Room,
};

static LOCAL_ADDR: &str = "0.0.0.0:12321";
/// Message capacity for broadcast channel from Room to Clients.
const MESSAGE_CAPACITY: usize = 256;
/// Event capacity for mpsc channel from Clients to Room.
const EVENT_CAPACITY: usize = 256;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    let (evt_tx, evt_rx) = mpsc::channel(EVENT_CAPACITY);
    // New broadcast::Receivers are spawned by subscribing to the Sender,
    // so we don't even need to keep this one around.
    let (msg_tx, _) = broadcast::channel(MESSAGE_CAPACITY);
    let room = Room::new(evt_rx, msg_tx.clone());

    let locals = tokio::task::LocalSet::new();

    locals.spawn_local(async move {
        if let Err(e) = room.run().await {
            // This can't happen, because our `Room::run()` ended up only
            // ever returning `OK(())`, if it even returns.
            log::error!("Error running Room: {}", &e);
        }
    });

    let listener = TcpListener::bind(LOCAL_ADDR).await.unwrap();
    log::info!("Bound to {}", LOCAL_ADDR);
    let mut client_n: usize = 0;

    locals.spawn_local(async move {
        loop {
            match listener.accept().await {
                Ok((socket, addr)) => {
                    log::debug!("Rec'd connection {} from {:?}", client_n, &addr);

                    let client = Client::new(
                        client_n, socket, msg_tx.subscribe(), evt_tx.clone()
                    );
                    log::debug!("Client {} created.", client_n);
                    // This `spawn_local()` is a separate _function_, not the
                    // `LocalSet` method of the same name. It ensures that
                    // this future is run on the same thread as the current
                    // `LocalSet` task.
                    tokio::task::spawn_local(async move {
                        client.start().await
                    });
                    
                    client_n += 1;
                },
                Err(e) => {
                    log::error!("Error with incoming connection: {}", &e);
                }
            }
        }
    });

    locals.await;
}
