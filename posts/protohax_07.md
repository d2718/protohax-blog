title: Protohackers in Rust, Part 07
subtitle: The Future Episode
time: 2023-03-23 22:08:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the seventh Protohackers problem
mathjax: true

You are reading an element of a series on solving the
[Protohackers](https://protohackers.com) problems. Some previous offerings,
if you're interested:

  * [Problem 0: Smoke Test](https://d2718.net/blog/posts/protohax_00.html)
  * [Problem 1: Prime Time](https://d2718.net/blog/posts/protohax_01.html)
  * [Problem 2: Means to an End](https://d2718.net/blog/posts/protohax_02.html)
  * [Problem 3: Budget Chat](https://d2718.net/blog/posts/protohax_03.html)
  * [Problem 4: Unusual Database Program](https://d2718.net/blog/posts/protohax_04.html)
  * Problem 6: Line Reversal Protocol
    - [Part a: The Protocol](https://d2718.net/blog/posts/protohax_06a.html)
    - [Part b: Observations and Architecture](https://d2718.net/blog/posts/protohax_06b.html)
    - [Part c: The Client](https://d2718.net/blog/posts/protohax_06c.html)
    - [Part d: The Main Task](https://d2718.net/blog/posts/protohax_06d.html)

[The Eighth Protohackers Problem](https://protohackers.com/problem/7) is to
implement a vaguely TCP-like protocol that can deal with unreliable and
out-of-order transmission, and handle multiple concurrent clients through
the same UDP socket. You then implement a "line-reversal" service on top of
that. (The client will send a series of newline-delimited lines, and your
service will echo each line, with the characters in reverse order, back to
the client as it receives them.)[^dumb_service]

[^dumb_service]: Yeah, this is a pretty dumb service, but it's not really the point of the problem.

It will be helpful to discuss the protocol involved before we start talking
about architecture; we'll do both of those things before we write any code.

Also, at some point, we'll implement the
[`Future`](https://doc.rust-lang.org/std/future/trait.Future.html) trait
ourselves for the first time.[^subtitle]

[^subtitle]: Hence the subtitle of this post.

## A glance at the protocol

Each "message" will be a single UDP packet of not more than 1,000 bytes,
containing entirely ASCII text. Each message will begin with a field that
identifies the "type" of message, followed by a field with a "session ID"
(essentially an identifier that uniquely identifies clients so your service
can tell them apart), followed by zero or more other fields, depending on
message type. Fields are delimited by forward slashes; each message also
begins and ends with a forward slash. This means that slashes within fields
must be escaped with backslashes, and backslashes themselves must then also
be escaped.

An example of each of the four types of messages:

```text
/connect/3263827/

/data/3263827/0/_Either\/Or_ is my favorite Elliot Smith album./

/ack/3263827/46/

/close/3263827/
```

We'll go into more detail later (and you can always look at  [the problem
statement](https://protohackers.com/problem/7) for maximum detail), but
broadly

  * A client sends a `/connect/` message, identifying itself with a string of
    digits (the session ID) to initiate a session.
  * Data is transmitted in both directions with `/data/` messages, and
    receipts are acknowleged with `/ack/` messages.
  * The client sends a `/close/` message when it's finished (or the server can
    send one and hang up[^udp_hangup] if the client is behaving badly).

[^udp_hangup]: As much as you can "hang up" a UDP connection: you just stop worrying about messages with that session ID.

Now we have enough information to make talking about architecture make sense.

## Architecture

This solution will have somewhat of an inverted structure when compared to
several of our previous solutions. Before, we generally had one "main" task
processing information, and a series of "client" tasks, each managing some
socket or connection and communicating via channesl to the main task. Here,
the central task will be managing a single UDP socket, and directing data
to multiple independent processing tasks (one for each client).

ASCII art!

```text
                ┌-----------------------------------┐                        ┌-----------------------------┐
                |                                   |-->--| mpsc |-->-- -->--|                             |
                |                                   |                        |     Single Session Task     |
                |                                   |                        |     -------------------     |
                |                                   |                        |                             |
                |                                   |                        | Tracks all state for single |
                |                                   |                        | session                     |
                |                                   |                        |                             |
                |                                   |                        | Generates response messages |
                |            Main Task              |--<--| mpsc |--<--+--<--| for said session            |
                |            ---------              |                  |     |                             |
   | single |   |                                   |                  |     └-----------------------------┘
===|  UDP   |===| Maintains a map of session IDs    |                  A
   | socket |   | to IP addresses and mpsc channels |                  |     ┌-----------------------------┐
                |                                   |-->--| mpsc |-->--|-->--|                             |
                | Routes incoming messages down     |                  |     |     Single Session Task     |
                | the right channels and outgoing   |                  +--<--|                             |
                | messages to the right addresses   |                  |     └-----------------------------┘
                |                                   |                  A
                |                                   |                  |     ┌-----------------------------┐
                |                                   |-->--| mpsc |-->--|-->--|                             |
                |                                   |                  |     |     Single Session Task     |
                |                                   |                  +--<--|                             |
                /                                   /                  |     └-----------------------------┘
                                                                       |
                /                                   /                  A                etc...
                |                                   |                  |
                └-----------------------------------┘                  /
```

Again, we have individual [`mpsc`](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html)
channels carrying data from the main task to the session tasks, and a single
`mpsc` carrying data back. But instead of isolating the connections with tasks
to simplify I/O, and pooling the data for processing, we're pooling the
connections and isolating the data processing to simplify the processing.

Here's what our `Cargo.toml` is going to look like:

```toml
[package]
name = "line"
version = "0.1.0"
edition = "2021"

[dependencies]
futures = "^0.3"
smallvec = "^1.10"
tokio = { version = "^1.26", features = ["macros", "net", "rt", "sync", "time"] }
tracing = "^0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

We will, of course, be using [Tokio](https://docs.rs/tokio/latest/tokio/index.html);
the `macros` feature so we can use `#[tokio::main]` and `select!`, the
`net` feature for the UDP socket, the `rt` feature because we need a
runtime (obvs), `sync` for the channels, and `time` because we need to
track how long we've been waiting to have our `/data/` messages
`/ack/`nowleged and decide if we need to retransmit them.

We're also using the [`tracing`](https://docs.rs/tracing/latest/tracing/) and
[`tracing-subscriber`](https://docs.rs/tracing-subscriber/0.3.16/tracing_subscriber/)
crates for logging (both of which are also by the `tokio` project). I'm not
going to talk about these, but they're why we have `event!` macro calls instead of
`log::trace!` macro calls.

The "official" [`futures`](https://docs.rs/futures/latest/futures/index.html)
crate gives us [`FuturesUnordered`](https://docs.rs/futures/latest/futures/stream/struct.FuturesUnordered.html),
which we'll use to track message acknowlegement timeouts.

Finally, we have [`smallvec`](https://docs.rs/smallvec/latest/smallvec/),
whose [`SmallVec`](https://docs.rs/smallvec/latest/smallvec/struct.SmallVec.html)
is a largely `Vec`-compatible type that can store a small amount of elements
entirely on the stack. We will make a newtype from these to use as keys
for our main task's map.

## Some types

We'll start by defining and implementing some types that will be used by both
the main and session tasks (some will be used to communicate between them).
This will get us a surprising amount of mileage.

The very top of `src/types.rs` will look like this:

```rust
/*!
Types to be sent between or otherwise used in common between tasks.
*/

use std::{
    borrow::Borrow,
    convert::AsRef,
    fmt::{Debug, Display, Formatter},
    io::Write,
};

use smallvec::SmallVec;
```

### `MsgBlock`

Our first type is the `MsgBlock`, which is just a newtype that wraps a
1 KiB array. These will be used to hold the raw data that comes in
(and later goes out over) the wire. Messages can be at most 1000 bytes,
so an even kilobyte should be able to hold any conforming message.

```rust
/// Buffer length for storing incoming and outgoing messages.
pub const BLOCK_SIZE: usize = 1024;

pub struct MsgBlock([u8; BLOCK_SIZE]);

impl MsgBlock {
    pub fn new() -> MsgBlock { MsgBlock([0u8; BLOCK_SIZE])}
}

impl AsRef<[u8]> for MsgBlock {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl AsMut<[u8]> for MsgBlock {
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.as_mut()
    }
}
```

We have implemented [`AsRef<[u8]>`](https://doc.rust-lang.org/std/convert/trait.AsRef.html)
and [`AsMut<[u8]>`](https://doc.rust-lang.org/std/convert/trait.AsMut.html)
so we can treat our array like a slice and `Write` to it.

### `SessionId`

The main task will maintain a `BTreeMap` whose keys are session IDs to store
routing info for each session (a channel `Sender` end for incoming data, a
return IP address for outgoing data).

>Numeric field values must be smaller than 2147483648. This means sessions are limited to 2 billion bytes of data transferred in each direction.

We don't care about the maximum byte stream length right now, but this does
mean that session IDs will all fit into a signed, 32-bit integer. Using `i32`s
as map keys will involve the main task parsing the incoming digits for each
incoming message. We can, instead, store the bytes of the session ID portion
of the message, and index the map by byte slices. This involves no more
parsing than finding the slashes that delimit the ID field. But we can't just
use slices as keys for a map; that data has to be owned by something. We
could try using a 10-byte array, but not every session ID will be 10 digits
long. We could use a `Vec<u8>`, but that ends up being a lot of little
heap allocations, and we'd like to avoid that.

Enter the [`SmallVec`](https://docs.rs/smallvec/latest/smallvec/struct.SmallVec.html);
it's like a `Vec`, but uses an array as a backing store, and won't allocate on the
heap until it's filled up that array. So we'll use a `SmallVec` with a 10-byte
backing array. We'll wrap it in a newtype and implement `Borrow<[u8]>` so that
we can do lookups on the `BTreeMap` using byte slices.

```rust
/// Maximum length (in digits) of numerical values in the protocol.
pub const NUM_LENGTH: usize = 10;

/// For use as keys in a BTreeMap from session ids to channels.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct SessionId(SmallVec<[u8; NUM_LENGTH]>);

impl AsRef<[u8]> for SessionId {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// We are using these as keys in a BTreeMap, and we want to be able to
/// make table lookups using byte slices.
impl Borrow<[u8]> for SessionId {
    fn borrow(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// This is so we can copy these directly from the bytes of an incoming
/// packet.
impl From<&[u8]> for SessionId {
    fn from(bytes: &[u8]) -> SessionId {
        let v = if bytes.len() > NUM_LENGTH {
            SmallVec::from_slice(&bytes[..NUM_LENGTH])
        } else {
            SmallVec::from_slice(bytes)
        };
        SessionId(v)
    }
}

impl Display for SessionId {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "[")?;
        for &b in self.as_ref().iter() {
            write!(f, "{}", b as char)?;
        }
        write!(f, "]")
    }
}

/// The derived Debug impl prints it as a slice of bytes. We want to see
/// the numbers.
impl Debug for SessionId {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "SessionId([{}])", self)
    }
}
```

You'll notice that we derive a bunch of traits; several of these are necessary
for use as keys in a `BTreeMap`. We also implement `Display` and use it to
write a custom `Debug` impl. This is because (as the commentary mentions) the
derived `Debug` impl would just print it like a slice of bytes, and we want
it to actually look like a string of digits in our log and error messages.

### `Pkt`

The `Pkt` type will represent a partially-parsed incoming message. The main
task has a lot of work to do, so it will only parse as much of each incoming
message as it needs to in order to route it to the appropriate session task.
This type will hold the raw data and as much helpful metadata as the main
task has gleaned from it.

First we define an enum to represent the message type.

```rust
/// The four types of messages in the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PktType {
    Ack,
    Close,
    Connect,
    Data,
}
```

The `Pkt` will (in addition to the raw data) store its type, pointers to the
byte range of its ID digits (so it can easily be sliced and ID'd), and its
length (because we get this for free when reading from the UDP socket, and
it's handy to have).

We will implement a constructor that takes a `MsgBlock` into which we have
read a UDP packet and does the necessary partial parsing. We'll also implement
an `.id()` method that returns the byte slice of its ID digits, and we'll
give it a custom `Debug` implementation for the same reason as before: We
want to see the text of the message in our logging, not the integer values
of the bytes.

```rust
/// The partially-parsed data from a single UDP packet.
///
/// Enough of the data is parsed to determine its type and the session ID.
pub struct Pkt {
    pub ptype: PktType,
    pub data: MsgBlock,
    pub id_start: usize,
    pub id_end: usize,
    pub length: usize,
}

impl Pkt {
    /// Given a 1KB block containing the raw data from a UDP packet, parse
    /// enough of it to give us a `Pkt`.
    pub fn new(data: MsgBlock, length: usize) -> Result<Pkt, &'static str> {
        let bytes = data.as_ref();
        if length == 0 {
            return Err("no data");
        } else if bytes[0] != b'/' {
            return Err("no initial /");
        } else if bytes[length-1] != b'/' {
            return Err("no final /");
        }
        
        let second_slash = 1 + bytes[1..length].iter()
            .position(|&b| b == b'/')
            .ok_or("no second /")?;
        let ptype = match &bytes[1..second_slash] {
            b"ack" => PktType::Ack,
            b"close" => PktType::Close,
            b"connect" => PktType::Connect,
            b"data" => PktType::Data,
            _ => return Err("unrecognized MsgType"),
        };
        
        let id_start = second_slash + 1;
        let id_end = id_start + bytes[id_start..length].iter()
            .position(|&b| b == b'/' )
            .ok_or("no third /")?;
        if matches!(ptype, PktType::Close | PktType::Connect) {
            if id_end + 1 != length {
                return Err("too many fields");
            }
        }
        
        Ok(Pkt { ptype, data, id_start, id_end, length})
    }

    /// Expose the bytes of the session ID portion of the message.    
    pub fn id(&self) -> &[u8] {
        &self.data.as_ref()[self.id_start..self.id_end]
    }
}

/// The derived Debug impl will just print this as a 1KiB slice of u8s;
/// we want to see the actual text.
impl Debug for Pkt {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.debug_struct("Pkt")
            .field("ptype", &self.ptype)
            .field("id", &String::from_utf8_lossy(self.id()))
            .field("data", &String::from_utf8_lossy(&self.data.as_ref()[..self.length]))
            .finish()
    }
}
```

### `Response`

The `Response` type will represent an _outgoing_ message. These will be
generated by the individual session tasks and returnd to the main task
for delivery. Because they are generated by the session tasks, we will
skip the implementation of the constructor functions for now, and flesh
them out when we talk about the session tasks.

```rust
/// A response to be sent back to the client.
pub struct Response {
    data: MsgBlock,
    pub rtype: PktType,    
    length: usize,
    id_start: usize,
    id_end: usize,
}

impl Response {

    // ...Constructor methods elided...

    /// Expose the bytes of the session ID.
    pub fn id(&self) -> &[u8] {
        &self.data.as_ref()[self.id_start..self.id_end]
    }

    /// Expose the slice containing the whole message.
    pub fn bytes (&self) -> &[u8] {
        &self.data.as_ref()[..self.length]
    }
}

/// The derived Debug impl will spit out the data as a 1KiB slice of bytes;
/// we just want to read the text.
impl Debug for Response {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("data", &String::from_utf8_lossy(&self.data.as_ref()[..self.length]))
            .finish()
    }
}
```

