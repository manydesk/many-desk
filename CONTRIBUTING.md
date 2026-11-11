# Contributing to ManyDesk

Thanks for helping. This page covers how to build, test and send a change.

## Building

ManyDesk is one Rust application in `app/`. Run it during development with:

```bash
cd app && cargo run --release
```

Packages are built by the scripts in `scripts/` (see the README's *Building from source*). CI builds every platform on each pull request.

## Testing

```bash
cd app && cargo test --release --lib
```

Most features involve two computers. Two copies can run on one machine with separate settings: `HOME=<dir>` on macOS and Linux, `MANYDESK_CONFIG_DIR=<dir>` on Windows. The README lists the environment variables that drive a session without a person at the keyboard.

## Guidelines

- **Follow the existing code.** Match the naming, comment style and structure of the surrounding module.
- **Every user-facing string goes through `app/assets/i18n.json`**, with all four languages (az, en, ru, tr).
- **Keep the protocol compatible.** Peers of different versions meet; new messages and fields must be optional, and an older peer must keep working.
- **Security first.** Anything that widens what a viewer can do must be behind a host permission.
- **One topic per pull request**, with a description of how you tested it.

## How changes land

- `main` takes changes only through pull requests. Nobody pushes to it directly, the maintainer included.
- Open a pull request from a branch of your fork. CI builds and tests every platform; for outside contributors it starts once the maintainer approves the run.
- The maintainer ([CODEOWNERS](.github/CODEOWNERS)) reviews, approves and merges. A push after the approval needs a new one, and review threads must be resolved.
- Releases are cut by the maintainer: a `v<version>` tag on `main` builds and publishes every package. Only the maintainer can create, move or delete release tags.

## Reporting bugs and security issues

Use the issue templates for bugs and ideas. Report security vulnerabilities privately, as described in [SECURITY.md](SECURITY.md).
