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

## Full-stack Compose startup

Copy the environment template and set `GEMINI_API_KEY` when the demo needs an
ambiguity-resolved write. The key is supplied to the resolver at runtime; it is
ignored by Git and excluded from Docker build contexts.

```bash
cp .env.example .env
# Set GEMINI_API_KEY in .env.
docker compose up --build -d
psql 'postgresql://localhost:5433/demo?sslmode=disable'
```

Compose starts MongoDB, the constrained resolver, a one-shot schema-discovery
service, and then the proxy. Schema discovery must exit successfully before the
proxy starts, so its catalog and SQL field validation always use a persisted
profile. The stack uses the internal `mongodb` and `ambiguity-resolver` host
names; host-side connection settings in `.env` cannot accidentally redirect a
container to a different database.

To refresh a schema profile after changing demo data, rerun only discovery and
restart the proxy:

```bash
docker compose run --rm schema-discovery
docker compose restart proxy
```

## One-command demo

Run the complete protocol and write-correctness demonstration from a checkout
with a provider key in `.env`:

```bash
./scripts/run-demo.sh
```

The script starts the stack, resets only the seeded ambiguity fixture, runs
schema discovery, and then uses a disposable `psql` container to issue a read,
a clear `INSERT` plus nested `UPDATE`, and the constrained LLM-resolved nested
write. After each write, it reads the target document from MongoDB with
`mongosh`; persistence is proved independently of the proxy response.

It deliberately targets the seeded `demo.customers` collection, regardless of
any local `MONGO_DATABASE` or `MONGO_COLLECTION` settings. The script needs a
valid Google or OpenAI provider key because its final write is intentionally
ambiguous and must fail closed if the resolver cannot return a validated
decision.
