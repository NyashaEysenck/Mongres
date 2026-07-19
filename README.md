# Mongo PostgreSQL Proxy

A Rust proxy that exposes MongoDB through the PostgreSQL wire protocol, with deterministic SQL-to-MongoDB writes and a constrained ambiguity resolver.

## Current status

The project foundation is in place. No database protocol or MongoDB behavior has been implemented yet.

## Local development

```bash
cargo test --workspace
```

Project goals, scope, implementation phases, acceptance criteria, and engineering standards are in [docs](docs/).

