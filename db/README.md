# Nostr db

Nostr event database with [LMDB](https://github.com/LMDB/lmdb) as backend. Inspired by [strfry](https://github.com/hoytech/strfry)

### Search language features

The `search` feature enables full-text search. With default features enabled,
Japanese and Korean tokenization use dictionaries downloaded by Lindera build
scripts. To omit those dictionaries, use `default-features = false` with
`features = ["search"]` on the `nostr-db` dependency. Other language features
remain enabled. Add `search-japanese` or `search-korean` to restore either
language. These language features do not enable search on their own.
