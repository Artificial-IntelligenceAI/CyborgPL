# CyborgPL

# MUST READ: CyborgPL may be abandoned, until I get a bigger Claude Subscription (and when I want to work on it). because, as you can see in my GitHub. I'm working on more projects, like a horror MC mod, etc.

CyborgPL is a hobby programming language that compiles to native machine code via LLVM. It's a personal project for exploring language design and compiler internals — not a production tool.

> **Status: early work in progress.** Syntax and semantics are still actively being designed and can change without warning between commits. Several core features (functions, memory management, error handling) are only partially built out. Don't build anything real on top of this yet.

## What's implemented so far

- A full pipeline: lexer → parser → AST → type check → LLVM codegen → LLVM's standard `-O2` optimization pipeline (inlining, GVN, dead-code elimination, instcombine, loop optimizations, etc.) → native object file → linked binary (via [inkwell](https://github.com/TheDan64/inkwell)).
- `var:type 'name' = value;` variable declarations, with `ref:var:type 'name'` for every read/write reference.
- `START ... END` as the program's entry point; `func 'name'*'param': type, ...* -> type { ... }` for function definitions, called via `ref:func 'name'*arg, arg, ...*` (mirroring `print*...*`'s own bracketing of a list of things) — or its shorter alternative spelling, `>> 'name'*arg, arg, ...*`, both work everywhere a call can appear (a print segment, a bare statement, a var-decl value, a `length*...*`/`append*...*` argument, another call's argument, ...). Parameters and return types support `[precision:N]` and `bignum` — arguments are coerced to match, and a returned `bignum` is always an independent copy the caller owns.
- `if`/`else`, `while`, and real lexical block scoping (a variable declared inside a block is gone once the block ends).
- Every value (a literal or a `ref:var:type 'name'` reference) must be individually wrapped in `( )` to be used in any expression — `(a) + (b)`, not `a + b`. A reassignment's target is the one exception (`ref:var:type 'name' = ...` stays bare, since it's a place, not a value); a function call doesn't need an extra wrap around the whole call either, though each argument still needs its own.
- `print*"literal text" (expr) "more text"*;` — literal and computed segments mixed freely; a computed segment needs no separate marker of its own since every value already starts with `(` (or a function-call name), which is enough to tell it apart from literal text.
- Math operators: `x` (multiply), `xx` (power), `xxx` (tetration), postfix `!` (factorial, on `num`/`numw`/`bignum`), plus the usual comparisons and boolean logic.
- Variables can share a name across different types, disambiguated by the type stated at each reference site.
- `num` supports selectable precision — `[precision:16/32/64/128]` — including a from-scratch software implementation of IEEE-754 quad precision (128-bit), since Apple's toolchain has no native support for it.
- Any numeric literal (`num`, `int`, `bignum`, `numw`'s own numeric part) can use `,` as a digit-grouping separator for readability — `(1,000,000)` — purely cosmetic, stripped at the lexer level, same "readability aid, not enforced grouping" role Rust's `_` plays (`(1,00,0)` is accepted too, just not conventionally grouped, and still equals `1000`). Implemented once in the shared lexer, so every numeric type gets it automatically. Never ambiguous with an actual argument/element separator, since every value is individually parenthesized — a real separating comma always follows a closing `)`, never a bare digit.
- `bignum`: arbitrary-precision decimal via GMP, with automatic memory management (handles are freed when they go out of scope, get reassigned, or a function returns) — no manual `free` needed. `xx`/`xxx`/unary negation/comparisons (`<`, `==`, etc.) all work on it, including as `if`/`while` conditions. A bare numeric literal (`var:bignum 'x' = (999999999999999999999999999999999999999);`) keeps its full precision — the parser preserves a literal's original digit text specifically for this. `bignum` can also mix directly with `num`/`numw` in an expression (e.g. `(ref:var:bignum 'n') - (1)`) — the plain side is promoted to match the bignum side's own precision automatically. An intermediate binary-op result widens to the larger of its two operands' own precisions (mirroring `num`'s "widen to the larger operand" behavior) rather than always computing at a fixed default — negation preserves an operand's own precision the same way, while factorial still deliberately forces a fixed default precision regardless of the operand's, matching `num`/`int`'s own factorial convention. A bare literal combined with a `bignum` inside a `while` loop (e.g. `acc + (1)` in a running total) is constructed once, before the loop starts, rather than paying for a fresh GMP allocation on every single iteration — measured at roughly 1.68x faster on a tight accumulation loop. A chained `+`/`-`/`x`/`/` expression (`a + b + c + d`, `a x b x c x d`) accumulates into a single handle instead of allocating and discarding a fresh one for every intermediate step — measured at roughly 1.8x faster for one extra `+`/`-` term up to ~2.5-2.8x for a 4-op chain, similarly for `x`, and ~1.45x for `/` (division's own cost dominates more, so the allocation removed is a smaller slice of the total). A chained `xx` expression (`a xx b xx c`) gets the same treatment too, on its right operand specifically -- `xx` parses right-associative, so a real chain's reusable intermediate is the exponent side, not the base -- measured at roughly 1.28-1.7x. `xxx` (tetration)'s own internal computation loop (used by a single `xxx`, not just a chain of them) got the same fix independently: it used to allocate a fresh handle on every internal step of computing one tetration, now reuses one handle throughout — measured at roughly 1.6x faster even at a modest tower height. Underneath all of that, every `bignum` construction (whether or not it's one of the reuse cases above) now draws from [mimalloc](https://github.com/microsoft/mimalloc) instead of asking the system allocator fresh each time — a `bignum` costs two heap allocations at construction (the handle itself, plus GMP's own internal digit-storage buffer), and reusing already-freed memory instead of hitting the system allocator cuts a tight construct/use/free cycle by roughly 2.5-8x depending on how much other work surrounds it. (An earlier version hand-rolled its own freelist pool for this — replaced with mimalloc since a hand-rolled pool never gives memory back to the OS once cached, fine for a short script but a real problem the moment a longer-running program exists; mimalloc's decay-based reclamation gets the same reuse speedup while still releasing memory that's gone unused for a while, the same fix real-world Rust services reach for — e.g. TiKV's `tikv-jemallocator` — when the default allocator doesn't give memory back well enough on its own.)
- `numw` ("number word"): a `num` in every runtime respect (including `[precision:16/32/64/128]`), but its own type, that also accepts an English-magnitude-word literal form — `var:numw 'apples' = '1 million';`. Supports negatives and decimals (`'-2.5 billion'`); a bare number with no word is just that number.
- `stch` ("stitch"): text concatenation — `("count is ") stch (42)` — auto-converting a non-`str` operand to the same display text `print` would give it. Always produces a fresh, independently-owned `str`, usable in a loop-accumulated string or returned from a function, with the same kind of automatic memory management `bignum` already has (every `str` in a variable is its own owned copy, freed at scope exit/reassignment/return — no manual `free` needed). Storing a `stch` result or a str-returning call's value adopts the existing buffer directly instead of taking a redundant extra copy, mirroring the same optimization `bignum` already has — measured roughly 1.2x faster on a loop that repeatedly builds and stores one.
- `input:type 'name' [from*(source)*];` — reads into a new variable, for `str` (raw text) or `num` (parsed as a number; invalid/non-numeric input is a fatal runtime error with a clear message, not a silent default). No `[from*...*]` clause reads a line from stdin, same as before. With one, `source` (a `file` or a plain `str` path) is read as a whole file — its *entire* content becomes the `str` (unlike stdin's version, this doesn't strip a trailing newline), or the entire content is parsed as one number. No built-in prompt for the stdin case — `print` your own prompt text first.
- A static type-checking pass runs between parsing and codegen, catching type mistakes (a wrong type stated at `ref:var:TYPE`, mismatched operator operands, wrong function argument types/count, a non-`bool` `if`/`while` condition, a returned value that doesn't match the declared return type) with clear, readable errors — instead of a Rust-level panic or a confusing generic message. It doesn't change what's *allowed*: every coercion codegen already does (num/numw mixing at any precision, num/numw promoting into bignum, etc.) still works exactly the same; this only adds clearer errors for what already didn't work.
- `clock:num 'name';` — reads elapsed seconds (as a decimal) since the program started into a new variable. Read it before and after any span of code (a loop, a function call) and subtract to measure how long it took.
- `file`: a path string, behaving exactly like `str` (same automatic memory management, freely interchangeable with `str`) but its own type for clarity. Writing: `print*...* [to*(dest)*];` optionally redirects one `print` call to the file at `dest` instead of the screen; `overwrite*...* [to*(dest)*];` always writes to a file (never the screen). Both always replace the destination's entire content (no append), and both crash with a clear message if the file can't be opened. `dest` can be a `file` or a plain `str` path directly. Reading: see `input:` above. `read*(source)*;` is a shorthand for the common "read a file and show me what's in it" case — reads `source`'s whole content and prints it directly, with no variable declared in between; `source` can be a `file` or a plain `str` path, same as everywhere else, and it crashes the same way `input:`/`overwrite` do if the file can't be opened.
- Comments: `#` at the start of a line comments out that one line; `#N` (e.g. `#5`) comments out that line and the N-1 lines below it. Only recognized as the first non-whitespace thing on a line, never trailing after code on the same line.
- `var:array:TYPE 'name' = {(v1), (v2), ...};` — a growable array of any existing type (`num`, `numw`, `bool`, `str`, `bignum`, `file`, `int`; no nested arrays). `ref:var:array:TYPE 'name'*(index)*` reads or writes a single element (1-indexed — the first element is index 1); `append*(array), (value)*;` grows it by one; `(length*array*)` gives its element count as a `num` — the array argument doesn't need its own `( )` wrap (unlike most values), since indexing an array can never sensibly appear there anyway. An out-of-range index crashes with a clear message rather than reading/writing past the buffer. Reassigning or storing an array always makes an independent deep copy (heap-owned elements — `str`/`file`/`bignum` — get their own copy too), so two array variables never end up sharing the same underlying buffer; automatic memory management frees an array (and, for heap-owned element types, every element in it) at scope exit, reassignment, or function return, same as `bignum`/`str`.
- `int`: a genuine whole-number type — a real integer at the LLVM level, not a float with a rule bolted on, so `+`/`-`/`x`/`xx`/`xxx`/`!` are all exact and `/` truncates rather than producing a fraction. `[precision:8/16/32/64]`, default 64 — real hardware integer widths, unlike `num`'s IEEE-754 float widths. Doesn't coerce from `num`/`numw`/`bignum` (only from `int` itself, like `bool`) — a fractional value ending up in an `int` is exactly what the type exists to rule out, checked both for a direct decimal literal (`var:int 'x' = (5.5);`) and a decimal literal mixed into an `int` expression. Freely coerces between its own different widths (mirroring `num`/`numw`'s "any precision" convention) — widening is always exact, narrowing is checked at runtime and crashes if the value doesn't actually fit. Every arithmetic op — including inside `xx`/`xxx`/`!`'s internal loops, and immediately on computation rather than only when later stored somewhere — crashes with a clear message on overflow rather than silently wrapping (two's-complement), the same "loud failure over silent wrong data" precedent as an out-of-range array index; division by zero and negating a type's minimum value crash the same way. When every operand in an expression is a literal (e.g. `(5000000000) x (5000000000)`, or a single literal that doesn't fit its declared/paired width on its own), the type checker catches the overflow at compile time instead of letting the program build successfully only to crash the moment it's run — a genuine variable or function-call result is still only checked at runtime, since its value isn't known until the program actually executes.
- `bigint`: arbitrary-precision integer (GMP's `mpz_t`) — `int`'s exact counterpart, but never overflows, since it just grows to hold whatever value it's given instead of being bound to a fixed hardware width. No `[precision:N]` at all — there's no size to choose. Isolated like `int`: only coerces from `bigint` itself, no mixing with `num`/`numw`/`bignum`/`int` in the same expression, and a fractional literal assigned to it is a compile-time error. `+`/`-`/`x`/`/`/`xx`/`xxx`/`!` plus the usual comparisons all work, with automatic scope-based memory management (freed at scope exit/reassignment/return) mirroring `bignum`'s. `/` truncates (integer division, like `int`); a negative `xx` exponent or a division by zero crashes with a clear message rather than producing a fraction/UB, the same "loud failure" precedent as `int`. Works as a function parameter/return type and as `array:bigint`, same as every other type. A chained `+`/`-`/`x`/`/` expression (`a + b + c + d`) accumulates into a single handle instead of allocating a fresh one per step, `xx` gets the same treatment on its right operand (mirroring `bignum`'s identical chain-fusion optimizations), and a bare literal combined with a `bigint` inside a `while` loop is constructed once before the loop rather than every iteration. See `examples/bigint.cyborgpl`.

## Requirements

- Rust (stable, 2024 edition)
- [LLVM](https://llvm.org/) 22.x — on macOS: `brew install llvm`
- [GMP](https://gmplib.org/) — on macOS: `brew install gmp`
- [mimalloc](https://github.com/microsoft/mimalloc) — on macOS: `brew install mimalloc` (backs `bignum`'s allocator; see below)

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

A `.cyborgpl` file's settings live in a separate `.cyborgsettings` file, linked in explicitly -- not a CLI flag, and not auto-discovered by filename convention, so a settings file can never affect a program without that program's own source asking for it by path:

```
linkto*("/absolute/path/to/name.cyborgsettings")*;
```

as its own top-level line (alongside `func`/`START`, not inside either). The path must be absolute -- a relative path is a hard error. The settings file must consent back, with `allow.link*("/absolute/path/to/name.cyborgpl")*` as its required first line (also absolute), naming the exact `.cyborgpl` file linking to it -- a mutual handshake, so neither file's say-so alone is enough. Any mismatch (missing file, wrong path on either side, a relative path, malformed `allow.link` line) is a hard error, refusing to compile. `linkto`'s argument is a literal string only for now, no variables. After the handshake, the rest of the settings file is `setting.value` per line; currently the only recognized setting is `optimize` (`true`/`false`, default `true`), disabling LLVM's optimizer entirely when set to `false` -- for an honest look at a program's real, unoptimized behavior/timing. See `examples/settings_demo.cyborgpl` / `examples/settings_demo.cyborgsettings`. An unrecognized setting name, or a value that isn't `true`/`false`, is a hard error rather than being silently ignored.

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
- `>> 'name'*args*` — a shorter alternative spelling for `ref:func 'name'*args*`, added alongside it (not replacing it) after actually using the array feature and noticing how verbose a call reads inline; `>>` is only ever this call-shorthand outside a string literal, so it can't collide with `>`/`>=`'s comparison meaning or with `>>` appearing literally inside quoted text
- `var:array:TYPE 'name'` — the element type stated via colon-chaining, matching `ref:var:TYPE`/`input:TYPE`/`clock:TYPE`'s existing shape; `{ }` (not `[ ]`, already taken by `[precision:N]` and `[to/from*(dest)*]`) for array literals, with each element still individually wrapped in `( )` like every other value; growable rather than fixed-size; 1-based indexing (`ref:var:array:TYPE 'name'*(1)*` is the first element) — a deliberate departure from C/Python/Rust/JS's 0-based convention; `append*(array), (value)*;` as a dedicated statement rather than a value-position operation; `(length*array*)` as a value-position construct (still needing its own outer `( )` wrap, unlike a function call) — flagged as "a lot of clutter" after actually reading the feature, which prompted dropping the *inner* wrap around the array argument itself (`(length*(array)*)` → `(length*array*)`), since an indexed reference could never validly appear there anyway; supporting every existing type as an element type (including the heap-owned `str`/`bignum`/`file`) rather than starting narrower
- `var:int` — a genuine whole-number type (real integer arithmetic, not a float with a rule bolted on); doesn't coerce from `num`/`numw`/`bignum` at all (only from `int` itself, like `bool`); `+`/`-`/`x`/`xx`/`xxx`/`!` all crash with a clear message on overflow rather than silently wrapping (two's-complement), and `/` truncates rather than producing a fraction; `[precision:8/16/32/64]` added in a same-day follow-up once the user asked for it directly, with any width freely coercing to any other (widening always exact, narrowing overflow-checked at runtime) — mirroring `num`'s "any precision" convention rather than requiring an exact width match; catching an all-literal overflowing expression at compile time (rather than only at runtime) added after the user asked, in general, whether CyborgPL could catch mistakes at compile time the way Rust does — narrowed down together to the one bounded, fully-decidable case (every operand a literal) rather than the undecidable general one (a value only known once the program runs)
- `,` (not `_`) as a numeric literal's digit-grouping separator — asked about after noticing Rust's `1_000_000`, then chosen directly by the user (`,`, matching how the number would actually be written out by hand) once it was clear the language needed to pick one

**Defaulted by Claude** (conventional choices from the first scaffolding commit, never revisited):
- `if`/`else`/`while` keywords and brace-delimited blocks
- Remaining comparison/logical operators' exact spelling (`<`, `>`, `<=`, `>=`, `==`, `&&`, `||`)

## Acknowledgments

Built in collaboration with [Claude Code](https://claude.com/claude-code) — most of the implementation work (lexer, parser, codegen, the from-scratch fp128 float support, the GMP-backed bignum type) was written together with it, with language design decisions made by the project author.

## License

Licensed under the [Apache License 2.0](LICENSE).
