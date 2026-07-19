# Software Engineering Standards

This document defines the engineering practices and consistency rules for the MongoDB-to-PostgreSQL proxy. It applies to Rust code, the Python ambiguity resolver, infrastructure, tests, documentation, and operational tooling.

## 1. Engineering principles

- Prefer correctness, safety, and explainability over SQL feature breadth.
- Keep the SQL parser, typed plan, ambiguity policy, and MongoDB executor as separate layers.
- Make deterministic behavior the default. Ambiguous or unsupported behavior must fail closed.
- Do not hide data-shape assumptions. Record them in schema profiles, errors, tests, or documentation.
- Keep changes small enough to review and easy to revert.
- Optimize only after measuring a representative workload.

## 2. Architecture boundaries

The system is organized into explicit boundaries:

1. **Protocol layer** — PostgreSQL wire handling, sessions, authentication, result encoding, and SQLSTATE errors.
2. **SQL layer** — parsing, identifier resolution, validation, and conversion to a typed internal representation.
3. **Schema layer** — MongoDB sampling, inference, profile versioning, and catalog metadata.
4. **Policy layer** — ambiguity detection and validation of any resolver decision.
5. **Execution layer** — deterministic construction and execution of MongoDB driver operations.
6. **Resolver service** — constrained recommendations only; never direct database access or command execution.

Dependencies must flow inward through typed interfaces. The executor must not parse SQL, the resolver must not execute MongoDB operations, and the protocol layer must not contain MongoDB query-building logic.

## 3. Rust standards

- Format all Rust code with `cargo fmt` and enforce it in CI.
- Keep the project warning-clean with `cargo clippy --all-targets --all-features -- -D warnings`.
- Use `thiserror` (or equivalent typed errors) for library errors and preserve error context when crossing layers.
- Use `Result` for recoverable failures; do not use `unwrap`, `expect`, or panic in request-handling paths unless an invariant is documented and impossible to violate from input.
- Prefer owned/domain types at layer boundaries over passing parser AST nodes or unvalidated BSON values.
- Use exhaustive matching for statement kinds, ambiguity decisions, and BSON/SQL type mappings.
- Keep async code non-blocking. Move blocking work to an explicit blocking boundary.
- Use `tracing` with structured fields. Do not use ad hoc `println!` logging in production paths.
- Avoid `unsafe`; any required use needs a documented safety argument and focused review.
- Pin or constrain dependency versions and remove unused dependencies promptly.

### Module design and size

- Organize code around cohesive responsibilities and stable interfaces; a module should have one clear reason to change.
- Do not split small, tightly related code merely to satisfy an arbitrary file-count or line-count target. Prefer local helpers when they keep one responsibility understandable.
- Treat a module approaching **500 lines** as a design-review trigger. At that point, assess whether parsing, validation, lowering, execution, wire encoding, data models, or tests belong in focused submodules.
- A production source file of roughly **700 lines or more** requires an explicit modularization decision in the change review. Keep it monolithic only when there is a documented, compelling cohesion reason.
- Extract submodules by responsibility—not by mechanical line ranges. For example, the SQL engine should separate plan types, statement-specific lowering, field/path resolution, and tests when its single file becomes difficult to navigate.
- Keep the public API in the parent module concise; keep internal details private to their submodules and avoid circular dependencies.

## 4. Python service standards

- Format with `ruff format`, lint with `ruff check`, and type-check public boundaries with `mypy` or an equivalent checker.
- Define all resolver requests and responses with Pydantic models.
- Keep the resolver endpoint side-effect free: it may inspect the supplied evidence, but it may not connect to MongoDB or execute returned text.
- Validate the decision against an allowlist before returning it.
- Set explicit request, model, and overall deadlines.
- Return structured errors and avoid leaking prompts, credentials, or sensitive document values in logs.
- Keep provider-specific LLM code behind a small adapter so the policy contract remains testable without a model call.

## 5. SQL and MongoDB consistency rules

- Every supported SQL statement is translated through a typed intermediate representation.
- Never concatenate user SQL, identifiers, or values into raw MongoDB command text.
- User values must be represented as typed parameters or validated literals.
- Field-path interpretation must be centralized; do not implement dotted-path rules separately in reads, writes, and catalog code.
- SQL-to-BSON coercion must be explicit, documented, and tested. No silent lossy conversion.
- Use the MongoDB driver's structured APIs and typed BSON builders.
- Return MongoDB's actual matched, modified, inserted, and deleted counts.
- Unsupported SQL features must return a stable PostgreSQL SQLSTATE and an actionable message.
- Never execute a write when ambiguity remains unresolved.

## 6. API and error conventions

- Use stable domain error codes internally and map them to PostgreSQL SQLSTATEs at the protocol boundary.
- Error messages should state what failed, identify the relevant collection/field when safe, and suggest a corrective action.
- Do not expose stack traces, connection strings, prompts, or secrets to clients.
- Include a correlation/request ID in logs and, where appropriate, in diagnostic fields.
- Keep protocol behavior consistent: every command must produce either the expected completion/result or one well-formed error response.

## 7. Testing standards

Every feature that changes behavior must include tests at the lowest useful layer and at least one integration path where the layers interact.

- **Unit tests:** schema inference, SQL validation, type mapping, path resolution, ambiguity detection, decision validation, and error mapping.
- **Integration tests:** real MongoDB behavior for inserts, nested updates, deletes, filters, affected-row counts, and failures.
- **Protocol tests:** queries sent through a real PostgreSQL client or driver, including catalog introspection.
- **Contract tests:** resolver responses are constrained, invalid responses are rejected, and no generated MongoDB command is accepted.
- **Regression tests:** every production bug gets a focused test before or with the fix.
- Use deterministic fixtures and fixed clocks/IDs where practical. Tests must not depend on an external hosted LLM.
- Test both positive and fail-closed paths, especially for mixed types, missing fields, arrays, nulls, and dotted-key collisions.

## 8. Security and data handling

- Treat all SQL, identifiers, BSON values, schema metadata, and model output as untrusted input.
- Store credentials in environment/configuration providers, never source files or fixtures.
- Redact secrets and configurable sensitive fields from logs and audit records.
- Apply least privilege to MongoDB credentials and resolver service permissions.
- Enforce maximum query sizes, sampling limits, resolver payload sizes, and timeouts.
- Do not send full documents to the LLM when a minimized schema excerpt is sufficient.
- Review new dependencies and network calls for supply-chain and data-exfiltration risk.

## 9. Observability and operations

- Emit structured logs for connection lifecycle, statement classification, execution outcome, ambiguity decisions, and failures.
- Include collection, operation type, duration, row count, schema-profile version, and correlation ID where safe.
- Never log complete SQL values or complete documents by default.
- Provide health/readiness checks for the proxy, MongoDB, and resolver dependency state.
- Make timeouts, retry policy, sampling size, log level, and resolver behavior configurable.
- Preserve enough audit information to explain why a write was made without storing unnecessary sensitive data.

## 10. Repository and change management

- Use clear, imperative commit messages, for example `Add nested update translation`.
- Keep commits focused; do not combine formatting churn with behavior changes.
- Update relevant documentation and acceptance criteria with behavior changes.
- Every pull request/change description should state: purpose, affected layers, safety implications, test evidence, and known limitations.
- Do not commit build output, local credentials, database dumps, editor state, or generated secrets.
- Keep examples runnable from a clean checkout using documented commands.

## 11. Definition of done

A change is complete only when:

- The behavior is implemented behind the correct architectural boundary.
- Formatting, linting, type checks, and relevant tests pass.
- Error and fail-closed behavior are covered.
- Security, logging, and configuration implications have been considered.
- Documentation and acceptance criteria match the implementation.
- The demo remains reproducible from the documented setup.
