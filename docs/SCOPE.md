# Scope

## In scope

### PostgreSQL-facing proxy

- PostgreSQL startup, authentication, sessions, and query responses using the PostgreSQL wire protocol.
- Compatibility with `psql` first, followed by validation with DBeaver or another standard PostgreSQL client.
- PostgreSQL type OID mapping for the supported MongoDB/BSON types.
- Minimal `pg_catalog` and `information_schema` emulation for table and column discovery.

### Schema discovery

- Sampling MongoDB collections.
- Inferring field paths, nested objects, arrays, observed BSON types, missing-field rates, and shape conflicts.
- Detecting mixed types, scalar/object/array conflicts, and literal dotted-key versus nested-path collisions.
- Persisting a versioned schema profile for use by catalog emulation, SQL translation, and ambiguity checks.

### SQL and deterministic execution

The initial SQL surface is intentionally narrow:

- `SELECT ... FROM ... WHERE ...`
- `INSERT ... VALUES ...`
- `UPDATE ... SET ... WHERE ...`
- `DELETE ... WHERE ...`
- Nested field paths.
- Basic comparison, null, `IN`, `AND`, and `OR` predicates.
- Parameterized values where supported by the wire-protocol implementation.

The executor must construct MongoDB operations from a typed internal plan. User SQL must never become raw MongoDB expressions or unchecked aggregation stages.

### Ambiguity resolution

The write-time ambiguity gate covers:

- Mixed BSON types for a targeted field.
- Nested path versus literal dotted field interpretation.
- Missing fields on some matching documents.
- Scalar, object, and array shape conflicts.
- Coercions where multiple target types are plausible.

The gate detects every listed condition. In the initial MVP, it resolves only a
sampled-missing nested path, because the deterministic executor already owns
that `$set` behavior. Mixed types, conflicting shapes, coercions, and literal
dotted-key collisions remain reject-only. The Python LLM service receives
minimized schema evidence and the proposed write and returns only an
allowlisted decision, confidence, and audit rationale.

### Demo and operations

- Docker Compose for MongoDB, the Rust proxy, and the optional Python resolver.
- Seed data covering normal nested documents and intentional ambiguity cases.
- One-command startup and a documented demo script.
- Structured logging and an audit record for ambiguity decisions and write outcomes.
- A clean-machine installation path that requires container tooling, not native MongoDB extension compilation.

### Correctness contract

- Filters must preserve the supported SQL predicate semantics, including nested paths, null/missing behavior, and parameter values.
- Writes must use the same field-path and type rules as reads and schema discovery.
- `matched`, `modified`, `inserted`, and `deleted` counts must be reported according to MongoDB's result, without fabricated estimates.
- Unsupported or ambiguous operations must fail before a write is sent to MongoDB.
- Retries and write concern behavior must be explicit; the proxy must not silently repeat a non-idempotent write.

## Out of scope for the initial release

- REST or HTTP as a client-facing database interface.
- A read-only LLM fallback.
- Raw LLM-generated MongoDB pipelines or commands.
- Full SQL compatibility.
- `HAVING`, subqueries, window functions, CTEs, transactions spanning arbitrary documents, and broad PostgreSQL extension compatibility.
- Full join and aggregation breadth. Simple joins or aggregates may be added later only with dedicated semantic tests; they are not part of the core write/protocol claim.
- Reproducing every PostgreSQL system catalog object.
- Query planning or performance parity with native MongoDB queries.
- Automatic schema migration or document cleanup.

Unsupported SQL must fail clearly with a valid PostgreSQL error rather than being approximated.
