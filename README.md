# Mongo PostgreSQL Proxy

A Rust proxy that exposes MongoDB through the PostgreSQL wire protocol, with deterministic SQL-to-MongoDB writes and a constrained ambiguity resolver.

## Current status

The project foundation is in place. No database protocol or MongoDB behavior has been implemented yet.

## Local development

```bash
cargo test --workspace
```

Project goals, scope, implementation phases, acceptance criteria, and engineering standards are in [docs](docs/).

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
