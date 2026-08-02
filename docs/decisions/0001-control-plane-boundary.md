# ADR 0001: Buzz Server is a separate headless control plane

Status: proposed

Buzz Server is an independent deployable application, not a relay subsystem and
not a replacement for `buzz-acp`. It connects to the relay over public Buzz/Nostr
boundaries and owns operational agent desired state only.

This preserves relay authority, permits separate deployment, avoids coupling the
MVP to Desktop/Tauri, and leaves bridges as independent API/event clients.

