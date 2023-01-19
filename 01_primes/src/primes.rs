/*!
A primality checker.
*/

#[derive(Debug)]
pub struct Primes {
    known: Vec<u64>
}

impl Default for Primes {
    fn default() -> Self {
        Self { known: vec![2] }
    }
}

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