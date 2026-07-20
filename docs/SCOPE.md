# Scope

## In scope

### PostgreSQL-facing proxy

- PostgreSQL startup, configurable authentication, sessions, typed bound parameters, and query responses using the PostgreSQL wire protocol. Trust mode may exist only as an explicit local-demo configuration.
- Compatibility with `psql` first, followed by validation with DBeaver or another standard PostgreSQL client.
- PostgreSQL type OID mapping for the supported MongoDB/BSON types.
- Minimal `pg_catalog` and `information_schema` emulation for table and column discovery.

### Schema discovery

- Sampling configured MongoDB collections, with one persisted schema profile per collection and catalog/query routing across the configured set.
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
- Literal and typed bound parameter values through the PostgreSQL extended-query protocol.

The executor must construct MongoDB operations from a typed internal plan. User SQL must never become raw MongoDB expressions or unchecked aggregation stages.

### Ambiguity resolution

The write-time ambiguity gate covers:

- Mixed BSON types for a targeted field.
- Nested path versus literal dotted field interpretation.
- Missing fields on some matching documents.
- Scalar, object, and array shape conflicts.
- Coercions where multiple target types are plausible.

The gate detects every listed condition. The first required resolved case is a
mixed scalar type assignment: the LLM selects one Rust-generated, lossless
candidate such as `keep_string`, `parse_integer_losslessly`, or `reject`.
Rust validates the candidate and performs the conversion before its normal
deterministic `$set`. Mixed shapes and literal dotted-key collisions remain
reject-only until each has its own deterministic execution primitive. The
Python LLM service receives minimized schema evidence, the proposed write, and
candidate IDs; it returns only an allowlisted candidate, confidence, and audit
rationale.

### Demo and operations

- Docker Compose for MongoDB, the Rust proxy, and the optional Python resolver.
- Seed data covering normal nested documents and intentional ambiguity cases.
- One-command startup and a documented demo script.
- Structured redacted logging and an inspectable audit record for ambiguity decisions and write outcomes.
- A clean-machine installation path that requires container tooling, not native MongoDB extension compilation.

### Correctness contract

- Filters must preserve the supported SQL predicate semantics, including nested paths, null/missing behavior, and typed bound parameter values.
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
- Generic LLM-directed type coercion, mixed-shape repair, and literal dotted-key execution. Only Rust-generated candidates backed by dedicated deterministic primitives may become allowlisted decisions.
- Reproducing every PostgreSQL system catalog object.
- Query planning or performance parity with native MongoDB queries.
- Automatic schema migration or document cleanup.

Unsupported SQL must fail clearly with a valid PostgreSQL error rather than being approximated.
