# Likely Updates

A running note of things that have come up as likely next steps or known gaps while building CyborgPL — not a committed roadmap, not a timeline, just an honest snapshot of what's been deferred so far and why. Language design items here are open questions for the project author to decide, not things an AI assistant should just go implement.

## Language design (not yet decided)

- Higher hyperoperator levels beyond `xxx` (tetration), if ever wanted.
- Whether real lexical block scoping (added for automatic bignum/str cleanup) should extend further.
- `numw`'s magnitude-word vocabulary is currently a fixed list (thousand/million/billion/trillion/quadrillion/quintillion) and only accepts one word after the number — no compound number words (`'one hundred thousand'`) and no expanding the list without a design decision.
- Whether `input:` should ever grow a built-in prompt (e.g. `input:str 'name' "Enter your name: ";`), or support `numw`/`bignum` — deliberately left out of the first version; right now you `print` your own prompt text first, and only `str`/`num` can be read.
- Whether `clock:` should ever support absolute/wall-clock time (not just elapsed-since-start), or a `bignum`/higher-precision variant for very short spans — deliberately left out of the first version; right now it's `num` only.
- Whether file writing should ever support append (only overwrite exists now), or whether `[to*(dest)*]`/`[from*(dest)*]` should extend to any other destination/source beyond a file.
- Whether `input:` reading from a file should ever support `numw`/`bignum`, or read just one line instead of the whole file (currently: whole file only, for both `str` and `num`) — deliberately left narrow for the first version, same pattern as stdin `input:`'s own initial scope.
- Arrays: nested arrays (`array:array:num`) are deliberately unsupported for the first version — `ElementType` can't itself be an array. (Array-typed function parameters and return values, and array-literal function arguments, already work — no design decision needed there, since `array:TYPE` is a `Type` like any other and every existing function-boundary/coercion path already handles it uniformly.) Not yet decided/started: a fixed-size array variant, removing/shrinking an array (only `append`, growing, exists), or any built-in way to iterate an array other than a manual `while` loop with `length`/indexing.

## Known implementation gaps

- `str` now has runtime construction via `stch` and real memory management (every `str` in a variable is its own `strdup`'d copy, freed at scope exit/reassignment/return, mirroring `bignum`). One accepted inefficiency: a value stored somewhere always gets its own fresh copy even when the source was already an unshared temporary (a `stch` result, a str-returning call) — a redundant extra copy in that case, same simplification already accepted for `bignum`.
- Intermediate bignum binary-op results always compute at the default precision (256 bits) regardless of operand precision, unlike `num`'s "widen to the larger operand" behavior.
- `mem2reg` now runs before object-file emission (promotes alloca/store/load into plain SSA values wherever safe). Nothing beyond it yet — inlining, dead code elimination, constant folding across function boundaries, etc. are all still deliberately deprioritized.
- The new type checker (`src/typecheck.rs`) only checks *shape* compatibility, not value validity — e.g. `var:bignum 'x' = (ref:var:str 'y');` type-checks fine (`str` is a valid bignum source, for numeric-literal text like `"3.14"`), but if `'y'` holds actual non-numeric text, that's still only caught at runtime (or not caught at all -- `bignum_set_str` on garbage input is undefined). Deliberately out of scope for a checker that mirrors codegen's existing coercion rules rather than adding new restrictions.
- `input:str 'x';` (stdin) strips the trailing newline; `input:str 'x' [from*(dest)*];` (a file) does not -- it hands back the file's exact, complete content. Not a bug (the file-source case was deliberately defined as "whole content, unmodified"), but worth knowing since it means a file written by `overwrite`/`print [to*...*]` (which always appends a newline) will read back with that newline still attached.
- Found while stress-testing arrays: any `build_alloca` placed inside a hand-built runtime loop's body block (as opposed to the function's entry block) is a genuine dynamic stack allocation on every iteration, since LLVM only reclaims it when the function returns — enough iterations (well under a million) segfaults the compiled program. Fixed for all the array codegen helpers via a new `entry_alloca` (see `AGENTS.md`), but `compile_tetration`/`compile_bignum_tetration`/`compile_factorial`/`compile_bignum_factorial` (pre-existing, unrelated to arrays) likely have the same latent bug and haven't been checked/fixed yet.

## Bigger, not-yet-started ideas

- True arbitrary precision beyond what GMP's `mpf_t` offers, or a fixed-width 256-bit float type — discussed as a much larger undertaking, unstarted.

Nothing here is a promise. It's context for whoever — human or AI — picks this project up next.
