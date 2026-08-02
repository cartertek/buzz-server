# ADR 0001: Buzz Server is a separate headless control plane

Status: accepted

Buzz Server is an independent deployable application, not a relay subsystem and
not a replacement for `buzz-acp`. It connects to the relay over public Buzz/Nostr
boundaries and owns operational agent desired state only.

This preserves relay authority, permits separate deployment, avoids coupling the
MVP to Desktop/Tauri, and leaves bridges as independent API/event clients.

Buzz Server may run anywhere with network reachability to its configured relays.
It neither shares relay storage nor assumes host co-location. It supports multiple
explicit communities as isolated client workspaces, following Desktop's boundary;
this is not a Buzz Server multi-tenancy abstraction.

