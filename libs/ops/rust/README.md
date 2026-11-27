# libs/ops — Rust

Used by `core` and `geodata`.

```sh
cargo test
```

Provides `partition` in chunk F1; the rest arrives with F4. Mirrors `../go`, and
both must pass `../conformance/`.

The partition count is a `NonZeroU16` so that "zero partitions" is not
representable — the invariant is in the type rather than in a comment asking
callers to be careful.
