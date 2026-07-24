# For AI agents working in this repo

CyborgPL is an early-stage, work-in-progress hobby programming language that compiles to native code via LLVM. If you're an AI assistant reading this to get oriented, here's what actually matters.

## The one rule that overrides everything else

**Language syntax and semantics are the project author's creative decisions, not yours.** Don't add, remove, or change a keyword, operator, grammar rule, or block-scoping/typing behavior on your own judgment — propose options and let the human decide, even for choices that feel small or "obviously correct" from a language-design-101 perspective. This project's history includes multiple corrections after an AI assistant made exactly this mistake (see the "Design credit" section in [README.md](README.md) for which parts of the syntax were actually designed by the author versus defaulted by an assistant during early scaffolding).

Backend/implementation choices — how codegen works internally, which crate to use, how memory management is implemented under the hood — are fine to reason about and act on directly. Anything a user would notice as "the language does X" is not.

## Orientation

- `src/lexer.rs`, `src/token.rs` — tokenizing `.cyborgpl` source.
- `src/parser.rs`, `src/ast.rs` — recursive-descent parser and the AST it builds.
- `src/codegen.rs` — the biggest file by far; walks the AST and emits LLVM IR via [inkwell](https://github.com/TheDan64/inkwell). Also owns block-scoping and `bignum`'s automatic memory management.
- `src/main.rs` — CLI entry point; compiles a `.cyborgpl` file (or a built-in demo) to an object file, links it with `cc`, then **runs the result as a child process**. If you're ever measuring the compiled program's memory/CPU behavior, measure that child process (`/tmp/cyborgpl_out`), not the `CyborgPL` binary itself — they're different processes and conflating them gives meaningless numbers.
- `runtime/fp128/quadmath.c` — a from-scratch software implementation of IEEE-754 quad precision (128-bit floats), since Apple's toolchain has no native 128-bit float type. `runtime/fp128/test_quadmath.c` is its standalone correctness harness (run it directly if you ever touch `quadmath.c`).
- `runtime/gmp/bignum_shim.c` — a thin C wrapper around GMP's `mpf_t`, giving `bignum` simple fixed function signatures to call from LLVM IR.
- `runtime/io/input_shim.c` — backs `input:str`/`input:num`, reading a line from stdin via `getline`.
- `examples/*.cyborgpl` — working example programs, all verified to actually run, not just written and assumed correct.

## Verify, don't assume

This codebase has a strong established pattern: nothing gets reported as working, fixed, or broken without actually compiling and running a real `.cyborgpl` test file first (`cargo run -- path/to/file.cyborgpl`). Code review alone has repeatedly missed real bugs here (an ABI mismatch only visible on a function's *second* call, a pointer-identity comparison that silently never matched, a memory measurement that was quietly measuring the wrong process). Test before claiming.

## Known rough edges (not secrets, just current state)

- Intermediate bignum binary-op results always compute at the default precision (256 bits), regardless of the operands' own precision.

If you're fixing or extending any of the above, the fix belongs in `src/codegen.rs`'s bignum handling — read the surrounding comments there first, they explain the reasoning, not just the mechanism.
