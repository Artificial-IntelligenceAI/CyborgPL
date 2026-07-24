# Likely Updates

A running note of things that have come up as likely next steps or known gaps while building CyborgPL — not a committed roadmap, not a timeline, just an honest snapshot of what's been deferred so far and why. Language design items here are open questions for the project author to decide, not things an AI assistant should just go implement.

## Language design (not yet decided)

- Precision (`[precision:N]`) for function parameters and return types — currently only variable declarations support it.
- Enforcing `ref:var:TYPE`'s stated type against the actual declaration — right now it's informational only ("clarity, for now"), since no real type checker exists yet.
- Bignum through function parameters and return types — currently unsupported and untested.
- Higher hyperoperator levels beyond `xxx` (tetration), if ever wanted.
- Whether real lexical block scoping (added for automatic bignum cleanup) should extend further — e.g. shadowing edge cases, function-parameter scoping.
- `numw`'s magnitude-word vocabulary is currently a fixed list (thousand/million/billion/trillion/quadrillion/quintillion) and only accepts one word after the number — no compound number words (`'one hundred thousand'`) and no expanding the list without a design decision.

## Known implementation gaps

- A `bignum` returned from a function *call* still leaks its handle — automatic memory management currently covers named variables and binary-op intermediates, not this path.
- Bare numeric literals assigned to `bignum` lose precision beyond ~17 digits, since `Token::Num` parses through `f64` at the lexer stage (same as `num` always has). Fixing this properly means making `Token::Num` carry its original literal text everywhere — a bigger change than any single round so far.
- Intermediate bignum binary-op results always compute at the default precision (256 bits) regardless of operand precision, unlike `num`'s "widen to the larger operand" behavior.
- No LLVM optimization passes run on generated code (`mem2reg`, inlining, etc.) — deliberately deprioritized until the language's basics are finished.
- No memory management for anything except `bignum` — `str` and its underlying allocations are never freed.

## Bigger, not-yet-started ideas

- A real type checker — would also unlock actually enforcing `ref:var:TYPE`.
- True arbitrary precision beyond what GMP's `mpf_t` offers, or a fixed-width 256-bit float type — discussed as a much larger undertaking, unstarted.

Nothing here is a promise. It's context for whoever — human or AI — picks this project up next.
