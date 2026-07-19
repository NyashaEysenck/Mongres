# Mongo PostgreSQL Proxy

A Rust proxy that exposes MongoDB through the PostgreSQL wire protocol, with deterministic SQL-to-MongoDB writes and a constrained ambiguity resolver.

## Current status

The MVP has schema discovery, typed SQL lowering, deterministic MongoDB CRUD,
schema-backed catalog projection, and a PostgreSQL wire-protocol server. The
server currently uses trust/no-op authentication and supports PostgreSQL text
results. Write-time ambiguity resolution and external GUI/driver validation
remain next.

## Local development

```bash
cargo test --workspace
```

## Run the local protocol proof

First discover and persist a profile for the collection you want to expose:

```bash
MONGO_URI=mongodb://localhost:27017 \
MONGO_DATABASE=demo \
MONGO_COLLECTION=customers \
cargo run -p mongo-pg-schema-discovery
```

Then start the proxy and connect with any client that supports PostgreSQL's
simple-query protocol:

```bash
MONGO_URI=mongodb://localhost:27017 \
MONGO_DATABASE=demo \
MONGO_COLLECTION=customers \
PROXY_LISTEN_ADDR=127.0.0.1:5433 \
cargo run -p mongo-pg-proxy

psql 'postgresql://localhost:5433/demo?sslmode=disable'
```

Within `psql`, `\dt`, `information_schema.columns`, and the supported
`SELECT`/`INSERT`/`UPDATE`/`DELETE` subset work against the active MongoDB
collection. Quote nested field names in an `INSERT` column list because the
PostgreSQL grammar accepts each column there as one identifier:

```sql
INSERT INTO customers (name, "profile.address.city")
VALUES ('Amina', 'Harare');
```

Project goals, scope, implementation phases, acceptance criteria, engineering standards, the live [implementation checklist](docs/IMPLEMENTATION_CHECKLIST.md), and [write semantics](docs/WRITE_SEMANTICS.md) are in [docs](docs/).

## MongoDB fixture and live integration test

Start the seeded local MongoDB fixture:

```bash
docker compose up -d mongodb
```

Run the live schema-discovery test against an isolated database:

```bash
MONGO_INTEGRATION_URI=mongodb://localhost:27017 \
MONGO_INTEGRATION_DATABASE=mongo_pg_proxy_test \
cargo test -p mongo-pg-schema-discovery --test mongodb_integration -- --ignored
```

Stop the fixture when finished:

```bash
docker compose down
```
