# Buzz Server

Buzz Server is a proposed optional, headless Buzz client. Its first milestone is
creating and operating always-on Buzz agents
without depending on Buzz Desktop or a user's laptop.

Buzz Server is not the Buzz relay, is not a replacement for `buzz-acp`, and is not
yet a full headless implementation of Buzz Desktop. The relay remains the Nostr
transport and shared-state authority. `buzz-acp` remains the bridge between Buzz
events and ACP-compatible agent runtimes.

## Initial scope

- discover and invoke Buzz backend providers;
- bundle a self-hosted provider;
- manage desired and observed agent lifecycle state;
- generate and authorize server-native agent identities;
- deploy `buzz-acp` plus an ACP runtime through a supervisor interface;
- implement Docker Compose as the first supervisor driver;
- verify relay connectivity, authorization, and runtime health;
- manage multiple explicitly configured, isolated communities and relays;
- expose a private administrative API and CLI.

Longer-term possibilities include more Buzz Desktop capabilities, additional
supervisors, third-party providers, richer identity administration, and bridges
such as Discord. They are explicitly outside the first implementation milestone.

## Status

Architecture, planning, and executable Phase 0 compatibility scaffold. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
[docs/COMPATIBILITY_WITH_BUZZ.md](docs/COMPATIBILITY_WITH_BUZZ.md),
[docs/MVP_PLAN.md](docs/MVP_PLAN.md), [docs/PHASE_0_PROOFS.md](docs/PHASE_0_PROOFS.md), and [docs/FOLLOW_UP_IMPLEMENTATION_PLAN.md](docs/FOLLOW_UP_IMPLEMENTATION_PLAN.md).

