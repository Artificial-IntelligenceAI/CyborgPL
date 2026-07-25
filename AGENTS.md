# For AI agents working in this repo

CyborgPL is an early-stage, work-in-progress hobby programming language that compiles to native code via LLVM. If you're an AI assistant reading this to get oriented, here's what actually matters.

## The one rule that overrides everything else

**Language syntax and semantics are the project author's creative decisions, not yours.** Don't add, remove, or change a keyword, operator, grammar rule, or block-scoping/typing behavior on your own judgment — propose options and let the human decide, even for choices that feel small or "obviously correct" from a language-design-101 perspective. This project's history includes multiple corrections after an AI assistant made exactly this mistake (see the "Design credit" section in [README.md](README.md) for which parts of the syntax were actually designed by the author versus defaulted by an assistant during early scaffolding).

Backend/implementation choices — how codegen works internally, which crate to use, how memory management is implemented under the hood — are fine to reason about and act on directly. Anything a user would notice as "the language does X" is not.

## Orientation

- `src/lexer.rs`, `src/token.rs` — tokenizing `.cyborgpl` source.
- `src/parser.rs`, `src/ast.rs` — recursive-descent parser and the AST it builds.
- `src/typecheck.rs` — a static type-checking pass run between parsing and codegen. Deliberately additive only: it mirrors codegen's own coercion rules exactly rather than adding new restrictions, so it should reject nothing that currently works, only turn what already didn't work into a clear error instead of a Rust/LLVM-level panic. If codegen's coercion rules ever change, this file needs the matching update or the two will drift out of sync.
- `src/codegen.rs` — the biggest file by far; walks the AST and emits LLVM IR via [inkwell](https://github.com/TheDan64/inkwell). Also owns block-scoping and `bignum`'s automatic memory management.
- `src/main.rs` — CLI entry point; compiles a `.cyborgpl` file (or a built-in demo) to an object file, links it with `cc`, then **runs the result as a child process**. If you're ever measuring the compiled program's memory/CPU behavior, measure that child process (`/tmp/cyborgpl_out`), not the `CyborgPL` binary itself — they're different processes and conflating them gives meaningless numbers.
- `runtime/fp128/quadmath.c` — a from-scratch software implementation of IEEE-754 quad precision (128-bit floats), since Apple's toolchain has no native 128-bit float type. `runtime/fp128/test_quadmath.c` is its standalone correctness harness (run it directly if you ever touch `quadmath.c`).
- `runtime/gmp/bignum_shim.c` — a thin C wrapper around GMP's `mpf_t`, giving `bignum` simple fixed function signatures to call from LLVM IR.
- `runtime/io/input_shim.c` — backs `input:str`/`input:num` reading from stdin (`getline`), and `cyborg_parse_num_or_die` (the number-validation half of `input:num`, shared between the stdin and file-source cases).
- `runtime/io/file_shim.c` — backs `print`'s/`overwrite`'s `[to*(dest)*]` file destination and `input:`'s `[from*(dest)*]` file source; `cyborg_fopen_or_die`/`cyborg_read_file_or_die` crash with a clear message instead of codegen having to emit its own null-check IR.
- `runtime/clock/clock_shim.c` — backs `clock:num`, elapsed seconds since the program started via `clock_gettime(CLOCK_MONOTONIC, ...)`, with the start timestamp captured by a C constructor that runs before `main`.
- `runtime/array/array_shim.c` — backs `var:array:TYPE`, a type-erased (element-size-only) growable buffer. Codegen alone knows and enforces the concrete element type at every call site (opaque pointers mean it can `load`/`store` through a raw slot pointer with no casting); this shim never sees anything but byte sizes. Freeing individual elements (for `str`/`bignum`/`file` element types, each independently heap-owned) is entirely codegen's job, via a real runtime loop over 1..=length — this shim's own `cyborg_array_free` only ever frees its own buffer.
- `runtime/int/int_shim.c` — one function, `cyborg_int_die(message)`, the shared crash-with-message path backing `var:int`'s overflow/division-by-zero guards. Overflow detection itself happens directly in codegen via LLVM's `llvm.s{add,sub,mul}.with.overflow.i64` intrinsics (declared like any other external function, by name and signature — no special "intrinsic" API needed); this shim only handles what to do once an overflow/div-by-zero is actually detected.
- `examples/*.cyborgpl` — working example programs, all verified to actually run, not just written and assumed correct.

## Verify, don't assume

This codebase has a strong established pattern: nothing gets reported as working, fixed, or broken without actually compiling and running a real `.cyborgpl` test file first (`cargo run -- path/to/file.cyborgpl`). Code review alone has repeatedly missed real bugs here (an ABI mismatch only visible on a function's *second* call, a pointer-identity comparison that silently never matched, a memory measurement that was quietly measuring the wrong process, a `while`-loop-in-a-user-program that only segfaulted after hundreds of thousands of iterations — see `entry_alloca` below). Test before claiming, and for anything that loops, test with a large iteration count, not just a handful.

`entry_alloca` (`src/codegen.rs`, next to `current_function`): any `build_alloca` call that ends up inside a runtime loop's body basic block (the cond/body/end blocks a hand-built LLVM loop uses, as opposed to a Rust-level `for` loop over a fixed, compile-time-known list) is a genuine *dynamic* stack allocation on every iteration — LLVM doesn't reclaim it until the function returns, only at loop-body-scope-exit like you'd expect from source-level semantics. Enough iterations (well under a million) overflows the stack and segfaults the compiled program. If you're adding a new runtime loop (another array helper, a new hyperoperator, anything with a `cond`/`body`/`end` block trio) and need scratch space inside the body, use `entry_alloca` instead of `self.builder.build_alloca` directly.

## Known rough edges (not secrets, just current state)

- Intermediate bignum binary-op results always compute at the default precision (256 bits), regardless of the operands' own precision.

If you're fixing or extending any of the above, the fix belongs in `src/codegen.rs`'s bignum handling — read the surrounding comments there first, they explain the reasoning, not just the mechanism.
