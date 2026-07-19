# Acceptance Criteria

## A. Schema discovery

- [ ] A command can connect to MongoDB and sample a configured set of collections.
- [ ] The output identifies collection names and field paths, including nested paths.
- [ ] The output records observed BSON types and maps unambiguous fields to conservative SQL types.
- [ ] Arrays, null values, and missing fields are represented explicitly.
- [ ] Mixed-type fields and scalar/object/array conflicts are flagged as ambiguous.
- [ ] Literal dotted keys are distinguished from nested paths where the sample permits that distinction.
- [ ] A versioned schema profile is persisted and consumed by the proxy.

## B. PostgreSQL protocol compatibility

- [ ] `psql` can connect using a PostgreSQL connection string.
- [ ] Authentication failures are returned as PostgreSQL errors.
- [ ] `SELECT 1`-style session probes and common client startup queries do not break the session.
- [ ] MongoDB collections appear as tables in the emulated catalog.
- [ ] `information_schema.columns` exposes discovered fields and SQL types.
- [ ] `psql` meta-commands such as `\\dt` and `\\d <table>` work for the demo collections.
- [ ] A standard PostgreSQL driver receives valid row descriptions, values, command completion, and errors.
- [ ] The supported wire-protocol flow works without a REST adapter or custom MongoDB client library.
- [ ] DBeaver or an equivalent standard PostgreSQL GUI can connect and inspect the emulated catalog.

## C. Deterministic reads

- [ ] Supported `SELECT` statements produce results from MongoDB without an LLM call.
- [ ] `WHERE` predicates are translated only from the supported SQL subset.
- [ ] Nested field filters use the intended MongoDB path.
- [ ] A fixture matrix proves supported filters do not produce false-positive or false-negative document matches.
- [ ] Null, missing, array, and mixed-type predicate behavior is documented and tested.
- [ ] Unsupported SQL is rejected with a valid PostgreSQL feature or syntax error.
- [ ] No user-provided SQL can inject a raw MongoDB pipeline or operator.

## D. Reliable writes

- [ ] `INSERT` persists the expected document in MongoDB.
- [ ] `UPDATE` persists changes to a nested document path correctly.
- [ ] `DELETE` removes only documents matching the translated filter.
- [ ] The proxy returns actual MongoDB affected-row counts for inserts, updates, and deletes.
- [ ] MongoDB write failures become real PostgreSQL errors with useful SQLSTATEs/messages.
- [ ] Bulk or partial failures preserve the available affected-count/error details.
- [ ] `matched`, `modified`, `inserted`, and `deleted` counts are taken from MongoDB results and are not inferred.
- [ ] Repeated execution and retry behavior is documented; non-idempotent writes are not silently retried.
- [ ] A correctness fixture proves that the same field-path rules are used for schema discovery, filters, and writes.
- [ ] A write never executes if its field interpretation is unresolved.

## E. Ambiguity-only LLM behavior

- [ ] Clear writes complete without calling the LLM service.
- [ ] The ambiguity detector catches mixed types, dotted-key/path collisions, missing-field shape ambiguity, and scalar/object/array conflicts.
- [ ] The resolver receives schema evidence and the proposed write, not unrestricted database access.
- [ ] The resolver can return only an allowlisted decision, confidence, and rationale.
- [ ] The resolver cannot return executable MongoDB commands or pipelines accepted by the proxy.
- [ ] Invalid, low-confidence, timed-out, or unavailable resolver responses fail closed with a PostgreSQL error.
- [ ] A valid resolver decision is revalidated by Rust before execution.
- [ ] The decision and resulting write are auditable.

## F. Installation and demo

- [ ] A new user can start the complete demo with documented Docker Compose commands.
- [ ] The clean-machine demo requires only the documented container tooling and does not require compiling `mongo_fdw`, PyMongoSQL, or another native extension.
- [ ] Startup includes health/readiness checks and fails with an actionable message when MongoDB or the resolver is unavailable.
- [ ] Seed data includes a normal nested write case and a genuine ambiguity case.
- [ ] The documented demo shows schema discovery, a `psql` `SELECT`, a persisted nested write, and an ambiguity-resolved write.
- [ ] The demo verifies results by reading MongoDB after each write.
- [ ] The README clearly documents supported SQL, limitations, configuration, and failure behavior.

## G. Quality and safety

- [ ] Unit tests cover inference, SQL planning, type mapping, ambiguity detection, and decision validation.
- [ ] Integration tests run against a real MongoDB instance.
- [ ] End-to-end tests exercise the proxy through the PostgreSQL wire protocol.
- [ ] Logs redact credentials and configurable sensitive values.
- [ ] The project has deterministic error behavior for unsupported or unsafe operations.
- [ ] The final review records evidence for all three core claims: protocol compatibility, reliable writes, and easy installation.
