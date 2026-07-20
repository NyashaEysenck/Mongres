# Implementation Checklist

This is the living progress record for the project. Completed items are checked only after their relevant quality checks pass.

**Current focus:** final standard-GUI validation. The implementation and clean
demo now satisfy the original core claim through `psql`, a standard PostgreSQL
driver, deterministic MongoDB writes, and a real Gemini-approved mixed-type
write. DBeaver validation is the only remaining original-goal proof item.

## Core completion checklist

The detailed implementation plan is in
[REQUIREMENTS_ALIGNMENT_PLAN.md](REQUIREMENTS_ALIGNMENT_PLAN.md). This list
tracks the original product goal, not optional future hardening.

- [x] PostgreSQL wire protocol works through `psql`.
- [x] A standard PostgreSQL driver works with typed bound parameters.
- [ ] DBeaver connects, inspects the catalog, reads, and writes through the proxy.
- [x] Schema discovery samples and persists profiles for configured collections.
- [x] Catalog emulation exposes configured collections and discovered columns.
- [x] Deterministic `SELECT`, `INSERT`, `UPDATE`, and `DELETE` execution is implemented.
- [x] Nested writes persist correctly and are verified by direct MongoDB read-back.
- [x] A real Gemini call resolves a genuine mixed-type write by selecting only a Rust-generated candidate.
- [x] The deterministic executor applies the selected candidate and verifies the stored BSON type.
- [x] The clean Compose demo runs end to end without native MongoDB SQL extensions.
- [x] Evidence is recorded in [DEMO_EVIDENCE.md](DEMO_EVIDENCE.md).

## Phase 1 — Foundation and reproducible development environment

- [x] Create the Rust workspace and crate boundaries.
- [x] Define shared error kinds and PostgreSQL SQLSTATE mapping.
- [x] Add local configuration template (`.env.example`).
- [x] Create the constrained Python resolver contract and health endpoint.
- [x] Add project README and engineering standards.
- [x] Add Rust formatting, tests, and strict Clippy quality gates.
- [x] Add a MongoDB Compose fixture and seeded demo data.
- [x] Containerize the Rust proxy and Python resolver for a full-stack Compose startup.

## Phase 2 — Schema discovery

- [x] Model sample documents independently of the MongoDB driver.
- [x] Infer nested paths, types, shapes, array presence, and missing-document counts.
- [x] Detect mixed types, scalar/object/array conflicts, and dotted-key collisions.
- [x] Add pure unit tests for all schema ambiguity cases.
- [x] Sample a real MongoDB collection through the Rust driver.
- [x] Persist a versioned schema profile in `__pgproxy_schema`.
- [x] Add a MongoDB-backed integration test for sampling and profile persistence.
- [x] Define manual schema-profile refresh behavior for the demo and local usage.

## Phase 3 — SQL parsing and typed plans

- [x] Add `sqlparser-rs` with the PostgreSQL dialect.
- [x] Lower supported `SELECT`, `INSERT`, `UPDATE`, and `DELETE` statements into typed plans.
- [x] Support nested field paths, literals, placeholders, comparisons, `IN`, `IS NULL`, `AND`, and `OR`.
- [x] Resolve fields against the discovered schema profile.
- [x] Return explicit errors for joins, subqueries, aggregation, unsupported modifiers, unknown fields, and unsafe unfiltered writes.
- [x] Add parser/lowering unit tests for supported and rejected SQL.
- [x] Add executor-facing type-coercion policy based on inferred BSON types.

## Phase 4 — Deterministic MongoDB executor

- [x] Translate typed `SELECT` plans into deterministic MongoDB `find` calls.
- [x] Add a live integration test for nested `SELECT` filters and projections.
- [x] Translate typed `INSERT` plans into validated BSON documents and insert calls.
- [x] Add a live integration test for a persisted nested `INSERT` and inserted-row count.
- [x] Translate typed `UPDATE` plans into safe nested `$set` operations.
- [x] Translate typed `DELETE` plans into validated delete calls.
- [x] Map SQL values to BSON without silent lossy coercion; reject unbound parameters until typed protocol binding exists.
- [x] Return actual matched, modified, inserted, and deleted counts.
- [x] Map MongoDB and partial-write failures to proxy errors backed by PostgreSQL SQLSTATEs.
- [x] Add live integration tests for filters, nested writes, row counts, no-match behavior, and duplicate-key failures.
- [x] Define and test no-retry/majority-write-concern behavior.

## Phase 5 — PostgreSQL protocol and catalog

- [x] Build catalog projections from schema profiles for collections and columns.
- [x] Implement minimal `pg_catalog` and `information_schema` responses.
- [x] Add PostgreSQL type/OID mappings and text result-row encoding.
- [x] Implement explicit trust and configured cleartext startup authentication with `pgwire`.
- [x] Implement simple-query dispatch from the wire protocol to SQL plans/executor.
- [x] Return PostgreSQL row descriptions, command completion, and SQLSTATE errors.
- [x] Verify `psql` table listing, column inspection, reads, nested writes, and affected-row tags against local MongoDB.
- [x] Verify one standard PostgreSQL driver with bound `SELECT` and `UPDATE` parameters through the proxy.
- [ ] Verify DBeaver catalog inspection.

## Phase 6 — Write-time ambiguity resolution

The implementation detects every ambiguity category from the original diagram.
It resolves only the cases with dedicated deterministic primitives: sampled
missing nested paths and bounded mixed scalar assignments. Conflicting shapes
and literal dotted-key writes remain fail-closed.

- [x] Detect mixed types, conflicting shapes, dotted-key/path collisions, and sampled missing fields from schema profiles.
- [x] Define the Rust allowlist: `UseNestedPath`, `KeepString`, `ParseIntegerLosslessly`, or `Reject`.
- [x] Make the Python and Rust resolution contracts versioned and identical.
- [x] Implement resolver request handling, an injectable non-executing advisor/model-adapter boundary, timeout, and confidence policy.
- [x] Apply a validated `UseNestedPath` decision through the existing deterministic nested-write executor path.
- [x] Validate response version, target field, allowlisted decision, and confidence in Rust before execution.
- [x] Ensure clear writes never invoke the resolver.
- [x] Fail closed for rejected, invalid, unavailable, timed-out, stale-profile, or low-confidence decisions.
- [x] Record schema version, minimized ambiguity evidence, decision, confidence, and outcome in redacted audit records.
- [x] Add contract tests proving no raw LLM-generated MongoDB command, pipeline, operator, path, or coercion is accepted.
- [x] Configure Google Gemini by default and OpenAI as an alternative inside the constrained adapter boundary; provider credentials remain environment-only.
- [x] Add a live MongoDB-plus-resolver end-to-end test for clear bypass, accepted mixed-type persistence, and failed-resolution no-write behavior.

## Phase 7 — End-to-end demo and hardening

- [x] Add proxy and resolver containers to the Compose stack.
- [x] Provide a one-command startup and scripted demo.
- [x] Demonstrate schema discovery, `psql` read, persisted nested write, and the required mixed-type ambiguity-resolved write.
- [x] Verify every demo write by reading MongoDB afterwards.
- [x] Add health/readiness endpoints and configurable resolver timeouts for the demo stack.
- [x] Redact credentials and sensitive values from resolver audit records.
- [x] Add regression fixtures for null/missing fields, arrays, mixed types, dotted keys, and partial failures (real-Mongo execution remains environment-gated).
- [x] Complete clean Compose installation test and final documentation review.
