# ADR 0004: Provider discovery is part of the MVP

Status: proposed

Buzz Server's initial product contract includes searching for trusted
`buzz-backend-*` executables, invoking provider v1 `info` and `deploy`, and
reporting provider configuration schemas through its private API.

This supersedes the ordering in `MVP_PLAN.md` that places all external-provider
compatibility in Phase 3. Phase 3 retains only richer lifecycle capability
negotiation, signed Desktop-deployment ingress, and additional provider
sandboxing/hardening.

Provider installation is an administrator trust decision because current Buzz
deployment payloads contain the agent private key and owner authorization.
