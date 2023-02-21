title: Protohackers in Rust, Part 06 (c)
subtitle: The Client
time: 2023-02-20 13:08:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the seventh Protohackers problem
mathjax: true

You are reading an element of a series on solving the [Protohackers](https://protohackers.com) problems. Some previous offerings, if you're interested:

  * [Problem 0: Smoke Test](https://d2718.net/blog/posts/protohax_00.html)
  * [Problem 1: Prime Time](https://d2718.net/blog/posts/protohax_01.html)
  * [Problem 2: Means to an End](https://d2718.net/blog/posts/protohax_02.html)
  * [Problem 3: Budget Chat](https://d2718.net/blog/posts/protohax_03.html)
  * [Problem 4: Unusual Database Program](https://d2718.net/blog/posts/protohax_04.html)

This post itself is the third in a _subseries_ on solving [The Seventh Problem](https://protohackers.com/problem/6). [Here is the first part](https://d2718.net/blog/posts/protohax_06a.html); [here is the second part](https://d2718.net/blog/posts/protohax_06b.html).

## The Heartbeat

Here's a thorny detail of our client tasks' operation: At any point, a connected device can request that we supply it with a "heartbeat" message at regular intervals, in addition to the business of reading and writing the data that's part of the client's usual operation. Let's work that out.

Our client task will, naturally, be looping over a `select!` statement; sometimes that `select!` needs to have something that resolves at regular intervals, and sometimes it doesn't. So we'll define a struct that contains just an [`Option<tokio::time::Interval>`](https://docs.rs/tokio/latest/tokio/time/fn.interval.html); the `Interval` is a type that has an `async` method whose return type resolves at regular intervals. Here's an example:

```rust
use std::time::Duration()

#[tokio::main]
fn main() {
  let mut i = tokio::time::interval(Duration::from_millis(20));

  i.tick().await; // First one resolves immediately.
  i.tick().await; // This will resolve 20 milliseconds later.
  some_expensive_task();
  // The following line will resolve exactly 20 ms after the previous
  // tick (as long as some_expensive_task() takes less than 20 ms).
  i.tick().await;
}
```

Our `Heartbeat` type that wraps the `Interval` will have a method that resolves regularly when it's supposed to be beating, and then if it's not supposed to be beating, it just never resolves. This way we can use the same `select!` statement, and the heartbeat arm will just act like it's not there when the client hasn't requested a heartbeat.

The tippy top of our `src/clients.rs`:

```rust
/*!
Bookkeeping and behavior of the types of clients.
*/
use std::time::Duration;

use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, UnboundedSender},
    time::{interval_at, Instant, Interval},
};
use tracing::{event, span, Level};

use crate::{
    bio::*,
    events::Event,
    obs::{Infraction, Obs},
};
```

Our struct is simple:

```rust
/// A struct to provide an optional heartbeat.
///
/// Can be set to either resolve at regular intervals, or never, so it can
/// be used in a join! either way.
struct Heartbeat(Option<Interval>);
```

The constructor and the following two methods should be pretty self-explanatory.

```rust

impl Heartbeat {
    fn new() -> Heartbeat {
        Heartbeat(None)
    }

    /// Set this heart to not beat.
    fn stop(&mut self) {
        self.0 = None;
    }

    /// Return whether this heartbeat is beating.
    fn is_beating(&self) -> bool {
        self.0.is_some()
    }
}
```

The method to start it beating will take the heartbeat interval arugment in deciseconds, because that's the way the client sends it to us. This method is complicated slightly by the fact that the `Interval`'s first tick always resolves _immediately_, and we don't want it to start beating until one of the intervals has passed. So intead of `tokio::time::interval()`, we create one with [`tokio::time::interval_at()`](https://docs.rs/tokio/latest/tokio/time/fn.interval_at.html), which allows you to delay the `Interval`'s start until some point in the future; we will delay it exactly one beat interval.

```rust
impl Heartbeat {

    /* ... snip ... */

    /// Set this heart to beat every `n` deciseconds.
    fn set_decisecs(&mut self, n: u32) {
        if n == 0 {
            self.stop();
            return;
        }
        let millis: u64 = (n as u64) * 100;
        let period = Duration::from_millis(millis);
        self.0 = Some(interval_at(
            // Set it to start exactly one period from now.
            Instant::now().checked_add(period).unwrap(),
            period,
        ));
    }
}
```

And then finally the `.beat()` method, which, if the heartbeat is set, returns after the `Interval`'s `.tick()` resolves; if it's not set, it returns after [`std::future::pending()`](https://doc.rust-lang.org/std/future/fn.pending.html) which, you may have guessed, never resolves.

```rust
impl Heartbeat {

    /* ... snip ... */

    /// Either resolves or doesn't, depending how this Heartbeat is set.
    async fn beat(&mut self) {
        if let Some(ref mut i) = &mut self.0 {
            i.tick().await;
        } else {
            std::future::pending::<()>().await;
        }
    }
}
```

Don't worry about these `pending()` futures accumulating or piling up or whatever; remember that these are run in a `select!` statement, which _cancels_ all the futures except the one that resolves first.

## The Client

We will use a struct to hold the various handles that a client task needs and give it methods to manage the task. Each client will be given a unique ID number by the main task; this will allow us to emit more useful debugging messages, as well as allow the client task to identify itself in `Event` messages it sends back to the main task. We'll start with its constructor, which will be called from the main task before its forked off into its own subtask.

```rust
/// When a client first connects, it is unidentified.
///
/// Once an IAmCamera or IAmDispatcher message is received, this will then
/// run as the appropriate type.
pub struct Client {
    id: usize,
    heart: Heartbeat,
    socket: IOPair,
    /// Channel to the main task.
    tx: UnboundedSender<Event>,
    /// Channel from the main task.
    rx: Receiver<Infraction>,
}

impl Client {
    pub fn new(
        id: usize,
        socket: TcpStream,
        tx: UnboundedSender<Event>,
        rx: Receiver<Infraction>,
    ) -> Client {
        Client {
            id,
            tx,
            rx,
            heart: Heartbeat::new(),
            socket: IOPair::from(socket),
        }
    }
}
```

Let's stub out our `Client`'s function with some pseudocode first; I think that'll be clearer than a wall of sentences with a bunch of conditional clauses. We'll wrap the meat of our `.run()` function in an outer function to do some error handling and ensure that the connection gets shutdown nicely regardless of how the function ends.

```
run function:
    - call inner run function
    - log any returned errors
    - inform client of errors if appropriate
    - shutdown socket
    - send main task disconnection Event

inner run function:
    select! loop:

        < receive message from client
            IAmCamera =>
                - inform main task we're a camera
                - run as camera
            IAmDispatcher =>
                - inform main task which roads we cover
                - run as dispatcher
            WantHeartbeat =>
                if already beating, return with error
                otherwise start heartbeat
            any other message => return with error

        < heartbeat resolves
            send hearbeat message to client

run as camera function:
    - close socket from main task

    select! loop:

        < receive message from client
            Plate => send observation Event to main task
            WantHeartbeat =>
                if already beating, return with error
                otherwise start heartbeat
            any other message => return with error
        
        < heartbeat resolves
            send heartbeat message to client

run as dispatcher function:
    select! loop:

        < receive message from client
            WantHeartBeat =>
                if already beating, return with error
                otherwise start heartbeat
            any other message => return with error
        
        < receive Infraction in channel from main task
            send ticket message to client
        
        < heartbeat resolves
            send heartbeat message to client
```

At any point, if the client disconnects, receiving a message from it will return an error (even if the disconnection is clean, recall our `bio::Error::Eof` variant), and we'll return an error back to the outer `.run()` function for cleanup and shutdown.

Let's start with our wrapped fun functions, because that will give us a more concrete idea of what we'll need to take care of in our outer run function. First, we'll need this guy:

```rust
/// Error message with which we'll panic if any of the client tasks find the
/// channel to the main task closed. This should never happen, and if it does,
/// the program can't do any more work anyway, so it's totally reasonable
/// to just die.
static CHANNEL_CLOSED: &str = "main channel closed";
```

Okay, here we go.

```rust
impl Client {

    /* ... snip ... */

    async fn run_as_camera( ... ) {
        // We're going to call this function, but we haven't written it  yet.
    }

    async fn run_as_dispatcher( ... ) {
        // Same deal here.
    }

    async fn wrapped_run(&mut self) -> Result<(), Error> {
        loop {
            tokio::select! {
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message: {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::IAmCamera{ road, mile, limit } => {
                            let evt = Event::Camera{ id: self.id };
                            self.tx.send(evt).expect(CHANNEL_CLOSED);
                            // Server reports limit in mi/hr, but all of our
                            // calculations (and our dispatching) are done in
                            // hundredths of mi/hr.
                            let limit = limit * 100;
                            return self.run_as_camera(
                                road, mile, limit
                            ).await;
                        },

                        ClientMessage::IAmDispatcher{ roads } => {
                            let evt = Event::Dispatcher{
                                id: self.id, roads
                            };
                            self.tx.send(evt).expect(CHANNEL_CLOSED);
                            return self.run_as_dispatcher().await;

                        },

                        ClientMessage::WantHeartbeat{ interval } => {
                            if self.heart.is_beating() {
                                return Err(Error::ProtocolError(
                                    "multiple heartbeat requests".into()
                                ));
                            } else {
                                self.heart.set_decisecs(interval);
                            }
                        },

                        msg => {
                            return Err(Error::ProtocolError(format!(
                                "illegal unident msg: {:?} ", &msg
                            )));
                        },
                    }
                },

                _ = self.heart.beat() => {
                    self.socket.write(ServerMessage::Heartbeat).await?;
                },
            }
        }
    }
```

We see from this what the signature of our `.run_as_camera()` method needs: the camera client's road, mile marker, and that road's speed limit. So let's bang out that one next.

```rust
impl Client {
    
    /* ... snip ... */

    async fn run_as_camera(
      &mut self,
      road: u16,
      mile: u16,
      limit: u16
    ) -> Result<(), Error> {
        // Don't need this anymore.
        self.rx.close();

        span!(Level::TRACE, "running as Camera", client = self.id);
        loop {
            tokio::select! {
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::Plate{ plate, timestamp } => {
                            let pos = Obs{ mile, timestamp};
                            let evt = Event::Observation{
                                plate, road, limit, pos
                            };
                            self.tx.send(evt).expect(CHANNEL_CLOSED);
                        },

                        ClientMessage::WantHeartbeat{ interval } => {
                            if self.heart.is_beating() {
                                return Err(Error::ProtocolError(
                                    "multiple heartbeat requests".into()
                                ));
                            } else {
                                self.heart.set_decisecs(interval);
                            }
                        },

                        msg => {
                            return Err(Error::ProtocolError(
                                format!("illegal Camera msg: {:?}", &msg)
                            ));
                        },
                    }
                },

                _ = self.heart.beat() => {
                    self.socket.write(ServerMessage::Heartbeat).await?;
                }
            }
        }
    }
}
```

And then finally our method for dispatcher clients. We've told the main task which roads we're responsible for, we don't even need to know that to do our job. We just issue tickets when the main task says so.

```rust
impl Client {
    
    /* ... snip ... */

    async fn run_as_dispatcher(&mut self) -> Result<(), Error> {
        span!(Level::TRACE, "running as Dispatcher", client = self.id);
        loop {
            tokio::select! {
                msg = self.socket.read() => {
                    event!(Level::TRACE, "client {} message {:?}", &self.id, &msg);
                    match msg? {
                        ClientMessage::WantHeartbeat{ interval } => {
                            if self.heart.is_beating() {
                                return Err(Error::ProtocolError(
                                    "multiple heartbeat requests".into()
                                ));
                            } else {
                                self.heart.set_decisecs(interval);
                            }
                        },

                        msg => {
                            return Err(Error::ProtocolError(
                                format!("illegal Dispatcher msg: {:?}", &msg)
                            ));
                        },
                    }
                },

                msg = self.rx.recv() => {
                    event!(Level::TRACE,
                        "client {} recv'd: {:?}", &self.id, &msg
                    );
                    match msg {
                        Some(Infraction{ plate, road, start, end, speed }) => {
                            let msg = ServerMessage::Ticket{
                                plate, road, speed,
                                mile1: start.mile,
                                timestamp1: start.timestamp,
                                mile2: end.mile,
                                timestamp2: end.timestamp,
                            };
                            self.socket.write(msg).await?;
                            event!(Level::DEBUG,
                                "client {} wrote Infraction for {:?}", &self.id, &plate
                            );
                        },

                        None => {
                            event!(Level::ERROR, "Dispatcher {} channel closed", &self.id);
                            let msg = ServerMessage::Error {
                                msg: LPString::from(
                                    &"server error, sorry"
                                )
                            };
                            self.socket.write(msg).await?;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

```

I'm going to make one refinement here. Notice that the `ClientMessage::WantHeartbeat` match arms are identical in all three methods. I'm going to abstract that code out into a `Client::try_start_heart()` method that returns an `Error` if the heart is already beating, and collapse down the contents of those arms.

```rust
impl Client {

    /* ... snip ... */

    async fn try_start_heart(&mut self, interval: u32) -> Result<(), Error> {
        if self.heart.is_beating() {
            return Err(Error::ProtocolError(
                "multiple heartbeat requests".into()
            ));
        } else {
            self.heart.set_decisecs(interval);
            Ok(())
        }
    }

    /* ... snip ... */

}
```

And now each of the `WantHeartbeat` match arms looks like

```rust
                        ClientMessage::WantHeartbeat{ interval } => {
                            self.try_start_heart(interval).await?
                        },
```

Which is much more concise.

You can see also now why our channels from the main task to the client tasks carry a special type (`obs::Infraction`) and not just `ServerMessage::Ticket` variants. In the latter case the client tasks would have to match on the received value, and we'd have to make a decision about what we wanted to do about variants of `ServerMessage` that _weren't_ `Ticket`. By using the `Infraction` type, we avoid noise at the `.recv()` site[^decision-fatigue] and also make it impossible to screw up when writing our main task by sending the wrong type of message.[^screwup]

[^decision-fatigue]: And are also freed from making decisions about what to do with invalid message variants.

[^screwup]: We can still obviously screw it up plenty of other ways, but let's go ahead and count this particular blessing here.

So now the outer `.run()` method. It'll take `mut self` and consume the `Client` struct; obviously we won't need it anymore after it's done running. Also, this will allow us to move the `IOPair` into its own self-consuming `.shutdown()` method at the end. We'll run our `.wrapped_run()` method, and we'll react differently based on the returned `Error` variant[^inevitable]

  * `Error::Eof` means that the client disconnected cleanly, so we won't generate any error messages.
  * `Error::IOError` means that we encountered an actual read/write error. We'll log it and inform the connected device that we screwed up.
  * `Error::ProtocolError` means the connected device sent us some invalid data or otherwise didn't adhere to the protocol; we let them know it was their fault.

Then we'll close the socket and let the main task know the device has disconnected.

[^inevitable]: You may have noticed that it will always return an `Err`. There are no breaks out of the loop or `Ok(())` returns. We could have written this function (and the other run functions that it wraps) to just return our own `RunResult` enum type or something, but then we wouldn't have gotten the convenience and concision that comes with using `?` on `Result`s.

```rust
impl Client {

    /* ... snip ... */

    pub async fn run(mut self) {
        span!(Level::TRACE, "run", client = &self.id);

        if let Err(e) = self.wrapped_run().await {
            match e {
                Error::Eof => { /* clean */ }
                Error::IOError(_) => {
                    event!(Level::ERROR, "client {}: {:?}", &self.id, &e);
                    let msg = ServerMessage::Error {
                        msg: LPString::from(&"the server encountered an error"),
                    };
                    let _ = self.socket.write(msg).await;
                }
                Error::ProtocolError(cerr) => {
                    event!(Level::ERROR, "client {} error: {}", &self.id, &cerr);
                    let msg = ServerMessage::Error {
                        msg: LPString::from(&cerr),
                    };
                    let _ = self.socket.write(msg).await;
                }
            }
        }

        if let Err(e) = self.socket.shutdown().await {
            event!(
                Level::ERROR,
                "client {}: error shutting down socket: {:?}",
                &self.id,
                &e
            );
        }
        if let Err(e) = self.tx.send(Event::Gone { id: self.id }) {
            event!(Level::ERROR, "error sending Gone({}): {:?}", &self.id, &e);
        }
        event!(Level::TRACE, "client {} disconnects", &self.id);
    }
}
```

## One More Episode

We have now specified the workings of our client tasks. Next time, in the final post about Problem 6, we'll write our main task. Now that we have everything else written, we should have a pretty coherent idea of what needs to happen, and the process should be pretty straightforward.