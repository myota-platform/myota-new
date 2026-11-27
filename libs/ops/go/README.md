# libs/ops — Go

Used by `gateway` and `activity`.

```sh
go test -race ./...
```

Provides `Partition` in chunk F1; the outbox, idempotency, job claim and
dead-letter operations arrive with F4. Mirrors `../rust`, and both must pass
`../conformance/`.
