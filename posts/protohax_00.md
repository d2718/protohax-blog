title: Protohackers in Rust, Part 00
subtitle: Dipping the toe in async and Tokio
time: 2023-01-13 11:53:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: Some introduction to async Rust through solving the first Protohackers problem

While idly batting solutions to the first couple of [Protohackers](https://protohackers.com/) problems back and forth, someone (whose opinion about these types of things is much more trustworthy than mine) suggested that I blog about solving them in Rust. Since I'm not really a teacher anymore and therefore lack a chronic captive audience, that was enough for me.

Someone else in the conversation (whose opinion's trustworthiness also exceeds mine), somewhat newish to Rust, balked at my suggestion to reach, as I had, for the mighty [Tokio](https://tokio.rs) in service of hacking together solutions for these. The abstraction level seemed a little high for his comfort; he also seemed a little understandably hesitant to plow into async Rust. So I figured that I might use that to give a little bit of a theme to this series of posts beyond just, "Watch me fumble around until I stumble on solutions." If you want an introduction to `async` and Tokio centered around working demonstrations in pseudoreal situations, read on.

Let's begin with a little foundation before I start getting abused by the borrow checker. If you want something comprehensive, and you have the time, [Asynchronous Programming in Rust](https://rust-lang.github.io/async-book/) is the gold-standard source for this material. (There is also [a book](https://book.async.rs/) that focuses on `async-std`, but I haven't read it.) Otherwise, you can stick around here with me and accrue just enough superficial knowledge to get yourself in trouble. Okay, let's go!

## `async` TL; DR

Honestly, `async` functions in Rust are pretty easy to use; you just have to

  0. invoke them in an `async` context (another `async` function or as a spawned "task"---more on that later)
  1. `.await` them

That's essentially all that's necessary to start messing around, but it's nice to know a little bit about futures.

## Futures

An `async` function returns a future, which is, in the strictest sense, something that implements [`std::future::Future`](https://doc.rust-lang.org/std/future/trait.Future.html). More helpfully, a future represents some computation still to come, that may complete and return a value, although probably (and this is the whole point) not right away. In practice, this is almost always some I/O operation; these tend to be "slow" in comparison to other stuff that happens at runtime. In this sense, calling an `async` function and `.await`ing the returned future is like calling a blocking function; the difference is that instead of waiting around for the function to return, the asynchronous runtime can switch to letting some other part of your program run for a while. In _this_ sense, it's kind of like spawning a new thread, but instead of the OS scheduling your concurrent execution, it's your program.

While _writing_ futures can be complicated and hairy, _using_ them is generally much simpler (as it should be!). What follows is a trivial example, but it illustrates how intimidating this shouldn't be.

A simple synchronous function:

```rust
fn answer() -> i32 {
    42
}
```
and how you use it:

```rust
let a = answer();
```

Here's that same version, but asynchronous, so it returns a future:[^0]

```rust
async fn answer() -> i32 {
    42
}
```
It's kind of cool how the compiler does that for you. This no longer returns an `i32`, but some processing that _resolves to_ an `i32`.  And here's now you use it:

```rust
let a = answer().await;
```

Or, if you don't want to start awaiting it immediately,

```rust
let fut = answer();
// Some intervening code...
let a = fut.await;
```

Now this was trivial to the point of being dumb. The future returned resolves immediately; the runtime isn't going to switch tasks. For a nontrivial example, we turn to the first Protohackers problem.

[^0]: Technically, `answer()` doesn't _return_ a future; it _is_ a future. The actual function doesn't run until you `.await` it.

## Interlude: Tasks and Terminology

A _task_ is the term used for the asynchronous analog to a _thread_. It's a path of execution through your code that can be suspended in specific places (generally while waiting on some I/O operation to complete) to allow some other path to proceed. The relationship between tasks (execution paths scheduled by your program's async runtime) and threads (execution paths scheduled by your OS) can be complex and not always clear. Sometimes the runtime will run a single task on a thread; sometimes it will coordinate multiple tasks on the same thread. Different async runtimes offer different amounts of control over this relationship. I will try to be consistent with my use of the terms _task_ and _thread_ to maintain this distinction.

## Protohackers Problem 0: The Smoke Test

[The first Protohackers problem](https://protohackers.com/problem/0) is to implement the TCP Echo service ([RFC 862](https://www.rfc-editor.org/rfc/rfc862.html), older than at least one of my ex-girlfriends). This is pretty easy, but still nontrivial, especially because they want us to support at least _five simultaneous clients_.

For illustrative purposes, let's start with the simplest thing we can do that's synchronous and RFC 862-compliant. This shouldn't satisfy the Smoke Test,[^1] but we'll show how easy it is to bring in Tokio and handle concurrent clients.

[^1]: Because of the whole "5 simultaneous clients" thing, but it does, for some reason.

The plan is: we'll listen for a connection, pick it up, read everything that comes down the socket, and write it back. Let's start.

```bash
$ cargo new 00_smoke --name smoke
```

(I'm going to use the `--name` flag for each of these because I want the _directory_ names to start with a number so they'll stay in order, but _crate_ names have to start with a letter.)

We're going to use [`env_logger`](https://docs.rs/env_logger/latest/env_logger/) (together with the [`log`](https://docs.rs/log/0.4.17/log/) logging facade) because it's simple and easy. Our `Cargo.toml` will start off like this:

```toml
[package]
name = "smoke"
version = "0.1.0"
edition = "2021"

[dependencies]
env_logger = "^0.4"
log = "^0.4"
```

So here's our simple synchronous version:

```rust
/*!
Single-threaded implementation of TCP Echo.
*/
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
};

static ADDR: &str = "0.0.0.0:12321";

/// Suck everything out of a socket and spit it back.
fn handle(mut sock: TcpStream, client_n: u64) -> std::io::Result<()> {
    let mut buff: Vec<u8> = Vec::new();

    let n_read = sock.read_to_end(&mut buff)?;
    log::debug!("Client {} read {} bytes.", client_n, n_read);

    sock.write_all(&buff)?;
    sock.flush()?;
    sock.shutdown(Shutdown::Both)?;
    log::debug!("Client {} responded and shut down.", client_n);

    Ok(())
}

fn main() {
    env_logger::init();

    let listener = TcpListener::bind(ADDR).unwrap();
    log::info!("Listening on {}", listener.local_addr().unwrap());

    // We'll number our clients in sequence to help us keep track of them.
    // This will be more helpful for debugging when we get to the async
    // version.
    let mut client_n: u64 = 0;

    loop {
        if let Ok((sock, addr)) = listener.accept() {
            log::debug!(
                "Accepted connection {} from {}", client_n, addr
            );

            if let Err(e) = handle(sock, client_n) {
                log::error!(
                    "error handling client {}: {}", client_n, &e
                );
            }

            client_n += 1;
        }
    }
}
```
Now this actually _works_, but I don't think it should, and it's certainly not processing these connections concurrently or simultaneously:

```
$ RUST_LOG=debug ./smoke
[2023-01-14T16:52:05Z INFO  smoke] Listening on 0.0.0.0:12321
[2023-01-14T16:52:10Z DEBUG smoke] Accepted connection 0 from 206.189.113.124:33934
[2023-01-14T16:52:10Z DEBUG smoke] Client 0 read 0 bytes.
[2023-01-14T16:52:10Z DEBUG smoke] Client 0 responded and shut down.
[2023-01-14T16:52:10Z DEBUG smoke] Accepted connection 1 from 206.189.113.124:33936
[2023-01-14T16:52:10Z DEBUG smoke] Client 1 read 14 bytes.
[2023-01-14T16:52:10Z DEBUG smoke] Client 1 responded and shut down.
[2023-01-14T16:52:11Z DEBUG smoke] Accepted connection 2 from 206.189.113.124:33938
[2023-01-14T16:52:11Z DEBUG smoke] Client 2 read 5091 bytes.
[2023-01-14T16:52:11Z DEBUG smoke] Client 2 responded and shut down.
[2023-01-14T16:52:11Z DEBUG smoke] Accepted connection 3 from 206.189.113.124:33940
[2023-01-14T16:52:11Z DEBUG smoke] Client 3 read 100000 bytes.
[2023-01-14T16:52:12Z DEBUG smoke] Client 3 responded and shut down.
[2023-01-14T16:52:13Z DEBUG smoke] Accepted connection 4 from 206.189.113.124:33954
[2023-01-14T16:52:14Z DEBUG smoke] Client 4 read 1028 bytes.
[2023-01-14T16:52:14Z DEBUG smoke] Client 4 responded and shut down.
[2023-01-14T16:52:14Z DEBUG smoke] Accepted connection 5 from 206.189.113.124:33956
[2023-01-14T16:52:14Z DEBUG smoke] Client 5 read 1012 bytes.
[2023-01-14T16:52:14Z DEBUG smoke] Client 5 responded and shut down.
[2023-01-14T16:52:14Z DEBUG smoke] Accepted connection 6 from 206.189.113.124:33958
[2023-01-14T16:52:14Z DEBUG smoke] Client 6 read 1063 bytes.
[2023-01-14T16:52:14Z DEBUG smoke] Client 6 responded and shut down.
[2023-01-14T16:52:14Z DEBUG smoke] Accepted connection 7 from 206.189.113.124:33964
[2023-01-14T16:52:14Z DEBUG smoke] Client 7 read 1012 bytes.
[2023-01-14T16:52:14Z DEBUG smoke] Client 7 responded and shut down.
[2023-01-14T16:52:14Z DEBUG smoke] Accepted connection 8 from 206.189.113.124:33966
[2023-01-14T16:52:14Z DEBUG smoke] Client 8 read 1007 bytes.
[2023-01-14T16:52:14Z DEBUG smoke] Client 8 responded and shut down.
```

I don't know much about what goes on with TCP sockets on the OS side of the `TcpListener` struct, but my guess is that the OS is buffering connections for us somehow.

To handle clients simultaneously, we could spawn each call to `handle()` in its own thread. The meat of our `main()` function would now become:

```rust
        if let Ok((sock, addr)) = listener.accept() {
            log::debug!(
                "Accepted connection {} from {}", client_n, addr
            );

            // No error handling here; errors will just get swallowed.
            std::thread::spawn(move || { handle(sock, client_n) });

            client_n += 1;
        }
```

Now we're getting some out-of-order action, meaning we're actually dealing with several connections at once. See how it accepts connections 4 through 8 before it reads from any of them, and then seems to service them in arbitrary order:

```
$ RUST_LOG=debug ./smoke
[2023-01-14T17:35:47Z INFO  smoke] Listening on 0.0.0.0:12321
[2023-01-14T17:35:54Z DEBUG smoke] Accepted connection 0 from 206.189.113.124:55792
[2023-01-14T17:35:54Z DEBUG smoke] Client 0 read 0 bytes.
[2023-01-14T17:35:54Z DEBUG smoke] Client 0 responded and shut down.
[2023-01-14T17:35:55Z DEBUG smoke] Accepted connection 1 from 206.189.113.124:55794
[2023-01-14T17:35:55Z DEBUG smoke] Client 1 read 14 bytes.
[2023-01-14T17:35:55Z DEBUG smoke] Client 1 responded and shut down.
[2023-01-14T17:35:55Z DEBUG smoke] Accepted connection 2 from 206.189.113.124:55796
[2023-01-14T17:35:55Z DEBUG smoke] Client 2 read 5091 bytes.
[2023-01-14T17:35:55Z DEBUG smoke] Client 2 responded and shut down.
[2023-01-14T17:35:55Z DEBUG smoke] Accepted connection 3 from 206.189.113.124:55798
[2023-01-14T17:35:55Z DEBUG smoke] Client 3 read 100000 bytes.
[2023-01-14T17:35:56Z DEBUG smoke] Client 3 responded and shut down.
[2023-01-14T17:35:57Z DEBUG smoke] Accepted connection 4 from 206.189.113.124:55804
[2023-01-14T17:35:57Z DEBUG smoke] Accepted connection 5 from 206.189.113.124:55806
[2023-01-14T17:35:58Z DEBUG smoke] Accepted connection 6 from 206.189.113.124:55808
[2023-01-14T17:35:58Z DEBUG smoke] Accepted connection 7 from 206.189.113.124:55814
[2023-01-14T17:35:58Z DEBUG smoke] Accepted connection 8 from 206.189.113.124:55816
[2023-01-14T17:35:58Z DEBUG smoke] Client 7 read 1083 bytes.
[2023-01-14T17:35:58Z DEBUG smoke] Client 7 responded and shut down.
[2023-01-14T17:35:58Z DEBUG smoke] Client 8 read 1058 bytes.
[2023-01-14T17:35:58Z DEBUG smoke] Client 8 responded and shut down.
[2023-01-14T17:35:58Z DEBUG smoke] Client 5 read 1029 bytes.
[2023-01-14T17:35:58Z DEBUG smoke] Client 5 responded and shut down.
[2023-01-14T17:35:58Z DEBUG smoke] Client 6 read 1052 bytes.
[2023-01-14T17:35:58Z DEBUG smoke] Client 6 responded and shut down.
[2023-01-14T17:35:58Z DEBUG smoke] Client 4 read 1085 bytes.
[2023-01-14T17:35:58Z DEBUG smoke] Client 4 responded and shut down.
```

But this is actually pretty silly. First of all, we're spawning entire threads to just do a little bit of socket sucky-blowy, and second of all, my cheap little single-core VPS isn't even going to be doing anything in parallel; only one of those threads can be run at a time. ([This microbenchmark's README](https://github.com/jimblandy/context-switch) contains an interesting discussion of an admittely limited comparison of threads-vs-Tokio-tasks context switch time and resource usage.)

All right, already, let's do this asynchronously.

## Tokio: I/O Made `async` Made Easy

Tokio is essentially an asynchronous reimplementation of parts of the standard library (mostly I/O stuff), along with a runtime to drive it all, plus some facilities for managing tasks. Many of the types it offers, particularly in the [`tokio::fs`](https://docs.rs/tokio/latest/tokio/fs/index.html), [`tokio::io`](https://docs.rs/tokio/latest/tokio/io/index.html), [`tokio::net`](https://docs.rs/tokio/latest/tokio/net/index.html), and [`tokio::sync`](https://docs.rs/tokio/latest/tokio/sync/index.html) modules behave similarly to their counterparts in [`std::fs`](https://docs.rs/rustc-std-workspace-std/latest/std/fs/index.html), [`std::io`](https://docs.rs/rustc-std-workspace-std/latest/std/io/index.html), [`std::net`](https://docs.rs/rustc-std-workspace-std/latest/std/net/index.html), or [`std::sync`](https://docs.rs/rustc-std-workspace-std/latest/std/sync/index.html), except that they can be `.await`ed so that something else can get done while _they're_ waiting for whatever _they_ need to make progress.

We'll add `tokio` to our `Cargo.toml` file, including the `rt` feature, which we need in order to use its runtime, the `net` feature so we have access to the types we want from `tokio::net`, and the `io-util` feature:


```toml
[package]
name = "smoke"
version = "0.1.0"
edition = "2021"

[dependencies]
env_logger = "^0.10"
log = "^0.4"
tokio = { version = "^1", features = ["io-util", "net", "rt"] }
```

And we'll change our `use` directives at the top from

```rust
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
};
```

to

```rust
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream}
}
```

There are `AsyncRead` and `AsyncWrite` traits, but they only provide methods that the runtime uses to interact with them.[^2] The methods we want to use are in the "extension" traits, provided by the `io-util` feature, that we have included here. (Also there's no use for the `Shutdown` enum.)

[^2]: One might also use them when implementing one's own future, but that is, as of yet, beyond our scope.

Our `handle()` function becomes

```rust
/// Suck everything out of a socket and spit it back.
async fn handle(mut sock: TcpStream, client_n: u64) -> std::io::Result<()> {
    let mut buff: Vec<u8> = Vec::new();

    let n_read = sock.read_to_end(&mut buff).await?;
    log::debug!("Client {} read {} bytes.", client_n, n_read);

    sock.write_all(&buff).await?;
    //sock.flush()?;
    sock.shutdown().await?;
    log::debug!("Client {} responded and shut down.", client_n);

    Ok(())
}
```

The signature has gained an `async`, the methods on `TcpStream` have all gained `.await`s, we've lost the `.flush()` (because `.shutdown()` flushes), and `.shutdown()` doesn't need a direction specified. That's it; it is structurally identical to our synchronous version.

And our `main()` function is only a little more different.

```rust
fn main() {
    env_logger::init();
    
    let mut client_n: u64 = 0;

    // This is the most significant difference. We need to configure and spin
    // up an async runtime...
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();

    // ...and run our main loop in with it.
    rt.block_on(async {
        let listener = TcpListener::bind(ADDR).await.unwrap();
        log::info!("Listening on {}", listener.local_addr().unwrap());

        loop {
            if let Ok((sock, addr)) = listener.accept().await {
                log::debug!(
                    "Accepted connection {} from {}", client_n, addr
                );

                // Instead of spawning a thread, we spawn a task.
                rt.spawn(async move { handle(sock, client_n).await });

                client_n += 1;
            },
        }
    });
}
```

The big difference is that we need to instantiate an async runtime and run all the `async` stuff in there. The [`Runtime::block_on()`](https://docs.rs/tokio/latest/tokio/runtime/struct.Runtime.html#method.block_on) method takes an `async` block[^3] and devotes the current thread to running that block to completion. This is a common async runtime entry point.

[^3]: Technically, `Runtime::block_on()` takes a _future_. Much like prepending `async` to a function's signature cues the compiler to turn it into a future, prepending `async` to a block does the same thing. Fidgety detail: This is _not_ an `async` closure (Rust does not support those, at least not yet); it is a _synchronous_ closure with an `async` block for a body. To be honest, I'm not exactly sure what all the technical differences are, but it at least becomes obvious that the closure is synchronous when you observe that execution does not move past it until it is complete.

And now this works!

```
$ RUST_LOG=debug ./smoke
[2023-01-15T02:27:55Z INFO  smoke] Listening on 0.0.0.0:12321
[2023-01-15T02:28:00Z DEBUG smoke] Accepted connection 0 from 206.189.113.124:32832
[2023-01-15T02:28:00Z DEBUG smoke] Client 0 read 0 bytes.
[2023-01-15T02:28:00Z DEBUG smoke] Client 0 responded and shut down.
[2023-01-15T02:28:00Z DEBUG smoke] Accepted connection 1 from 206.189.113.124:32834
[2023-01-15T02:28:00Z DEBUG smoke] Client 1 read 14 bytes.
[2023-01-15T02:28:00Z DEBUG smoke] Client 1 responded and shut down.
[2023-01-15T02:28:00Z DEBUG smoke] Accepted connection 2 from 206.189.113.124:32836
[2023-01-15T02:28:00Z DEBUG smoke] Client 2 read 5091 bytes.
[2023-01-15T02:28:00Z DEBUG smoke] Client 2 responded and shut down.
[2023-01-15T02:28:01Z DEBUG smoke] Accepted connection 3 from 206.189.113.124:32838
[2023-01-15T02:28:01Z DEBUG smoke] Client 3 read 100000 bytes.
[2023-01-15T02:28:01Z DEBUG smoke] Client 3 responded and shut down.
[2023-01-15T02:28:03Z DEBUG smoke] Accepted connection 4 from 206.189.113.124:32844
[2023-01-15T02:28:03Z DEBUG smoke] Accepted connection 5 from 206.189.113.124:32846
[2023-01-15T02:28:03Z DEBUG smoke] Accepted connection 6 from 206.189.113.124:32848
[2023-01-15T02:28:03Z DEBUG smoke] Accepted connection 7 from 206.189.113.124:32850
[2023-01-15T02:28:03Z DEBUG smoke] Client 7 read 1058 bytes.
[2023-01-15T02:28:03Z DEBUG smoke] Client 7 responded and shut down.
[2023-01-15T02:28:03Z DEBUG smoke] Accepted connection 8 from 206.189.113.124:32856
[2023-01-15T02:28:03Z DEBUG smoke] Client 5 read 1006 bytes.
[2023-01-15T02:28:03Z DEBUG smoke] Client 5 responded and shut down.
[2023-01-15T02:28:03Z DEBUG smoke] Client 4 read 1056 bytes.
[2023-01-15T02:28:03Z DEBUG smoke] Client 4 responded and shut down.
[2023-01-15T02:28:03Z DEBUG smoke] Client 6 read 1008 bytes.
[2023-01-15T02:28:03Z DEBUG smoke] Client 6 responded and shut down.
[2023-01-15T02:28:03Z DEBUG smoke] Client 8 read 1032 bytes.
[2023-01-15T02:28:03Z DEBUG smoke] Client 8 responded and shut down.
```

## A Couple Final Tweaks

While this works, I'm not quite satisfied. There are three changes I'd like to make, one for convenience, one that might make our program a little more robust, and one that makes the code just plain more responsible.

Notice that the vast majority of our `main()` function happens inside the block passed to the async runtime. Tokio offers us a way to make our _main_ function `async`, sparing us from explicitly fiddling with runtimes. We'll enable the `macros` feature in our `Cargo.toml`:

```toml
tokio = { version = "^1", features = ["io-util", "macros" "net", "rt"] }
```

And we'll employ the `#[tokio::main]` macro to allow us to mark our `main()` function as `async`.[^4] This allows us to elide the explicit creation of a runtime and subsequent block passing, making our `main()` look even more like our synchronous (threaded) version.

[^4]: This is normally not allowed. I don't think it's actually happening here, either; I think the macro renames your `main()` function and writes a new synchronous `main()` that wraps your main function in a runtime. 

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    let listener = TcpListener::bind(ADDR).await.unwrap();
    log::info!("Listening on {}", listener.local_addr().unwrap());

    let mut client_n: u64 = 0;

    loop {
        if let Ok((sock, addr)) = listener.accept().await {
            log::debug!(
                "Accepted connection {} from {}", client_n, addr
            );

            // tokio::task::spawn() is the exact async analog to the
            // threaded std::thread::spawn().
            tokio::task::spawn(async move { handle(sock, client_n).await });

            client_n += 1;
        }
    }
}
```

The default `#[tokio::main]` macro invokes the default Tokio runtime, which will spawn threads and swap tasks between them as it sees fit to try to work as efficiently as possible. The `current_thread` flavor attempts to spawn all tasks and do all the work on the thread in which the runtime was created.[^unsend][^threads] I will almost always reach for this one because I am targeting my single-core VPS, which can only run one thread at a time, anyway.

[^unsend]: Because it uses only one thread, this runtime can also spawn tasks that do not implement [`Send`](https://doc.rust-lang.org/std/marker/trait.Send.html), but that's way beyond scope here.

[^threads]: The `current_thread` runtime actually will spawn additional threads for operations that block at the OS level; it just won't spawn new threads to run async tasks. This detail is also kind of beyond scope.

To motivate the second modification, I draw your attention to this section of the log output:

```
[2023-01-15T02:42:08Z DEBUG smoke] Accepted connection 3 from 206.189.113.124:47544
[2023-01-15T02:42:08Z DEBUG smoke] Client 3 read 100000 bytes.
[2023-01-15T02:42:08Z DEBUG smoke] Client 3 responded and shut down.
```

The other requests were all reasonably sized, but that 100,000 bytes gave me pause.[^5] What if the client had sent a megabyte? Or a lot more? Our service could very well fall over.[^6] To robustify our program against this type of eventuality,[^7] instead of slurping the entire request into memory and then vomiting it back, we'll modify our `handle()` function to read size-limited chunks into a buffer, and echo the request back piece by piece.

We're going to need to `use std::io::ErrorKind` for this.

```rust
/// We'll read a kilobyte at a time.
const BUFFSIZE: usize = 1024;

/// Suck small mouthfuls out of a socket and spit them back until there's
/// no more data coming.
async fn handle(mut sock: TcpStream, client_n: u64) -> std::io::Result<()> {
    let mut buff = [0u8; BUFFSIZE];

    loop {
        // Read no more than will fit in `buff`.
        match sock.read(&mut buff).await {
            // Ok(0) means EOF.
            Ok(0) => break,
            // n bytes have been read into `buff`.
            Ok(n) => {
                log::debug!("Client {} read {} bytes.", client_n, n);
                // Write the first n bytes of `buff` back into the socket.
                sock.write_all(&buff[..n]).await?;
                log::debug!("Client {} wrote {} bytes.", client_n, n);
            },
            Err(e) => {
                // WouldBlock means just that; we'll try it again in a moment.
                // Any other error, and we'll just give up.
                if e.kind() != ErrorKind::WouldBlock {
                    break;
                }
            }
        }
    }

    log::debug!("Closing client {}.", client_n);
    sock.shutdown().await?;
    log::debug!("Client {} responded and shut down.", client_n);

    Ok(())
}
```

[^5]: Full disclosure: I was kind of expecting it, but only because I am currently working on a web server that accepts POST requests, and so was primed to be worrying about it.

[^6]: We are implementing TCP Echo as an exercise, and we've already satisfied our examiner, so this would obviously not be the end of the world, but I'm trying to introduce some more psuedorealism here.

[^7]: Or _attack_ even. Some bad actor might have it in for our Echo service!

And finally, we'll add some error handling. In this case, it isn't really any more than logging errors, but at least they won't be getting lost, and our program won't drop any connections without telling us why.

## Final Form

`Cargo.toml`:

```toml
[package]
name = "smoke"
version = "0.1.0"
edition = "2021"

[dependencies]
env_logger = "^0.10"
log = "^0.4"
tokio = { version = "^1", features = ["io-util", "macros", "net", "rt"] }
```

`src/main.rs`:

```rust
/*!
Protohackers Problem 0: Smoke Test

Asynchronous TCP Echo implementation.
*/
use std::io::ErrorKind;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream}
};

static ADDR: &str = "0.0.0.0:12321";

/// We'll read a kilobyte at a time.
const BUFFSIZE: usize = 1024;

/// Suck small mouthfuls out of a socket and spit them back until there's
/// no more data coming.
///
/// If we encounter any errors, we'll just give up and close the connection.
async fn handle(mut sock: TcpStream, client_n: u64) {
    let mut buff = [0u8; BUFFSIZE];

    loop {
        // Read no more than will fit in `buff`.
        match sock.read(&mut buff).await {
            // Ok(0) means EOF.
            Ok(0) => break,
            // n bytes have been read into `buff`.
            Ok(n) => {
                log::debug!("Client {} read {} bytes.", client_n, n);
                // Write the first n bytes of `buff` back into the socket.
                if let Err(e) = sock.write_all(&buff[..n]).await {
                    log::error!(
                        "Client {}: error writing to socket: {}",
                        client_n, &e
                    );
                    break;
                }
                log::debug!("Client {} wrote {} bytes.", client_n, n);
            },
            Err(e) => {
                // WouldBlock means just that; we'll try it again in a moment.
                // Any other error, and we'll just give up.
                if e.kind() != ErrorKind::WouldBlock {
                    log::error!(
                        "Client {}: error reading from socket: {}",
                        client_n, &e
                    );
                    break;
                }
            }
        }
    }

    log::debug!("Closing client {}.", client_n);
    if let Err(e) = sock.shutdown().await {
        log::error!(
            "Client {}: error shutting down connection: {}",
            client_n, &e
        );
    }
    log::debug!("Client {} disconnected.", client_n);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    // We'll leave this `.unwrap()` in here instead of "handling" it because
    // if this fails, we'd just have to give up anyway.
    let listener = TcpListener::bind(ADDR).await.unwrap();
    log::info!("Listening on {}", listener.local_addr().unwrap());

    let mut client_n: u64 = 0;

    loop {
        match listener.accept().await {
            Ok((sock, addr)) => {
                log::debug!(
                    "Accepted connection {} from {}", client_n, addr
                );

                // tokio::task::spawn() is the exact async analog to the
                // threaded std::thread::spawn().
                tokio::task::spawn(async move { handle(sock, client_n).await });

                client_n += 1;
            },
            Err(e) => {
                log::error!("Error accepting connection: {}", &e);
            }
        }
    }
}
```

And it works:

```
$ RUST_LOG=debug ./smoke
[2023-01-15T17:34:19Z INFO  smoke] Listening on 0.0.0.0:12321
[2023-01-15T17:34:25Z DEBUG smoke] Accepted connection 0 from 206.189.113.124:53010
[2023-01-15T17:34:25Z DEBUG smoke] Closing client 0.
[2023-01-15T17:34:25Z DEBUG smoke] Client 0 disconnected.
[2023-01-15T17:34:26Z DEBUG smoke] Accepted connection 1 from 206.189.113.124:53012
[2023-01-15T17:34:26Z DEBUG smoke] Client 1 read 14 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 1 wrote 14 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Closing client 1.
[2023-01-15T17:34:26Z DEBUG smoke] Client 1 disconnected.
[2023-01-15T17:34:26Z DEBUG smoke] Accepted connection 2 from 206.189.113.124:53014
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 read 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 wrote 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 read 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 wrote 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 read 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 wrote 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 read 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 wrote 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 read 995 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 wrote 995 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Closing client 2.
[2023-01-15T17:34:26Z DEBUG smoke] Client 2 disconnected.
[2023-01-15T17:34:26Z DEBUG smoke] Accepted connection 3 from 206.189.113.124:53016
[2023-01-15T17:34:26Z DEBUG smoke] Client 3 read 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 3 wrote 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 3 read 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 3 wrote 1024 bytes.

... a great many Client 3 reads and writes elided ...

[2023-01-15T17:34:26Z DEBUG smoke] Client 3 read 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 3 wrote 1024 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 3 read 736 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Client 3 wrote 736 bytes.
[2023-01-15T17:34:26Z DEBUG smoke] Closing client 3.
[2023-01-15T17:34:26Z DEBUG smoke] Client 3 disconnected.
[2023-01-15T17:34:28Z DEBUG smoke] Accepted connection 4 from 206.189.113.124:53020
[2023-01-15T17:34:28Z DEBUG smoke] Accepted connection 5 from 206.189.113.124:53022
[2023-01-15T17:34:28Z DEBUG smoke] Accepted connection 6 from 206.189.113.124:53024
[2023-01-15T17:34:28Z DEBUG smoke] Accepted connection 7 from 206.189.113.124:53026
[2023-01-15T17:34:28Z DEBUG smoke] Accepted connection 8 from 206.189.113.124:53834
[2023-01-15T17:34:28Z DEBUG smoke] Client 4 read 590 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 4 wrote 590 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 4 read 424 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 4 wrote 424 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Closing client 4.
[2023-01-15T17:34:28Z DEBUG smoke] Client 4 disconnected.
[2023-01-15T17:34:28Z DEBUG smoke] Client 5 read 171 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 5 wrote 171 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 8 read 480 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 8 wrote 480 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 5 read 831 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 5 wrote 831 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Closing client 5.
[2023-01-15T17:34:28Z DEBUG smoke] Client 5 disconnected.
[2023-01-15T17:34:28Z DEBUG smoke] Client 8 read 565 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 8 wrote 565 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Closing client 8.
[2023-01-15T17:34:28Z DEBUG smoke] Client 8 disconnected.
[2023-01-15T17:34:28Z DEBUG smoke] Client 6 read 748 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 6 wrote 748 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 7 read 394 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 7 wrote 394 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 6 read 319 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 6 wrote 319 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Closing client 6.
[2023-01-15T17:34:28Z DEBUG smoke] Client 6 disconnected.
[2023-01-15T17:34:28Z DEBUG smoke] Client 7 read 695 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Client 7 wrote 695 bytes.
[2023-01-15T17:34:28Z DEBUG smoke] Closing client 7.
[2023-01-15T17:34:28Z DEBUG smoke] Client 7 disconnected.
```

I promise that next time that, even if I don't manage to make it shorter (it _is_ a more complex problem, after all), I will at least spend less of your time talking about futures and providing trivial examples.