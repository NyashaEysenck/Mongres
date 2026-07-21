# Platform Support

## Verified

- macOS with Docker Desktop, local MongoDB, and `psql`.
- External MongoDB reachable from Docker, including local MongoDB through `host.docker.internal`.

## Expected but not separately verified

- Linux with Docker Engine/Compose and a network-reachable MongoDB URI.
- Windows with Docker Desktop and a MongoDB URI reachable from containers.
- Remote and Atlas MongoDB deployments using standard connection URIs.

## Client support

Verified client path: `psql` using the PostgreSQL simple-query flow.

The proxy also implements typed extended-query binding for supported scalar
types. It is not a claim of compatibility with every PostgreSQL driver or GUI.

GUI clients that require extensive PostgreSQL catalog introspection, including DBeaver and DataGrip, are not yet supported. The proxy speaks the PostgreSQL wire protocol but does not yet emulate the full session and catalog behavior those tools expect.
