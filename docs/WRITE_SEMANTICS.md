# Deterministic Write Semantics

This document defines how the proxy handles supported SQL writes through the
PostgreSQL wire layer.

## Translation contract

- `INSERT` creates BSON documents only from typed `InsertPlan` columns and values.
- Nested paths create nested documents, for example `profile.city` becomes `{ profile: { city: ... } }`.
- `UPDATE` uses only a typed `$set` document and applies it with MongoDB `update_many`, matching SQL's all-matching-rows behavior.
- `DELETE` uses only the typed predicate and MongoDB `delete_many`.
- The executor never accepts raw SQL, raw MongoDB operators from users, or LLM output.

## Values and field paths

- SQL `NULL`, boolean, integer, floating-point, and string literals map directly to BSON equivalents without implicit string-to-number or number-to-string coercion.
- PostgreSQL `$1`, `$2`, and later positional placeholders are supported through typed protocol binding when the schema provides an unambiguous compatible scalar type. Unsupported parameter OIDs and ambiguous field types are rejected.
- Literal dotted MongoDB keys are rejected by `find` and write execution until an explicitly validated aggregation implementation is added.
- Duplicate or parent/child-overlapping write paths, such as `profile` and `profile.city`, are rejected before MongoDB is called.
- Paths with operator-like segments beginning with `$` are rejected for updates.

## Counts and errors

- `INSERT` reports the count of inserted IDs returned by MongoDB.
- `UPDATE` reports MongoDB's `matched_count` and `modified_count` separately.
- `DELETE` reports MongoDB's `deleted_count`.
- MongoDB write failures become structured proxy database errors. Their messages state that a write may have partially applied and callers must inspect state before manually retrying.
- The PostgreSQL wire layer maps these proxy errors to PostgreSQL error frames and SQLSTATEs; it does not convert an error into a successful command completion.

## Retry and acknowledgement policy

- `apply_deterministic_write_policy` configures majority write concern and disables driver retryable writes.
- The executor itself never reissues a write after a failure.
- Callers creating clients for proxy writes must use `deterministic_write_client` or apply the policy before obtaining the target database.
- A network failure remains an explicit ambiguous outcome; it is not proof that the write did not reach MongoDB.

## Current limitations

- MongoDB guarantees atomicity per document, not for an arbitrary multi-document SQL update or delete.
- The initial executor does not expose transaction control or distributed rollback.
- Partial bulk-write count details are preserved in the underlying MongoDB error text only; richer diagnostic fields will be added with the PostgreSQL error encoder.
