# noetl/catalog

A generalized, event-sourced catalog of **NoETL's internal resources** —
playbooks first — persisted through [EHDB](https://github.com/noetl/ehdb).

> **Status: design phase.** The design specification is at
> [`design/catalog-model.md`](design/catalog-model.md). The crates here are
> scaffolding plus a real CI gate; the model is landed incrementally behind
> flags, with the spec as the reference.

## What this is

NoETL registers a growing set of internal resources — playbooks, and the other
asset types the platform references by name and version. Today they live in a
single `noetl.catalog` table whose shape is specific to what was needed at the
time. This repository holds the **generalized model** that replaces that shape:
one entity/relation model in which adding a new resource type is **data, not a
schema migration**.

## What this is *not*

**It is not a datastore.** EHDB is the database. The catalog is the model and API
layer on top of it:

- a catalog write becomes an **EHDB event** on the chain;
- every catalog read is served from an **EHDB-derived projection**;
- there is no second database, no engine of our own, and no external datastore.

This follows `noetl/ai-meta`'s self-sufficiency rule: *self-sufficient means
NoETL owns its own state — it does not mean no dependencies.* Proven libraries
are welcome; a separate thing to deploy, size, quorum and recover is not.

The boundary is load-bearing, not stylistic. A catalog owning its own storage
would be a second source of truth for facts the event log already holds, and the
two would disagree. That is the failure NoETL has paid for most often; see
`agents/rules/representation-drift.md` in `noetl/ai-meta`.

## Reference, not template

The relational patterns are informed by the
[adiona data model](https://github.com/adiona/adiona-datamodel/tree/master/mysqldb)
— specifically its entity/attribute/value core and its typed, self-referencing
taxonomy. It is a **reference for *what* a flexible catalog needs**, not a schema
to port. Its own extensibility stops short of the goal here: it needs a new
table per entity type (`item_attributes`, `trip_attributes`, `trip_category`), so
adding a type means DDL. The model in this repo replaces that with one
polymorphic identity, which is also what maps cleanly onto an event-sourced
store where every "table" is a projection.

See the spec for the mapping in full.

## Layout

```
crates/catalog-model/   the entity/relation model
design/                 design specification and decision records
.github/workflows/ci.yml  fmt + source hygiene + tests + clippy -D warnings
```

## Development

```bash
cargo fmt --all --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

The toolchain is pinned to **1.99.0** in `rust-toolchain.toml`, matching every
other NoETL Rust repo.

### The CI gate is real

Every CI step can fail the build. None is suffixed `|| true` — five NoETL repos
carried exactly that suppression on clippy, which gates nothing
([ai-meta#374](https://github.com/noetl/ai-meta/issues/374)), and `ai-meta`
itself ran with no CI at all. Each gate in this repo was proven to **fail on a
planted defect** before being relied on; the proofs are recorded in the design
directory.

## License

Apache-2.0. See [LICENSE](LICENSE).
