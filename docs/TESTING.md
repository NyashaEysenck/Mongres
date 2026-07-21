# Testing

## Rust unit and integration coverage

```bash
cargo test --workspace
```

Tests requiring a live MongoDB service are marked ignored. Run them with a dedicated test database:

```bash
MONGO_INTEGRATION_URI=mongodb://localhost:27017 \
MONGO_INTEGRATION_DATABASE=mongo_pg_proxy_test \
cargo test -p mongo-pg-schema-discovery --test mongodb_integration -- --ignored
```

The Mongo executor and PostgreSQL-driver integration tests use the same two
environment variables. Run an individual ignored suite when the local MongoDB
test database is available:

```bash
MONGO_INTEGRATION_URI=mongodb://localhost:27017 \
MONGO_INTEGRATION_DATABASE=mongo_pg_proxy_test \
cargo test -p mongo-pg-mongo-executor --test update_integration -- --ignored
```

## Resolver tests

Create a Python virtual environment and install the resolver package:

```bash
python3 -m venv .venv
.venv/bin/pip install -e services/ambiguity-resolver
cd services/ambiguity-resolver
../../.venv/bin/python -m unittest tests/test_main.py
```

## End-to-end manual verification

Configure a dedicated MongoDB test database as described in
[INSTALLATION.md](INSTALLATION.md). Run schema discovery, connect with `psql`,
then verify supported reads and writes directly in that MongoDB database.
