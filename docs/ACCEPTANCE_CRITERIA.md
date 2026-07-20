# Acceptance Criteria

`[x]` indicates implemented behavior with source-level coverage and, where
applicable, a documented live MongoDB or Compose test. The only remaining
original-goal validation item is DBeaver GUI verification.

## A. Schema discovery

- [x] A command can connect to MongoDB and sample configured collections.
- [x] A configured collection set is sampled, profiled independently, and exposed through one proxy process.
- [x] The output identifies the collection and field paths, including nested paths.
- [x] The output records observed BSON types and maps unambiguous fields to conservative SQL types.
- [x] Arrays, null values, and missing fields are represented explicitly.
- [x] Mixed-type fields and scalar/object/array conflicts are flagged as ambiguous.
- [x] Literal dotted keys are distinguished from nested paths where the sample permits that distinction.
- [x] A versioned schema profile is persisted and consumed by the proxy.

## B. PostgreSQL protocol compatibility

- [x] `psql` can connect using a PostgreSQL connection string in local trust/no-op mode.
- [x] Credential authentication failures are returned as PostgreSQL errors; wire-level coverage verifies valid credentials and SQLSTATE `28P01` rejection.
- [x] `SELECT 1`-style session probes and common client startup queries do not break the session.
- [x] The active MongoDB collection appears as a table in the emulated catalog.
- [x] `information_schema.columns` exposes discovered fields and SQL types.
- [x] `psql` meta-commands such as `\\dt` and `\\d <table>` work for the demo collection.
- [x] Typed bound parameters execute through the PostgreSQL extended-query protocol without lossy conversion.
- [x] A standard PostgreSQL driver receives valid row descriptions, values, and command completion in a recorded integration test.
- [x] The supported wire-protocol flow works without a REST adapter or custom MongoDB client library.
- [ ] DBeaver can connect and inspect the emulated catalog.

## C. Deterministic reads

- [x] Supported `SELECT` statements produce results from MongoDB without an LLM call.
- [x] `WHERE` predicates are translated only from the supported SQL subset.
- [x] Nested field filters use the intended MongoDB path.
- [x] A fixture matrix proves supported filters do not produce false-positive or false-negative document matches.
- [x] Null, missing, array, and mixed-type predicate behavior is covered by regression fixtures.
- [x] Unsupported SQL is rejected with a valid PostgreSQL feature or syntax error.
- [x] No user-provided SQL can inject a raw MongoDB pipeline or operator.

## D. Reliable writes

- [x] `INSERT` persists the expected document in MongoDB.
- [x] `UPDATE` persists changes to a nested document path correctly.
- [x] `DELETE` removes only documents matching the translated filter.
- [x] The proxy returns actual MongoDB affected-row counts for inserts, updates, and deletes.
- [x] MongoDB write failures become real PostgreSQL errors with useful SQLSTATEs/messages.
- [x] Bulk or partial failures fail as PostgreSQL errors without silently inventing affected-row counts.
- [x] `matched`, `modified`, `inserted`, and `deleted` counts are taken from MongoDB results and are not inferred.
- [x] Repeated execution and retry behavior is documented; non-idempotent writes are not silently retried.
- [x] A correctness fixture proves that the same field-path rules are used for schema discovery, filters, and writes.
- [x] A write never executes if its field interpretation is unresolved.

## E. Ambiguity-only LLM behavior

- [x] Clear writes complete without calling the LLM service.
- [x] The ambiguity detector catches mixed types, dotted-key/path collisions, missing-field shape ambiguity, and scalar/object/array conflicts.
- [x] The resolver receives schema evidence and the proposed write, not unrestricted database access.
- [x] The resolver can return only an allowlisted decision, confidence, and rationale.
- [x] The resolver cannot return executable MongoDB commands or pipelines accepted by the proxy.
- [x] Invalid, low-confidence, timed-out, or unavailable resolver responses fail closed with a PostgreSQL error.
- [x] A valid resolver decision is revalidated by Rust before execution.
- [x] The decision and resulting write are auditable in a redacted in-memory record.
- [x] A real LLM call selects a Rust-generated, lossless candidate for a genuine mixed-type write, and Rust executes only that validated candidate.
- [x] Structural decisions remain reject-only unless each has a dedicated deterministic executor primitive and integration coverage.

## F. Installation and demo

- [x] A clean Compose environment can start the complete demo with documented Docker Compose commands; a successful run is recorded.
- [x] The clean Compose demo requires only the documented container tooling and does not require compiling `mongo_fdw`, PyMongoSQL, or another native extension.
- [x] Startup includes proxy health/readiness checks and fails with an actionable message when MongoDB or the resolver is unavailable.
- [x] Seed data includes a normal nested write case and a genuine mixed-type ambiguity case.
- [x] The documented demo shows schema discovery, a `psql` `SELECT`, a persisted nested write, and a mixed-type ambiguity-resolved write.
- [x] The demo verifies results by reading MongoDB after each write.
- [x] The README documents supported SQL, limitations, configuration, and failure behavior.

## G. Quality and safety

- [x] Unit tests cover inference, SQL planning, type mapping, ambiguity detection, and decision validation.
- [x] Integration tests exist for a real MongoDB instance and are run when its explicit test environment is available.
- [x] End-to-end tests exercise the proxy through the PostgreSQL wire protocol.
- [x] Resolver audit records redact credentials and sensitive write values.
- [x] The project has deterministic error behavior for unsupported or unsafe operations.
- [x] The final review records evidence for all three core claims: protocol compatibility, reliable writes, and easy installation.
