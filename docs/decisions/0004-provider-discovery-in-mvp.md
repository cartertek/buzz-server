# ADR 0004: Provider discovery is part of the MVP

Status: superseded by [ADR 0012](0012-built-in-local-backend-first.md)

**Subsequent implementation:** provider discovery and provider v1 compatibility
were later implemented while the built-in local backend remained the primary
durable lifecycle path.

## Superseded decision

Buzz Server's initial product contract includes searching for trusted
`buzz-backend-*` executables, invoking provider v1 `info` and `deploy`, and
reporting provider configuration schemas through its private API.

Provider discovery and provider v1 compatibility are Phase 1 work. Phase 3 retains only richer lifecycle capability negotiation, signed Desktop-deployment ingress, and additional provider
sandboxing/hardening.

Provider installation is an administrator trust decision because current Buzz
deployment payloads contain the agent private key and owner authorization.
