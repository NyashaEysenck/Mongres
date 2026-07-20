# Demo Evidence

This file records the local verification evidence for the original project
claim: PostgreSQL protocol compatibility, reliable MongoDB writes, and an easy
container-based install path.

Date recorded: 2026-07-20

## Environment

- Repository: `codex-hackathon`
- Container runtime: Docker Compose
- MongoDB image: `mongo:8.0`
- `psql` image: `postgres:16-alpine`
- Resolver provider: Google Gemini
- Resolver model: `gemini-2.5-flash`
- Exposed PostgreSQL endpoint: `localhost:5433`
- Demo database: `demo`
- Demo collections: `customers,mixed_statuses`

Provider API keys were supplied through the local `.env` file and are not
recorded here.

## Commands run

Reset the stack and volumes:

```bash
docker compose down --volumes --remove-orphans
```

Run the full demo:

```bash
./scripts/run-demo.sh
```

Run the live mixed-type resolver test against local MongoDB and the resolver:

```bash
MONGO_INTEGRATION_URI=mongodb://localhost:27017 \
MONGO_INTEGRATION_DATABASE=mongo_pg_proxy_test \
AMBIGUITY_RESOLVER_URL=http://127.0.0.1:8000/v1/resolve \
cargo test -p mongo-pg-proxy live_mongodb_and_resolver_write_flow -- --ignored --nocapture
```

## Observed demo result

Schema discovery completed for both configured demo collections:

```text
discovered 7 fields from 2 sampled documents in demo.customers
discovered 2 fields from 2 sampled documents in demo.mixed_statuses
Schema discovery completed; PostgreSQL wire protocol is ready.
```

The `psql` protocol proof returned rows from MongoDB:

```text
>>> SELECT name, active, profile.address.city FROM customers
     name     | active | profile.address.city
--------------+--------+----------------------
 Amina Ndlovu | t      | Harare
 Tendai Moyo  | f      |
(2 rows)
```

The deterministic insert and nested update persisted to MongoDB:

```text
>>> INSERT INTO customers (_id, name, active) VALUES ('customer-demo-1784556560', 'Demo Customer', true)
INSERT 0 1

>>> UPDATE customers SET profile.address.country = 'Zimbabwe' WHERE _id = 'customer-demo-1784556560'
UPDATE 1

MongoDB verification for customer-demo-1784556560:
{
  _id: 'customer-demo-1784556560',
  name: 'Demo Customer',
  active: true,
  profile: {
    address: {
      country: 'Zimbabwe'
    }
  }
}
```

The real LLM-approved mixed-type write executed through PostgreSQL and was
verified directly from MongoDB:

```text
>>> UPDATE mixed_statuses SET status = '1' WHERE _id = 'status-001'
UPDATE 1

MongoDB mixed-type verification for status-001:
[
  {
    value: '1',
    bsonType: 'string'
  }
]

Demo complete: every write was issued through PostgreSQL and read back from MongoDB.
```

The live Rust integration test also passed:

```text
test ambiguity::live_tests::live_mongodb_and_resolver_write_flow ... ok
```

## Claims covered

- Standard protocol: `psql` connects and executes supported SQL through the
  PostgreSQL wire protocol.
- Standard driver: `tokio-postgres` integration coverage exists for connection,
  typed bound parameters, reads, updates, and command completion.
- Reliable writes: writes are translated and executed deterministically by Rust,
  with affected-row tags returned from MongoDB results.
- LLM boundary: the resolver selects only a Rust-generated candidate ID; it
  never emits MongoDB commands, pipelines, filters, or executable text.
- Easy install path: the full proof runs through Docker Compose and does not
  require compiling `mongo_fdw`, PyMongoSQL, or native MongoDB SQL extensions.

## Remaining validation

DBeaver GUI validation is still pending because DBeaver is not installed in the
current local environment. This does not change the implementation boundary; it
is a standard-client evidence item.
