# Usage Guide

This guide explains how to run the MongoDB PostgreSQL proxy, query MongoDB with
PostgreSQL clients, and exercise the deterministic write plus LLM ambiguity
flow.

## Prerequisites

- Docker and Docker Compose
- A Google Gemini API key for the ambiguity demo
- Optional for local development: Rust toolchain and Python 3.11+

The default provider is Google Gemini with `gemini-2.5-flash`. OpenAI remains
configurable, but the demo is set up for Gemini.

## Configure the demo

Create a local environment file:

```bash
cp .env.example .env
```

Edit `.env` and set:

```env
AMBIGUITY_LLM_PROVIDER=google
GEMINI_API_KEY=your_key_here
GEMINI_MODEL=gemini-2.5-flash
MONGO_DATABASE=demo
MONGO_COLLECTIONS=customers,mixed_statuses
PROXY_AUTH_MODE=trust
```

Do not commit `.env`.

## Run the full proof

From the repository root:

```bash
./scripts/run-demo.sh
```

The script will:

1. Start MongoDB, the resolver, schema discovery, and the proxy.
2. Discover schemas for `customers` and `mixed_statuses`.
3. Connect through `psql` using the PostgreSQL wire protocol.
4. Run a normal `SELECT`.
5. Run an `INSERT`.
6. Run a deterministic nested `UPDATE`.
7. Run a mixed-type ambiguous `UPDATE` resolved by the LLM.
8. Read MongoDB directly after writes to prove persistence.

Expected final line:

```text
Demo complete: every write was issued through PostgreSQL and read back from MongoDB.
```

## Connect manually with psql

Start the stack:

```bash
docker compose up --build -d
```

Connect:

```bash
psql 'postgresql://localhost:5433/demo?sslmode=disable'
```

Useful commands:

```sql
\dt
\d customers
SELECT name, active, profile.address.city FROM customers;
INSERT INTO customers (_id, name, active) VALUES ('manual-001', 'Manual User', true);
UPDATE customers SET profile.address.country = 'Zimbabwe' WHERE _id = 'manual-001';
DELETE FROM customers WHERE _id = 'manual-001';
```

Nested paths in `SELECT` and `UPDATE` can be written as dotted paths:

```sql
SELECT profile.address.city FROM customers;
UPDATE customers SET profile.address.country = 'Zimbabwe' WHERE _id = 'manual-001';
```

For `INSERT` column lists, quote nested field names because PostgreSQL parses
insert columns as identifiers:

```sql
INSERT INTO customers (_id, name, "profile.address.city")
VALUES ('manual-002', 'Nested User', 'Harare');
```

## Supported SQL surface

Supported:

- `SELECT`
- `INSERT`
- `UPDATE`
- `DELETE`
- nested field paths
- typed literals
- PostgreSQL `$1`, `$2`, ... placeholders through the extended-query protocol
- `=`, `<>`, `<`, `<=`, `>`, `>=`
- `IN`
- `IS NULL`
- `AND`
- `OR`

Not supported:

- joins
- grouping
- window functions
- subqueries
- transactions
- arbitrary MongoDB pipelines
- LLM-generated queries or MongoDB commands

Unsupported SQL fails with a PostgreSQL error instead of falling back to an LLM.

## Authentication modes

Local demo mode:

```env
PROXY_AUTH_MODE=trust
```

Configured password mode:

```env
PROXY_AUTH_MODE=cleartext
PROXY_AUTH_USER=demo_user
PROXY_AUTH_PASSWORD=demo_password
```

Cleartext authentication should only be used on a trusted local network or
behind TLS.

## Ambiguity resolver behavior

The resolver is intentionally constrained.

It receives:

- schema evidence
- the target write operation
- Rust-generated candidate IDs
- a short preview of the write value

It returns only:

- selected candidate ID
- confidence
- short rationale

It cannot return:

- MongoDB commands
- MongoDB pipelines
- filters
- operators
- executable SQL
- arbitrary field paths

Rust revalidates the response before execution. Invalid, low-confidence,
timed-out, stale, or unavailable resolver responses fail closed and do not
write to MongoDB.

## Refresh schema profiles

If MongoDB documents change shape, rerun discovery and restart the proxy:

```bash
docker compose run --rm schema-discovery
docker compose restart proxy
```

The proxy uses persisted schema profiles from `__pgproxy_schema`; it does not
infer fields at query time.

## Stop the stack

```bash
docker compose down
```

To remove demo volumes as well:

```bash
docker compose down --volumes --remove-orphans
```

## DBeaver status

DBeaver should be configured as a PostgreSQL connection to:

- Host: `localhost`
- Port: `5433`
- Database: `demo`
- SSL: disabled
- Username/password: only required when `PROXY_AUTH_MODE=cleartext`

DBeaver validation is the remaining standard-GUI evidence item. The proxy does
not require a DBeaver-specific adapter; DBeaver should use its normal
PostgreSQL driver path.
