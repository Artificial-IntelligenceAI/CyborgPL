# A small cross-language timing comparison

Not a rigorous benchmark suite — one tiny workload, run a handful of times on
one machine, no statistical rigor. Posted for honesty and curiosity, not as a
claim that CyborgPL is fast (it isn't, yet) or that this says anything
general about Python/Rust/Java's real-world performance.

## The workload

Same idea in every language: start a timer, run a loop 300,000 times
accumulating into a counter, stop the timer, print the elapsed time. Two
variants per language where practical:

- **A native number accumulator** (CyborgPL `num`, Rust `u64`, Java `long`) —
  the fair, apples-to-apples comparison, since all three are plain
  fixed-width machine numbers.
- **An arbitrary-precision accumulator** (CyborgPL `bignum` via GMP, Java
  `BigInteger`, Python `int` — Python's `int` is *always* arbitrary
  precision, there's no separate "native" variant to write) — not
  comparable to each other in a strict sense, since each language's
  arbitrary-precision implementation works completely differently
  internally (GMP's `mpf_t` is actually an arbitrary-precision *float*, not
  even an integer type; CPython's `int` has a small-int fast path that
  keeps small values cheap; Java's `BigInteger` is immutable, so every
  `.add()` allocates a new object).

Source files are all in this directory, exactly as run.

## Results

| Language | Accumulator | Elapsed (seconds, a few runs) |
|---|---|---|
| CyborgPL | `bignum` (GMP) | 0.034, 0.040, 0.047 |
| CyborgPL | `num` (native float) | 0.0002, 0.0002, 0.0003 |
| Rust | `u64` (native int) | 0.00007, 0.00011, 0.00012 |
| Java | `BigInteger` | 0.00534, 0.00534, 0.00540 |
| Java | `long` (native int) | 0.00051, 0.00058, 0.00068 |
| Python | `int` (arbitrary precision, small-int fast path) | 0.0091, 0.0128, 0.0130 |

## Honest caveats

- **This is one run of a tiny, unrepresentative loop, not a real benchmark.**
  No warmup iterations, no statistical repeats, no attempt to control for
  system noise. Treat every number as "roughly this order of magnitude,"
  not precise.
- **Java is measured cold — no JIT warmup.** The JVM's real performance
  advantage comes from its JIT compiler optimizing hot code after it's run
  many times; a single 300,000-iteration run barely gives it the chance.
  Java's numbers here likely understate what it can actually do.
- **CyborgPL is a brand-new hobby compiler** with exactly one optimization
  pass (`mem2reg`) — no inlining, no loop optimizations, no constant
  folding across statements. Rust, the JVM, and CPython are all mature,
  heavily-engineered systems with years (Rust, CPython) or decades (JVM) of
  optimization work behind them. The gap here reflects that difference in
  maturity, not some fundamental ceiling on what CyborgPL's approach could
  achieve.
- **The `bignum`/`BigInteger`/Python-`int` row isn't a fair three-way
  comparison** — see "The workload" above. It's included for completeness,
  not as a ranking.
- Measured on: macOS 26.5.2, Apple M5. Python 3.9.6, rustc 1.96.1, OpenJDK
  26.0.1 (Homebrew), and whatever CyborgPL/LLVM commit was current when this
  was written.

## Reproducing

```bash
# CyborgPL (from the repo root)
cargo run -- benchmarks/clock_test_bignum.cyborgpl
cargo run -- benchmarks/clock_test_num.cyborgpl

# Python
python3 benchmarks/clock_test.py

# Rust
rustc -O benchmarks/clock_test.rs -o /tmp/clock_test_rust && /tmp/clock_test_rust

# Java (on macOS via Homebrew, openjdk is keg-only -- not on PATH by
# default, so this used the full path: /opt/homebrew/opt/openjdk/bin/)
javac benchmarks/ClockTestBigInteger.java benchmarks/ClockTestLong.java -d /tmp
java -cp /tmp ClockTestBigInteger
java -cp /tmp ClockTestLong
```
