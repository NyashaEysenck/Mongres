# Mongo PostgreSQL Proxy

A Rust proxy that exposes MongoDB through the PostgreSQL wire protocol, with deterministic SQL-to-MongoDB writes and a constrained ambiguity resolver.

## Current status

The MVP has schema discovery, typed SQL lowering, deterministic MongoDB CRUD,
schema-backed catalog projection, and a PostgreSQL wire-protocol server. The
server currently uses trust/no-op authentication and supports PostgreSQL text
results. The write-time ambiguity gate, constrained resolver contract, and
fail-closed proxy integration are implemented. A production model-provider
configuration and external GUI/driver validation remain next.

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

## Run the constrained resolver

The resolver has no MongoDB access and returns only a typed recommendation. It
is optional for clear writes, but required when the schema marks a write as the
one supported ambiguity: a sampled-missing nested path.

```bash
python3 -m venv /tmp/mongo-pg-resolver-venv
/tmp/mongo-pg-resolver-venv/bin/pip install -e services/ambiguity-resolver
cp .env.example .env
# Set GEMINI_API_KEY in .env before continuing.
/tmp/mongo-pg-resolver-venv/bin/uvicorn app.main:app \
  --app-dir services/ambiguity-resolver --host 127.0.0.1 --port 8000
```

Set `AMBIGUITY_RESOLVER_URL`, `AMBIGUITY_RESOLVER_TIMEOUT_MS`, and
`AMBIGUITY_RESOLVER_MIN_CONFIDENCE` when starting the proxy; defaults are in
[.env.example](.env.example). The resolver defaults to Google Gemini: export
`GEMINI_API_KEY` in the repository-root `.env` file (never commit the key). Set
`AMBIGUITY_LLM_PROVIDER=openai` with `OPENAI_API_KEY` to use OpenAI instead.
Both providers are forced to a narrow JSON schema and still cannot execute or
emit MongoDB commands.

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
