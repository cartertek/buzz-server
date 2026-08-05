# Provider compatibility fixture

Vendored from `block/buzz` at commit `7ff5fc31895efe6265a379d01637c8ee301872e5`:

`crates/buzz-backend-kubernetes/tests/fixtures/provider-wire/`

The upstream fixtures are Apache-2.0. The full-launch request is the recorded
output of Desktop's real deployment-payload builder; the other requests and
responses cover Kubernetes-provider refusal paths that do not need a live
cluster. `tests/provider_compat.rs` contains a completeness guard so adding an
upstream fixture requires updating this vendored corpus.

Buzz at the pinned revision does not publish a provider-protocol library crate.
The wire fixtures therefore remain the compatibility arbiter without importing
the Desktop Tauri application crate or maintaining a private protocol fork.
