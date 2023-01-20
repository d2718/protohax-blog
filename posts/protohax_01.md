title: Protohackers in Rust, Part 01
subtitle: Prime numbers and an async subtlety
time: 2023-01-19 11:15:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the second Protohackers problem

(This is the second post in a series. If you're not sure what's going on or why you're here, reading the somewhat wordy [first post](https://d2718.net/blog/posts/protohax_00.html) may give you some context.)

[The second Protohackers problem](https://protohackers.com/problem/1) involves implementing a service to check if numbers are prime. This is a little tougher than the previous problem because we have to understand the content of the user's request in order to respond appropriately. The client will send us a JSON-formatted request; if it is well-formed, we will send a JSON response indicating whether the queried number is prime or not, and wait for additional requests; if the client sends a _malformed_ request, we will return a malformed response and disconnect. Oh, yeah, and we have to be able to service at least five simultaneous clients.

Let's get set up:

```bash
$ cargo new 01_prime --name prime
```

We'll use the same logging crate we used before. The initial content of our `Cargo.toml`:

```toml
[package]
name = "primes"
version = "0.1.0"
edition = "2021"

[dependencies]
env_logger = "^0.10"
log = "^0.4"
```

We're undoubtedly going to need [Tokio](https://tokio.rs/) at some point, but we'll cross that bridge in a bit.

## Prime Numbers

Before we start building our server, we should be sure we can determine whether a number is prime, right? This is the "business logic" portion of our program. There [are](https://docs.rs/primes/latest/primes/) [plenty](https://docs.rs/num-prime/latest/num_prime/) [of](https://docs.rs/is_prime/2.0.9/is_prime/) [crates](https://huonw.github.io/primal/primal/index.html) that will do this for us, and I get the sense that solving _this_ particular part of the problem isn't really the thrust of the exercise, but I majored in Math[^math], and also solved the first 70-or-so [Project Euler problems](https://projecteuler.net/archives) in three different languages, so I figure this is something we can get right ourselves. Also, I _think_ that writing this ourselves (at least the way I'm planning on writing it) will give us another opportunity to talk about a detail of `async` task scheduling in Rust.

[^math]: That's "Maths" if you're from one of those "zed" countries.

The maximally naïve approach to checking a number's primality is to divide it by all numbers[^all_numbers] less than it; if it's evenly divisible by any of them, it's composite. We can reduce the amount of arithmetic we need to do by observing two things:

  * If a number is divisible by _n_, then it's also divisible by all the factors of _n_.
  * For every factor a number has that's greater than its square root, it has a complementary factor that's less than its square root.

The first statement means that we only need to try dividing by _prime_ numbers. Given any composite that would evenly divide our candidate prime, any prime that divides _it_ will also evenly divide our candidate. The second statement means that we only need to check any given prime candidate for divisibility _up through its square root_.

[^all_numbers]: By "all numbers" I mean "all integers greater than 1", of course. Everything is divisible by 1, division by 0 doesn't make sense, and non-integers and negative numbers aren't really involved when talking about primality.

So our primality tester will have a vec where it stores discovered primes. Storing these will speed up subsequent prime checking if we reuse our struct over the course of the program. 

(The following few snippets will be from `src/primes.rs`.)

```rust
/*!
A primality checker.
*/

#[derive(Debug)]
pub struct Primes {
    known: Vec<u64>
}
```

It will start with 2 in it:

```rust
impl Default for Primes {
    fn default() -> Self {
        Self { known: vec![2] }
    }
}
```

And it'll have a method for finding and pushing the next prime onto its vec:

```rust
/// Return an upper bound for the square root of `n`.
fn sqrt_sup(n: u64) -> u64 {
    let x = n as f64;
    x.sqrt().ceil() as u64
}

impl Primes {
    /// Find and append the next prime number to the internal list.
    fn push_next(&mut self) {
        // This `unwrap()`ping should be fine because the only public
        // constructor guarantees at least one element.
        let mut n = self.known.last().unwrap() + 1;

        'guessing: loop {
            let sqrt = sqrt_sup(n);

            'trying: for &p in self.known.iter() {
                if n % p == 0 {
                    break 'trying;
                } else if p >= sqrt {
                    break 'guessing;
                }
            }

            n += 1;
        }

        self.known.push(n);
    }
}
```

(I require this Rust feature rarely enough that I tend to forget about it, but it's useful: Rust has [named loops](https://doc.rust-lang.org/book/ch03-05-control-flow.html#loop-labels-to-disambiguate-between-multiple-loops) to enable finer-grained `break`ing. You can also [return values from loops](https://doc.rust-lang.org/book/ch03-05-control-flow.html#returning-values-from-loops), just like from any other scope.)

I'll write a test just to make sure this is working:

```rust
#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_push_next() {
        let mut p = Primes::default();
        assert_eq!(&p.known, &[2]);

        p.push_next(); // 3
        p.push_next(); // 5
        p.push_next(); // 7
        p.push_next(); // 11
        assert_eq!(&p.known, &[2, 3, 5, 7, 11]);
    }
}
```

Oh, yeah, and in order to run the test (or use the module) I have to include it. This is our `src/main.rs` so far:

```rust
/*!
Protohackers Problem 1: Prime Time

Primality testing service.
*/

pub mod primes;

fn main() {
    println!("Hello, world!");
}
```

This much, at least, works so far.

```
dan@lauDANum:~/blog/protohax-blog/01_primes$ RUST_LOG=debug cargo test
   Compiling primes v0.1.0 (/home/dan/blog/protohax-blog/01_primes)
    Finished test [unoptimized + debuginfo] target(s) in 0.34s
     Running unittests src/main.rs (target/debug/deps/primes-64f54b35ec28ae2c)

running 1 test
test primes::test::test_push_next ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Now we need our public method to check an individual number for primality. This will iterate through our stored primes, checking our candidate for divisibility until one of our primes exceeds its square root. If we iterate through all of our stored primes before this happens, we'll just keep pushing new primes onto our `known` vec until we've found one large enough. Obviously at any point in this process, if one of our prime numbers divides our candidate evenly, we will immedately stop and return `false`.

```rust
impl Primes {

    // push_next() method elided.

    /// Return `true` if `n` is prime.
    pub fn is_prime(&mut self, n: u64) -> bool {
        // In this case, obviously it can't be prime.
        if n < 2 { return false; }

        let sqrt = sqrt_sup(n);

        // Check for divisibility against all primes generated so far.
        for &p in self.known.iter() {
            if n == p {
                return true;
            } else if n % p == 0 {
                return false;
            } else if p >= sqrt {
                return true;
            }
        }

        // Continue generating primes and checking until we're convinced
        // `n` is prime.
        loop {
            self.push_next();
            let p = *self.known.last().unwrap();

            if n % p == 0 {
                return false;
            } else if p >= sqrt {
                return true;
            }
        }
    }
}
```

A test, too, just to make sure:

```rust
#[cfg(test)]
mod test {
    use super::*;

    // First test elided.

    #[test]
    fn test_is_prime() {
        let mut p = Primes::default();
        let pvec: Vec<u64> = (0u64..30).into_iter()
            .filter(|n| p.is_prime(*n))
            .collect();
        
        assert_eq!(
            &pvec,
            &[2, 3, 5, 7, 11, 13, 17, 19, 23, 29]
        )
    }
}
```

And everything works so far:

```
running 2 tests
test primes::test::test_push_next ... ok
test primes::test::test_is_prime ... ok
```

You may observe that the operation of `Primes` is not thread-safe. Multiple threads possibly attempting to find and push new primes onto the list might not actually be disastrous, but it would interfere with the intended operation, and certainly leave us in a state wouldn't expect.

But wait! We're only using the `current_thread` runtime flavor; it doesn't matter if it's _thread_ safe, because we'll only ever have a single thread at a time working the business logic. A better question is: Is it _task_ safe?

## Skippable Digression: Rust Isn't Javascript

And this is enough to make us all love it.[^crowdpleaser] More to the point, Tokio isn't V8 or Node or Deno or whatever Firefox's JS engine is called. Rust Futures often get compared to Javascript Promises, but there are some important differences; like so many things in Rust, there are details there for those who want to sweat them. Here's an example of what I'm getting at: In Javascript you have a function

[^crowdpleaser]: This has been a cheap crowd-pleaser, and I'm fine with that.

```js
async function do_something(data, callback) {

    // Stuff that requires waiting happens, then eventually you have some
    // `value`, which gets called by

    callback(value);
}
```
And then you call it in your program:

```js
// preceding code

do_something(foo, bar);

// plenty

// more

// code

// here

// after

// the function call...
```

So `do_something()` works on `foo` for a while (maybe it `fetch()`es something), eventually calculates or builds `value` somehow, and at some later point in time, the subsequent code's execution gets interrupted by `bar()` being run.

If you have essentially the same Rust function

```rust
async fn do_something(data: &[u8], callback: Fn(&str) -> ()) {

    // Stuff that requires waiting happens, then eventually you get a String
    // called `value`, and you

    callback(&value);
}
```

And you do the same thing:

```rust
// preceding code

do_something(&foo, bar);

// also

// more

// code

// etc...
```

`bar()` will _never_ run. The call to `do_something()` generates a future which is neither assigned to a value nor `.await`ed, so it just goes straight to the bit bucket.[^ignored_future]

[^ignored_future]: The compiler will probably warn you about this.

Here are two things about Rust futures that conspire to give us a surprising amount of control over the execution of our asynchronous programs, but might seem weird if you're coming from a language like Go[^goroutines] or Javascript[^autonomous_promises].

[^goroutines]: Whose goroutines just seem to take care of themselves.

[^autonomous_promises]: Whose Promises seem to start executing right away on their own schedule.

  * Futures only execute when `.await`ed (or are passed to some runtime construct that explicitly drives them, like [`join!`](https://docs.rs/tokio/latest/tokio/macro.join.html) or [`select!`](https://docs.rs/tokio/latest/tokio/macro.select.html)).
  * The _only_ time a task can be preempted by another task is at a point where it is `.await`ing a future.

Consider a hypothetical function that

  1. Reads a newline-terminated JSON message from a socket.
  2. Calculates a checksum for that message.
  3. Verifies the checksum over the socket.
  4. If the checksum is good, decodes the message and returns it.

```rust
use std::io::ErrorKind;
use serde::Deserialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

#[derive(Debug, Deserialize)]
struct Message {
    // elided; doesn't matter
}

/// Read from `sock` until a newline is encountered, then return the
/// data read.
///
/// This function is optimized for simplicity, not for performance.
async fn read_line(sock: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut buff: Vec<u8> = Vec::new();

    loop {
        match sock.read_u8().await? {               // <- ACTUALLY HERE
            b'\n' => { return Ok(buff); },
            b     => { buff.push(b); },
        }
    }
}

/// Read a checksum-verified `Message` from `socket`. 
async fn receive_checked_message(
    socket: &mut TcpStream
) -> std::io::Result<Message> {
    let bytes = read_line(socket).await?;           // <- HERE

    // If the message is long, this may take a nontrivial amount of time.
    let checksum: u32 = bytes.iter().map(|b| *b as u32).sum();
    let checksum_json = serde_json::json!({
        "type": "checksum",
        "value": checksum
    });
    // This serialization really shouldn't fail.
    let checksum_msg = serde_json::to_vec(&checksum_json).unwrap();

    socket.write_all(&checksum_msg).await?;         // <- HERE
    let verification = read_line(socket).await?;    // <- HERE
    if &verification != b"OK" {
        return Err(std::io::Error::new(ErrorKind::Other, "checksum failed"));
    }

    // If the message is long, this also may take a nontrivial amount of time.
    let message: Message = serde_json::from_slice(&bytes).map_err(|e|
        std::io::Error::new(
            ErrorKind::Other, format!("deserialization failed: {}", &e)
        )
    )?;

    Ok(message)
}
```

A task running the above function `receive_checked_message()` will only ever be put on hold by the runtime to work on a different task during the `.await`ed function calls (the places I have marked `// <- HERE`). In fact, the two code paths through the `read_line()` function can only be preempted at the call to `sock.read_u8()` (marked `// <- ACTUALLY HERE`). Any stretch of code _between_ the `.await` points will execute straight through.[^thread_preempt]

[^thread_preempt]: The _thread_ might get preempted, of course, but if your struct or process is available to multiple threads, Rust already forces you to wrap it in some form of locking structure (like a [`Mutex`](https://doc.rust-lang.org/std/sync/struct.Mutex.html) or an [`RwLock`](https://doc.rust-lang.org/std/sync/struct.RwLock.html)).

Noting that all the methods of our `Primes` struct are synchronous, we see that there is nowhere any of them would be preempted.

## TL; DR

Our `Primes` struct is task safe, but because ours will be static (so it'll be easily available to any task), the compiler will make us wrap it in a `Mutex` anyway. And because it'll be a static that requires allocation, we'll have to wrap the `Mutex` in a `once_cell::sync::Lazy`.

## The Hard Part: Actually implementing the protocol

[The problem statement](https://protohackers.com/problem/1) gives us an explanation that is kind enough to be clear and point out some fidgety spots.

Requests will be JSON-formatted and newline-delimited, so a well-formed request will be a single line of JSON that looks like this:

```json
{"method":"isPrime","number":42}
```

The `method` and `number` fields are required; `method` must be `"isPrime"` and `number` must be a valid JSON number. This seems simple enough that we could parse it with a regular expression or something homegrown, but I'm going to get [Serde](https://serde.rs/) involved because the problem also says: "Extraneous fields are to be ignored." The prospect of extraneous fields makes me a little nervous. Also, we are supposed to accept any valid JSON number, which includes floats[^not_prime], so I'm going to take advantage of `serde_json`'s [`Number`](https://docs.rs/serde_json/latest/serde_json/value/struct.Number.html) type to handle both floats and integers.

[^not_prime]: Which obviously aren't prime.

So now our `Cargo.toml`:

```toml
[package]
name = "primes"
version = "0.1.0"
edition = "2021"

[dependencies]
env_logger = "^0.10"
log = "^0.4"
once_cell = "^1.17"
serde = { version = "^1.0", features=["derive"] }
serde_json = "^1.0"
```

And the beginning of `src/main.rs`:

```rust
/*!
Protohackers Problem 1: Prime Time

See [Protohackers Problem 1](https://protohackers.com/problem/1)
for the full specification.
*/
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::value::Number;

pub mod primes;

use primes::Primes;

static PRIMES: Lazy<Mutex<Primes>> = Lazy::new(||
    Mutex::new(Primes::default())
);

/// Deserialization target for requests.
///
/// By default, `serde` will ignore unknown fields, which is exactly the
/// behavior we want.
#[derive(Debug, Deserialize)]
struct Request {
    method: String,
    /// serde_json::value::Number can be f64, i64, or u64. 
    number: Number,
}

/// Deserialize a request; if valid, determine if it's prime.
///
/// If the request is _malformed_, return an error.
fn process_request(bytes: &[u8]) -> Result<bool, String> {
    let req: Request = serde_json::from_slice(&bytes).map_err(|e| format!(
        "Unable to deserialize request: {}", &e
    ))?;

    if &req.method != "isPrime" {
        return Err(format!("Unrecognized method: {:?}", &req.method));
    }

    // If it's not an integer, it can't possibly be prime.
    let n = match req.number.as_i64() {
        None => { return Ok(false); }
        Some(n) => n,
    };

    // Not only is this an easy false, it also guarantees that n can be safely
    // cast to a u64, which is what our `Primes::is_prime()` method wants.
    if n < 2 {
        return Ok(false);
    }
    let n = n as u64;

    // As a single-threaded program, we would have already panicked if this
    // Mutex were poisoned, so we .unwrap() confidently here.
    let is_prime = PRIMES.lock().unwrap().is_prime(n);

    Ok(is_prime)
}
```

Tokio has its own [`tokio::sync`](https://docs.rs/tokio/latest/tokio/sync/index.html) module with `async` versions of many of the types from [the standard library](https://doc.rust-lang.org/std/sync/) (like `Mutex` and some channel flavors). You'll notice that we eschew Tokio's `Mutex` for the standard library version. This is because (as according to `wc` we just spent 800 words discussing) we never need to lock our `Primes` struct in an `async` fashion; we will never need to hold the lock over an `.await` point, so we don't need the extra complication the `async` version offers us.

Now we need a function to handle connections. It'll read lines from a socket, pass them to `process_request()`, and respond accordingly. The appropriate response to a well-formed request is a newline-terminated JSON message of the form

```json
{"method":"isPrime","prime":false}
```
(Obviously with `"prime":true` if the number in the request is prime.)

In response to _malformed_ requests, we are supposed to return a malformed response of our own and close the connection.

> A response is malformed if it is not a well-formed JSON object, if any required field is missing, if the method name is not `"isPrime"`, or if the `prime` value is not a boolean.

It's not explicitly required by the spec, but we are going be good netizens and return a useful error message of the form

```json
{"method":"isPrime","error":"A description of the error."}
```
This will still be considered "malformed" because it's missing the `"prime"` field, but if our clients decide to inspect our responses they can perhaps glean some useful information. We don't need to use Serde for either of these responses; the `format!` macro will do just fine.

One more detail to note upfront: Our `handle()` function will accept a `TcpStream`; we will [`.split()`](https://docs.rs/tokio/latest/tokio/net/struct.TcpStream.html#method.split) the `TcpStream` into a [`ReadHalf`](https://docs.rs/tokio/latest/tokio/net/tcp/struct.ReadHalf.html) and a [`WriteHalf`](https://docs.rs/tokio/latest/tokio/net/tcp/struct.WriteHalf.html) so that we can wrap the `ReadHalf` in a [`BufReader`](https://docs.rs/tokio/latest/tokio/io/struct.BufReader.html) in order to take advantage of the [`AsyncBufReadExt`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncBufReadExt.html) trait's [`.read_line()`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncBufReadExt.html#method.read_line) method, so we don't have to implement asynchronous line-at-a-time reading ourselves.

We add `tokio` to our `Cargo.toml`:

```toml
tokio = { version = "^1", features = ["io-util", "macros", "net", "rt"] }
```

We need

  * `io-util` for the `AsyncReadExt`, `AsyncBufReadExt`, &c (all the "extension" reading and writing traits)
  * `macros` so we can make our `main()` function `async` when we get there
  * `net` to give us the network types, like `TcpStream`
  * `rt` because we're obviously going to be using `tokio`'s runtime

We'll add the following `use` directives to the top of `src/main.rs`:

```rust
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream}
};
```

We don't need `TcpListener` _yet_, but we will when we write our `main()` function, so we're adding it now. And here's our handler function; it's a little long and has a nested `match`:

```rust
/// Listen for requests on `socket` and respond appropriately.
///
/// If we encounter any errors or malformed requests, we'll close the
/// connection.
async fn handle(mut socket: TcpStream, client_n: usize) {
    let (read_half, mut write_half) = socket.split();
    let mut reader = BufReader::new(read_half);
    let mut buff = String::new();

    loop {
        match reader.read_line(&mut buff).await {
            // We've reached EOF; the other end has almost assuredly closed
            // the connection.
            Ok(0) => { break; },
            Ok(n) => {
                log::debug!(
                    "Client {} read {} bytes: {:?}.", client_n, n, &buff
                );

                let result = process_request(buff.as_bytes());
                let response = match &result {
                    &Ok(is_prime) => format!(
                        "{{\"method\":\"isPrime\",\"prime\":{}}}\n",
                        is_prime
                    ),
                    &Err(ref e) => format!(
                        "{{\"method\":\"isPrime\",\"error\":{:?}}}\n", e
                    ),
                };

                log::debug!(
                    "Client {} sending response: {:?}", client_n, &response
                );

                if let Err(ref e) = write_half.write_all(
                    response.as_bytes()
                ).await {
                    log::error!(
                        "Client {}: error writing response: {}", client_n, e
                    );
                    break;
                }

                // If `process_request()` returns an `Err`, the request was
                // malformed, so we're closing the connection.
                if result.is_err() { break; }

                // `.read_line()` appends to the target buffer, so we have
                // to clear the request we just processed out of it.
                buff.clear();
            },
            Err(e) => {
                log::error!(
                    "Client {}: error reading line from socket: {}",
                    client_n, &e
                );
                break;
            },
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

And then our `main()` function will be pretty much like it was last time: We'll listen for incoming connections, and for each successful connection we'll spawn a task in which we pass the connection to our handler function.

So here's our final `src/main.rs` (we already saw the final form of `src/primes.rs`):

```rust
/*!
Protohackers Problem 1: Prime Time

See [Protohackers Problem 1](https://protohackers.com/problem/1)
for the full specification.
*/
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::value::Number;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream}
};

pub mod primes;

use primes::Primes;

static LOCAL_ADDR: &str = "0.0.0.0:12321";

static PRIMES: Lazy<Mutex<Primes>> = Lazy::new(||
    Mutex::new(Primes::default())
);

/// Deserialization target for requests.
///
/// By default, `serde` will ignore unknown fields, which is exactly the
/// behavior we want.
#[derive(Debug, Deserialize)]
struct Request {
    method: String,
    /// serde_json::value::Number can be f64, i64, or u64. 
    number: Number,
}

/// Deserialize a request; if valid, determine if it's prime.
///
/// If the request is _malformed_, return an error.
fn process_request(bytes: &[u8]) -> Result<bool, String> {
    let req: Request = serde_json::from_slice(&bytes).map_err(|e| format!(
        "Unable to deserialize request: {}", &e
    ))?;

    if &req.method != "isPrime" {
        return Err(format!("Unrecognized method: {:?}", &req.method));
    }

    // If it's not an integer, it can't possibly be prime.
    let n = match req.number.as_i64() {
        None => { return Ok(false); }
        Some(n) => n,
    };

    // Not only is this an easy false, it also guarantees that n can be safely
    // cast to a u64, which is what our `Primes::is_prime()` method wants.
    if n < 2 {
        return Ok(false);
    }
    let n = n as u64;

    // As a single-threaded program, we would have already panicked if this
    // Mutex were poisoned, so we .unwrap() confidently here.
    let is_prime = PRIMES.lock().unwrap().is_prime(n);

    Ok(is_prime)
}

/// Listen for requests on `socket` and respond appropriately.
///
/// If we encounter any errors or malformed requests, we'll close the
/// connection.
async fn handle(mut socket: TcpStream, client_n: usize) {
    let (read_half, mut write_half) = socket.split();
    let mut reader = BufReader::new(read_half);
    let mut buff = String::new();

    loop {
        match reader.read_line(&mut buff).await {
            // We've reached EOF; the other end has almost assuredly closed
            // the connection.
            Ok(0) => { break; },
            Ok(n) => {
                log::debug!(
                    "Client {} read {} bytes: {:?}.", client_n, n, &buff
                );

                let result = process_request(buff.as_bytes());
                let response = match &result {
                    &Ok(is_prime) => format!(
                        "{{\"method\":\"isPrime\",\"prime\":{}}}\n",
                        is_prime
                    ),
                    &Err(ref e) => format!(
                        "{{\"method\":\"isPrime\",\"error\":{:?}}}\n", e
                    ),
                };

                log::debug!(
                    "Client {} sending response: {:?}", client_n, &response
                );

                if let Err(ref e) = write_half.write_all(
                    response.as_bytes()
                ).await {
                    log::error!(
                        "Client {}: error writing response: {}", client_n, e
                    );
                    break;
                }

                // If `process_request()` returns an `Err`, the request was
                // malformed, so we're closing the connection.
                if result.is_err() { break; }

                // `.read_line()` appends to the target buffer, so we have
                // to clear the request we just processed out of it.
                buff.clear();
            },
            Err(e) => {
                log::error!(
                    "Client {}: error reading line from socket: {}",
                    client_n, &e
                );
                break;
            },
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
    log::info!("Listening on {}", LOCAL_ADDR);

    let mut client_n: usize = 0;

    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                log::debug!("Accepted client {} from {:?}", client_n, &addr);

                tokio::spawn(async move {
                    handle(socket, client_n).await
                });
                client_n += 1;
            },
            Err(e) => {
                log::error!("Error accepting connection: {}", &e);
            }
        }
    }
}
```

It works; I'll spare you the output.

The problem for next time inolves storing some state with each connection, and also reading and writing a binary format where Serde won't help us,[^serde_binary] but the asynchronous accept-communicate-shutdown arc of each connection will be essentially the same. The fourth problem will be considerably tougher, because the connections will have to communicate with each other, but we'll worry about that then.

[^serde_binary]: We _could_ use Serde for this, but implementing the `Serialize` and `Deserialize` traits for our nine-byte binary format would be far more trouble than benefit.