# ADR 0005: Use Buzz Server as the product name

Status: proposed

The product name is **Buzz Server**. Its explanatory description is:

> The server-side automation and agent operations control plane for Buzz.

This follows the familiar Desktop/Server product distinction: Buzz Desktop is
the interactive consumer application, while Buzz Server provides durable,
headless services and APIs. It also leaves room for future automation and bridge
capabilities without changing the name.

The documentation must distinguish **Buzz Server** from **Buzz Relay**:

- Buzz Relay is the Nostr transport and shared-event authority.
- Buzz Server is an optional application/control plane that connects to a relay.

Do not use “Headless Buzz Desktop” as the product label. That phrase describes an
implementation analogy, but does not communicate the product's purpose.

Do not use “Buzz Remote” for the product. “Remote” sounds like a deployment
provider or executor and is narrower than the planned server-side API and
automation scope.
