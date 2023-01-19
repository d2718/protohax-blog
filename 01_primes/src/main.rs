/*!
Protohackers Problem 1: Prime Time

See [Protohackers Problem 1](https://protohackers.com/problem/1)
for the full specification.
*/
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::value::Number;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream}
};

pub mod primes;

use primes::Primes;

static LOCAL_ADDR: &str = "0.0.0.0:12321";

static PRIMES: Lazy<Mutex<Primes>> = Lazy::new(||
    Mutex::new(Primes::default())
);

/// Deserialization target for requests.
///
/// By default, `serde` will ignore unknown fields, which is exactly the
/// behavior we want.
#[derive(Debug, Deserialize)]
struct Request {
    method: String,
    /// serde_json::value::Number can be f64, i64, or u64. 
    number: Number,
}

/// Deserialize a request; if valid, determine if it's prime.
///
/// If the request is _malformed_, return an error.
fn process_request(bytes: &[u8]) -> Result<bool, String> {
    let req: Request = serde_json::from_slice(&bytes).map_err(|e| format!(
        "Unable to deserialize request: {}", &e
    ))?;

    if &req.method != "isPrime" {
        return Err(format!("Unrecognized method: {:?}", &req.method));
    }

    // If it's not an integer, it can't possibly be prime.
    let n = match req.number.as_i64() {
        None => { return Ok(false); }
        Some(n) => n,
    };

    // Not only is this an easy false, it also guarantees that n can be safely
    // cast to a u64, which is what our `Primes::is_prime()` method wants.
    if n < 2 {
        return Ok(false);
    }
    let n = n as u64;

    // As a single-threaded program, we would have already panicked if this
    // Mutex were poisoned, so we .unwrap() confidently here.
    let is_prime = PRIMES.lock().unwrap().is_prime(n);

    Ok(is_prime)
}

/// Listen for requests on `socket` and respond appropriately.
///
/// If we encounter any errors or malformed requests, we'll close the
/// connection.
async fn handle(mut socket: TcpStream, client_n: usize) {
    let (read_half, mut write_half) = socket.split();
    let mut reader = BufReader::new(read_half);
    let mut buff = String::new();

    loop {
        match reader.read_line(&mut buff).await {
            // We've reached EOF; the other end has almost assuredly closed
            // the connection.
            Ok(0) => { break; },
            Ok(n) => {
                log::debug!(
                    "Client {} read {} bytes: {:?}.", client_n, n, &buff
                );

                let result = process_request(buff.as_bytes());
                let response = match &result {
                    &Ok(is_prime) => format!(
                        "{{\"method\":\"isPrime\",\"prime\":{}}}\n",
                        is_prime
                    ),
                    &Err(ref e) => format!(
                        "{{\"method\":\"isPrime\",\"error\":{:?}}}\n", e
                    ),
                };

                log::debug!(
                    "Client {} sending response: {:?}", client_n, &response
                );

                if let Err(ref e) = write_half.write_all(
                    response.as_bytes()
                ).await {
                    log::error!(
                        "Client {}: error writing response: {}", client_n, e
                    );
                    break;
                }

                // If `process_request()` returns an `Err`, the request was
                // malformed, so we're closing the connection.
                if result.is_err() { break; }

                // `.read_line()` appends to the target buffer, so we have
                // to clear the request we just processed out of it.
                buff.clear();
            },
            Err(e) => {
                log::error!(
                    "Client {}: error reading line from socket: {}",
                    client_n, &e
                );
                break;
            },
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
    log::info!("Listening on {}", LOCAL_ADDR);

    let mut client_n: usize = 0;

    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                log::debug!("Accepted client {} from {:?}", client_n, &addr);

                tokio::spawn(async move {
                    handle(socket, client_n).await
                });
                client_n += 1;
            },
            Err(e) => {
                log::error!("Error accepting connection: {}", &e);
            }
        }
    }
}