# Implementation Plan

Current implementation status is tracked in [IMPLEMENTATION_CHECKLIST.md](IMPLEMENTATION_CHECKLIST.md).
Deterministic write behavior is specified in [WRITE_SEMANTICS.md](WRITE_SEMANTICS.md).
The remaining validation needed for the original product requirement is
DBeaver GUI verification; implementation evidence is recorded in
[DEMO_EVIDENCE.md](DEMO_EVIDENCE.md).

## Phase 1: Foundation and reproducible environment

Create a Rust workspace and Python resolver service with shared configuration and error conventions.

Deliverables:

- Rust crates for proxy, SQL engine, schema discovery, catalog, Mongo executor, and common types.
- Python ambiguity-resolver service with a strict request/response schema.
- Docker Compose for MongoDB, proxy, resolver, and seeded demo data.
- A containerized demo image and a clean-machine smoke test requiring no Rust/Python toolchain or native MongoDB extension installation.
- Environment-based configuration for MongoDB URI, listen address, credentials, sampling limits, and resolver settings.

## Phase 2: Schema discovery

Implement sampling and inference before SQL translation so every later component consumes the same schema evidence.

Deliverables:

- Discovery CLI and library.
- Versioned schema profile persisted in a metadata collection.
- Inference for nested fields, arrays, nulls, missing fields, BSON type mixtures, and dotted-key collisions.
- Unit tests using representative fixture documents.

Exit condition: discovery output correctly identifies all fields and intentional conflicts in the demo dataset.

## Phase 3: Typed SQL intermediate representation

Parse SQL with `sqlparser-rs` and translate supported statements into a typed, validated intermediate representation.

Deliverables:

- AST validation and identifier resolution against schema profiles.
- Plans for `SELECT`, `INSERT`, `UPDATE`, and `DELETE`.
- Parameter and SQL-to-BSON type conversion rules.
- PostgreSQL SQLSTATE errors for syntax, semantic, unsupported-feature, and type failures.

Exit condition: supported statements and intentional rejection cases have deterministic unit-test coverage.

## Phase 4: Deterministic Mongo executor

Implement MongoDB operations from the internal representation, with writes as the priority.

Deliverables:

- `find` execution with projection and basic filtering.
- `insert_one`/bulk insert, `update_one`/`update_many`, and delete execution.
- Correct nested `$set`/`$unset` behavior.
- Real affected-row counts from MongoDB results.
- Partial-failure conversion into useful PostgreSQL errors.
- A correctness matrix for nested paths, null versus missing fields, filter edge cases, type coercion, and repeated execution.
- Explicit MongoDB write concern and retry policy; no automatic retry of non-idempotent writes unless the operation is proven safe.

Exit condition: integration tests verify that writes persist correctly and report accurate counts/errors.

## Phase 5: PostgreSQL protocol and catalog emulation

Expose the engine through `pgwire` and make standard clients believe they are connected to a PostgreSQL database.

Deliverables:

- Startup/session handling with configurable authentication. Trust/no-op mode is limited to the local demo configuration.
- Result sets, row descriptions, command completion, and error responses.
- BSON-to-PostgreSQL type/OID mapping.
- Minimal `pg_catalog` and `information_schema` views backed by discovery profiles.
- Wire-level smoke tests using `psql`, one PostgreSQL driver, and DBeaver catalog discovery.

Exit condition: `psql` can connect, list collections as tables, inspect columns, run a `SELECT`, and run a write.

## Phase 6: Ambiguity gate and constrained LLM resolver

Add the LLM only at the point where deterministic rules identify a real write ambiguity.

Deliverables:

- Rust ambiguity detector and allowlisted decision model.
- Python resolver endpoint with structured validation and timeout/error handling.
- Decision validation in Rust against the original schema evidence.
- Fail-closed behavior when the resolver is unavailable, uncertain, or returns an invalid decision.
- Audit records containing profile version, ambiguity, decision, and Mongo result.

Exit condition: the resolver can select only Rust-generated candidates, and a
real mixed-type demo write applies one validated, lossless candidate through the
deterministic executor.

## Phase 7: Demo proof and final validation

Deliverables:

- Complete the requirements-alignment phases for standard tools, multi-collection routing, bounded mixed-type resolution, and reproducible installation proof.
- Add proxy health/readiness and redacted ambiguity audit records.
- Security review of parameter handling, command construction, and resolver/audit redaction.
- Regression corpus for nested paths, arrays, null/missing fields, mixed types, and bulk failures.
- README and usage guide with one-command startup, demo script, limitations, and troubleshooting.
- A clean Compose installation test and a short evidence note showing protocol compatibility, write correctness, and install prerequisites.

## Suggested work order

1. Seed data and schema discovery.
2. Direct executor tests against MongoDB.
3. SQL IR and deterministic writes.
4. PostgreSQL wire protocol and catalog.
5. Ambiguity resolver integration.
6. End-to-end demo and hardening.

## Core-claim verification

Before calling the scoped product complete, record evidence for each product claim:

| Claim | Required evidence |
| --- | --- |
| Standard protocol | `psql` and at least one PostgreSQL driver connect and execute supported queries; DBeaver can inspect the emulated catalog. |
| Reliable writes | Fixture-based tests prove filter selection, nested updates, counts, errors, and fail-closed ambiguity behavior against a real MongoDB. |
| Easy install | A clean Compose smoke test starts the demo with documented container commands and no native extension build. |
