title: Protohackers in Rust, Part 02
subtitle: In which we learn an important lesson about traits
time: 2023-01-21 11:50:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the third Protohackers problem

(This is the third post in a series. If you haven't already, you may wish to first consume [part 0](https://d2718.net/blog/posts/protohax_00.html) and [part 1(https://d2718.net/blog/posts/protohax_01.html).)

In [the third Protohackers problem](https://protohackers.com/problem/2), each client will send us a series of prices with timestamps for some "asset", and then query us with time intervals over which we are supposed to respond with the average price of that asset over the requested interval. This will involve two new things that previous problems did not:

  1. We will have to keep track of some state over the course of each client's lifetime.
  2. The message format involved with be _binary_, and not a textual format, so we can't lean on [Serde](https://serde.rs) like we have in the past.[^serde]

[^serde]: Or rather we _can_, but it'd be more trouble than it's worth, so we _won't_.

## The Message Format

Each message a client will send will be nine bytes long. The first byte will identify the message type; the following two chunks of four bytes each will be big-endian, signed, 32-bit integers, whose meaning will be dependent on the type of message.

There are two types of message:

```
 byte # | 0  u8    | 1..5  i32  | 5..9  i32 |
Insert: | 73 ('I') | timestamp  | price     |
 Query: | 81 ('Q') | start time | end time  |
```

An "Insert" message will tell us the price of the client's asset at a particular point in time; a "Query" message requests the average price of that asset between the provided start and end times (inclusive).[^time_avg] We'll start by defining an enum to represent a possible message.

[^time_avg]: The "average" requested is the average of all the inserted price values with timestamps in the supplied range, _not_ (thankfully) the timewise average of the asset over the requested interval (which would definitely be harder).

`src/msg.rs`:

```rust
/*!
Reading the 9-byte message format.
*/

/// Represents the types of messages expected from clients.
#[derive(Debug, Clone, Copy)]
pub enum Msg {
    Insert{ timestamp: i32, price: i32, },
    Query{ begin: i32, end: i32, }
}
```

Now, for reading these from a client, we're going to be clever[^clever] and define a trait for reading messages from an asynchronous reader, and we'll suck messages out using it.

[^clever]: As it so often does in software development, this will end up working out poorly for us.

```rust
/// A trait for reading `Msg`s from an async reader.
pub trait MsgReader: AsyncReadExt + Unpin {

    /// Read a `Msg`.
    async fn read_msg(&mut self) -> Result<Msg, String> {
        let mut buff = [0u8; 9];
        self.read_exact(&mut buff).await.map_err(|e| format!(
            "read error: {}", &e
        ))?;

        // This stanza is mildly hinky. We're going to copy the two 32-bit
        // chunks into this four-byte array, then convert each one
        // big-endianly into an i32.
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
```

Great, now we can just call this method on our `TcpStream` or whatever and get `Msg`s right from the source.

```
$ cargo check
    Checking means v0.1.0 (/home/dan/blog/protohax-blog/02_means)
error[E0706]: functions in traits cannot be declared `async`
  --> src/msg.rs:17:5
   |
17 |     async fn read_msg(&mut self) -> Result<Msg, String> {
   |     -----^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |     |
   |     `async` because of this
   |
   = note: `async` trait functions are not currently supported
   = note: consider using the `async-trait` crate: https://crates.io/crates/async-trait
   = note: see issue #91611 <https://github.com/rust-lang/rust/issues/91611> for more information

For more information about this error, try `rustc --explain E0706`.
error: could not compile `means` due to previous error
```

Oh, shazbat. Sad trombone noise.

## Rust Doesn't Support `async` Traits

Or, more accurately, it doesn't support `async` functions in traits, at least [not out of the box](https://crates.io/crates/async-trait), [not quite yet](https://github.com/rust-lang/rust/issues/91611).[^why_hard]

[^why_hard]: [This](https://smallcultfollowing.com/babysteps/blog/2019/10/26/async-fn-in-traits-are-hard/) is a good discussion of why this is hard.

But what about Tokio with [all its `async` I/O traits](https://docs.rs/tokio/latest/tokio/io/index.html#traits)? We'be been using (for example) `AsyncReadExt` and `AsyncWriteExt` liberally these past couple of problems. What's up with those? We've been `.await`ing all those trait methods.

```rust
// A sampling from the first two solutions.

let n_read = sock.read_to_end(&mut buff).await?;

sock.write_all(&buff).await?;

match sock.read(&mut buff).await // { ...

match reader.read_line(&mut buff).await // { ...

```

Well, if you look at, for example, [the `AsyncReadExt` documentation](https://docs.rs/tokio/latest/tokio/io/trait.AsyncReadExt.html), you'll see that _none of those functions are `async`_. Each one is a "synchronous" function that returns a specialized [`Future`](https://doc.rust-lang.org/std/future/trait.Future.html) type. For most of these methods, the documentation gives an example of an `async` signature that would be equivalent. For [`AsyncReadExt::read_exact()`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncReadExt.html#method.read_exact), whose actual signature is[^missing_bound]

```rust
fn read_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> ReadExact<'a, Self>
```

[^missing_bound]: I have left out the `where Self: Unpin` trait bound because this detail distracts from the issue at hand; please don't toot at me pointing this out.

the documentation explicitly says this is equivalent to

```rust
async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<usize>
```

Yeah, that Tokio team is a bunch of real Chads.

Okay, so we can't be as cool as we want to be without getting into writing our own future types, which is definitely beyond the scope of only the third post in this series.[^future_futures] However, we don't need to throw our neat method out; we'll just turn it around and `impl` it on the `Msg` type, taking an `AsyncReadExt` as an argument. This doesn't feel quite as elegant to me, but at least the compiler will let us do it. Here's our newly-legal `src/msg.rs`:

[^future_futures]: But which we might conceivably do in a future installment. Maybe we could even call it the `Future` installment.

```rust
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

impl Msg {

    /// Suck a `Msg` out of the provided async reader.
    pub async fn read_from<R>(reader: &mut R) -> Result<Msg, String>
    where
        R: AsyncReadExt + Unpin
    {
        let mut buff = [0u8; 9];
        reader.read_exact(&mut buff).await.map_err(|e| format!(
            "read error: {}", &e
        ))?;

        // This stanza is mildly hinky. We're going to copy the two 32-bit
        // chunks into this four-byte array, then convert each one
        // big-endianly into an i32.
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
```
## Handling Connections

Because our clients still don't need to communicate or share data in any way, our `main()` function will essentially be the same as the last two: We'll listen on a port and pass each successful connection to our handler function in its own spawned task. Let's just go ahead and get that out of the way here:

`src/main.rs`:

```rust
/*!
Protohackers Problem 2: Tracking Prices

Keeping track of asset prices over time and reporting averages.
[Full spec.](https://protohackers.com/problem/2)
*/

use tokio::net::{TcpListener, TcpStream};

pub mod msg;

const LOCAL_ADDR: &str = "0.0.0.0:12321";

async fn handler(mut socket: TcpStream, client_n: usize) {
    // To do
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
```

Okay, let's think about our handler function. We need to read `Msg`s from the connection; we'll store the (timestamp, price) data from each `Msg::Insert`, and when we receive a `Msg::Query` we'll iterate through the appropriate range of stored price values and average them, and respond (as per the spec) with the average as a single big-endian `i32`. Also, according to the spec,[^nice_spec]

  * we can round this value in _either_ direction, at our discretion
  * we should send back 0 if there are no prices in the requested interval (or if there can't possibly be any prices, because the end is before the start, for instance)

To store our timestamped values, I'm going to reach for the [`BTreeMap`](https://doc.rust-lang.org/stable/std/collections/struct.BTreeMap.html). It's easily iterable, and entries will always be inserted in key order (unlike with a `Vec`, where insertions will be more complicated if prices arrive out of chronological order, which we're warned they might). Let's write what we can so far.

Excerpts of `src/main.rs`:

```rust
use std::collections::BTreeMap;

use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream}
};

// a couple of lines elided

async fn handler(mut socket: TcpStream, client_n: usize) {
    let mut prices: BTreeMap<i32, i32> = BTreeMap::new();

    loop {
        match Msg::read_from(&mut socket).await {
            Ok(Msg::Insert{ timestamp: t, price: p }) => {
                prices.insert(t, p);
            },
            Ok(Msg::Query{ begin: begin, end: end }) => {
                // We need to figure out how to average over the appropriate
                // section of our `prices` map.
            },
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
```

Now what we need is a function to iterate through a BTreeMap and average all the values in a given range of keys. We'll trade some generality for ease in this particular case by having it also return 0 on empty or invalid ranges.

```rust
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

    if n == 0 { 0 }
    else { (tot / n) as i32 }
}
```

And then we modify our handler function to use it:

```rust
            Ok(Msg::Query{ begin, end }) => {
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
```

And this satisfies the test now, but like last time, I think we could do better.

## A Small Refactor

This, in particular is bothering me:


```
$ RUST_LOG=error ./means
[2023-01-20T20:04:05Z ERROR means] Client 0: read error: early eof
[2023-01-20T20:04:14Z ERROR means] Client 4: read error: early eof
[2023-01-20T20:04:14Z ERROR means] Client 3: read error: early eof
[2023-01-20T20:04:14Z ERROR means] Client 2: read error: early eof
[2023-01-20T20:04:14Z ERROR means] Client 1: read error: early eof
[2023-01-20T20:04:18Z ERROR means] Client 5: read error: early eof
[2023-01-20T20:04:20Z ERROR means] Client 9: read error: early eof
[2023-01-20T20:04:20Z ERROR means] Client 8: unrecognized type: 106
[2023-01-20T20:04:20Z ERROR means] Client 6: read error: early eof
[2023-01-20T20:04:20Z ERROR means] Client 7: read error: early eof
[2023-01-20T20:04:22Z ERROR means] Client 10: read error: early eof
```

It looks like we're throwing[^throw] an error every time a client disconnects. My guess is that every time `Msg::read_from()` calls `.read_exact()` on a disconnected client, it's returning an [`ErrorKind::UnexpectedEof`](https://doc.rust-lang.org/stable/std/io/enum.ErrorKind.html#variant.UnexpectedEof). We _expect_ clients to disconnect eventually; I would rather not bubble up an error in this case.

[^throw]: Rust doesn't really _throw_ errors; what it does is more like gently underhanding them up to the calling stack frame.

How could we refactor this to achieve the desired behavior?

  * We could change `Msg::read_from()` to return a `std::io::Result<Msg>` so the caller could examine the `ErrorKind` and forego reporting an error on this particular variant. This would necessitate building and returning a `std::io::Error` when the first byte of the message isn't an expected `b'I'` or `b'Q'`, which doesn't quite feel like the right thing to do. It would also impose the complication of mucking around with `std::io::ErrorKind`s on the caller.
  * We could define our own `Result`-like enum with a third variant for `Eof`, but this would lose the benefits of working with an actual `std::result::Result`.[^result]
  * We could define our own combined error type, with a variant for `std::io::Error`s and a variant for reporting invalid query type values. This seems like a lot of fuss for what is more or less an extra bit of information.

[^result]: Not that we're using them, except for maybe the benefit of least surprise.

There are probably even worse things we could do (and undoubtedly better things that I just haven't thought of), but I think the easiest solution in this particular case is to just change the signature of `Msg::read_from()` to return a `Result<Option<Msg>, String>`, and return `Ok(None)` if the client has disconnected. We're not signalling an _erroneous_ situation; we're just signalling that there are no more `Msg`s to read.

So here is our modified `src/msg.rs`:

```rust
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
```

and our `src/main.rs`:

```rust
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
```

And this both satisfies the test and assuages my mild aesthetic misgivings.

```
$ RUST_LOG=error ./means
[2023-01-21T16:42:48Z ERROR means] Client 8: unrecognized type: 63
```

The only error reported now is when someone sends a message with an illegal type.

The next problem involves implementing a simple chat server, and so our various connections are going to have to interact. This will complicate things noticeably and also introduce us to a couple of types from the [`tokio::sync`](https://docs.rs/tokio/latest/tokio/sync/index.html) module.