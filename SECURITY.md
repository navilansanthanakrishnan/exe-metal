# Security policy

## Supported versions

exe-metal is pre-1.0. Only the latest release receives fixes.

| Version | Supported |
|---|---|
| 0.1.x | yes |
| < 0.1 | no |

## Reporting a vulnerability

Please **do not open a public issue** for a security problem.

Use GitHub's private reporting:
[Report a vulnerability](https://github.com/NavilanSanthanakrishnan/exe-metal/security/advisories/new)

You should get an acknowledgement within a few days. If a fix is warranted, the
advisory will credit you unless you prefer otherwise.

## What is in scope

exe-metal downloads and executes third-party binaries, so the interesting
surface is the supply chain and the trust boundaries around it:

- **Download integrity.** Runtimes, renderers and the Steam installer are
  fetched over HTTPS from the URLs in `src/catalog.rs`. A way to make exe-metal
  fetch and execute something else is in scope.
- **Path handling.** Bottle names become directory names under the exe-metal
  root. `auto::sanitize` strips path separators for exactly this reason; a way
  past it is in scope.
- **Code signing.** `util::sign_adhoc` re-signs bundles after overlaying DLLs.
  A way to get an unexpected binary signed and loaded is in scope.
- **Privilege.** exe-metal runs entirely as your user and never escalates. It
  prints the one command that needs `sudo` (installing Rosetta) rather than
  running it. Anything that escalates without asking is in scope.

## What is not in scope

- **Wine's own security model.** A Wine prefix is not a sandbox and has never
  claimed to be. Windows software running in a bottle can read your home
  directory through the `Z:` drive mapping. This is inherent to Wine, documented
  upstream, and not a vulnerability in this project. Treat a bottle as running
  the software with your own privileges — because it is.
- **Vulnerabilities in Wine, DXMT, DXVK, D3DMetal or the Steam client.** Report
  those to their projects; see [THIRD-PARTY.md](THIRD-PARTY.md).
- **Running untrusted executables.** exe-metal will run what you point it at.
  That is its purpose.
- **Anti-cheat and DRM.** Out of scope by design; see
  [CONTRIBUTING.md](CONTRIBUTING.md#scope).

## Hardening notes

If you are running software you do not fully trust, use a dedicated bottle. A
bottle is a self-contained prefix, so `exe-metal remove <bottle>` deletes
everything it wrote. It does **not** isolate the rest of your filesystem — Wine
maps your home directory into the prefix — so for genuinely untrusted binaries
use a VM instead.
