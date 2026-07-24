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

`*Warm.java` variants also exist for `long`/`BigInteger`: they run the same
loop 20 times inside one JVM process, so the JIT compiler gets a real chance
to optimize the hot loop, addressing the "Java measured cold" caveat below.
Runs 0–2 or so are still cold (interpreted/being JIT-compiled); later runs
show the actual steady-state, warmed performance.

**A methodology pitfall hit and fixed while building these**: the first
version of `ClockTestLongWarm` had no `volatile` sink for the accumulator —
once warmed, its JIT-compiled loop dropped to ~0 seconds, because a
sufficiently optimizing JIT can prove `for (i=0..300000) acc+=1` reduces to
the closed-form `acc=300000` and eliminate the loop entirely. This is the
exact same issue `std::hint::black_box` exists to prevent in Rust — an
optimizer smart enough to prove a loop's result is a compile-time constant
is under no obligation to actually run it. Fixed by writing to a `static
volatile` field every iteration, which the JIT can't prove is safe to
eliminate (another thread could theoretically observe it).

## Results

| Language | Accumulator | Elapsed (seconds, a few runs) |
|---|---|---|
| CyborgPL | `bignum` (GMP) | 0.034, 0.040, 0.047, 0.054 |
| CyborgPL | `num` (native float) | 0.0002, 0.0002, 0.0003, 0.0003 |
| Rust | `u64` (native int) | 0.00007, 0.00011, 0.00012, 0.00033 |
| Java | `BigInteger`, cold | 0.00534, 0.00534, 0.00540, 0.00516 |
| Java | `BigInteger`, warmed (steady-state, runs 10-19) | ~0.0017–0.0036 (noisy — likely GC pressure, see below) |
| Java | `long`, cold | 0.00051, 0.00058, 0.00068 |
| Java | `long`, warmed (steady-state, runs 10-19) | ~0.0000651, essentially flat |
| Python | `int` (arbitrary precision, small-int fast path) | 0.0091, 0.0128, 0.0130, 0.0138 |

Warmed `long` is the standout result here: once the JIT compiles it,
CyborgPL's fully-native-compiled `num` loop (~0.0002-0.0003s) is actually
*slower* than JIT-warmed Java's `long` loop (~0.000065s) — a real
illustration of what a mature, adaptive JIT can do once it's had the chance
to specialize hot code, versus CyborgPL's single fixed compile with one
optimization pass. Warmed `BigInteger` doesn't show the same clean
convergence — it stays noisy (roughly 0.0015-0.0036s, no flat steady
state like `long` reached), plausibly because every `.add()` call
allocates a new immutable `BigInteger` object, so warmup doesn't remove
garbage-collection pressure the way it removes interpretation/compilation
overhead for a simple numeric loop.

## Honest caveats

- **This is a handful of runs of a tiny, unrepresentative loop, not a real
  benchmark.** No statistical repeats beyond the 20-run warm variants, no
  attempt to control for system noise (other processes, thermal
  throttling, etc.). Treat every number as "roughly this order of
  magnitude," not precise.
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

# Java, warmed (20 runs in one JVM, look at the later runs for steady-state)
javac benchmarks/ClockTestBigIntegerWarm.java benchmarks/ClockTestLongWarm.java -d /tmp
java -cp /tmp ClockTestBigIntegerWarm
java -cp /tmp ClockTestLongWarm
```
