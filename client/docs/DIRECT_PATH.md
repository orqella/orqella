# Direct-path rejection test

The desktop client inherits the core invariant:

> SSH application streams are never opened until the selected iroh path is
> direct. A relay-only SSH path fails closed.

## Controlled test

`crates/orqella-desktop-transport/src/bin/relay-target.rs` is a test-only target
that:

- binds an iroh endpoint using the public test relay;
- advertises its direct addresses but is run on a Windows VM behind QEMU user
  networking, where those direct addresses are not reachable from the Linux
  source;
- accepts `orqella/ssh/1` and logs `STREAM_OPENED` only if the source opens a
  bidirectional SSH stream.

Linux source test:

```bash
ORQELLA_RELAY_TARGET_ID=<target-id> \
ORQELLA_RELAY_URL=<relay-url> \
cargo test -p orqella-desktop-transport \
  --test relay_only_remote -- --ignored --nocapture
```

Result:

```text
test remote_relay_only_target_gets_no_ssh_stream ... ok
```

The Windows target log contained the endpoint/relay advertisement but **no**
`STREAM_OPENED` line after the 30-second window. The client rejected the
relay-only path before opening an SSH stream.

This test uses the public iroh relay for convenience. Production Orqella uses
managed relays only for signaling; application data remains direct-only.
