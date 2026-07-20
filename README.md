# Mongo PostgreSQL Proxy

A Rust proxy that exposes MongoDB through the PostgreSQL wire protocol, with deterministic SQL-to-MongoDB writes and a constrained ambiguity resolver.

## Current status

The current implementation has schema discovery, typed SQL lowering,
deterministic MongoDB CRUD, multi-collection catalog projection, typed
PostgreSQL protocol parameters, and a PostgreSQL wire-protocol server. The
server supports explicit local trust mode or configured cleartext credentials.
The write-time ambiguity gate, constrained Google/OpenAI resolver configuration,
real Gemini mixed-type write flow, and fail-closed proxy integration are
implemented.

The only remaining original-goal validation item is DBeaver GUI verification.
`psql`, a standard PostgreSQL driver, the clean Compose demo, deterministic
nested writes, and real LLM-approved mixed-type writes have been verified.

## Current implementation boundary

The proxy exposes a configured MongoDB collection allowlist as SQL tables and
supports a deliberately narrow SQL subset: `SELECT`, `INSERT`, `UPDATE`, and
`DELETE` with schema-known fields, supported literals, typed placeholders,
nested paths, comparisons, `IN`, `IS NULL`, `AND`, and `OR`. Joins, grouping,
subqueries, windows, transactions, and general SQL breadth are not supported.

Only clear writes execute directly. The resolver may authorize only the one
currently supported ambiguous write class: a Rust-generated candidate for a
sampled-missing nested path or a bounded mixed scalar field. Shape conflicts and
literal dotted-key writes fail closed until Rust has a dedicated deterministic
primitive for them. Trust authentication is for local demonstration only; do not
expose the proxy publicly.

Set `PROXY_AUTH_MODE=cleartext` together with `PROXY_AUTH_USER` and
`PROXY_AUTH_PASSWORD` to require a PostgreSQL username and password. Cleartext
authentication must run only on a trusted network or over TLS; the Compose demo
defaults to `trust` mode.

## Local development

```bash
cargo test --workspace
```

## Run the local protocol proof

First discover and persist a profile for the collection you want to expose:

```bash
MONGO_URI=mongodb://localhost:27017 \
MONGO_DATABASE=demo \
MONGO_COLLECTIONS=customers,ambiguous_profiles \
cargo run -p mongo-pg-schema-discovery
```

Then start the proxy and connect with any client that supports PostgreSQL's
simple-query protocol:

```bash
MONGO_URI=mongodb://localhost:27017 \
MONGO_DATABASE=demo \
MONGO_COLLECTIONS=customers,ambiguous_profiles \
PROXY_LISTEN_ADDR=127.0.0.1:5433 \
cargo run -p mongo-pg-proxy

psql 'postgresql://localhost:5433/demo?sslmode=disable'
```

Within `psql`, `\dt`, `information_schema.columns`, and the supported
`SELECT`/`INSERT`/`UPDATE`/`DELETE` subset work against their matching
allowlisted MongoDB collection. Quote nested field names in an `INSERT` column
list because the PostgreSQL grammar accepts each column there as one identifier:

```sql
INSERT INTO customers (name, "profile.address.city")
VALUES ('Amina', 'Harare');
```

## Run the constrained resolver

The resolver has no MongoDB access and returns only a typed candidate ID. It is
optional for clear writes. For the mixed-scalar `status` demo, it selects from
Rust-generated candidates; Rust validates the selection and performs the fixed
lossless conversion (if selected). It cannot provide MongoDB syntax or an
execution strategy.

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

Project goals, scope, implementation phases, acceptance criteria, engineering
standards, the live [implementation checklist](docs/IMPLEMENTATION_CHECKLIST.md),
[usage guide](docs/USAGE_GUIDE.md), [demo evidence](docs/DEMO_EVIDENCE.md), and
[write semantics](docs/WRITE_SEMANTICS.md) are in [docs](docs/).

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
container to a different database. MongoDB, the resolver, and the proxy each
have Compose health checks; the proxy is ready only after it has loaded the
persisted schema and is accepting PostgreSQL connections.

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
a clear `INSERT` plus nested `UPDATE`, and the constrained LLM-resolved
mixed-type write. After each write, it reads the target document from MongoDB
with `mongosh`; persistence is proved independently of the proxy response.

It deliberately targets the seeded `demo.customers` collection, regardless of
any local `MONGO_DATABASE`, `MONGO_COLLECTIONS`, or legacy `MONGO_COLLECTION`
settings. The script needs a
valid Google or OpenAI provider key because its final write is intentionally
ambiguous and must fail closed if the resolver cannot return a validated
decision.
