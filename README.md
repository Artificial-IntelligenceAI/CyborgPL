# CyborgPL

CyborgPL is a hobby programming language that compiles to native machine code via LLVM. It's a personal project for exploring language design and compiler internals — not a production tool.

> **Status: early work in progress.** Syntax and semantics are still actively being designed and can change without warning between commits. Several core features (functions, memory management, error handling) are only partially built out. Don't build anything real on top of this yet.

## What's implemented so far

- A full pipeline: lexer → parser → AST → LLVM codegen → native object file → linked binary (via [inkwell](https://github.com/TheDan64/inkwell)).
- `var:type 'name' = value;` variable declarations, with `ref:var:type 'name'` for every read/write reference.
- `START ... END` as the program's entry point; `fn 'name'(...) -> type { ... }` for function definitions.
- `if`/`else`, `while`, and real lexical block scoping (a variable declared inside a block is gone once the block ends).
- `print*"literal text" (expr) "more text"*;` — literal and computed segments mixed freely.
- Math operators: `x` (multiply), `xx` (power), `xxx` (tetration), plus the usual comparisons and boolean logic.
- Variables can share a name across different types, disambiguated by the type stated at each reference site.
- `num` supports selectable precision — `[precision:16/32/64/128]` — including a from-scratch software implementation of IEEE-754 quad precision (128-bit), since Apple's toolchain has no native support for it.
- `bignum`: arbitrary-precision decimal via GMP, with automatic memory management (handles are freed when they go out of scope, get reassigned, or a function returns) — no manual `free` needed.

## Requirements

- Rust (stable, 2024 edition)
- [LLVM](https://llvm.org/) 22.x — on macOS: `brew install llvm`
- [GMP](https://gmplib.org/) — on macOS: `brew install gmp`

This has only been built and tested on Apple Silicon macOS. `.cargo/config.toml` points at Homebrew's LLVM prefix (`/opt/homebrew/opt/llvm`); if your LLVM lives elsewhere, adjust `LLVM_SYS_221_PREFIX` there.

## Building and running

```bash
git clone https://github.com/Artificial-IntelligenceAI/CyborgPL.git
cd CyborgPL
cargo run -- examples/hello.cyborgpl
```

Running with no file argument compiles a small built-in demo instead:

```bash
cargo run
```

## Examples

```
fn 'add'('a': num, 'b': num) -> num {
    return ref:var:num 'a' + ref:var:num 'b';
}

START
    var:num 'first' = 5;
    var:num 'second' = 2.5;
    print*"first + second = " (ref:var:num 'first' + ref:var:num 'second')*;

    var:bignum 'big' = "3.14159265358979323846264338327950288419716939937510582097494459230781640628620899862803482534211706798214808651328230664709384460955058223172535940812848111745028410270193852110555964462294895493038196";
    print*"pi to 200 digits: " (ref:var:bignum 'big')*;
END
```

[`examples/chaos_print.cyborgpl`](examples/chaos_print.cyborgpl) stress-tests `print` in a single call — escaped quotes/tabs, emoji in both literal text and variable names, all four types, and the `xx`/`xxx` operators:

```
START

var:num 'apples🍎' = 42;
var:str 'weird_str' = "spécial \tvalue\n with \\backslash\\ and a 100% discount";
var:bool 'is_chaotic?!' = true;
var:bignum 'huge💥' = "31415926535897932384626433832795028841971693993751";

print*"Chaos test: \"quoted\", tab:\there, newline-escaped-below" (ref:var:str 'weird_str') "|| 🍎🔥💀 emoji-in-text || num=" (ref:var:num 'apples🍎') " || bool=" (ref:var:bool 'is_chaotic?!') " || bignum=" (ref:var:bignum 'huge💥') " || (num xx 2) x 3 + num xxx 2 = " (ref:var:num 'apples🍎' xx 2 x 3 + ref:var:num 'apples🍎' xxx 2) " || 100% done."*;

END
```

See [examples/](examples/) for more.

## Contributing

This is a solo, exploratory project and the language design is still very much in flux — I'm not reviewing external pull requests right now, since core syntax decisions are still being made and would likely conflict with in-progress work. That said, if you have ideas, questions, or spot a bug, **opening an issue is very welcome**.

## Design credit

The language design is mine; Claude Code implemented it. To be specific about which parts of the syntax were actual design decisions versus implementation defaults it picked and I never revisited:

**Designed by me:**
- `var:type 'name' = value;` variable syntax
- `START ... END` as the program's entry point
- `ref:var:type 'name'` for every reference, and the "quote everything" rules
- `x` / `xx` / `xxx` (multiply/power/tetration) — I chose to fully replace `*`; Claude proposed the `xx`/`xxx` naming and I confirmed it
- Same-name-different-type variables
- `[precision:16/32/64/128]` syntax
- The decision to add `bignum` via GMP, kept separate from `num`
- Base type keywords (`num`, `str`, `bool`)
- Automatic scope-based memory management (no manual `free`) — chosen among options Claude presented

**Defaulted by Claude** (conventional choices from the first scaffolding commit, never revisited):
- `fn 'name'(param: type, ...) -> type { ... }` function declaration syntax
- `if`/`else`/`while` keywords and brace-delimited blocks
- Comparison/logical operators' exact spelling

## Acknowledgments

Built in collaboration with [Claude Code](https://claude.com/claude-code) — most of the implementation work (lexer, parser, codegen, the from-scratch fp128 float support, the GMP-backed bignum type) was written together with it, with language design decisions made by the project author.

## License

Licensed under the [Apache License 2.0](LICENSE).
