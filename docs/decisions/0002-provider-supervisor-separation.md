# ADR 0002: Separate providers from supervisors

Status: accepted; MVP deployment-path clause partially superseded by
[ADR 0012](0012-built-in-local-backend-first.md)

A Buzz backend provider converts an authorized agent deployment into external
compute. A supervisor driver applies and observes a service specification.

The original MVP selected a bundled self-hosted provider delegating to a Compose
driver. ADR 0012 replaces that first deployment path with a Server-native local
backend. The architectural distinction remains: a future Docker Compose provider
or other backend provider is a deployment adapter, while the mechanism that
keeps a launched service alive is supervision and is not encoded into provider
identity.
