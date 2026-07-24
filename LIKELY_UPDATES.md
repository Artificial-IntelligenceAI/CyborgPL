# Likely Updates

A running note of things that have come up as likely next steps or known gaps while building CyborgPL — not a committed roadmap, not a timeline, just an honest snapshot of what's been deferred so far and why. Language design items here are open questions for the project author to decide, not things an AI assistant should just go implement.

## Language design (not yet decided)

- Enforcing `ref:var:TYPE`'s stated type against the actual declaration — right now it's informational only ("clarity, for now"), since no real type checker exists yet.
- Higher hyperoperator levels beyond `xxx` (tetration), if ever wanted.
- Whether real lexical block scoping (added for automatic bignum/str cleanup) should extend further.
- `numw`'s magnitude-word vocabulary is currently a fixed list (thousand/million/billion/trillion/quadrillion/quintillion) and only accepts one word after the number — no compound number words (`'one hundred thousand'`) and no expanding the list without a design decision.
- Reading user input (stdin) into a variable — e.g. a name typed at runtime becoming a `var:str`'s value. No I/O of any kind exists yet (`print` is output-only). `str` getting real runtime memory management (this session, `stch`'s round) removes what used to be a blocking prerequisite, so this is now realistic to build whenever wanted — syntax (a new keyword? `str`-only at first, or `num` too?) is still undecided.

## Known implementation gaps

- `str` now has runtime construction via `stch` and real memory management (every `str` in a variable is its own `strdup`'d copy, freed at scope exit/reassignment/return, mirroring `bignum`). One accepted inefficiency: a value stored somewhere always gets its own fresh copy even when the source was already an unshared temporary (a `stch` result, a str-returning call) — a redundant extra copy in that case, same simplification already accepted for `bignum`.
- Intermediate bignum binary-op results always compute at the default precision (256 bits) regardless of operand precision, unlike `num`'s "widen to the larger operand" behavior.
- No LLVM optimization passes run on generated code (`mem2reg`, inlining, etc.) — deliberately deprioritized until the language's basics are finished.

## Bigger, not-yet-started ideas

- A real type checker — would also unlock actually enforcing `ref:var:TYPE`.
- True arbitrary precision beyond what GMP's `mpf_t` offers, or a fixed-width 256-bit float type — discussed as a much larger undertaking, unstarted.

Nothing here is a promise. It's context for whoever — human or AI — picks this project up next.
