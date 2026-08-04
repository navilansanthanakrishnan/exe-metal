## What this changes

<!-- One or two sentences. What was wrong, or what is now possible. -->

## How it was verified

<!-- Required. "Should work" is not a result — the failures in this project are
     silent, so a diff that reads correctly can still do nothing.
     Paste the command and what it returned. -->

```
```

- [ ] `cargo fmt` / `cargo clippy --all-targets` clean
- [ ] `cargo test` passes
- [ ] `./tests/smoke.sh` passes — **required if this touches launching,
      prefixes, renderers, or Steam**, since CI cannot run it
- [ ] Ran it against a real game or app, if relevant

## What was NOT verified

<!-- Be specific. "Renderer selection is correct but no 32-bit game has
     actually rendered a frame through it" is more useful than silence. -->

## Notes

- [ ] If this adds a `diagnose` signature: it has a regression test holding a
      real **healthy** log that must not trip it
- [ ] If this adds a field to `Perf` or `WindowCfg`: `exe-metal repair`
      migrates existing bottles, which otherwise keep the old value forever
