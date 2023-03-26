title: Protohackers in Rust, Part 06 (d)
subtitle: The Main Task
time: 2023-02-22 13:42:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the seventh Protohackers problem
mathjax: true

You are reading an element of a series on solving the [Protohackers](https://protohackers.com) problems. Some previous offerings, if you're interested:

  * [Problem 0: Smoke Test](https://d2718.net/blog/posts/protohax_00.html)
  * [Problem 1: Prime Time](https://d2718.net/blog/posts/protohax_01.html)
  * [Problem 2: Means to an End](https://d2718.net/blog/posts/protohax_02.html)
  * [Problem 3: Budget Chat](https://d2718.net/blog/posts/protohax_03.html)
  * [Problem 4: Unusual Database Program](https://d2718.net/blog/posts/protohax_04.html)

This post itself is the fourth (and final) in a _subseries_ on solving [The Seventh Problem](https://protohackers.com/problem/6):

  * [first part](https://d2718.net/blog/posts/protohax_06a.html)
  * [second part](https://d2718.net/blog/posts/protohax_06b.html)
  * [third part](https://d2718.net/blog/posts/protohax_06c.html)

## The Main Task

Let's think about all the things we want the main task to do:

  * Listen on a socket; when a connection comes in, create a new main -> client channel for it, build the `Client` struct, and `.run()` it in its own task.
  * Receive `Event`s on a central `mpsc` channel:
    + `Event::Plate` => Add the observation to the appropriate `Car` (or insert a new `Car` with this observation), and, if warranted, issue a ticket:
      - If a dispatcher covering the road involved is currently connected, send an `Infraction` down that client task's channel.
      - If there are no connected dispatchers that can deal with it, store it.
    + `Event::Camera` => Drop the channel to the camera's client task; nothing gets sent to those.
    + `Event::Dispatcher` => Register this client as a Dispatcher, and register the roads assigned to it as being assigned to this client task. If there are any stored tickets for any of those roads waiting to be issued, send all of the appropriate `Infraction`s down this new client task's channel.
    + `Event::Gone` => If the client is
      - unidentified => Drop the channel to that client task.
      - a camera => Do nothing, we've already dropped that task's channel.
      - a dispatcher => Unregister that dispatcher, remove that dispatcher from responsibility for all of its roads, and drop its channel.

There's a lot about state here, so we'll start with some data structures, but first we'll get our "preamble" out of the way.

`src/main.rs`:

```rust
/*!
Protohackers Problem 6: Speed Daemon

Implement an
[automatic ticket-issusing system](https://protohackers.com/problem/6).
*/
mod bio;
mod clients;
mod events;
mod obs;

use std::collections::BTreeMap;

use tokio::{net::TcpListener, sync::mpsc};
use tracing::{event, Level};
use tracing_subscriber::{filter::EnvFilter, fmt::layer, prelude::*};

use crate::{
    bio::{LPString, LPU16Array},
    clients::Client,
    events::Event,
    obs::{Car, Infraction},
};

static LOCAL_ADDR: &str = "0.0.0.0:12321";
/// Size of outgoing channels; return channel is unbounded.
const CHAN_SIZE: usize = 16;
```

### State

The main task is responsible for all[^client-state] the state that needs to be tracked while the program is running. I needs to handle

  * observations of cars, obviously
  * the `Sender` ends of client task channels, and which `id`s and what kinds of clients they feed
  * the road responsibility of connected dispatchers

[^client-state]: Well, each client task had to maintain the state of its own `Heartbeat`, and camera client tasks had to remember the locations of their cameras, but the main task has responsibility for a) the vast majority of the state, and b) all the _shared, mutable_ state.

For tracking individual vehicle observations, we'll have a map that maps plates (`LPString`s) to `Car` structs.

As we've already seen[^past-ids], client tasks will be uniquely identified by a client number. We'll have a map from client ids (`usize`s) to `mpsc::Sender<Event>`s to hold the channels of unidentified clients. These will be dropped when we learn that these clients are cameras, but we will move them to a different map if we learn they are dispatchers. However, we'll also need to keep track of each dispatcher's responsibility (so we can unregister them when they disconnect). To that end, we'll define a struct for the main-task-end of each dispatcher channel:

```rust
/// Holds a handle to each dispatcher task from the main task.
#[derive(Debug)]
struct Dispatcher {
    chan: mpsc::Sender<Infraction>,
    roads: LPU16Array,
}
```
[^past-ids]: And has served us well in previous problems.

So when an unidentified client becomes a dispatcher client, we'll move its channel from the unidentified map to the dispatcher map, which maps client ids to these `Dispatcher` structs. Additionally, so that we can easily select from dispatchers when we need to issue a ticket, we'll store a map from road numbers (`u16`s) to `Vec`s of client ids responsible for each road.

And finally, we need a place to store undelivered tickets that are waiting for the appropriate dispatcher to connect, so we'll also have a map from road ids to `Vec<Infraction>`s.

So the beginning of our `main()` function now looks like this:

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::registry()
        .with(layer())
        .with(EnvFilter::from_default_env())
        .init();

    // Holds channels to as-of-yet-unidentified clients.
    let mut unid_clients: BTreeMap<usize, mpsc::Sender<Infraction>> = BTreeMap::new();
    // Maps ids to Dispatchers.
    let mut dispatchers: BTreeMap<usize, Dispatcher> = BTreeMap::new();
    // Maps road numbers to dispatcher ids.
    let mut coverage: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    // Maps roads to undelivered tickets.
    let mut tickets: BTreeMap<u16, Vec<Infraction>> = BTreeMap::new();
    // Maps plates to cars.
    let mut cars: BTreeMap<LPString, Car> = BTreeMap::new();
```

We use `BTreeMap`s instead of `HashMap`s because `BTreeMap` performance is generally better with integer keys and small sizes, both of which we have here.

### Accepting Connections

The main task has two kinds of events to which it needs to respond:

  * new incoming connections
  * channel `Event`s from the client tasks

Our main loop will `select!` on those two things.

First, we'll deal with incoming connections. We need to instantiate a new channel, store its send end in the `unid_clients` map, and pass its receive end, as well as a clone of the return channel's send end, to a new `Client` struct. Then it'll spawn a task to `.run()` the `Client`.

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() {

    /* ... snip ... */

    let listener = TcpListener::bind(LOCAL_ADDR).await.unwrap();
    event!(
        Level::DEBUG,
        "listening on {:?}",
        &listener.local_addr().unwrap()
    );
    // Receives Events from clients.
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let mut client_n: usize = 0;

    loop {
        tokio::select! {
            res = listener.accept() => match res {
                Ok((stream, addr)) => {
                    event!(Level::DEBUG,
                        "accpted client {} from {:?}", &client_n, &addr
                    );
                    let (out_tx, out_rx) = mpsc::channel::<Infraction>(CHAN_SIZE);
                    let client = Client::new(
                        client_n, stream, tx.clone(), out_rx
                    );
                    unid_clients.insert(client_n, out_tx);
                    tokio::spawn(async move {
                        client.run().await;
                    });
                    client_n += 1;
                },
                Err(e) => {
                    event!(Level::WARN,
                        "error accepting client: {}", &e
                    );
                }
            },

            /* rx.recv() arm goes here */

        }
    }
}
```

### Handling `Event`s

Before we get into the other arm of the `select!` loop, we're going to write a couple of helper functions for our state management. I don't really like functions like this---that is, functions that mutate their arguments;[^mutate-args] I would, in general, rather have the state encapsulated in a struct and have the mutation be done by calling the struct's methods; I think this approach makes it much clearer. However, I don't think we need to go to all that trouble here; this couple of functions are a compromise between a bunch of extra work to really do it right and having a huge wall of logic that's kind of confusing.

[^mutate-args]: At least not in Rust. In languages that are heavier on the functional syntax, that is of course how it's done.

The first function is for removing a dispatcher's `id` from all the `coverage` map entries that contain it when the dispatcher logs out.

```rust
/// Remove all of a given dispatcher's roads from the coverage map.
fn remove_roads(coverage: &mut BTreeMap<u16, Vec<usize>>, id: usize, d: Dispatcher) {
    for road in d.roads.as_slice().iter() {
        let mut empty_vec = false;
        if let Some(v) = coverage.get_mut(road) {
            if let Some(i) = v.iter().position(|&oid| oid == id) {
                // The order of dispatcher ids in each Vec doesn't matter, and
                // Vec::remove() is O(n), while this is O(1).
                v.swap_remove(i);
                if v.is_empty() {
                    empty_vec = true;
                }
            }
        }
        // If there are no more connected dispatchers with responsibility for
        // this road, just go ahead and remove the whole thing from the map.
        if empty_vec {
            coverage.remove(road);
        }
    }
}
```

And this next one is for handling `Infraction`s once they're made. If there's a connected dispatcher that can handle the infraction, it gets sent to that dispatcher; if not, it gets stored in the map of pending tickets.

```rust
async fn dispose_ticket(
    coverage: &BTreeMap<u16, Vec<usize>>,
    dispatchers: &BTreeMap<usize, Dispatcher>,
    tickets: &mut BTreeMap<u16, Vec<Infraction>>,
    ticket: Infraction,
) {
    if let Some(ids) = coverage.get(&ticket.road) {
        for id in ids.iter() {
            if let Some(d) = dispatchers.get(id) {
                event!(
                    Level::TRACE,
                    "sending ticket to Dispatcher {}: {:?}",
                    id,
                    &ticket
                );
                if let Err(e) = d.chan.send(ticket).await {
                    event!(Level::ERROR, "can't send to dispatcher {}: {}", id, &e);
                }
                return;
            }
        }
    }

    event!(
        Level::TRACE,
        "no Dispatcher available; storing ticket {:?}",
        &ticket
    );
    tickets.entry(ticket.road).or_default().push(ticket);
}
```

Now we're ready to tackle the `Event`-receiving arm of our `select!` loop. We'll consider it one chunk at a time.

If we get an `Event::Observation` we'll either create a new `Car` from that observation, or we'll call `Car::observed()` on the appropriate `Car`, which stores the observation and returns an `Infraction` if a ticket is warranted; in this case, we'll rout the ticket with the `dispose_ticket()` function we just wrote.

```rust
async fn main() {

    /* ... snip ... */

    loop {
        tokio::select! {

            /* connection handling select! arm here */

            evt = rx.recv() => {
                event!(Level::TRACE,
                    "main rec'd {:?}", &evt
                );

                // None means that the channel from the client tasks has been
                // `.close()`d, which
                //      a) shouldn't happen
                //      b) would prevent the program from functioning
                // so we'll just go ahead and die if that happens.
                let evt = evt.expect("main channel received None");
                match evt {
                    Event::Observation{ plate, road, limit, pos } => {
                        if let Some(car) = cars.get_mut(&plate) {
                            if let Some(ticket) = car.observed(road, limit, pos) {
                                dispose_ticket(
                                    &coverage,
                                    &dispatchers,
                                    &mut tickets,
                                    ticket
                                ).await;
                            }
                        } else {
                            cars.insert(plate, Car::new(plate, road, pos));
                        }
                    },

                    /* other match arms here */

                }
            },
        }
    }
```

Next we deal with a client disconnecting: `Event::Gone{ id }`. If the client is a dispatcher, there'll be an entry in `dispatchers`, and we'll need to use our `remove_roads()` function to remove his responsibility from `coverage`. If it's not a dispatcher client, we'll try to remove the associated channel end from the `unid_clients` map. If it's a camera client, this end of the channel will have already been dropped.

```rust
                    Event::Gone{ id } => {
                        if let Some(d) = dispatchers.remove(&id) {
                            remove_roads(&mut coverage, id, d);
                            event!(Level::TRACE,
                                "removed Dispatcher id {}", &id
                            );
                        } else if unid_clients.remove(&id).is_some() {
                            event!(Level::TRACE,
                                "removed unidentified client {}", &id
                            );
                        }
                    },
```

If a client identifies itself as a camera, we'll remove this end of its channel from `unid_clients` and drop it; camera's don't receive anything from the main task.

```rust
                    Event::Camera{ id } => {
                        if unid_clients.remove(&id).is_some() {
                            event!(Level::TRACE,
                                "removed {}'s channel", &id
                            );
                        } else {
                            event!(Level::WARN,
                                "no unidentified client with id {}", &id
                            );
                        }
                    },
```

Finally, if a client identifies itself as a dispatcher, we'll remove our end of the channel from `unid_clients`, register the client's responsibility in the `coverage` map, slap the channel in a `Dispatcher` struct together with a record of the dispatcher's responsibilities, and stow that in the `dispatchers` map. Also, while registering the roads the client covers, if any of those roads have pending tickets waiting for a dispatcher, we'll send those `Infraction`s down the channel.

```rust
                    Event::Dispatcher{ id, roads } => {
                        if let Some(chan) = unid_clients.remove(&id) {
                            for &road in roads.as_slice().iter() {
                                coverage.entry(road).or_default().push(id);
                                if let Some(ticket_vec) = tickets.remove(&road) {
                                    event!(Level::TRACE,
                                        "sending {} pending tickets", &ticket_vec.len()
                                    );
                                    for ticket in ticket_vec.into_iter() {
                                        if let Err(e) = chan.send(ticket).await {
                                            event!(Level::ERROR,
                                                "error sending backlogged ticket to Dispatcher {}: {}",
                                                &id, &e
                                            )
                                        }
                                    }
                                }
                            }

                            let d = Dispatcher { chan, roads };
                            dispatchers.insert(id, d);
                            event!(Level::TRACE,
                                "moved chan {} and inserted Dispatcher", &id
                            );
                        } else {
                            event!(Level::ERROR,
                                "no client with id {}", &id
                            );
                        }
                    },
```

And that's it. This was kind of a slog, but it works now. I'm also pretty pleased with some elements of the implementation, like the cancellation-safe `ClientMessage` reading method.

I will mention again that if you want to look at the whole thing all together (instead of in the chunks presented in this series of posts), all the code for the problems I've covered are in [a GitHub repository](https://github.com/d2718/protohax-blog).

## Retrospective

As I mentioned in a footnote in the first post, I had a bug[^two-bugs] in what ended up being the `Car::observed()` method that caused it to occasionally fail to issue a ticket when it should have. Unable to get any more log info from the _client_ end than what the Protohackers test page puts out, I made a couple of wrong guesses about what the problem was.

My first guess was that, since the method for reading `ClientMessage`s wasn't cancellation-safe, I must have been dropping observation messages from camera clients. I was predisposed to assume this because I was already worried about this being a potential failure point; I had decided to only add the extra complexity of assuring cancellation-safety in the event that it proved to be an issue. Also, due to the vast amount of log output I was generating, [fly.io](https://fly.io/)'s logging facility was dropping some of the log output, too, which contributed to my misconception. As a result, I restructured and rewrote a bunch of the program, although ultimately I only needed to fix the one method. (Since finishing, I've seen other working Rust implementations, and the cancellation thing just isn't a problem.)

I also thought (since fly.io was missing some of my log output) that maybe fly.io's free tier just wouldn't allow me the amount of concurrent connections/messages/requests/whatever that were coming in during the really hot portion of the test.[^fly-iok] I spent a great deal of time grepping through log files and doing some shotgun debugging until I noticed my actual problem and fixed it.

The end result is that going back over my code, thinking carefully about every path, reorganizing some of it, and fiddling with most of it, made the program I ended up with much better than it would have been otherwise.

[^two-bugs]: Actually, I think it was _two_ separate bugs.

[^fly-iok]: While the log thing was a little bit of a problem, fly.io was completely blameless on this front.
