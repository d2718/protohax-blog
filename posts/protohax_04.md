title: Protohackers in Rust, Part 04
subtitle: Nothing new here
time: 2023-01-29 21:42:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the fourth Protohackers problem

This post is part of a series on solving [Protohackers](https://protohackers.com) problems. Some previous offerings, if you're interested:

  * [Problem 0: Smoke Test](https://d2718.net/blog/posts/protohax_00.html)
  * [Problem 1: Prime Time](https://d2718.net/blog/posts/protohax_01.html)
  * [Problem 2: Means to an End](https://d2718.net/blog/posts/protohax_02.html)
  * [Problem 3: Budget Chat](https://d2718.net/blog/posts/protohax_03.html)

[The fifth Protohackers problem](https://protohackers.com/problem/4) involves implementing a key--value store accessed over UDP. There are two types of requests.

If a request contains an `=`, it is an _insert_ request. The string of bytes before the first `=` is considered the key; the rest of the bytes are the value. An insert request associates the value with the key. For example, in response to the request

```
frogs=blue
```

our store should associate the value `blue` with the key `frogs`. This is considered an "[upsert](https://en.wiktionary.org/wiki/upsert)" operation; if there is already a value associated with the given key, we will replace it with the new value. Requests beginning with the `=` byte will set the value associated with the empty key. It should be clear that keys cannot contain the `=` byte, but otherwise their contents are arbitrary. Insert requests do not generate responses.

If a request does _not_ contain an `=`, it is a _retrieve_ request. If the contents of the request correspond to a key in our store, we will send back the key and its corresponding value; the contents of this response will be the _exact_ contents of an insert request that would have set the value. For example, if the key `frogs` were associated with the data `blue`, in response to the request

```
frogs
```

 we would respond with `frogs=blue`. If the contents of the retrieve request is _not_ a key in our store, we can either send back a response with an empty value, or no response at all. (We will choose the latter.)

Our software must also have a nonblank _version_ value associated with the key `version`. Requests to retrieve this value should behave like any other retrieve, but requests to set `version` should be ignored.

Requests and responses must all be shorter than 1000 bytes.

## Easy, let's do it

This problem doesn't even really warrant an asynchronous solution, but we're going to implement one anyway because that's what we're all about (at least for the duration of this series). Our server will run two tasks, one that reads/writes from/to our UDP socket, the other which does store/retrieve operations on our key--value store, which will just be a plain `HashMap<Vec<u8>, Vec<u8>>`. The two will communicate with a pair of [(`mpsc`) channels](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html). The socket task will push messages that each contain a request and a return address into one socket, the store task will process them and, when warranted, return response messages back to the socket task to be sent. Having the store in its own task which only communicates through channels allows us to avoid worrying about locking; there's only ever one scope that reads from or writes to it.

```bash
$ cargo new 04_udp --name udp
```

The `Cargo.toml` should be familiar at this point:

```toml
[package]
name = "udp"
version = "0.1.0"
edition = "2021"

[dependencies]
env_logger = "^0.10"
log = "^0.4"
tokio = { version = "^1", features = ["macros", "net", "rt", "sync"] }
```

And here's the "preamble" portion of our `src/main.rs` (this will be our only code file); all this stuff should make sense:

```rust
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
```

In order to avoid heap allocation[^heap_allocation], we will read requests into and build responses in 1000-byte arrays. We'll go ahead and create a type alias for this buffer "type" and a "constructor" to return us a zeroed array.

[^heap_allocation]: Because we're going `async` when we don't even need to, let's just go ahead and maintain the overengineering theme. Worrying about heap-vs-stack allocation speeds and heap fragmentation is almost assuredly silly in this case, but this honestly doesn't complicate our solution that much.

```rust
/// Array large enough to hold any legal request.
type Buffer = [u8; BUFFSIZE];
/// Contructor for the `Buffer` type that creates zeroed Buffers.
fn buffer() -> Buffer { [0u8; BUFFSIZE] }
```

We'll also define a struct for sending requests and responses to and from the store task; the symmetry of our situation allows us to use a single type for communication in both directions. Because we're using fixed-size buffers, we need to remember to hang on to the actual length of the data.

```rust
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
```

We'll go ahead and define a function for each of our two tasks. Our socket-managing task will `select!` between trying to read a packet from the socket and a `Msg` from the store task. If it gets a packet, it just copies the data into a `Msg` and sends it to the store task; if it gets a `Msg` back, it'll write the contents as a response.

```rust
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
```

Now for the function to manage the store. It'll just listen for messages on one channel, and for each one it'll access the store (if warranted), and (if warranted) build and return a response through the other channel.

```rust
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

        // If the request contains an '=', it's an insert.
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

        // Otherwise, it's a retrieve.
        } else {
            if let Some(val) = store.get(request) {
                // The extra byte in the length of the response is for the '='.
                let length = val.len() + request.len() + 1;

                // The slicing involved in copying the response data into the
                // Buffer is a little tricky.
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
```

And for the main function, we'll just create the two channels an pass the endpoints to our functions. We'll run our functions with the [`tokio::join!`](https://docs.rs/tokio/latest/tokio/macro.join.html) macro, which takes a list of futures and automatically awaits on driving them all to completion. (`join!` will also return a tuple of values, one for each future, but neither of our futures return anything---or even terminate and return at all, for that matter---so we don't care.)

```rust
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
```

That's it. This works, no debugging, no shaking our fists at the compiler; I told you it was going to be easy. Next time we have to write a mischevious[^mischevious] proxy for the chat server we implemented last time, and that will be mildly interesting; after that we're building an automated system for issuing traffic tickets, which will be a dramatic increase in complexity.

[^mischevious]: I hesitate to use the word "malicious" here, and you'll see why.