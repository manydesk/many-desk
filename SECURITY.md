# Security Policy

ManyDesk gives other people control of a computer, so we treat security reports as the highest priority.

## Supported versions

Only the latest release receives security fixes. The app checks for updates on start and offers to install them.

## Reporting a vulnerability

**Do not open a public issue.** Report it privately through
[GitHub Security Advisories](https://github.com/manydesk/many-desk/security/advisories/new).

Please include:

- the affected version and platforms,
- a description of the issue and its impact,
- steps to reproduce, or a proof of concept.

We acknowledge reports within 3 working days and keep you updated until a fix is released. We are glad to credit you in the release notes unless you prefer otherwise.

## Scope

In scope: the ManyDesk application on every platform, its protocol (rendezvous messages, the MDP session transport), authentication (unattended-access password, TOTP, trusted devices), permissions, and the release packages and update manifest.

Out of scope: third-party infrastructure ManyDesk relies on (public Nostr relays, MQTT brokers, STUN and TURN servers), and issues that require an already compromised computer.
