# Requirements-Alignment Implementation Plan

This plan closes the gap between the current implementation and the original
product requirement: a MongoDB proxy that combines PostgreSQL-tool
compatibility, reliable deterministic writes, and a reproducible installation
path. The authoritative task tracking is the checklist in this document and
[IMPLEMENTATION_CHECKLIST.md](IMPLEMENTATION_CHECKLIST.md).

## Non-negotiable safety rules

- The LLM selects only a Rust-generated candidate ID. It never returns BSON,
  MongoDB operators, field paths, pipelines, filters, or executable text.
- Every candidate has a Rust validator and deterministic executor primitive
  before it is sent to the resolver.
- Candidate validation checks the original schema-profile version, target path,
  operation, lossless conversion rules, confidence, and allowlist membership.
- Unsupported type, shape, and dotted-key cases remain fail-closed. Detection
  alone never authorizes a write.
- Joins and broad aggregation are not required. The original diagram's
  `JOIN/GROUP BY` branch is future work, consistent with the stated SQL-breadth
  exclusion.

## Phase A — Standard PostgreSQL-tool compatibility

1. Implement configurable credential authentication at the protocol boundary;
   keep trust mode only as an explicit local-demo setting.
2. Implement typed extended-query parameter binding. Decode PostgreSQL OIDs
   into the existing typed SQL values; reject unknown OIDs and text that cannot
   be converted losslessly.
3. Add a real-driver integration test, using a standard PostgreSQL driver,
   covering connection, catalog discovery, bound parameters, result values,
   command tags, and PostgreSQL errors.
4. Validate DBeaver against the Compose stack and record the version, driver,
   connection settings, table discovery, column inspection, read, and write.
5. Keep `psql` as a scripted smoke test, but make the driver test the durable
   protocol-compatibility proof.

**Acceptance:** `psql`, DBeaver, and one normal PostgreSQL driver connect
without a custom MongoDB client or REST adapter and execute the supported SQL
surface with correct protocol responses.

## Phase B — Multi-collection discovery and catalog routing

1. Replace the single active-collection runtime setting with an allowlisted
   configured collection set.
2. Discover and persist one schema profile per configured collection.
3. Project every configured collection into `pg_catalog` and
   `information_schema`.
4. Route a typed SQL plan to its resolved collection and load the matching
   schema profile; never infer fields at query time.
5. Add isolation tests proving a collection's schema, writes, and errors never
   leak into another configured collection.

**Acceptance:** two seeded collections appear as separate tables through the
same PostgreSQL connection and retain independent schema and write behavior.

## Phase C — LLM-approved mixed-type write, executed deterministically

The first required type/shape demonstration is a narrowly bounded mixed scalar
type write. It proves the requested coercion/disambiguation role without
allowing the model to invent an execution strategy.

1. Extend the policy contract with Rust-generated candidate decisions for an
   exact target field, initially `keep_string`, `parse_integer_losslessly`, and
   `reject`. Candidate IDs are stable enums, not model-provided types.
2. Generate candidates only when all of these are true: the assignment targets
   one known scalar field, the schema observed the candidate BSON type, the
   SQL literal has a lossless conversion for that candidate, and no dotted-key
   or shape conflict is involved.
3. Extend the resolver request with candidate IDs and extend the response to
   select exactly one candidate ID plus confidence and rationale. Rust rejects
   any response that changes the request, profile, target, or candidate set.
4. Apply an accepted candidate by retyping the existing typed value in Rust,
   then execute the existing `$set` path. No raw command, pipeline, filter, or
   coercion instruction is accepted from the resolver.
5. Add a seeded `status` field containing both string and integer values. The
   demo updates one explicitly selected document with an ambiguous SQL string
   literal; the resolver selects `keep_string` or
   `parse_integer_losslessly`, Rust performs that exact conversion, and
   `mongosh` verifies both the value and BSON type.
6. Test accepted candidates, rejection, stale profiles, malformed IDs,
   unavailable/low-confidence responses, lossy conversion attempts, and the
   guarantee that no write occurs on failure.

**Acceptance:** a real LLM call resolves one genuine mixed-type ambiguity and
the deterministic executor persists only the Rust-validated candidate result.

## Phase D — Structural ambiguities after the mixed-type proof

1. Define a separate deterministic primitive for each supported structural
   case, beginning with nested path versus literal dotted key if needed.
2. For literal dotted keys, construct any required MongoDB update pipeline
   entirely in Rust and authorize it only through a dedicated candidate ID.
3. Do not permit scalar/document or array/document repair until its exact
   matching, update, and partial-failure semantics are specified and tested.

**Acceptance:** every newly supported structural candidate has a dedicated
executor primitive, an allowlist entry, a real MongoDB integration test, and a
fail-closed negative test.

## Phase E — Reliable-write regression evidence

1. Add a real-MongoDB correctness matrix for null versus missing fields,
   arrays, mixed types, nested filters, dotted-key collisions, no-match writes,
   duplicate keys, and partial/bulk failures.
2. Add wire-level versions of representative read, write, command-tag, and
   error scenarios.
3. Preserve structured partial-failure diagnostics where MongoDB makes them
   available, without retrying non-idempotent writes.
4. Define profile refresh scheduling, stale-profile behavior, and migrations;
   test changes in document shape between discovery runs.

**Acceptance:** the evidence shows deterministic filtering, path handling,
counts, failures, and no-write behavior across the documented supported cases.

## Phase F — Reproducible installation and final proof

1. Run the full Compose demo from a clean Docker environment using only the
   documented prerequisites and a provider key supplied at runtime.
2. Record image versions, commands, expected output, and MongoDB verification
   in a checked-in evidence note with no credentials.
3. The script must demonstrate discovery, `psql` read, nested deterministic
   write, mixed-type LLM-approved write, and MongoDB read-back for each write.
4. Add proxy health/readiness, structured redacted logs, and an inspectable
   audit sink so dependency failures are actionable during the demo.

**Acceptance:** a new user can reproduce the complete proof with Docker and a
provider key, without native extensions or local Rust/Python installation.

## Requirements-alignment checklist

### Standard tools

- [x] Add configurable cleartext credential authentication and document local trust mode.
- [ ] Add a wire-level authentication test proving valid credentials connect and invalid credentials return SQLSTATE `28P01`.
- [ ] Implement typed extended-query parameter binding.
- [ ] Add a standard PostgreSQL-driver integration test with bound parameters.
- [ ] Validate DBeaver catalog inspection, read, and write against Compose.
- [ ] Record `psql`, driver, and DBeaver compatibility evidence.

### Collections and catalogs

- [ ] Configure and discover multiple collections in one proxy process.
- [ ] Expose and route multiple collections through catalog emulation.
- [ ] Add cross-collection schema and write-isolation tests.

### Safe LLM disambiguation

- [ ] Add Rust-generated mixed-type candidate IDs and version the contract.
- [ ] Add lossless string-to-integer and string-preserving coercion primitives.
- [ ] Validate candidate IDs, profile version, target, operation, and confidence in Rust.
- [ ] Add a real MongoDB/LLM integration test for an accepted mixed-type write.
- [ ] Change the scripted demo to show the mixed-type decision and BSON-type read-back.
- [ ] Keep mixed shapes and dotted-key execution reject-only until dedicated primitives exist.

### Reliability and installation proof

- [ ] Add the remaining real-MongoDB and wire-level regression matrix.
- [ ] Define profile refresh, staleness, and migration behavior.
- [ ] Add structured partial-failure diagnostics.
- [ ] Add proxy readiness, structured redacted logs, and an inspectable audit sink.
- [ ] Run and record the clean-environment Compose demonstration.
- [ ] Publish a final evidence note for protocol compatibility, reliable writes, and easy installation.
