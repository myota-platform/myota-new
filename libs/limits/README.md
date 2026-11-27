# libs/limits

Numeric limits, defined once per language so the contract and the validators
cannot disagree about them.

```
libs/limits/go/
libs/limits/rust/
```

## Why this exists

The previous implementation declared a 20 MiB upload ceiling in its OpenAPI
document while the code enforced 1 GiB. Nothing caught it, because the two numbers
lived in different files maintained by different people. Here the contract
generator and the validators import the same constants.

## The two copies must agree

As with `libs/ops`, polyglot means two copies. Both test suites assert the
documented values and name the limit that moved, so editing one without the other
fails a test rather than shipping a divergence.
