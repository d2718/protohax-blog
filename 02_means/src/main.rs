/*!
Protohackers Problem 2: Tracking Prices

Keeping track of asset prices over time and reporting averages.
[Full spec.](https://protohackers.com/problem/2)
*/
use std::collections::BTreeMap;

use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream}
};

pub mod msg;

use crate::msg::Msg;

const LOCAL_ADDR: &str = "0.0.0.0:12321";

/// Average all the values in the given range of keys, inclusive.
///
/// Return 0 on empty (including necessarily empty) ranges.
fn range_average(map: &BTreeMap<i32, i32>, low: i32, high: i32) -> i32 {
    if high < low { return 0; }

    // This will hold the sum of the key values in the given range. We're
    // using an i64 so we don't risk overflowing if we have a lot of
    // large i32s.
    let mut tot: i64 = 0;
    // Normally you'd use a `usize` for this, but using an i64 means one
    // fewer cast during the division at the end.
    let mut n: i64 = 0;

    for(&t, &p) in map.iter() {
        if t < low {
            continue;
        } else if t <= high {
            tot += p as i64;
            n += 1;
        } else {
            break;
        }
    }

    let tot = map.iter().filter(|(&t, _)| low <= t && t <= high)
        .map(|(_, p)| *p)
        .sum();
    

    if n == 0 { 0 }
    else { (tot / n) as i32 }
}

async fn handler(mut socket: TcpStream, client_n: usize) {
    let mut prices: BTreeMap<i32, i32> = BTreeMap::new();

    loop {
        match Msg::read_from(&mut socket).await {
            Ok(Some(Msg::Insert{ timestamp, price })) => {
                log::debug!(
                    "Client {} rec'd I: t: {}, p: {}",
                    client_n, timestamp, price
                );
                prices.insert(timestamp, price);
            },
            Ok(Some(Msg::Query{ begin, end })) => {
                let avg = range_average(&prices, begin, end);
                log::debug!(
                    "Client {} rec'd Q: {} -> {}; responding {}",
                    client_n, begin, end, avg
                );
                let data = avg.to_be_bytes();
                if let Err(e) = socket.write_all(&data).await {
                    log::error!("Client {}: write error: {}", client_n, &e);
                    break;
                }
            },
            Ok(None) => {
                log::debug!("Client {} can read no more data.", client_n);
                break;
            }
            Err(e) => {
                log::error!("Client {}: {}", client_n, &e);
                break;
            }
        }
    }

    if let Err(e) = socket.shutdown().await {
        log::error!(
            "Client {}: error shutting down connection: {}", client_n, &e
        );
    }
    log::debug!("Client {} disconnecting.", client_n);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    let listener = TcpListener::bind(LOCAL_ADDR).await.unwrap();
    log::info!("Listening to {}", LOCAL_ADDR);

    let mut client_n: usize = 0;

    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                log::debug!("Accepted client {} from {:?}", client_n, &addr);
                tokio::spawn(async move {
                    handler(socket, client_n).await
                });
                client_n += 1;
            },
            Err(e) => {
                log::error!("Error connecting: {}", &e);
            },
        }
    }
}
