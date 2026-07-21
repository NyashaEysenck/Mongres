# Installation

## Prerequisites

- Docker Desktop or Docker Engine with Compose.
- An existing MongoDB deployment.
- `psql` for the verified client path. Docker Compose also provides a `psql`
  container, so installing it locally is optional.
- A Google or OpenAI API key only when testing an ambiguity-resolved write.

## Configure MongoDB

Copy the template:

```bash
cp .env.example .env
```

Set the MongoDB URI, database, and collection allowlist.

For a MongoDB server running on the same macOS machine as Docker:

```dotenv
MONGO_URI=mongodb://host.docker.internal:27017
MONGO_DATABASE=my_database
MONGO_COLLECTIONS=customers,orders
```

For remote MongoDB or Atlas, use its normal `mongodb://` or `mongodb+srv://` URI.

Set `GEMINI_API_KEY` when you need the bounded ambiguity resolver.

## Start the proxy

```bash
docker compose up --build -d
docker compose run --rm schema-discovery
docker compose restart proxy
```

Schema discovery writes versioned profiles to `__pgproxy_schema` in the configured MongoDB database. Rerun discovery and restart the proxy after intentional schema changes.

## Connect

```bash
psql 'postgresql://localhost:5433/mongo?sslmode=disable'
```

Or use the bundled client container:

```bash
docker compose run --rm psql \
  'postgresql://proxy:5433/mongo?sslmode=disable'
```

The connection database name is a PostgreSQL client requirement; MongoDB target selection comes from `MONGO_DATABASE` and `MONGO_COLLECTIONS`.

## Stop

```bash
docker compose down
```
