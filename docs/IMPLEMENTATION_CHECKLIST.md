# Implementation Checklist

This is the living progress record for the project. Completed items are checked only after their relevant quality checks pass.

**Current focus:** Phase 4 — deterministic nested `UPDATE` execution.

## Phase 1 — Foundation and reproducible development environment

- [x] Create the Rust workspace and crate boundaries.
- [x] Define shared error kinds and PostgreSQL SQLSTATE mapping.
- [x] Add local configuration template (`.env.example`).
- [x] Create the constrained Python resolver contract and health endpoint.
- [x] Add project README and engineering standards.
- [x] Add Rust formatting, tests, and strict Clippy quality gates.
- [x] Add a MongoDB Compose fixture and seeded demo data.
- [ ] Containerize the Rust proxy and Python resolver for a full-stack Compose startup.

## Phase 2 — Schema discovery

- [x] Model sample documents independently of the MongoDB driver.
- [x] Infer nested paths, types, shapes, array presence, and missing-document counts.
- [x] Detect mixed types, scalar/object/array conflicts, and dotted-key collisions.
- [x] Add pure unit tests for all schema ambiguity cases.
- [x] Sample a real MongoDB collection through the Rust driver.
- [x] Persist a versioned schema profile in `__pgproxy_schema`.
- [x] Add a MongoDB-backed integration test for sampling and profile persistence.
- [ ] Define schema-profile refresh scheduling and migration behavior.

## Phase 3 — SQL parsing and typed plans

- [x] Add `sqlparser-rs` with the PostgreSQL dialect.
- [x] Lower supported `SELECT`, `INSERT`, `UPDATE`, and `DELETE` statements into typed plans.
- [x] Support nested field paths, literals, placeholders, comparisons, `IN`, `IS NULL`, `AND`, and `OR`.
- [x] Resolve fields against the discovered schema profile.
- [x] Return explicit errors for joins, subqueries, aggregation, unsupported modifiers, unknown fields, and unsafe unfiltered writes.
- [x] Add parser/lowering unit tests for supported and rejected SQL.
- [ ] Add executor-facing type-coercion policy based on inferred BSON types.

## Phase 4 — Deterministic MongoDB executor

- [x] Translate typed `SELECT` plans into deterministic MongoDB `find` calls.
- [x] Add a live integration test for nested `SELECT` filters and projections.
- [x] Translate typed `INSERT` plans into validated BSON documents and insert calls.
- [x] Add a live integration test for a persisted nested `INSERT` and inserted-row count.
- [ ] Translate typed `UPDATE` plans into safe nested `$set` operations.
- [ ] Translate typed `DELETE` plans into validated delete calls.
- [ ] Map SQL values and prepared-statement parameters to BSON without silent lossy coercion.
- [ ] Return actual matched, modified, inserted, and deleted counts.
- [ ] Map MongoDB and partial-write failures to PostgreSQL-compatible errors.
- [ ] Add live integration tests for filters, nested writes, row counts, and failure paths.
- [ ] Define and test retry/write-concern behavior.

## Phase 5 — PostgreSQL protocol and catalog

- [ ] Build catalog projections from schema profiles for collections and columns.
- [ ] Implement minimal `pg_catalog` and `information_schema` responses.
- [ ] Add PostgreSQL type/OID mappings and result-row encoding.
- [ ] Implement startup, authentication, and session handling with `pgwire`.
- [ ] Implement simple-query dispatch from the wire protocol to SQL plans/executor.
- [ ] Return PostgreSQL row descriptions, command completion, and SQLSTATE errors.
- [ ] Verify `psql` table listing, column inspection, reads, and writes.
- [ ] Verify one standard PostgreSQL driver and DBeaver catalog inspection.

## Phase 6 — Write-time ambiguity resolution

- [ ] Implement a deterministic write ambiguity detector using schema profiles.
- [ ] Define the Rust allowlist for resolver decisions.
- [ ] Implement resolver request handling, model adapter, timeout, and confidence policy.
- [ ] Validate every resolver response in Rust before execution.
- [ ] Ensure clear writes never invoke the resolver.
- [ ] Fail closed for invalid, unavailable, or low-confidence decisions.
- [ ] Record schema version, ambiguity, decision, and outcome in audit logs.
- [ ] Add contract tests proving no raw LLM-generated MongoDB command is accepted.

## Phase 7 — End-to-end demo and hardening

- [ ] Add proxy and resolver containers to the Compose stack.
- [ ] Provide a one-command startup and scripted demo.
- [ ] Demonstrate schema discovery, `psql` read, persisted nested write, and ambiguity-resolved write.
- [ ] Verify every demo write by reading MongoDB afterwards.
- [ ] Add structured logging, health/readiness endpoints, and configurable timeouts.
- [ ] Redact credentials and sensitive values from logs/audit records.
- [ ] Add regression fixtures for null/missing fields, arrays, mixed types, dotted keys, and partial failures.
- [ ] Add baseline discovery and query/write latency benchmarks.
- [ ] Complete clean-machine installation test and final documentation review.
