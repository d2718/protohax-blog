title: Protohackers in Rust, Part 04
subtitle: Nothing new here
time: 2023-02-11 21:54:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the seventh Protohackers problem

This post is part of a series on solving [Protohackers](https://protohackers.com) problems. Some previous offerings, if you're interested:

  * [Problem 0: Smoke Test](https://d2718.net/blog/posts/protohax_00.html)
  * [Problem 1: Prime Time](https://d2718.net/blog/posts/protohax_01.html)
  * [Problem 2: Means to an End](https://d2718.net/blog/posts/protohax_02.html)
  * [Problem 3: Budget Chat](https://d2718.net/blog/posts/protohax_03.html)
  * [Problem 4: Unusual Database Program](https://d2718.net/blog/posts/protohax_04.html)

Because I am taking the time to blog about these, my Protohackers buddies are leaving me in the dust. Because Problem 6 is far more interesting (not to mention difficult) than Problem 5, we're skipping that one for now and will come back to it.

[The Seventh Protohackers Problem](https://protohackers.com/problem/6) is to implement a automatic system for issuing speeding tickets. The specification is pretty complicated; I'll lay out a simplified overview here:

There will be two types of clients, cameras and dispatchers. Cameras will report vehicles (identified by their plates) as being at specific locations on specific roads at specific times. The server's job is to determine, based on locations and times, if and when a vehicle has exceeded the speed limit. If a vehicle warrants a ticket, the server will notifiy a dispatcher client that has responsibility for the road on which that vehicle has exceeded the speed limit.

What makes this tricky is:

  * Time/location stamps will not necessarily arrive in chronological order.
  * Any given vehicle can be issued at ticket for at most one infraction per day (regardless of the number of infractions committed).
  * When the server has calculated that a given vehicle has committed and infraction, there may not necessarily be any connected dispatchers able to respond on the given road, and we'll have to wait until one shows up to issue the ticket.
  * Any client can request that the server send it a "heartbeat" signal at a specified regular interval, and we have to do our best to honor that request.

We also have to support at least 150 clients.

## The Communication Protocol

The cameras and dispatchers speak a binary protocol[^vaporators] that involves big-endian u8, u16, u32, and length-prefixed strings of 0-255 ASCII characters. Each message begins with a single byte value that determines its type, followed by zero or more specific fiels that correspond to that type. We'll start by defining types to represent them as well as read and write them.

[^vaporators]: Very similar to your vaporators.