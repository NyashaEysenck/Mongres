# Main Goal

Build a Rust proxy that exposes MongoDB through the real PostgreSQL wire protocol, allowing standard PostgreSQL clients such as `psql`, DBeaver, and existing PostgreSQL drivers to query and modify MongoDB.

The project succeeds when it combines all three properties that existing approaches fail to provide together:

1. PostgreSQL protocol compatibility.
2. Reliable, deterministic MongoDB writes with real affected-row counts and error handling.
3. A simple, reproducible installation and demonstration workflow.

These are product claims, not just implementation details:

- **Standard protocol** means a normal PostgreSQL client can connect over the PostgreSQL wire protocol without a custom REST adapter or database-specific client library.
- **Reliable writes** means the same SQL input produces a validated, deterministic MongoDB operation with correct filter semantics, nested-path behavior, affected-row counts, and explicit failure handling.
- **Easy install** means the demo can be started on a clean machine with documented container tooling and does not require users to compile native MongoDB extensions or install database driver build dependencies.

The proxy must use schema discovery to understand MongoDB document shapes. SQL translation and execution remain deterministic. An LLM is used only when a write encounters an ambiguity that cannot be resolved safely from schema evidence and explicit rules. The LLM may recommend a constrained decision, but it must never execute a write or generate an unchecked MongoDB pipeline.

The primary demonstration is:

1. Discover a MongoDB schema.
2. Connect with `psql` over the PostgreSQL wire protocol.
3. Run a normal `SELECT`.
4. Execute an `INSERT` or `UPDATE` that persists correctly in a nested document.
5. Execute a write involving a genuine sampled-missing nested-path ambiguity, show the constrained decision, and verify the deterministic write result.

The initial resolver does not coerce mixed types or conflicting shapes: those
cases remain fail-closed until a dedicated deterministic execution primitive
exists. The project must not claim full PostgreSQL or full SQL compatibility.
Unsupported syntax, unsafe coercions, and unresolved document-shape ambiguity
are expected to produce clear errors.
