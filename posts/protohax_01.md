title: Protohackers in Rust, Part 01
subtitle: Prime numbers and an async subtlety
time: 2023-01-15 22:04:00
og_image: https://d2718.net/blog/images/rust_logo.png
og_desc: More async Rust in action solving the second Protohackers problem

_This is the second post in a series. If you're not sure what's going on or why you're here, reading the somewhat wordy [first post](https://d2718.net/blog/posts/protohax_00.html) may give you some context._

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

Before we start building our server, we should be sure we can determine whether a number is prime, right? This is the "business logic" portion of our program. There [are](https://docs.rs/primes/latest/primes/) [plenty](https://docs.rs/num-prime/latest/num_prime/) [of](https://docs.rs/is_prime/2.0.9/is_prime/) [crates](https://huonw.github.io/primal/primal/index.html) that will do this for us, but I majored in Math[^math] and also solved the first 70-or-so [Project Euler problems](https://projecteuler.net/archives) in three different languages, so I figure this is something we can get right ourselves. Also, I _think_ that writing this ourselves (at least the way I'm planning on writing it) will give us another opportunity to talk about a detail of `async` task scheduling in Rust.

[^math]: That's "Maths" if you're from one of those "zed" countries.

The maximally naïve approach to checking a number's primality is to divide it by all numbers[^all_numbers] less than it; if it's evenly divisible by any of them, it's composite. We can reduce the amount of arithmetic we need to do by observing two things:

  * If a number is divisible by _n_, then it's also divisible by all the factors of _n_.
  * For every factor a number has that's greater than its square root, it has a complementary factor that's less than its square root.

The first statement means that we only need to try dividing by _prime_ numbers. Given any composite that would evenly divide our candidate prime, any prime that divides _it_ will also evenly divide our candidate. The second statement means that we only need to check any given prime candidate for divisibility _up through its square root_.

[^all_numbers]: By "all numbers" I mean "all integers greater than 1", of course. Everything is divisible by 1 and non-integers aren't really involved when talking about primality.

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

And also I'll write a test just to make sure this is working:

```rust
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