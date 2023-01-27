title: Protohackers in Rust, Part 03
subtitle: Synchronization and Cancellation
time: 2023-01-22 20:54:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the fourth Protohackers problem

(This is the fourth post in a series. You can click on the navigation links above to go back in time, or just [start at the beginning](https://d2718.net/blog/posts/protohax_00.html).)

[The fourth Protohackers problem](https://protohackers.com/problem/3) is to implement a chat server with a simple protocol. The meat of it is essentially this: Every line of text sent to the server should be broadcast to all joined connections _except_ the sender. There are a few other details:

  * Upon connection, the server will send a welcome message, to which the client must respond with the name they would like to use. If this name is conformant[^name_spec], the client will "join" the chat.
  * Upon joining, the server will send to the joining client a message containing the names of all _other_ joined clients.
  * When a client joins or leaves, it should be announced to all joined members _except_ the one joining/leaving.

[^name_spec]: More than zero alphanumeric ASCII characters.

## Architecture

This problem is complex enough that it's worth thinking about some structure.[^architecture] After some initial negotiation (the name exchange), each Client will join the Room. It will send Events (that it has joined, subsequent lines of chat dialog, and that it has left) over an [`mpsc`](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html) channel shared by all Clients to the Room. The Room will process these requests and decide what Messages (essentially lines of text) to then send back to the Clients in response, over a [`broadcast`](https://docs.rs/tokio/latest/tokio/sync/index.html#broadcast-channel) channel. Each Client will then relay these down its connection to the user. Each client will be identified by an ID number; all Events and Messages will be tagged with the originating client ID, so that each message can be relayed or not to the appropriate users. (Remember, for example, that any given user should _not_ see their own lines of dialog.)

[^architecture]: "Architecture" is probably a somewhat inflated term to use here, though.

Tokio's `tokio::sync::mpsc` channel functions (from a user's perspective) identically to the `std::sync::mpsc` channel, accepting messages from multiple sources and sending them all to a single consumer. The `tokio::sync::broadcast` channel doesn't have a standard library counterpart; it is essentially the inverse of the `mpsc`: It accepts messages from a single source[^bcast_sources] and clones them to be read by _every_ subscribed consumer.

[^bcast_sources]: It can actually accept messages from multiple sources, but we're only using one here.

Here's a diagram, if that helps:


```
--------         Messages                  ----------
|      |-->--| sync::broadcast |-->--+-->--|        |
| Room |                             |     | Client |---| TcpStream |---
|      |--<--| sync::mpsc |-<--+--<--|--<--|        |
--------         Events        |     |     ----------
                               |     V
                               A     |     ----------
                               |     +-->--|        |
                               |     |     | Client |---| TcpStream |---
                               +--<--|--<--|        |
                               |     |     ----------
                               |     V 
                               A     |     ----------
                               |     +-->--|        |
                               |     |     | Client |---| TcpStream |---
                               +--<--|--<--|        |
                               |     |     ----------
                               |     V
                               A     |
                                 etc.
```

Let's get typing.

```bash
cargo new 03_chat --name chat
```

`Cargo.toml`:

```toml
[package]
name = "chat"
version = "0.1.0"
edition = "2021"

[dependencies]
env_logger = "^0.10"
log = "^0.4"
tokio = { version = "^1", features = ["io-util", "macros", "net", "rt", "sync"] }
```

We're going to need Tokio's `sync` feature for the channel types.

`src/message.rs`:

```rust
/*!
Types to get passed through channels.
*/

/// Chunks of information sent from a Client to the Room.
#[derive(Clone, Debug)]
pub enum Event {
    Join{ id: usize, name: String },
    Leave{ id: usize },
    Text{ id: usize, text: String },
}

/// Lines of text to be sent from the Room to the Clients.
#[derive(Clone, Debug)]
pub enum Message {
    /// To be shown to everyone _but_ the Client with the given id.
    All{ id: usize, text: String },
    /// To be shown to _only_ the Client with the given id.
    One{ id: usize, text: String },
}
```

The `Client` itself will need

  * its `id`
  * a `TcpStream` connecting it to the user
  * an [`mpsc::Sender<Event>`](https://docs.rs/tokio/latest/tokio/sync/mpsc/struct.Sender.html) to send events to the Room
  * a [`broadcast::Receiver<Message>`](https://docs.rs/tokio/latest/tokio/sync/broadcast/struct.Receiver.html) to receive messages back from the Room

Two refinements:

  * We're going to split the `TcpStream` up into a [`ReadHalf`](https://docs.rs/tokio/latest/tokio/io/struct.ReadHalf.html) and a [`WriteHalf`](https://docs.rs/tokio/latest/tokio/io/struct.WriteHalf.html) so that we can wrap the `ReadHalf` in a [`BufReader`](https://docs.rs/tokio/latest/tokio/io/struct.BufReader.html) and use the [`AsyncBufReadExt::read_line()`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncBufReadExt.html#method.read_line) method.
  * We are going to wrap our `Message`s in [`Rc`](https://doc.rust-lang.org/std/rc/struct.Rc.html)s so that the `broadcast` channel doesn't have to clone the entirety of each `Message` for each joined `Client`.

`src/client.rs`:

```rust
/*!
Interface between a TcpStream and the rest of the program.
*/
use std::rc::Rc;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    net::TcpStream,
    sync::{broadcast::Receiver, mpsc::Sender}
};

use crate::message::{Event, Message};

pub struct Client {
    id: usize,
    from_user: BufReader<ReadHalf<TcpStream>>,
    to_user: WriteHalf<TcpStream>,
    from_room: Receiver<Rc<Message>>,
    to_room: Sender<Event>,
}
```

The `Room` will need the narrow ends of the `mpsc` and `broadcast` channels, as well as a way to keep track of client names.

`src/room.rs`:

```rust
/*!
The chat server's driving nexus.
*/
use std::{
    collections::BTreeMap,
    rc::Rc,
};

use tokio::{
    sync::{broadcast::Sender, mpsc::Receiver}
};

use crate::message::{Event, Message};

pub struct Room {
    /// Stores the names of connected clients.
    clients: BTreeMap<usize, String>,
    from_clients: Receiver<Event>,
    to_clients: Sender<Rc<Message>>,
}
```

## The Client

The Client's action is the most complicated; it has to deal with messy "real-world" data coming in over the TCP connection as well as messages from the `Room`. It also has to provide the initial "client name" negotiation. Let's start with that.

```rust
static WELCOME_TEXT: &[u8] = b"Welcome. Please enter the name you'd like to use.\n";
static BAD_NAME_TEXT: &[u8] = b"Your name must consist of at least one ASCII alphanumeric character.\n";

pub struct Client {
    id: usize,
    from_user: BufReader<ReadHalf<TcpStream>>,
    to_user: WriteHalf<TcpStream>,
    from_room: Receiver<Rc<Message>>,
    to_room: Sender<Event>,
}

/// Ensure name consists of more than zero ASCII alphanumerics.
fn name_is_ok(name: &str) -> bool {
    if name.len() < 1 { return false; }
    
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() { return false; }
    }

    true
}

impl Client {
    /// Send welcome message, get name from client, and validate it.
    async fn get_name(&mut self) -> Result<String, String> {
        self.to_user.write_all(WELCOME_TEXT).await.map_err(|e| format!(
            "write error: {}", &e
        ))?;

        let mut name = String::new();
        if let Err(e) = self.from_user.read_line(&mut name).await {
            return Err(format!("read error: {}", &e));
        }
        // If the read happened properly, this should end with an `\n'
        // we want to remove.
        let _ = name.pop();

        if !name_is_ok(&name) {
            // We don't really care if this fails.
            let _ = self.to_user.write_all(BAD_NAME_TEXT).await;
            return Err(format!("invalid name: {:?}", &name));
        }

        Ok(name)
    }
}
```

Now we need the actual chat interaction logic. There are two types of events to which the `Client` needs to react:

  1. an incoming line from the user
  2. a `Message` being relayed from `Room`

In order to await both of those, we are going to use the [`tokio::select!`](https://docs.rs/tokio/latest/tokio/macro.select.html) macro. It's like a `match` statement, except it "matches" on a series of futures, and executes the arm associated with whichever completes first. We're going to use it in a `loop`, which is typical, to repeatedly select from and handle futures of different kinds.

If reading a line from the user happens first, we'll send an `Event::Text` through the `mpsc` to the `Room`. If we get a `Message` from the `Room` first, we'll write the text (if appropriate[^appropriate]) to the user. We'll bail on any error except one: `broadcast` channels are of limited size; each one has an internal buffer that can only hold so many messages[^n_msgs]. It's possible that if a client subscribed to the channel goes long enough without `.recv()`ing, the buffer will fill up, and un`.recv()`d messages will get bumped out into the bit bucket (oldest first). The next time the subscriber tries to `.recv()`, the channel will emit a [`RecvError::Lagged(n)`](https://docs.rs/tokio/latest/tokio/sync/broadcast/error/enum.RecvError.html) indicating how many messages were missed. In this case, we'll inform the user they've missed some messages and forge on.

[^appropriate]: Remember, not all users should see all messages.

[^n_msgs]: Determined at creation time. The call to [`broadcast::channel(n)`](https://docs.rs/tokio/latest/tokio/sync/broadcast/fn.channel.html) sets the buffer to `n` messages.

```rust
impl Client {

    // get_name() method elided

    async fn run(&mut self) -> Result<(), String> {
        use tokio::sync::broadcast::error::RecvError;
        log::debug!("Client {} is running.", self.id);

        let name = self.get_name().await?;
        log::debug!("Client {} is {}", self.id, &name);
        let joinevt = Event::Join{ id: self.id, name };
        self.to_room.send(joinevt).await.map_err(|e| format!(
            "error sending Join event: {}", &e
        ))?;

        let mut line_buff: String = String::new();

        loop {
            tokio::select!{
                res = self.from_user.read_line(&mut line_buff) => match res {
                    Ok(0) => {
                        log::debug!(
                            "Client {} read 0 bytes; closing connection.",
                            self.id
                        );
                        return Ok(());
                    },
                    Ok(n) => {
                        log::debug!(
                            "Client {} rec'd {} bytes: {}",
                            self.id, n, &line_buff
                        );
                        // Every line has to end with '\n`. If we encountered
                        // EOF during this read, it might be missing.
                        if !line_buff.ends_with('\n') {
                            line_buff.push('\n');
                        }

                        let mut new_buff = String::new();
                        std::mem::swap(&mut line_buff, &mut new_buff);
                        // Now new_buff holds the line we just read, and
                        // line_buff is a new empty string, ready to be read
                        // into next time this branch completes.

                        let evt = Event::Text{ id: self.id, text: new_buff };
                        self.to_room.send(evt).await.map_err(|e| format!(
                            "unable to send event: {}", &e
                        ))?;
                    },
                    Err(e) => {
                        return Err(format!("read error: {}", &e));
                    }
                },

                res = self.from_room.recv() => match res {
                    // We can't match directly on an `Rc`; we have to
                    // dereference it to match "through" it; hence the
                    // ugly nested match here.
                    Ok(msg_ptr) => match *msg_ptr {
                        Message::All{ id, ref text } => {
                            if self.id != id {
                                self.to_user.write_all(text.as_bytes()).await
                                    .map_err(|e| format!(
                                        "write error: {}", &e
                                    ))?;
                            }
                        },
                        Message::One{ id, ref text } => {
                            if self.id == id {
                                self.to_user.write_all(text.as_bytes()).await
                                    .map_err(|e| format!(
                                        "write error: {}", &e
                                    ))?;
                            }
                        },
                    },
                    Err(RecvError::Lagged(n)) => {
                        log::warn!(
                            "Client {} dropped {} Message(s)",
                            self.id, n
                        );
                        let text = format!(
                            "Your connection has lagged and dropped {} message(s).", n
                        );
                        self.to_user.write_all(text.as_bytes()).await
                            .map_err(|e| format!(
                                "write error: {}", &e
                            ))?;
                    }
                    // We shouldn't ever encounter this error, but we have to
                    // match exhaustively, and it's the only other kind of
                    // RecvError.
                    Err(RecvError::Closed) => {
                        return Err("broadcast channel closed".into());
                    },
                }
            }
        }
    }
}
```

We also need a constructor (probably should have written that earlier) and a function to manage the lifetime of the connection.

```rust
impl Client {
    pub fn new(
        id: usize,
        socket: TcpStream,
        from_room: Receiver<Rc<Message>>,
        to_room: Sender<Event>,
    ) -> Client {
        let (from_user, to_user) = tokio::io::split(socket);
        let from_user = BufReader::new(from_user);

        Client { id, from_user, to_user, from_room, to_room }
    }

    // previously-written methods elided

    pub async fn start(mut self) {
        log::debug!("Client {} started.", self.id);

        if let Err(e) = self.run().await {
            log::error!("Client {}: {}", self.id, &e);
        }
        let leave = Event::Leave{ id: self.id };
        if let Err(e) = self.to_room.send(leave).await {
            log::error!(
                "Client {}: error sending Leave Event: {}",
                self.id, &e
            );
        }

        // Recombine our ReadHalf and WriteHalf into the original TcpStream
        // and attempt to shut it down.
        if let Err(e) = self.from_user.into_inner()
            .unsplit(self.to_user)
            .shutdown()
            .await
        {
            log::error!(
                "Client {}: error shutting down connection: {}",
                self.id, e
            );
        }

        log::debug!("Client {} disconnects.", self.id)
    }
}
```

Because our client is shut down and our user is disconnected at the end of `.start()`, we have it consume itself. It shouldn't do anything else at this point; it should definitely Drop.

I considered combining the constructor and the `.start()` methods into one single method, because the only thing the `Client` is going to do after popping into existence is almost immediately `.start().` Each `Client` is going to `.start()` in its own task, and you'll see that it'll be slightly more convenient to construct the `Client` _outside_ the task, before it starts, hence two separate methods.

## The Room

The `Room`, despite being the central nexus of the program's operation, is still somewhat simple in operation. It essentially just needs to turn `Event`s sent by the various `Client`s into `Message`s it sends back to them. That's it. The only extra thing we need is a function to build the list of names to send to newly-joined `Client`s. Oh, and let's make a constructor.

`src/room.rs`:

```rust
impl Room {
    pub fn new(
        from_clients: Receiver<Event>,
        to_clients: Sender<Rc<Message>>
    ) -> Room {
        let clients: BTreeMap<usize, String> = BTreeMap::new();

        Room { clients, from_clients, to_clients }
    }

    // Generate a list of names of clients currently connected.
    fn also_here(&self) -> String {
        if self.clients.is_empty() {
            return "* No one else is here.\n".into();
        }

        let mut name_iter = self.clients.iter();
        // This is safe because `clients` has at least 1 member.
        let (_, name) = name_iter.next().unwrap();
        let mut names = format!("* Also here: {}", name);

        for (_, name) in name_iter {
            names.push_str(", ");
            names.push_str(name.as_str())
        }
        names.push('\n');

        names
    }
}
```

Let's also write a method specifically for sending `Message`s down the broadcast channel. The channel will return an error if there are no listeners subscribed, but for us this isn't necessarily an erroneous situation; it may just mean that all the `Client`s have left by the time a message get sent, and this is fine. Nobody _should_ get that message. Handling this in its own function will make all the calling sites a little less noisy.

```rust
impl Room {

    // Stuff from before goes here.

    // Broadcast `msg` to all joined `Client`s, and deal with the non-error
    // if there aren't any.
    fn send(&self, msg: Message) {
        log::debug!("Room sending {:?}", &msg);
        match self.to_clients.send(Rc::new(msg)) {
            Ok(n) => { log::debug!("    reached {} clients.", &n); },
            Err(_) => { log::debug!("    no subscribed clients."); }
        }
    }
}
```


Now we need to get our running logic down. This should be pretty straightforward; we're just going to `.recv()` `Event`s from our `mpsc::Reciever<Event>` and act accordingly:

  * If it's an `Event::Join`, we'll generate an "also_here" message, add the client name to the `self.clients` map, and send a "so and so joined" `Message`.
  * If it's an `Event::Leave`, we'll remove the client's entry in `self.clients` and send a "so and so left" `Message`.
  * If it's an `Event::Text`, we'll send the text as a `Message`.

Normally if we encounter any errors, we'd bail, but there are only a couple of places where we might end up with an `Err` on our hands, and neither of them is really a problem, so we won't even bail.[^bail]

[^bail]: Unless something `panic()`s, but we don't anticipate that, and in any case, that's more like "literally dying to escape this social situation" than it is "leaving the party early because we're just not feeling it".

```rust

impl Room {

    // Stuff from before goes here.

    pub async fn run(mut self) -> Result<(), String> {
        log::debug!("Room is running.");

        while let Some(evt) = self.from_clients.recv().await {
            match evt {
                Event::Text{ id, text } => {
                    // The given `id` should definitely be in the clients map.
                    let name = self.clients.get(&id).unwrap();
                    let text = format!("[{}] {}", name, &text);
                    self.send(Message::All{ id, text });
                },

                Event::Join{ id, name } => {
                    let text = self.also_here();
                    self.clients.insert(id, name);
                    // It should be clear why this unwrap() will succeed.
                    let name = self.clients.get(&id).unwrap();
                    self.send(Message::One{ id, text });

                    let text = format!("* {} joins.\n", name);
                    self.send(Message::All{ id, text });
                },

                Event::Leave{ id } => {
                    if let Some(name) = self.clients.remove(&id) {
                        let text = format!("* {} leaves.\n", &name);
                        self.send(Message::All{ id, text });
                    }
                },
            }
        }

        Ok(())
    }
}
```

## Putting it all together

The `main()` function this time isn't _much_ more involved than it has been in the previous problems. We'll need to construct a `Room` and `.run()` it in its own task, hanging on to handles to its `mpsc::Receiver` and `broadcast::Sender`. When we receive successful connections, we'll clone those to instantiate a `Client` and then `.start()` it in its own task.

`src/main.rs`:

```rust
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
    tokio::spawn(async move {
        if let Err(e) = room.run().await {
            // This can't happen, because our `Room::run()` ended up only
            // ever returning `OK(())`, if it even returns.
            log::error!("Error running Room: {}", &e);
        }
    });

    let listener = TcpListener::bind(LOCAL_ADDR).await.unwrap();
    log::info!("Bound to {}", LOCAL_ADDR);
    let mut client_n: usize = 0;

    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                log::debug!("Rec'd connection {} from {:?}", client_n, &addr);

                let client = Client::new(
                    client_n, socket, msg_tx.subscribe(), evt_tx.clone()
                );
                tokio::spawn(async move {
                    client.start().await
                });
                
                client_n += 1;
            },
            Err(e) => {
                log::error!("Error with incoming connection: {}", &e);
            }
        }
    }
}
```

## The `current_thread` runtime isn't quite enough

Hrrm.

```
error: future cannot be sent between threads safely
   --> src/main.rs:37:18
    |
37  |       tokio::spawn(async move {
    |  __________________^
38  | |         if let Err(e) = room.run().await {
39  | |             // This can't happen, because our `Room::run()` ended up only
40  | |             // ever returning `OK(())`, if it even returns.
41  | |             log::error!("Error running Room: {}", &e);
42  | |         }
43  | |     });
    | |_____^ future created by async block is not `Send`
    |
    = help: the trait `Send` is not implemented for `Rc<Message>`
note: captured value is not `Send`
   --> src/main.rs:38:25
    |
38  |         if let Err(e) = room.run().await {
    |                         ^^^^ has type `Room` which is not `Send`
note: required by a bound in `tokio::spawn`
   --> /home/dan/.cargo/registry/src/github.com-1ecc6299db9ec823/tokio-1.24.2/src/task/spawn.rs:163:21
    |
163 |         T: Future + Send + 'static,
    |                     ^^^^ required by this bound in `tokio::spawn`
```

It goes on. This is just the first of several similar compiler admonitions.

We knew those `Rc`s we wrapped around our `Message`s in order to save a lot of `String` cloning were `!Send`. (We all knew that, right?[^unsend]) But I guess we also assumed that the `current_thread` runtime would just automatically allow us to run un`Send`able futures. Obviously this is not the case.

[^unsend]: In case you _aren't_ familiar with this particular jargonic morsel, `!Send` means that the type in question doesn't implement the [`std::marker::Send`](https://doc.rust-lang.org/std/marker/trait.Send.html) trait, which is necessary for it to be "sent across" a thread boundary (that is, moved to or accessed from a thread in which it wasn't originally instantiated). You'll notice right in the documentation of the trait it offers `Rc` as a type that specifically _isn't_ `Send` (and why).

There are a couple of things we could do here. An obvious one is to just use [`std::sync::Arc`](https://doc.rust-lang.org/std/sync/struct.Arc.html) instead of `Rc`. This would probably work, but it just seems like something we shouldn't have to do; nowhere is our program going to try to change the same reference count simultaneously from multiple threads.[^arc_perf]

[^arc_perf]: There is also evidently a _slight_ performance penalty to using `Arc` where `Rc` would suffice, but worrying about this particular detail is laughable here.

Fortunately, Tokio gives us a way to explicitly do what we want to do: [`tokio::task::LocalSet`](https://docs.rs/tokio/latest/tokio/task/struct.LocalSet.html). This is a way to explicitly ensure thread-local task-driving, specifically for this purpose. It's also, fortunately, convenient for us.

There are several ways to use the `LocalSet`; we'll use it by pushing the futures we want to run onto it using [`LocalSet::spawn_local()`](https://docs.rs/tokio/latest/tokio/task/struct.LocalSet.html#method.spawn_local), and then we'll `.await` it to drive them. We'll start with `.run()`ning the `Room`:

```rust
    let locals = task::LocalSet::new();

    locals.spawn_local(async move {
        if let Err(e) = room.run().await {
            // This can't happen, because our `Room::run()` ended up only
            // ever returning `OK(())`, if it even returns.
            log::error!("Error running Room: {}", &e);
        }
    });
```

This adds the supplied future to our set of local tasks, but it won't be run until we "start" the `LocalSet` by `.await`ing it. Because we want our connection-accepting loop to run concurrently with our `Room` (and the `Client`s we accept), we need to add it to our set as well; we wrap it in an `async` block and pass it to `.spawn_local()` just like above.

```rust
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
```

Notice the way we spawn our `Client` tasks has changed from `tokio::spawn()` to `tokio::task::spawn_local()`. This ensures that each of these tasks will also run on the same thread as the task that is spawning them. And finally, at the very end of `main()`, we `.await` the whole thing:

```rust
    locals.await;
```

And the Protohackers test log shows:

```
[Thu Jan 26 03:40:33 2023 UTC] [1client.test] NOTE:check starts
[Thu Jan 26 03:40:33 2023 UTC] [1client.test] NOTE:connected to 104.168.201.111 port 12321
[Thu Jan 26 03:40:37 2023 UTC] [1client.test] PASS
[Thu Jan 26 03:40:38 2023 UTC] [2clients.test] NOTE:check starts
[Thu Jan 26 03:40:38 2023 UTC] [2clients.test] NOTE:watchman connected to 104.168.201.111 port 12321
[Thu Jan 26 03:40:38 2023 UTC] [2clients.test] NOTE:watchman joined the chat room
[Thu Jan 26 03:40:38 2023 UTC] [2clients.test] NOTE:alice connected to 104.168.201.111 port 12321
[Thu Jan 26 03:40:38 2023 UTC] [2clients.test] NOTE:bob connected to 104.168.201.111 port 12321
[Thu Jan 26 03:40:39 2023 UTC] [2clients.test] NOTE:alice joined the chat room
[Thu Jan 26 03:40:39 2023 UTC] [2clients.test] NOTE:bob joined the chat room
[Thu Jan 26 03:40:39 2023 UTC] [2clients.test] FAIL:room membership message did not include 'watchman': * alice joins.
```

## A Race Condition, Sort Of

What? That's not even a room membership message? What's going on here? And I thought Rust couldn't have race conditions, what with ownership and the borrow checker and all.[^cool_bear]

[^cool_bear]: I really need [Amos](https://fasterthanli.me)'s Cool Bear as a foil here.

Well, if you're really determined, not even Rust can save you from every hazard you[^you] insist on blundering into. This isn't a race condition because stuff is nondeterministically executing out of the order in which we need it to; we[^we] literally just wrote it out of order. Let's look at this debug log and see if we can figure it out.

[^you]: And through use of the "impersonal you" here, I most definitely mean "me".

[^we]: Me, again.

```
$ RUST_LOG=debug ./chat

# ... skipping to the relevant part

[2023-01-26T04:19:21Z DEBUG chat] Rec'd connection 3 from 206.189.113.124:38042
[2023-01-26T04:19:21Z DEBUG chat] Client 3 created.
[2023-01-26T04:19:21Z DEBUG chat::client] Client 3 started.
[2023-01-26T04:19:21Z DEBUG chat::client] Client 3 is running.
[2023-01-26T04:19:21Z DEBUG chat::client] Client 2 is alice
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending One { id: 2, text: "* Also here: watchman\n" }
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending All { id: 2, text: "* alice joins.\n" }
[2023-01-26T04:19:21Z DEBUG chat::room]     reached 3 clients.
[2023-01-26T04:19:21Z DEBUG chat::client] Client 2 rec'd 22 bytes: "I think I'm alone now\n"
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending All { id: 2, text: "[alice] I think I'm alone now\n" }
[2023-01-26T04:19:21Z DEBUG chat::room]     reached 3 clients.
[2023-01-26T04:19:21Z DEBUG chat::client] Client 3 is bob
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending One { id: 3, text: "* Also here: watchman, alice\n" }
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending All { id: 3, text: "* bob joins.\n" }

# Ignoring the rest ...

```

There's the problem, right there. `bob` is seeing messages that he shouldn't. Let's go through that again.


```
[2023-01-26T04:19:21Z DEBUG chat] Rec'd connection 3 from 206.189.113.124:38042
```

At this point, `bob`'s connection has been accepted.

```
[2023-01-26T04:19:21Z DEBUG chat] Client 3 created.
```

Then immediately his `Client` is instantiated. This happens here in our `main()` function:

```rust
                    let client = Client::new(
                        client_n, socket, msg_tx.subscribe(), evt_tx.clone()
                    );
                    log::debug!("Client {} created.", client_n);
```

You can even see where the log message is printed. The relevant detail is that this is the point where `bob`'s `Client` subscribes to the `broadcast` channel (`msg_tx.subscribe()`); every `Rc<Message>` that comes down that pipe after this line executes will get to `bob`. 

```
[2023-01-26T04:19:21Z DEBUG chat::client] Client 3 started.
[2023-01-26T04:19:21Z DEBUG chat::client] Client 3 is running.
[2023-01-26T04:19:21Z DEBUG chat::client] Client 2 is alice
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending One { id: 2, text: "* Also here: watchman\n" }
```

Not that one, because that one only goes to `alice`, but this next one is the first once he receives:

```
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending All { id: 2, text: "* alice joins.\n" }
```

If you notice, it's the text of the message mentioned in the test log error message.

```
[2023-01-26T04:19:21Z DEBUG chat::room]     reached 3 clients.
[2023-01-26T04:19:21Z DEBUG chat::client] Client 2 rec'd 22 bytes: "I think I'm alone now\n"
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending All { id: 2, text: "[alice] I think I'm alone now\n" }
```

He also sees this one.

```
[2023-01-26T04:19:21Z DEBUG chat::room]     reached 3 clients.
[2023-01-26T04:19:21Z DEBUG chat::client] Client 3 is bob
```
But now _this_ is the point when `bob` joins the room. His name negotiation has completed, and his `Client` has sent the `Event::Join` up the `mpsc` to the `Room`.

(The relevant portion from `src/client.rs` in the `Client::run()` method:)

```rust
        let name = self.get_name().await?;
        let joinevt = Event::Join{ id: self.id, name };
        self.to_room.send(joinevt).await.map_err(|e| format!(
            "error sending Join event: {}", &e
        ))?;
```

_This_ is the point at which `bob` should start getting `Messages`; this next message _should_ be the first one that he sees (the room membership message the test client expected):

```
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending One { id: 3, text: "* Also here: watchman, alice\n" }
[2023-01-26T04:19:21Z DEBUG chat::room] Room sending All { id: 3, text: "* bob joins.\n" }
```

So the problem is that there are several `.await`s between when a `Client` subscribes to the `broadcast` channel and when it should actually start receiving messages, so any messages that end up being shoved into the tube in that gap will trip us up.[^debug_speed]

[^debug_speed]: I know you know this already, but I wanted to state it explicitly so I don't appear to be trying to misrepresent anything: The amount of time that passes between reading about my discovery of this problem and reading about my identification of the bug is much, much smaller than the amount of time that passed between those two events in real time as they were happening. And this is all to say nothing of the myriad smaller toe-stubs along the way like forgetting newlines at the ends of messages or just plain assigning to the wrong varaible.

What can we do about this? Can we somehow defer the subscription to the channel until that moment? Can we replace the client's `from_room: Receiver<Rc<Message>>` with `from_room: Option<Receiver<Rc<Nessage>>>` and pass a reference to the `Sender` end of that channel to the `Client::start()` method, which passes it to the `Client::run()` method, which then subscribes to it right as the join event is being sent? Can we use something like [`task::unconstrained()`](https://docs.rs/tokio/latest/tokio/task/fn.unconstrained.html) to ensure we can suck all the messages out and drop them on the ground at that point without risking being preempted and having more put back in?

Ah, hah! The `broadcast::Receiver` offers a [`.resubscribe()`](https://docs.rs/tokio/latest/tokio/sync/broadcast/struct.Receiver.html#method.resubscribe) method. This returns a new clone of the `Receiver` whose subscription begins at the moment of the method call; any messages already in the channel get dropped.[^dropped] The relevant stanza now becomes:

[^dropped]: Dropped for this particular `Reciever`, at least; this has no effect on other subscribers.

```rust
        let name = self.get_name().await?;
        let joinevt = Event::Join{ id: self.id, name };
        // Ignore anything already in this channel.
        self.from_room = self.from_room.resubscribe();
        self.to_room.send(joinevt).await.map_err(|e| format!(
            "error sending Join event: {}", &e
        ))?;
```

And now we're golden, right?

```
# ... everything except the last line of the Protohackers test log skipped.

[Fri Jan 27 03:26:33 2023 UTC] [2clients.test] FAIL:message to 'bob' was not correct (expected '[alice] Just one more thing'): [alice]  more thing
```

## `async` Cancellation

What is it this time? Let's look at our debug log:

```
[2023-01-27T03:26:33Z DEBUG chat::client] Client 2 rec'd 12 bytes: " more thing\n"

[2023-01-27T03:26:33Z DEBUG chat::room] Room sending All { id: 2, text: "[alice]  more thing\n" }
```

Well, that just looks like a glitch; the network must have just dropped a few bytes somewhere. Let's run it again. Their log:

```
[Fri Jan 27 03:32:02 2023 UTC] [2clients.test] FAIL:message to 'watchman' was not correct (expected '[alice] Just one more thing'): [alice]  more thing
```

Uh, our log:

```
[2023-01-27T03:32:02Z DEBUG chat::client] Client 2 rec'd 12 bytes: " more thing\n"
[2023-01-27T03:32:02Z DEBUG chat::room] Room sending All { id: 2, text: "[alice]  more thing\n" }
```

Um, hmm. The exact same problem. If we run it a few more times, the results are similar. The first chunk of `alice`'s line keeps getting swallowed.

While this is just a contrived challenge we are meeting, I honestly had to sleep on this one.[^not_sleep] The problem is apparent in the log message

[^not_sleep]: Technically, I correctly guessed the problem's source while lying awake in bed, but I didn't take another crack at it until I'd slept.

```
[2023-01-27T03:32:02Z DEBUG chat::client] Client 2 rec'd 12 bytes: " more thing\n"
```

Already, at this point, we've lost some bytes. Let's look at where this message is printed and what's going on right before then:

From `src/client.rs`:

```rust
            tokio::select!{
                res = self.from_user.read_line(&mut line_buff) => match res {
                    Ok(0) => {
                      -  log::debug!(
                            "Client {} read 0 bytes; closing connection.",
                            self.id
                        );
                        return Ok(());
                    },
                    Ok(n) => {
                        log::debug!(
                            "Client {} rec'd {} bytes: {:?}",
                            self.id, n, &line_buff
                        );
```

The problematic text has just been read into `line_buff` by `AsyncBufReadExt::read_line()`, so let's just have a look-see at [the documentation](https://docs.rs/tokio/latest/tokio/io/trait.AsyncBufReadExt.html#method.read_line). Ah hah! Observe the section labeled "[Cancel safety](https://docs.rs/tokio/latest/tokio/io/trait.AsyncBufReadExt.html#cancel-safety-1)", wherein we learn that this method is _not_ "cancellation safe".

Recall that the `select!` macro essentially "matches" against futures; it runs the arm of whichever future completes first. This means it is running _all_ of futures, and only dealing with the results of running _one_ of them. The other futures are _cancelled_; they are just aborted wherever they are. Some futures, like receiving a value from a channel, are, from a task-preemption standpoint, essentially atomic.[^atomic] If they don't complete, nothing has happened; you either suck a value out of a channel, or it's still there. These are considered _cancellation safe_ because if they are cancelled without completing, the state of the program is essentially no different than if they had not run at all. However, a method like `.read_line()` may need to make several separate reads, and if it gets cancelled after one or more of them, the underlying reader will still have had some bytes sucked out of it, and the internal buffer will have been changed. This is why it is _not_ cancellation safe; it's impossible to guarantee that starting it and cancelling it has the safe effect as not running it at all.

[^atomic]: I intend the term "atomic" to be by analogy to the more common use of the term, but please don't confuse them; the set of operations that cannot be interrupted by the _task_ scheduler is much larger than the set of operations that cannot be interrupted by the _thread_ scheduler.

Okay, so what do we do about this? Well, the documentation helpfully gives us some suggestions.[^suggestions]

  * We can use [`.read_until()`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncBufReadExt.html#method.read_until). This is essentially the same as `.read_line()`, but it puts data in a byte vector instead of a `String`. (It's also, and this is key, cancellation safe.)
  * We can use `.lines()`, which gives us a [struct](https://docs.rs/tokio/latest/tokio/io/struct.Lines.html) that's essentially an `async` iterator that yields lines via the `.next_line()` method.
  * We can use something from the [`tokio_util::codec`](https://docs.rs/tokio-util/latest/tokio_util/codec/index.html) module, which sounds complicated.

[^suggestions]: You hear a lot of noise about zero-cost abstractions and memory safety, but this kind of thing is also a huge driver of Rust fanboyism.

Let's just go with that first one. It seems like it's going to require the least rewriting. We'll just change our `line_buff` variable in `Client::run()` from a `String` to a `Vec<u8>` and add a conversion to a `String` before we stick the received line of text into our `Event::Text`. We have several choices about how exactly to make this conversion. The lines we receive are supposed to be just ASCII, so this should mean essentially just the compiler changing how it thinks about our pointer; our choices then really only differ in the way they deal with data that's outside the spec. I am going to choose [`String::from_utf8_lossy()`](https://doc.rust-lang.org/std/string/struct.String.html#method.from_utf8_lossy), because that's the most convenient; it'll just silently mangle non-UTF-8 data, as opposed to forcing me to deal with an error ([`String::from_utf8()`](https://doc.rust-lang.org/std/string/struct.String.html#method.from_utf8)) or `unsafe` and possibly undefined behavior ([`String::from_utf8_unchecked()`](https://doc.rust-lang.org/std/string/struct.String.html#method.from_utf8_unchecked)).

```rust
        let mut line_buff: String = String::new();
```

becomes

```rust
        let mut line_buff: Vec<u8> = Vec::new();
```

and what was the `.read_line()` arm of our `select!` becomes

```rust
                res = self.from_user.read_until(b'\n', &mut line_buff) => match res {
                    Ok(0) => {
                        log::debug!(
                            "Client {} read 0 bytes; closing connection.",
                            self.id
                        );
                        return Ok(());
                    },
                    Ok(n) => {
                        // Every line has to end with '\n`. If we encountered
                        // EOF during this read, it might be missing.
                        if !line_buff.ends_with(&[b'\n']) {
                            line_buff.push(b'\n');
                        }

                        let mut new_buff: Vec<u8> = Vec::new();
                        std::mem::swap(&mut line_buff, &mut new_buff);
                        // Now new_buff holds the line we just read, and
                        // line_buff is a new empty Vec, ready to be read
                        // into next time this branch completes.

                        let line_str = String::from_utf8_lossy(&new_buff);

                        // We'll move the log line down here to after we've
                        // converted our line to a String.
                        log::debug!(
                            "Client {} rec'd {} bytes: {:?}",
                            self.id, n, &line_str
                        );

                        let evt = Event::Text{ id: self.id, text: line_str.into() };
                        self.to_room.send(evt).await.map_err(|e| format!(
                            "unable to send event: {}", &e
                        ))?;
                    },
                    Err(e) => {
                        return Err(format!("read error: {}", &e));
                    }
                },
```

This works.

The one thing that bothers me about our implementation is the fact that room membership messages (the ones sent to newly-joined clients) get sent down the broadcast channel to _all_ clients, and then have to be ignored by everyone except the newly-joined client. Instead of a broadcast channel, we could have each client reading from its own `mpsc` channel. The `BTreeMap` that stores client names could also store the `Sender` end of each channel, then each `Message` could be individually sent to each `Client` who needs it. This requires a little more complexity in the `Room`'s running logic, and explicitly iterating through the map of (name, channel)s for each and every `Message` sent.

Instead, let's change our `Event::Join` variant to hold the `Sender` end of a [`oneshot`](https://docs.rs/tokio/latest/tokio/sync/oneshot/index.html) channel which can be used to return the room membership message to the `Client` and then forgotten about. This also has the advantage of simplifying our `Message` type, because room membership is the only thing communicated via the `Message::One` variant. This will require some surgery in a lot of places, but will ultimately make our design a little simpler.