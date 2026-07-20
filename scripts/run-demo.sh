#!/usr/bin/env bash
# Run the protocol and write-correctness proof against the disposable Compose fixture.

set -euo pipefail

readonly postgres_url='postgresql://proxy:5433/demo?sslmode=disable'
readonly demo_customer_id="customer-demo-$(date +%s)"

compose() {
  MONGO_DATABASE=demo MONGO_COLLECTION=customers docker compose "$@"
}

fail() {
  printf 'demo failed: %s\n' "$1" >&2
  exit 1
}

require_llm_key() {
  if ! grep -Eq '^(GEMINI_API_KEY|OPENAI_API_KEY)=.+' .env; then
    fail 'set GEMINI_API_KEY or OPENAI_API_KEY in .env before running the ambiguity-resolved write'
  fi
}

run_sql() {
  printf '\n>>> %s\n' "$1"
  compose run --rm -T psql "$postgres_url" -v ON_ERROR_STOP=1 -c "$1"
}

verify_customer() {
  printf '\nMongoDB verification for %s:\n' "$1"
  compose exec -T mongodb mongosh --quiet demo --eval \
    "printjson(db.customers.findOne({_id: '$1'}))"
}

wait_for_proxy() {
  local attempt
  for attempt in $(seq 1 30); do
    if compose run --rm -T psql "$postgres_url" -Atqc 'SELECT 1' >/dev/null 2>&1; then
      return
    fi
    sleep 2
  done
  compose ps >&2
  fail 'proxy did not accept PostgreSQL connections within 60 seconds'
}

require_llm_key

printf '%s\n' 'Starting the MongoDB, resolver, discovery, and proxy stack...'
compose up --build -d

# Reset only the seeded ambiguity fixture, then rediscover it. This makes a
# repeat run deterministic while leaving the rest of the demo collection intact.
compose exec -T mongodb mongosh --quiet demo --eval \
  "db.customers.updateOne({_id: 'customer-002'}, {\$unset: {'profile.address.city': ''}})" \
  >/dev/null
compose run --rm schema-discovery
compose restart proxy >/dev/null
wait_for_proxy

printf '%s\n' 'Schema discovery completed; PostgreSQL wire protocol is ready.'
run_sql 'SELECT name, active, "profile.address.city" FROM customers'

# The insert is clear under the refreshed profile. The following nested update
# demonstrates the deterministic executor constructing nested BSON on the new document.
run_sql "INSERT INTO customers (_id, name, active) VALUES ('$demo_customer_id', 'Demo Customer', true)"
run_sql "UPDATE customers SET \"profile.address.country\" = 'Zimbabwe' WHERE _id = '$demo_customer_id'"
verify_customer "$demo_customer_id"

# `profile.address.city` exists in the sample but is missing from customer-002.
# This is the one MVP ambiguity that Rust allowlists for a constrained LLM decision.
run_sql "UPDATE customers SET \"profile.address.city\" = 'Mutare' WHERE _id = 'customer-002'"
verify_customer 'customer-002'

printf '%s\n' '' 'Demo complete: every write was issued through PostgreSQL and read back from MongoDB.'
