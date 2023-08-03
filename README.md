# Rnostr

A high-performance and scalable [nostr](https://github.com/nostr-protocol/nostr) relay written in Rust.

## Features

- [Most NIPs support](#nips)
- Easy to use, no third-party service dependencies
- High performance, Events is stored in [LMDB](https://github.com/LMDB/lmdb), Inspired by [strfry](https://github.com/hoytech/strfry)
- Most configurations can be hot reloaded
- Scalability, can be used as a library to [create custom relays](./relay/README.md)

### [NIPs](https://github.com/nostr-protocol/nips)

- [x] NIP-01: Basic protocol flow description
- [x] NIP-02: Contact list and petnames
- [x] NIP-04: Encrypted Direct Message
- [x] NIP-09: Event deletion
- [x] NIP-11: Relay information document
- [x] NIP-12: Generic tag queries
- [ ] NIP-13: Proof of Work
- [x] NIP-15: End of Stored Events Notice
- [x] NIP-16: Event Treatment
- [x] NIP-20: Command Results
- [x] NIP-22: Event `created_at` Limits
- [x] NIP-26: Delegated Event Signing
- [x] NIP-28: Public Chat
- [x] NIP-33: Parameterized Replaceable Events
- [x] NIP-40: Expiration Timestamp
- [x] NIP-42: Authentication of clients to relays
- [x] NIP-45: Counting results. [experimental](#count)
- [x] NIP-50: Keywords filter. [experimental](#search)

### Extensions

The library [nostr-relay](./relay/) implements a simple extension mechanism to intercept user messages for custom processing. rnostr is built on top of [nostr-relay](./relay/) and implements several simple extensions.
All extensions support configuration in the [config file](./rnostr.example.toml).

[Custom relay and extensions](./relay/).

#### Metrics

Provide metrics url for [prometheus](https://prometheus.io/) scrape

#### Auth

[NIP-42](https://nips.be/42) Authentication, ip, auth pubkey and event pubkey whitelist blacklist

#### Rate limiter

Limit event write frequency.

#### Count

[NIP-45](https://nips.be/45) count results.
When the query results are too large (millions) will trigger a slow query. `setting.data.db_query_timeout`.

#### Search

[NIP-50](https://nips.be/50) Keywords filter. [nostr-db](./db/) implement a simple exact match pattern, case-insensitive, time-sorted full-text search. No performance optimization for multi-word queries, so it's experimental.

It reduces write concurrency and makes space usage significantly larger. So it is suitable for use in private or paid relay.

Now we only index the content of `kind: 1` note event.

## Usage

### Prepare source and config

```shell

git clone https://github.com/rnostr/rnostr.git
cd rnostr
mkdir config
cp ./rnostr.example.toml ./config/rnostr.toml

```

Edit the `./config/rnostr.toml`, remember to modify network.host to `0.0.0.0` for public access.

### Build and run

```shell

# Build
cargo build --release

# Show help
./target/release/rnostr relay --help

# Run with config hot reload
./target/release/rnostr relay -c ./config/rnostr.toml --watch

```

### Docker

```shell

# Create data dir
mkdir ./data

docker run -it --rm -p 8080:8080 \
  --user=$(id -u) \
  -v $(pwd)/data:/rnostr/data \
  -v $(pwd)/config:/rnostr/config \
  --name rnostr rnostr/rnostr:latest

```

Build by self

```shell

docker build . -t rnostr/rnostr

# Build in China need to configure the mirror.
docker build . -t rnostr/rnostr --build-arg BASE=mirror_cn

```

See docker compose [example](./docker-compose.yml)

### Commands

rnostr provides other commands such as import and export.

```shell

./target/release/rnostr --help

# Usage: rnostr <COMMAND>

# Commands:
#   import  Import data from jsonl file
#   export  Export data to jsonl file
#   bench   Benchmark filter
#   relay   Start nostr relay server
#   help    Print this message or the help of the given subcommand(s)

# Options:
#   -h, --help     Print help
#   -V, --version  Print version

```
