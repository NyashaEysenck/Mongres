# Mongo PostgreSQL Proxy

Use supported SQL against an existing MongoDB database through the PostgreSQL wire protocol.

The proxy exposes selected MongoDB collections as SQL tables, translates supported reads and writes deterministically in Rust, and returns PostgreSQL-style results over the PostgreSQL wire protocol.

## Why it exists

MongoDB SQL tooling is generally read-oriented, while write-capable alternatives often require native database extensions or lack a safe path through MongoDB schema drift. This project takes a different approach:

```text
PostgreSQL client → Rust PostgreSQL-wire proxy → MongoDB
```

For clear writes, Rust performs the translation and execution. For a narrow mixed-type write ambiguity, an LLM can only select from Rust-generated candidates; it cannot generate MongoDB commands, pipelines, filters, or executable code.

## What it supports

- Existing local, remote, self-hosted, or Atlas MongoDB connections.
- Schema discovery for an explicit collection allowlist.
- `SELECT`, `INSERT`, `UPDATE`, and `DELETE` for the supported SQL subset.
- Nested MongoDB paths, filters, `IN`, `IS NULL`, `AND`, `OR`, `LIMIT`, and typed literals.
- Deterministic bulk and single-document writes with real MongoDB result counts.
- A bounded LLM decision for one scalar string/integer mixed-type write case.

Not supported: general SQL breadth, joins, grouping, subqueries, transactions, DBeaver/DataGrip catalog introspection, or arbitrary LLM-generated MongoDB operations.

## Quick start

Set your MongoDB connection in `.env`. For Docker on macOS, use
`host.docker.internal` to reach a MongoDB server running on your machine:

```dotenv
MONGO_URI=mongodb://host.docker.internal:27017
MONGO_DATABASE=my_database
MONGO_COLLECTIONS=customers,orders
```

Start the stack, discover the selected collections, and restart the proxy:

```bash
docker compose up --build -d
docker compose run --rm schema-discovery
docker compose restart proxy
```

Connect with `psql`:

```bash
psql 'postgresql://localhost:5433/mongo?sslmode=disable'
```

```sql
SELECT name FROM customers LIMIT 10;

UPDATE customers
SET profile.address.country = 'Zimbabwe'
WHERE _id = 'customer-123';
```

See [installation](docs/INSTALLATION.md), [platform support](docs/PLATFORM_SUPPORT.md), [testing](docs/TESTING.md), and [write semantics](docs/WRITE_SEMANTICS.md).

## AI-assisted development

Codex, powered by GPT-5.6, was used as a development collaborator during this project. It assisted with implementation, Rust/Python refactoring, test and Compose workflows, documentation, and iterative debugging. The project direction, architecture, feature decisions, review criteria, and final validation remained user-directed. The LLM embedded in the product is separately constrained: it only selects an allowlisted ambiguity candidate and never executes or generates MongoDB commands.

## Security note

The default Compose configuration uses PostgreSQL trust authentication for local development. Do not expose it publicly. Configure cleartext authentication only on a trusted or TLS-protected network.
