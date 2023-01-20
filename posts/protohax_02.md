title: Protohackers in Rust, Part 01
subtitle: In which we learn an important lesson about traits
time: 2023-01-19 11:15:00
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

```
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

