# ADR 0003: Authorize deployments before provider invocation

Status: accepted

Server-native creation generates an agent identity and obtains a narrowly scoped
owner authorization before invoking any provider. This matches Buzz Desktop's
existing trust boundary and allows Desktop-signed and server-native deployments
to converge on one provider and supervisor pipeline.

The signer is a separate hardened service and never exposes arbitrary signing.
