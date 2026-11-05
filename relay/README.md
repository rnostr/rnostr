# Nostr relay

A high-performance and scalable [nostr](https://github.com/nostr-protocol/nostr) relay library.

## Usage

```shell

cargo add nostr-relay actix-web tracing-subscriber

```

See [demo](./examples/demo.rs)

### Custom extensions

See [extensions demo](../extensions/examples/demo.rs)

Claim application-specific tables with `app.extensions.write().claim_section("my_app")`.
Web setup finishes registration automatically; headless embedders call
`app.finish_registration()` after registering extensions and claiming their tables.

Environment overrides reach extension settings as text, exactly as set. String
fields need nothing. Boolean and numeric fields need
`#[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]` to
accept overrides such as `RNOSTR_MYEXT__MAX=5`; without it the override makes the
section fail to parse. The adapter also accepts quoted values in files, such as
`limit = "10"`.

Reject requests with `msg.reject(reason)` so the dispatcher sends an error and
closes any associated reconciliation. `Stop(reply)` and `Ignore` mark requests as
handled, including asynchronous work. `Extensions::call_message` sends handled
replies and returns unhandled requests as `Option<ClientMessage>`.

The next release adds `IncomingMessage::NegMsg`, `NegClose`, and
`ExtensionMessageResult::Reject`; update exhaustive matches when upgrading.
