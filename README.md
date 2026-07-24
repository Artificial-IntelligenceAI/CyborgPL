# CyborgPL

CyborgPL is a hobby programming language that compiles to native machine code via LLVM. It's a personal project for exploring language design and compiler internals — not a production tool.

> **Status: early work in progress.** Syntax and semantics are still actively being designed and can change without warning between commits. Several core features (functions, memory management, error handling) are only partially built out. Don't build anything real on top of this yet.

## What's implemented so far

- A full pipeline: lexer → parser → AST → LLVM codegen → native object file → linked binary (via [inkwell](https://github.com/TheDan64/inkwell)).
- `var:type 'name' = value;` variable declarations, with `ref:var:type 'name'` for every read/write reference.
- `START ... END` as the program's entry point; `func 'name'*'param': type, ...* -> type { ... }` for function definitions, called via `ref:func 'name'*arg, arg, ...*` (mirroring `print*...*`'s own bracketing of a list of things). Parameters and return types support `[precision:N]` and `bignum` — arguments are coerced to match, and a returned `bignum` is always an independent copy the caller owns.
- `if`/`else`, `while`, and real lexical block scoping (a variable declared inside a block is gone once the block ends).
- Every value (a literal or a `ref:var:type 'name'` reference) must be individually wrapped in `( )` to be used in any expression — `(a) + (b)`, not `a + b`. A reassignment's target is the one exception (`ref:var:type 'name' = ...` stays bare, since it's a place, not a value); a function call doesn't need an extra wrap around the whole call either, though each argument still needs its own.
- `print*"literal text" (expr) "more text"*;` — literal and computed segments mixed freely; a computed segment needs no separate marker of its own since every value already starts with `(` (or a function-call name), which is enough to tell it apart from literal text.
- Math operators: `x` (multiply), `xx` (power), `xxx` (tetration), plus the usual comparisons and boolean logic.
- Variables can share a name across different types, disambiguated by the type stated at each reference site.
- `num` supports selectable precision — `[precision:16/32/64/128]` — including a from-scratch software implementation of IEEE-754 quad precision (128-bit), since Apple's toolchain has no native support for it.
- `bignum`: arbitrary-precision decimal via GMP, with automatic memory management (handles are freed when they go out of scope, get reassigned, or a function returns) — no manual `free` needed. `xx`/`xxx`/unary negation/comparisons (`<`, `==`, etc.) all work on it, including as `if`/`while` conditions.
- `numw` ("number word"): a `num` in every runtime respect (including `[precision:16/32/64/128]`), but its own type, that also accepts an English-magnitude-word literal form — `var:numw 'apples' = '1 million';`. Supports negatives and decimals (`'-2.5 billion'`); a bare number with no word is just that number.

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
func 'add'*'a': num, 'b': num* -> num {
    return (ref:var:num 'a') + (ref:var:num 'b');
}

START
    var:num 'first' = (5);
    var:num 'second' = (2.5);
    print*"first + second = " (ref:var:num 'first') + (ref:var:num 'second')*;

    var:bignum 'big' = ("3.14159265358979323846264338327950288419716939937510582097494459230781640628620899862803482534211706798214808651328230664709384460955058223172535940812848111745028410270193852110555964462294895493038196");
    print*"pi to 200 digits: " (ref:var:bignum 'big')*;
END
```

[`examples/chaos_print.cyborgpl`](examples/chaos_print.cyborgpl) stress-tests `print` in a single call — escaped quotes/tabs, emoji in both literal text and variable names, all four types, and the `xx`/`xxx` operators:

```
START

var:num 'apples🍎' = (42);
var:str 'weird_str' = ("spécial \tvalue\n with \\backslash\\ and a 100% discount");
var:bool 'is_chaotic?!' = (true);
var:bignum 'huge💥' = ("31415926535897932384626433832795028841971693993751");

print*"Chaos test: \"quoted\", tab:\there, newline-escaped-below" (ref:var:str 'weird_str') "|| 🍎🔥💀 emoji-in-text || num=" (ref:var:num 'apples🍎') " || bool=" (ref:var:bool 'is_chaotic?!') " || bignum=" (ref:var:bignum 'huge💥') " || (num xx 2) x 3 + num xxx 2 = " (ref:var:num 'apples🍎') xx (2) x (3) + (ref:var:num 'apples🍎') xxx (2) " || 100% done."*;

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
- `numw` and its `'1 million'`-style literal syntax
- `func 'name'*param, ...*` — replacing the original defaulted `fn` keyword and `( )` parameter list with `func` and `*...*`, matching `print`'s own bracketing style and the `ref:func` call syntax
- `ref:func 'name'*arg, ...*` — replacing the original defaulted bare `'name'(...)` call syntax, mirroring `ref:var:TYPE 'name'`'s shape for reading a variable

**Defaulted by Claude** (conventional choices from the first scaffolding commit, never revisited):
- `if`/`else`/`while` keywords and brace-delimited blocks
- Comparison/logical operators' exact spelling

## Acknowledgments

Built in collaboration with [Claude Code](https://claude.com/claude-code) — most of the implementation work (lexer, parser, codegen, the from-scratch fp128 float support, the GMP-backed bignum type) was written together with it, with language design decisions made by the project author.

## License

Licensed under the [Apache License 2.0](LICENSE).
