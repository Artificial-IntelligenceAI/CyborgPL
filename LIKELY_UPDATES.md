# Likely Updates

A running note of things that have come up as likely next steps or known gaps while building CyborgPL — not a committed roadmap, not a timeline, just an honest snapshot of what's been deferred so far and why. Language design items here are open questions for the project author to decide, not things an AI assistant should just go implement.

## Language design (not yet decided)

- Higher hyperoperator levels beyond `xxx` (tetration), if ever wanted.
- Whether real lexical block scoping (added for automatic bignum/str cleanup) should extend further.
- `numw`'s magnitude-word vocabulary is currently a fixed list (thousand/million/billion/trillion/quadrillion/quintillion) and only accepts one word after the number — no compound number words (`'one hundred thousand'`) and no expanding the list without a design decision.
- Whether `input:` should ever grow a built-in prompt (e.g. `input:str 'name' "Enter your name: ";`), or support `numw`/`bignum` — deliberately left out of the first version; right now you `print` your own prompt text first, and only `str`/`num` can be read.
- Whether `clock:` should ever support absolute/wall-clock time (not just elapsed-since-start), or a `bignum`/higher-precision variant for very short spans — deliberately left out of the first version; right now it's `num` only.
- Reading from a file — deliberately deferred; `print`/`overwrite` only write for now. Whatever syntax it gets should probably mirror `input:`'s shape (`input:type 'name' [from*(dest)*];`?), but that's undecided.
- Whether file writing should ever support append (only overwrite exists now), or whether `[to*(dest)*]` should extend to any other destination beyond a file.
- Whether `input:`/`clock:` should be renamed/reframed as `var:input:type`/`var:clock:type` (and read back via `ref:var:input:type`/`ref:var:clock:type`) to visually group them under the `var:` family — raised and set aside for now. The real snag: "came from input/clock" isn't part of a variable's *type* today (an `input:str` variable is just an ordinary `str` afterward, freely interchangeable, reassignable, passable anywhere a `str` is expected) — folding the source into the reference syntax would make it part of the type identity, raising real questions (can it still pass to a plain `str` parameter? does it keep the tag after being reassigned with a hand-written value?) that weren't resolved.

## Known implementation gaps

- `str` now has runtime construction via `stch` and real memory management (every `str` in a variable is its own `strdup`'d copy, freed at scope exit/reassignment/return, mirroring `bignum`). One accepted inefficiency: a value stored somewhere always gets its own fresh copy even when the source was already an unshared temporary (a `stch` result, a str-returning call) — a redundant extra copy in that case, same simplification already accepted for `bignum`.
- Intermediate bignum binary-op results always compute at the default precision (256 bits) regardless of operand precision, unlike `num`'s "widen to the larger operand" behavior.
- `mem2reg` now runs before object-file emission (promotes alloca/store/load into plain SSA values wherever safe). Nothing beyond it yet — inlining, dead code elimination, constant folding across function boundaries, etc. are all still deliberately deprioritized.
- The new type checker (`src/typecheck.rs`) only checks *shape* compatibility, not value validity — e.g. `var:bignum 'x' = (ref:var:str 'y');` type-checks fine (`str` is a valid bignum source, for numeric-literal text like `"3.14"`), but if `'y'` holds actual non-numeric text, that's still only caught at runtime (or not caught at all -- `bignum_set_str` on garbage input is undefined). Deliberately out of scope for a checker that mirrors codegen's existing coercion rules rather than adding new restrictions.

## Bigger, not-yet-started ideas

- True arbitrary precision beyond what GMP's `mpf_t` offers, or a fixed-width 256-bit float type — discussed as a much larger undertaking, unstarted.

Nothing here is a promise. It's context for whoever — human or AI — picks this project up next.
