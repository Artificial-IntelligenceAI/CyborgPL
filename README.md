# CyborgPL

CyborgPL is a hobby programming language that compiles to native machine code via LLVM. It's a personal project for exploring language design and compiler internals — not a production tool.

> **Status: early work in progress.** Syntax and semantics are still actively being designed and can change without warning between commits. Several core features (functions, memory management, error handling) are only partially built out. Don't build anything real on top of this yet.

## What's implemented so far

- A full pipeline: lexer → parser → AST → type check → LLVM codegen → `mem2reg` → native object file → linked binary (via [inkwell](https://github.com/TheDan64/inkwell)).
- `var:type 'name' = value;` variable declarations, with `ref:var:type 'name'` for every read/write reference.
- `START ... END` as the program's entry point; `func 'name'*'param': type, ...* -> type { ... }` for function definitions, called via `ref:func 'name'*arg, arg, ...*` (mirroring `print*...*`'s own bracketing of a list of things). Parameters and return types support `[precision:N]` and `bignum` — arguments are coerced to match, and a returned `bignum` is always an independent copy the caller owns.
- `if`/`else`, `while`, and real lexical block scoping (a variable declared inside a block is gone once the block ends).
- Every value (a literal or a `ref:var:type 'name'` reference) must be individually wrapped in `( )` to be used in any expression — `(a) + (b)`, not `a + b`. A reassignment's target is the one exception (`ref:var:type 'name' = ...` stays bare, since it's a place, not a value); a function call doesn't need an extra wrap around the whole call either, though each argument still needs its own.
- `print*"literal text" (expr) "more text"*;` — literal and computed segments mixed freely; a computed segment needs no separate marker of its own since every value already starts with `(` (or a function-call name), which is enough to tell it apart from literal text.
- Math operators: `x` (multiply), `xx` (power), `xxx` (tetration), postfix `!` (factorial, on `num`/`numw`/`bignum`), plus the usual comparisons and boolean logic.
- Variables can share a name across different types, disambiguated by the type stated at each reference site.
- `num` supports selectable precision — `[precision:16/32/64/128]` — including a from-scratch software implementation of IEEE-754 quad precision (128-bit), since Apple's toolchain has no native support for it.
- `bignum`: arbitrary-precision decimal via GMP, with automatic memory management (handles are freed when they go out of scope, get reassigned, or a function returns) — no manual `free` needed. `xx`/`xxx`/unary negation/comparisons (`<`, `==`, etc.) all work on it, including as `if`/`while` conditions. A bare numeric literal (`var:bignum 'x' = (999999999999999999999999999999999999999);`) keeps its full precision — the parser preserves a literal's original digit text specifically for this. `bignum` can also mix directly with `num`/`numw` in an expression (e.g. `(ref:var:bignum 'n') - (1)`) — the plain side is promoted to a `bignum` automatically.
- `numw` ("number word"): a `num` in every runtime respect (including `[precision:16/32/64/128]`), but its own type, that also accepts an English-magnitude-word literal form — `var:numw 'apples' = '1 million';`. Supports negatives and decimals (`'-2.5 billion'`); a bare number with no word is just that number.
- `stch` ("stitch"): text concatenation — `("count is ") stch (42)` — auto-converting a non-`str` operand to the same display text `print` would give it. Always produces a fresh, independently-owned `str`, usable in a loop-accumulated string or returned from a function, with the same kind of automatic memory management `bignum` already has (every `str` in a variable is its own owned copy, freed at scope exit/reassignment/return — no manual `free` needed).
- `input:type 'name' [from*(source)*];` — reads into a new variable, for `str` (raw text) or `num` (parsed as a number; invalid/non-numeric input is a fatal runtime error with a clear message, not a silent default). No `[from*...*]` clause reads a line from stdin, same as before. With one, `source` (a `file` or a plain `str` path) is read as a whole file — its *entire* content becomes the `str` (unlike stdin's version, this doesn't strip a trailing newline), or the entire content is parsed as one number. No built-in prompt for the stdin case — `print` your own prompt text first.
- A static type-checking pass runs between parsing and codegen, catching type mistakes (a wrong type stated at `ref:var:TYPE`, mismatched operator operands, wrong function argument types/count, a non-`bool` `if`/`while` condition, a returned value that doesn't match the declared return type) with clear, readable errors — instead of a Rust-level panic or a confusing generic message. It doesn't change what's *allowed*: every coercion codegen already does (num/numw mixing at any precision, num/numw promoting into bignum, etc.) still works exactly the same; this only adds clearer errors for what already didn't work.
- `clock:num 'name';` — reads elapsed seconds (as a decimal) since the program started into a new variable. Read it before and after any span of code (a loop, a function call) and subtract to measure how long it took.
- `file`: a path string, behaving exactly like `str` (same automatic memory management, freely interchangeable with `str`) but its own type for clarity. Writing: `print*...* [to*(dest)*];` optionally redirects one `print` call to the file at `dest` instead of the screen; `overwrite*...* [to*(dest)*];` always writes to a file (never the screen). Both always replace the destination's entire content (no append), and both crash with a clear message if the file can't be opened. `dest` can be a `file` or a plain `str` path directly. Reading: see `input:` above.
- Comments: `#` at the start of a line comments out that one line; `#N` (e.g. `#5`) comments out that line and the N-1 lines below it. Only recognized as the first non-whitespace thing on a line, never trailing after code on the same line.
- `var:array:TYPE 'name' = {(v1), (v2), ...};` — a growable array of any existing type (`num`, `numw`, `bool`, `str`, `bignum`, `file`; no nested arrays). `ref:var:array:TYPE 'name'*(index)*` reads or writes a single element (1-indexed — the first element is index 1); `append*(array), (value)*;` grows it by one; `(length*(array)*)` gives its element count as a `num`. An out-of-range index crashes with a clear message rather than reading/writing past the buffer. Reassigning or storing an array always makes an independent deep copy (heap-owned elements — `str`/`file`/`bignum` — get their own copy too), so two array variables never end up sharing the same underlying buffer; automatic memory management frees an array (and, for heap-owned element types, every element in it) at scope exit, reassignment, or function return, same as `bignum`/`str`.

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

## Benchmarks

[benchmarks/](benchmarks/) has a small, honestly-caveated timing comparison against Python, Rust, and Java on one tiny loop — not a rigorous study, just curiosity about where a brand-new, mostly-unoptimized hobby compiler currently stands next to mature ones.

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
- `not` / `not=` — replacing the originally-defaulted `!` (boolean not) and `!=` (not-equal) entirely
- Postfix `!` for factorial, reusing the character freed up by the `not`/`not=` rename, on `num`/`numw`/`bignum`
- `stch` for text concatenation — a real word-operator, kept separate from `+` (which stays num-only), auto-converting non-`str` operands
- `input:type 'name';` for reading stdin — a dedicated declaration statement rather than treating `input` as a value; crashing on invalid `num` input rather than silently defaulting to 0
- The decision to add a real type checker, and to keep it purely additive — every existing auto-coercion still works exactly as before, it only replaces confusing errors with clear ones, rather than becoming stricter
- `clock:num 'name';` — elapsed time since the program started (not absolute/wall-clock time), as a dedicated declare-statement mirroring `input:`'s shape
- File writing: `[to*(dest)*]` as a literal bracketed clause (optional on `print`, required on `overwrite`) rather than a value-position keyword; `overwrite` (not "write") to name that it always replaces the whole file; always-overwrite rather than append; crash with a clear message on failure, matching `input:num`'s precedent; `file` as its own type (freely interchangeable with `str`) rather than reusing `str` directly
- `#`/`#N` for comments — replacing the previously-undocumented, Claude-defaulted `//` entirely; `#N` commenting out N total lines (rather than, say, a paired open/close block-comment marker) was the author's own idea
- File reading: extending `input:`'s existing shape with `[from*(source)*]` (mirroring `[to*(dest)*]`) rather than a new dedicated keyword, since reading from stdin vs. a file is just a different source for the same "read into a variable" action; reading a file's *whole* content rather than one line
- `var:array:TYPE 'name'` — the element type stated via colon-chaining, matching `ref:var:TYPE`/`input:TYPE`/`clock:TYPE`'s existing shape; `{ }` (not `[ ]`, already taken by `[precision:N]` and `[to/from*(dest)*]`) for array literals, with each element still individually wrapped in `( )` like every other value; growable rather than fixed-size; 1-based indexing (`ref:var:array:TYPE 'name'*(1)*` is the first element) — a deliberate departure from C/Python/Rust/JS's 0-based convention; `append*(array), (value)*;` as a dedicated statement rather than a value-position operation; `(length*(array)*)` as a value-position construct (still needing its own `( )` wrap, unlike a function call); supporting every existing type as an element type (including the heap-owned `str`/`bignum`/`file`) rather than starting narrower

**Defaulted by Claude** (conventional choices from the first scaffolding commit, never revisited):
- `if`/`else`/`while` keywords and brace-delimited blocks
- Remaining comparison/logical operators' exact spelling (`<`, `>`, `<=`, `>=`, `==`, `&&`, `||`)

## Acknowledgments

Built in collaboration with [Claude Code](https://claude.com/claude-code) — most of the implementation work (lexer, parser, codegen, the from-scratch fp128 float support, the GMP-backed bignum type) was written together with it, with language design decisions made by the project author.

## License

Licensed under the [Apache License 2.0](LICENSE).
