# Nostr relay

A high-performance and scalable [nostr](https://github.com/nostr-protocol/nostr) relay library.

## Usage

```shell

cargo add nostr-relay actix-web tracing-subscriber

```

See [demo](./examples/demo.rs)

### Custom extensions

See [extensions demo](../extensions/examples/demo.rs)

Environment overrides reach extension settings as text, exactly as set. String
fields need nothing. Boolean and numeric fields need
`#[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]` to
accept overrides such as `RNOSTR_MYEXT__MAX=5`; without it the override makes the
section fail to parse. The adapter also accepts quoted values in files, such as
`limit = "10"`.
