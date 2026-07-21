#!/usr/bin/env bash
# Run the protocol and write-correctness proof against the disposable Compose fixture.

set -euo pipefail

readonly postgres_url='postgresql://proxy:5433/demo?sslmode=disable'
readonly demo_customer_id="customer-demo-$(date +%s)"

compose() {
  MONGO_DATABASE=demo MONGO_COLLECTIONS=customers,mixed_statuses docker compose "$@"
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

section() {
  printf '\n%s\n' '================================================================'
  printf '%s\n' "$1"
  printf '%s\n' '================================================================'
}

mongo_eval() {
  compose exec -T mongodb mongosh --quiet demo --eval "$1"
}

show_schema_profiles() {
  printf '\nMongoDB schema profiles consumed by the proxy:\n'
  mongo_eval \
    "printjson(db.getCollection('__pgproxy_schema').aggregate([{\$match: {collection: {\$in: ['customers', 'mixed_statuses']}}}, {\$project: {_id: 0, collection: 1, profileVersion: '\$profile.profile_version', sampledDocuments: '\$profile.sampled_documents', inferredFieldCount: {\$size: '\$profile.fields'}}}, {\$sort: {collection: 1}}]).toArray())"
}

show_customers_snapshot() {
  printf '\nMongoDB customers snapshot (%s):\n' "$1"
  mongo_eval \
    "printjson(db.customers.aggregate([{\$match: {_id: {\$not: /^customer-demo-/}}}, {\$project: {_id: 1, name: 1, active: 1, city: '\$profile.address.city', country: '\$profile.address.country'}}, {\$sort: {_id: 1}}]).toArray())"
}

show_demo_customer() {
  printf '\nMongoDB demo customer %s (%s):\n' "$2" "$1"
  compose exec -T mongodb mongosh --quiet demo --eval \
    "printjson(db.customers.findOne({_id: '$2'}))"
}

show_mixed_statuses() {
  printf '\nMongoDB mixed_statuses BSON state (%s):\n' "$1"
  mongo_eval \
    "printjson(db.mixed_statuses.aggregate([{\$project: {_id: 1, status: 1, bsonType: {\$type: '\$status'}}}, {\$sort: {_id: 1}}]).toArray())"
}

show_mixed_status_schema_evidence() {
  printf '\nPersisted schema evidence for mixed_statuses.status:\n'
  mongo_eval \
    "printjson(db.getCollection('__pgproxy_schema').aggregate([{\$match: {collection: 'mixed_statuses'}}, {\$unwind: '\$profile.fields'}, {\$match: {'profile.fields.path': ['status']}}, {\$project: {_id: 0, path: '\$profile.fields.path', observedBsonTypes: '\$profile.fields.observed_types', observedShapes: '\$profile.fields.observed_shapes', presentDocuments: '\$profile.fields.present_documents', missingDocuments: '\$profile.fields.missing_documents'}}]).toArray())"
}

show_mixed_status_target() {
  printf '\nMongoDB target status-001 (%s):\n' "$1"
  mongo_eval \
    "printjson(db.mixed_statuses.aggregate([{\$match: {_id: 'status-001'}}, {\$project: {_id: 0, status: 1, bsonType: {\$type: '\$status'}}}]).toArray())"
}

show_resolver_decision() {
  local decision
  decision="$(compose logs --no-color --tail=30 ambiguity-resolver 2>&1 | grep 'ambiguity decision target_path=status ' | tail -n 1 || true)"
  printf '\nResolver decision recorded by the constrained service:\n'
  if [[ -n "$decision" ]]; then
    printf '%s\n' "$decision"
  else
    printf '%s\n' 'No resolver decision log was found; the write should be treated as unverified.'
  fi
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

section '0. Start the tool stack'
printf '%s\n' 'Starting MongoDB, the constrained LLM resolver, schema discovery, and the PostgreSQL-wire proxy...'
compose up --build -d

# Reset the two deterministic demo fixtures, then rediscover them. This makes
# repeat runs deterministic while leaving the rest of the collection intact.
printf '%s\n' 'Resetting deterministic demo fixtures so before/after output is readable...'
mongo_eval "db.customers.deleteMany({_id: /^customer-demo-/})" >/dev/null
mongo_eval "db.customers.updateOne({_id: 'customer-002'}, {\$unset: {'profile.address.city': ''}})" >/dev/null
mongo_eval "db.mixed_statuses.updateOne({_id: 'status-001'}, {\$set: {status: 'active'}})" >/dev/null

section '1. Schema discovery'
printf '%s\n' 'Sampling MongoDB collections and storing schema profiles in __pgproxy_schema...'
compose run --rm schema-discovery
show_schema_profiles

section '2. Start proxy from discovered schema'
compose restart proxy >/dev/null
wait_for_proxy

printf '%s\n' 'Schema discovery completed; PostgreSQL wire protocol is ready.'
show_customers_snapshot 'before SELECT'

section '3. Read MongoDB through psql over the PostgreSQL wire protocol'
run_sql 'SELECT name, active, profile.address.city FROM customers'

# The insert is clear under the refreshed profile. The following nested update
# demonstrates the deterministic executor constructing nested BSON on the new document.
section '4. Deterministic INSERT through SQL, verified directly in MongoDB'
show_demo_customer 'before INSERT; document should not exist' "$demo_customer_id"
run_sql "INSERT INTO customers (_id, name, active) VALUES ('$demo_customer_id', 'Demo Customer', true)"
show_demo_customer 'after INSERT; flat fields persisted' "$demo_customer_id"

section '5. Deterministic nested UPDATE through SQL, verified directly in MongoDB'
show_demo_customer 'before nested UPDATE; profile.address.country is absent' "$demo_customer_id"
run_sql "UPDATE customers SET profile.address.country = 'Zimbabwe' WHERE _id = '$demo_customer_id'"
show_demo_customer 'after nested UPDATE; Rust built nested BSON path' "$demo_customer_id"

# `status` is observed as both string and integer, but has no structural
# conflict. An unquoted SQL `1` has a native integer type, yet the field has
# existing string and integer BSON representations. The resolver selects only
# from Rust-owned integer candidates; Rust performs the fixed `$set` operation.
section '6. Mixed-type ambiguity: LLM selects a Rust-owned candidate, Rust executes'
printf '%s\n' "The field mixed_statuses.status is sampled as both string and integer."
printf '%s\n' 'SQL writes the unquoted integer literal 1. The resolver may only choose keep_integer, format_integer_as_string, or reject.'
show_mixed_status_schema_evidence
show_mixed_statuses 'before ambiguous write'
run_sql "UPDATE mixed_statuses SET status = 1 WHERE _id = 'status-001'"
show_resolver_decision
show_mixed_status_target 'after ambiguous write; BSON type proves the chosen deterministic result'

printf '%s\n' '' 'Demo complete: every write was issued through PostgreSQL and read back from MongoDB.'
