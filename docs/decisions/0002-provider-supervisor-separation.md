# ADR 0002: Separate providers from supervisors

Status: accepted

A Buzz backend provider converts an authorized agent deployment into external
compute. A supervisor driver applies and observes a service specification.

Buzz Server bundles a self-hosted provider. That provider delegates to a Compose
driver for the MVP. Compose is therefore not encoded into the provider identity
or the public control-plane model, allowing future supervisor implementations.

