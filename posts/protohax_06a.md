title: Protohackers in Rust, Part 06 (a)
subtitle: The Protocol
time: 2023-02-18 16:05:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the seventh Protohackers problem

This post is part of a series on solving [Protohackers](https://protohackers.com) problems. Some previous offerings, if you're interested:

  * [Problem 0: Smoke Test](https://d2718.net/blog/posts/protohax_00.html)
  * [Problem 1: Prime Time](https://d2718.net/blog/posts/protohax_01.html)
  * [Problem 2: Means to an End](https://d2718.net/blog/posts/protohax_02.html)
  * [Problem 3: Budget Chat](https://d2718.net/blog/posts/protohax_03.html)
  * [Problem 4: Unusual Database Program](https://d2718.net/blog/posts/protohax_04.html)

Because I am taking the time to blog about these, my Protohackers buddies are leaving me in the dust. Because Problem 6 is far more interesting (not to mention difficult) than Problem 5, we're skipping that one for now and will come back to it. This is going to be _looong_ and split into several entries. I'm going to elide all the stumbling blocks I encountered and backtracking/refactoring I did, or we'll never be done here.[^backtracking]

[^backtracking]: The short version, if you're interested, is that I had a subtle problem with the way I was checking and dealing with whether a vehicle had already received a ticket on a given day. I _thought_, though, that the problem stemmed from the fact that my socket-reading method wasn't cancellation safe, so I spent considerable effort refactoring with that in mind. Of course this didn't fix it, so I flailed around tweaking a great many aspects of the program before I finally pinpointed my problem and fixed it. Ultimately, I ended up with better, leaner code in several places, but the majority of my "fixes" weren't necessary to complete the exercise.

[The Seventh Protohackers Problem](https://protohackers.com/problem/6) is to implement an automatic system for issuing speeding tickets. The specification is pretty complicated; I'll lay out a simplified overview here:

There will be two types of clients, cameras and dispatchers. Cameras will report vehicles (identified by their plates) as being at specific locations on specific roads at specific times. The server's job is to determine, based on locations and times, if and when a vehicle has exceeded the speed limit. If a vehicle warrants a ticket, the server will notifiy a dispatcher client that has responsibility for the road on which that vehicle has exceeded the speed limit.

What makes this tricky is:

  * Time/location stamps will not necessarily arrive in chronological order.
  * Any given vehicle can be issued a ticket for at most one infraction per day (regardless of the number of infractions committed).
  * When the server has calculated that a given vehicle has committed an infraction, there may not necessarily be any connected dispatchers able to respond on the given road, and we'll have to wait until one shows up to issue the ticket.
  * Any client can request that the server send it a "heartbeat" signal at a specified regular interval, and we have to do our best to honor that request.

We also have to support at least 150 clients.

```bash
$ cargo new 06_speed --name speed
```

## The Communication Protocol

The cameras and dispatchers speak a binary protocol[^vaporators] that involves big-endian u8, u16, and u32 integers, as well as length-prefixed strings of 0-255 ASCII characters (and, in one place, a length-prefixed u16 array). Each message begins with a single byte value that determines its type, followed by zero or more specific fields that compose that type. We'll start by defining types to represent them then move on to reading and writing them.

[^vaporators]: Very similar to your vaporators.

All code in this blog entry is from the file `src/bio.rs`. We're going to need all this stuff:

```rust
/*!
Structs and methods for dealing with reading and writing the binary
protocol used in this exercise.

We go to a great deal of effort to make reading a cancellation-safe
process, but in hindsight (having looked at other solutions), this
turns out to not really be necessary.
*/

use std::{
    convert::TryInto,
    fmt::{Debug, Display, Formatter},
    io::{self, Cursor, ErrorKind, Read, Write},
};

use tokio::{
    io::{ReadHalf, WriteHalf},
    net::TcpStream,
};
```

Our first type is the length-prefixed string. It consists of a single unsigned byte denoting its length, followed by that many ASCII characters. Because it can be at most 255 bytes long, we'll just store these in fixed-sized 256-byte arrays.[^string_length] This will require no allocation.

[^string_length]: It turns out that we need much less. I don't think any of the license plates were more than seven characters long; if we didn't choose to send informative error messages, we probably wouldn't need more than seven bytes.

```rust
/// Class to represent the length-prefixed string.
///
/// As they are length-prefixed by a single u8, a 256-byte backing array
/// should be long enough to hold any possible string.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct LPString {
    bytes: [u8; 256],
    length: usize,
}

impl LPString {
    /// Expose the bytes that actually make up the message.
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

/// This is essentially the constructor.
impl<A> From<A> for LPString
where
    A: AsRef<[u8]> + Sized,
{
    fn from(a: A) -> Self {
        let a = a.as_ref();
        let mut bytes = [0u8; 256];
        let mut length = a.len();
        if length > 255 {
            // If the source is too long, we'll just copy what can fit.
            length = 255;
            bytes.copy_from_slice(&a[..length]);
        } else {
            bytes[..length].copy_from_slice(a);
        }

        LPString { bytes, length }
    }
}

impl Display for LPString {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &String::from_utf8_lossy(self.as_slice()))
    }
}

/// The derived Debug impl would just display this as an array of 256
/// numbers, which isn't helpful.
impl Debug for LPString {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LPString({:?})",
            &String::from_utf8_lossy(self.as_slice())
        )
    }
}
```

We have the struct itself, a public method for getting at its bytes, a way to construct them flexibly, and we also implemented our own versions of `Display` and `Debug`; arrays don't do `Display`, and the default `Debug` implementation would just print an array of 256 numbers, so we set these to convert lossily to UTF-8.

Next we have the length-prefixed array of 16-bit unsigned integers. The only use for this is for dispatcher clients to annouce, upon connection, which roads are their responsibility. As with the length-prefixed strings, these technically could have lengths up to 255 (508 bytes), but in practice they are always in the single digits. Because we don't have any other use for these, we will limit them to a much shorter, but still judicious, length.

```rust
/// Maximum length of the array of roads a Dispatcher will announce it's
/// covering. According to the spec, this should be 255, but in practice
/// it's never more than about seven. This should be more than long enougn.
const LPU16ARR_LEN: usize = 32;

/* ... snip ... */

/// The IAmDispatcher message sends a length-prefixed array of u16s.
///
/// We have to read it, but we never have to write one. It functions
/// much like the LPString above.
#[derive(Clone, Eq, PartialEq)]
pub struct LPU16Array {
    data: [u16; LPU16ARR_LEN],
    length: usize,
}

impl LPU16Array {
    /// Expose the set values in the array.
    pub fn as_slice(&self) -> &[u16] {
        &self.data[..self.length]
    }
}

// The derived Debug impl would show the entire LPU16ARR_LEN long backing
// array, and we only need to see the values we care about.
impl Debug for LPU16Array {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "LPU16Array({:?})", &self.as_slice())
    }
}
```

Because we never need to construct these ourselves (we will only ever read them from a TcpStream, using a method we will write later), we don't even have any kind of constructor, just a way to get at the data and a `Debug` implementation.

Given those, we can now define a couple of enums to represent the values that we can read or write from or to connected clients.

```rust
/// Messages sent from connected devices to the server.
#[derive(Debug)]
pub enum ClientMessage {
    Plate { plate: LPString, timestamp: u32 },
    WantHeartbeat { interval: u32 },
    IAmCamera { road: u16, mile: u16, limit: u16 },
    IAmDispatcher { roads: LPU16Array },
}

/// Messages sent back from the server to connected devices.
#[derive(Debug)]
pub enum ServerMessage {
    Error {
        msg: LPString,
    },
    Ticket {
        plate: LPString,
        road: u16,
        mile1: u16,
        timestamp1: u32,
        mile2: u16,
        timestamp2: u32,
        speed: u16,
    },
    Heartbeat,
}
```

These are just data; they don't have any methods of their own. Later we will define a structure to read and write these. This will be a little tricky because we want our reads to be [_cancellation safe_](https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety).[^prior_safety] Our tasks handling client connections are going to have to react to three different types of events: Event messages coming down a channel from the "main" task, a timer specifying when to send heartbeats, and these `ClientMessage`s coming from connected devices. That means we are going to need to `select!` between these futures, hence the necessity for cancellation safety.[^hetero]

[^prior_safety]: You may remember our cancellation-caused woes from [the Budget Chat episode](https://d2718.net/blog/posts/protohax_03.html).

[^hetero]: [This post](https://sunshowers.io/posts/nextest-and-tokio/) by the lead maintainer of [cargo-nextest](https://nexte.st/) is a worthwhile read about how awesome `select!` is.

### Our Read Strategy

The reading strategy we will take here is to use [`AsyncReadExt::read()`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncReadExt.html#method.read)[^read], (which _is_ cancellation-safe) to asynchronously pull some bytes into a buffer, and then read (or attempt to read) synchronously from that buffer (also cancellation-safe by virtue of not being cancellable).

[^read]: Exactly analogous to `std::io::Read::read()`.

It would be great to have a type that could conveniently read individual u8s, u16s, and u32s from our buffer (very much like synchronous versions of some of the methods provided by `AsyncReadExt`), so let's Frankenstein one together. We'll define a trait that does these things, then implement it for [`Cursor`](https://doc.rust-lang.org/std/io/struct.Cursor.html). Because `Cursor` implements the `Read` trait already, we'll make our trait an extension of `Read`, which makes supplying a default implementation easy.

```rust
/// Methods to (synchronously) read the types of values used in the
/// protocol. These are based on similar functions from the AsyncReadExt
/// trait that we'd like to be synchronous. We'll add in ones to read our
/// two complex types (string, array), too.
trait SpeedRead: Read {
    fn read_u8(&mut self) -> io::Result<u8> {
        let mut buff = [0u8; 1];
        self.read_exact(&mut buff)?;
        Ok(unsafe { *buff.get_unchecked(0) })
    }

    fn read_u16(&mut self) -> io::Result<u16> {
        let mut buff = [0u8; 2];
        self.read_exact(&mut buff)?;
        Ok(u16::from_be_bytes(buff))
    }

    fn read_u32(&mut self) -> io::Result<u32> {
        let mut buff = [0u8; 4];
        self.read_exact(&mut buff)?;
        Ok(u32::from_be_bytes(buff))
    }

    fn read_lpstring(&mut self) -> io::Result<LPString> {
        let mut bytes = [0u8; 256];
        let length = self.read_u8()? as usize;
        self.read_exact(&mut bytes[..length])?;
        Ok(LPString { bytes, length })
    }

    fn read_lpu16arr(&mut self) -> io::Result<LPU16Array> {
        let mut data = [0u16; LPU16ARR_LEN];
        let mut length = self.read_u8()? as usize;
        if length > data.len() {
            // If it's too long, just store as much as we can hold and
            // drop the rest on the floor.
            for n in data.iter_mut() {
                *n = self.read_u16()?;
            }
            for _ in data.len()..length {
                _ = self.read_u16()?;
            }
            length = data.len();
        } else {
            for n in data[..length].iter_mut() {
                *n = self.read_u16()?;
            }
        }

        Ok(LPU16Array { data, length })
    }
}

/// And we'll implement it for the Cursor, because that's what we're going
/// to use to read from our buffer.
impl<T: AsRef<[u8]>> SpeedRead for std::io::Cursor<T> {}
```

It was easy enough to add methods for reading our two container types, too.[^array_constructor]

[^array_constructor]: This is where we'll get our `LPU16Array`s.

Now we'll need our buffered `TcpStream` wrapper; we probably don't need to split the `TcpStream` in half, but it makes me less worried about the borrow checker to have the two halves separate.

```rust
/// Length of buffer used to read/write messages. Based on the above
/// limitation on the length of LPU16Arrays, the maximum message
/// length should be somewhat less than this.
const IO_BUFF_SIZE: usize = 300;

/* ... snip ... */

/// Wraps an async TcpStream to do cancellation-safe reading of
/// ClientMessages and writing of ServerMessages.
pub struct IOPair {
    reader: ReadHalf<TcpStream>,
    writer: WriteHalf<TcpStream>,
    /// buffer for reads
    buffer: [u8; IO_BUFF_SIZE],
    /// Index in the buffer up to where incoming data from the TcpStream
    /// has been written. Attempts to read ClientMessages from the buffer
    /// will stop here; incoming writes from the TcpStream will start here.
    write_idx: usize,
}

/// This is our constructor.
impl From<TcpStream> for IOPair {
    fn from(socket: TcpStream) -> Self {
        let (reader, writer) = tokio::io::split(socket);
        IOPair {
            reader,
            writer,
            buffer: [0u8; IO_BUFF_SIZE],
            write_idx: 0,
        }
    }
}
```

First we'll write a method for synchronously reading `ClientMessage`s from our internal buffer, and then we'll use it in our async read function we can `select!` on. On success, this will return not only the message, but also the cursor position (that is, how many bytes we had to read for the message), because our enclosing function will need to know how much of the buffer we've used.

```rust
impl IOPair {
    // Use a cursor to attempt to read a ClientMessage from the buffer.
    // If there isn't a full message in the buffer, the Cursor will
    // return ErrorKind::UnexpectedEof, which we will use as a signal
    // that we need to read more bytes into the buffer.
    fn inner_read(&mut self) -> Result<(ClientMessage, u64), io::Error> {
        let mut c = Cursor::new(&self.buffer[..self.write_idx]);

        let msg_type = c.read_u8()?;

        match msg_type {
            0x20 => {
                let plate = c.read_lpstring()?;
                let timestamp = c.read_u32()?;
                Ok((ClientMessage::Plate { plate, timestamp }, c.position()))
            }

            0x40 => {
                let interval = c.read_u32()?;
                Ok((ClientMessage::WantHeartbeat { interval }, c.position()))
            }

            0x80 => {
                let road = c.read_u16()?;
                let mile = c.read_u16()?;
                let limit = c.read_u16()?;
                Ok((ClientMessage::IAmCamera { road, mile, limit }, c.position()))
            }

            0x81 => {
                let roads = c.read_lpu16arr()?;
                Ok((ClientMessage::IAmDispatcher { roads }, c.position()))
            }

            b => Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("illegal message type: {:x}", &b),
            )),
        }
    }
}
```

We wrap a `Cursor` around the part of our buffer that contains data, and use our added trait methods to read from that. If the `Cursor` hits the end of the data before reading a whole message, it will return a `std::io::Error` of `ErrorKind::UnexpectedEof`; this will signal to our async read method that we need to read more bytes into the buffer.

This brings us to the meat of this module, the `IOPair::read()` method, our `async` method for reading `ClientMessages` on which we can safely `select!`. It starts by first calling the method we just wrote, trying to read a message from the internal buffer. If successful, it will return that message, but not before moving any unread data to the beginning of the buffer and repositioning the buffer's write pointer appropriately. If it _can't_ read a complete `ClientMessage` from the buffer (that is, if `.inner_read()` returns with an `ErrorKind::UnexpectedEof`), it reads some more bytes from the `TcpStream` into the buffer. If this read successfully returns 0 bytes, this either means that the internal buffer is full (the client has exceeded the maximum valid message size without sending a valid message), or that the client has disconnected; in either case, we're done and should hang up. Otherwise, we loop back around and try to read from the internal buffer again. If we encounter a non-fatal error (a `WouldBlock` or `Interrupted`), we'll [yield](https://docs.rs/tokio/latest/tokio/task/fn.yield_now.html), letting some other tasks run before trying again; on any legit error, we'll bail.

```rust
impl IOPair {

    /* ... snip ... */

    /// Read a ClientMessage.
    pub async fn read(&mut self) -> Result<ClientMessage, Error> {
        use tokio::io::AsyncReadExt;

        loop {
            // First, attempt to read from the internal buffer.
            match self.inner_read() {
                Ok((msg, cpos)) => {
                    let cpos = cpos as usize;
                    let extra = self.write_idx - cpos;
                    // If the message just read _from_ the buffer didn't
                    // use all the bytes that have been read _into_ the
                    // buffer, copy those extra significant bytes beyond
                    // the end of the message to the beginning of the
                    // buffer.
                    if extra > 0 {
                        let mut buff = [0u8; IO_BUFF_SIZE];
                        buff[..extra].copy_from_slice(&self.buffer[cpos..self.write_idx]);
                        self.buffer[..extra].copy_from_slice(&buff[..extra]);
                    }
                    self.write_idx -= cpos;
                    return Ok(msg);
                }
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                    /* No message or partial message in buffer. */
                }
                Err(e) if e.kind() == ErrorKind::InvalidData => {
                    return Err(Error::ProtocolError(format!("{}", &e)));
                }
                Err(e) => {
                    return Err(Error::IOError(e));
                }
            }

            // If a complete message couldn't be read from the internal
            // buffer, read more bytes into it.
            match self.reader.read(&mut self.buffer[self.write_idx..]).await {
                Ok(0) => {
                    if self.write_idx == IO_BUFF_SIZE {
                        return Err(Error::ProtocolError(
                            "filled buffer w/o sending valid message".into()
                        ));
                    } else {
                        return Err(Error::Eof);
                    }
                }
                Ok(n) => {
                    self.write_idx += n;
                }
                Err(e)
                    if e.kind() == ErrorKind::Interrupted || e.kind() == ErrorKind::WouldBlock =>
                {
                    tokio::task::yield_now().await;
                }
                Err(e) => {
                    return Err(Error::IOError(e));
                }
            }
        }
    }
}
```

This might seem backward; when thinking about this operation, the natural order of events would seem to be to fill the buffer first, _then_ try to read from it. However, in this case, if a single fill of the buffer brought in more than one message, before getting that second message out of the buffer, we would be trying to read from the `TcpStream` first. Even if there were data immediately available, this would still take much longer and delay that second message unnecessarily. (If more data _weren't_ immediately available to read from the `TcpStream`, we might be `.await`ing quite some time before that second message gets out of the buffer). This way, this function will return all complete messages in the buffer before it tries to read in more bytes.

### Our Write Strategy

This is much easier, because it doesn't have to be cancellation-safe,[^write-cancel] although it almost is, and it'd be easy to make it so. We only have three types of `ServerMessage`s; the `Heartbeat` is only a single byte, and the `Error` will only get sent at most one time per connection. The one we need to pay attention to is the `Ticket`; it's the most involved of all the messages, they may end up getting sent in rapid succession, and it's essentially where the rubber meets the road[^auto-analogy]; the Protohackers test server is largely going to be judging us on whether we dispatch the appropriate tickets. For the `Ticket` variant, we will write the entire thing to an internal buffer, then write the whole thing to the underlying stream with a single `async` write; making repeated small writes to the `TcpStream` would take longer and involve a lot more `.await`ing (and possible task switching) than necessary.

[^write-cancel]: We're not going to be `select!`ing on writes.

[^auto-analogy]: Automotive analogy incidental.

```rust
impl IOPair {

    /* ... snip ... */

    // Writes each of the various types of ServerMessage to the underlying
    // TcpStream. The only reason this function is wrapped is to convert
    // the error type.
    //
    // In the case of the ServerMessage::Ticket, in order to prevent
    // repeated async writes to the underlying TcpStream, we first
    // buffer the output and then write the buffer all at once.
    async fn inner_write(&mut self, smesg: ServerMessage) -> Result<(), io::Error> {
        use tokio::io::AsyncWriteExt;

        match smesg {
            ServerMessage::Heartbeat => {
                self.writer.write_all(&[0x41]).await?;
            }

            ServerMessage::Error { msg } => {
                // msg.length was originally cast _from_ a u8, so this should
                // always succeed.
                let len_byte: u8 = msg.length.try_into().unwrap();
                // Sending this message involves two separate writes. Were
                // we sending a lot of these, we'd probably buffer this data
                // and just make one; as it is, this is always going to be the
                // last message we'll send before hanging up on a client, so
                // we won't bother.
                self.writer.write_all(&[0x10, len_byte]).await?;
                self.writer.write_all(msg.as_slice()).await?;
            }

            ServerMessage::Ticket {
                plate,
                road,
                mile1,
                timestamp1,
                mile2,
                timestamp2,
                speed,
            } => {
                let mut c = Cursor::new([0u8; IO_BUFF_SIZE]);
                // plate.length was originally cast _from_ a u8, so this
                // should always succeed.
                let len_byte: u8 = plate.length.try_into().unwrap();

                c.write_all(&[0x21, len_byte])?;
                c.write_all(plate.as_slice())?;
                c.write_all(&road.to_be_bytes())?;
                c.write_all(&mile1.to_be_bytes())?;
                c.write_all(&timestamp1.to_be_bytes())?;
                c.write_all(&mile2.to_be_bytes())?;
                c.write_all(&timestamp2.to_be_bytes())?;
                c.write_all(&speed.to_be_bytes())?;

                // For some reason Cursor::position() returns a u64 and
                // not a usize. In any case, this should always be less
                // than IO_BUFF_SIZE and thus fit into a usize, regardless
                // of target platform.
                let length: usize = c.position().try_into().unwrap();
                let buff = c.into_inner();
                self.writer.write_all(&buff[..length]).await?;
            }
        }

        self.writer.flush().await
    }

    /// Send a ServerMessage to the connected client.
    pub async fn write(&mut self, smesg: ServerMessage) -> Result<(), Error> {
        self.inner_write(smesg).await.map_err(Error::IOError)
    }
}
```

Like the reading operation, the meat of the writing operation happens in a wrapped function, although this time it's only to convert the error type conveniently.

We complete our `IOError` implementation with a method to cleanly shut down the underlying `TcpStream`. This method consumes it, both because it requires taking ownership of the `.reader` and `.writer` members, and also because there's no use in keeping the `IOError` around after its underlying socket has been shut down.

```rust
impl IOPair {

    /* ... snip ... */

    /// Shuts down the underlying socket so it can be dropped gracefully.
    ///
    /// This consumes the IOPair, as it's useless once the socket is closed.
    pub async fn shutdown(self) -> Result<(), Error> {
        use tokio::io::AsyncWriteExt;

        let mut sock = self.reader.unsplit(self.writer);
        sock.shutdown().await.map_err(Error::IOError)
    }
}
```

## To Be Continued

That's everything we need to handle the specific exchange of bytes through the tubes to our client devices. In the series's next post, we'll go over storing observations, and then overall architecture; in a later post (or posts), we'll then get into the details of the buisness logic.