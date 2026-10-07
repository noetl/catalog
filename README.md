# noetl/catalog

A generalized, event-sourced catalog of **NoETL's internal resources** —
playbooks first — persisted through [EHDB](https://github.com/noetl/ehdb).

> **Status: design phase.** The design specification is at
> [`design/catalog-model.md`](design/catalog-model.md). The crates here are
> scaffolding plus a real CI gate; the model is landed incrementally behind
> flags, with the spec as the reference.

## What this is

A **generic catalog for noetl's own INTERNAL object types**, stored in EHDB and reachable
**only through its API**.

⚠⚠ **This is the internal catalog.** "Catalog" also names a *different* thing in NoETL — the
**business catalog**, holding domain data, part of playbooks, backed by external databases
and APIs. That one is not EHDB-bound and is not this repo. See "Two catalogs" below.

* **Generic** — nothing in the store knows what a "playbook" is. An object is
  `(resource_type, path, version)`; attributes and relations reference that identity.
  Adding an object type writes **rows, never schema**, and needs **no code change**.
* **noetl's own objects** — the scope is the six types noetl itself declares, discovered
  from `noetl.resource` rather than invented: `playbook`, `credential`, `mcp`, `agent`,
  `memory`, and `subscription`. See `design/catalog-model.md` §2.10.
* **API-only** — `/api/catalog/*` is the entire interface.

## Two catalogs — and `noetl/catalog` is the internal one

⚠⚠ **These are different concerns with different storage, and conflating them is the one
architectural mistake this page exists to prevent.**

| | **Internal catalog** — *this repo* | **Business catalog** — *not this repo* |
| :-- | :-- | :-- |
| **What it holds** | noetl's own objects: `playbook`, `credential`, `mcp`, `agent`, `memory`, `subscription` | domain data: hotels, flights, trips, items, categories, orders, their translations |
| **Storage** | **EHDB only.** No external datastore, ever | **anything** — external Postgres, a third-party API, an object store |
| **Interface** | `/api/catalog/*` over the EHDB-backed store | a **playbook step**, under that playbook's policy block |
| **Who owns it** | the platform | the domain / the application |
| **Schema** | one polymorphic identity; a new object type is **rows, not schema** | whatever the domain needs — a real relational schema is normal here |

### The line, stated once

> **The internal catalog holds the playbook that reads the business data. It never holds the
> business data.**

Concretely, from `noetl/travel`:

* `adiona/playbooks/catalog_list.yaml` is a **playbook** — an internal object. The internal
  catalog registers it, and records `uses_tool.postgres` and
  `uses_credential.adiona_actor` about it.
* What that playbook *reads* — `adiona.items`, `adiona.item_content`,
  `adiona.item_category` in **external Postgres**, via `kind: postgres` with
  `auth: adiona_actor` — is the **business catalog**. None of it enters EHDB.

**Business-catalog data must not be pushed into this repo's store.** Not as a resource type,
not as attributes, not as relations. The internal catalog's generality is over **noetl
object types**, never over arbitrary business schemas.

### The existing business-catalog mechanism, as it already works

Nothing needs designing here — it exists, and it is playbooks:

| domain surface | how a playbook reaches it |
| :-- | :-- |
| the adiona relational catalog | **53** `adiona/playbooks/*.yaml`, `kind: postgres`, `auth: adiona_actor`, against the external `adiona.*` schema |
| flights | `mcp/duffel` (3 playbooks) |
| hotels | `mcp/hotelbeds` (2 playbooks) |
| places | `mcp/google-places` (3 playbooks) |
| documents | `mcp/firestore` (3 playbooks) |

This is the shape `execution-model.md` already mandates: *any data touch happens inside a
playbook step under that playbook's policy block*, with the credential referenced by
keychain alias. The business catalog **is** that pattern; it is not a component to build.

### Why adiona was only ever "inspiration" here

Because adiona **is** a business catalog. Its relational/EAV model belongs to the business
side, and it already lives there — in external Postgres, read per step. What the internal
catalog took from it is *structural*: one polymorphic identity instead of a table per entity
type, the EAV collapse, a self-referencing taxonomy, a typed value union. Its **tables** were
never the target, which is why there is no DDL parser and why the acceptance proof is
set-equality over noetl's own objects.

⚠ **Localization is the clearest case.** It was added here citing adiona's 24
`_translate` / `_content` tables — and those are business-catalog tables.
`adiona.item_content` carries `lang_code` in external Postgres, read by a playbook step.
The `lang` dimension in this store therefore has **no demonstrated internal consumer**: it
is inert (`lang` defaults to `None`; the neutral read excludes translations) and is not
built on further.

## What this is *not*

**It is not a datastore.** EHDB is the database. The catalog is the model and API layer
over it.

**It has no SQL surface of any kind.** No DDL parser, no SQL query interface, no
SQL-shaped access layer, and no query language standing in for one. This is enforced
mechanically: `ac1_the_crate_runs_no_sql_and_links_no_database_driver` asserts that no
manifest links a database driver and no non-test source contains a SQL literal, and it
prints the population it scanned.

**It is not an Adiona port.** See below.

## The API

```
GET  /api/catalog/health
POST /api/catalog/types                  declare an object type       (auth)
GET  /api/catalog/types                  known noetl types + declared-here
GET  /api/catalog/types/{name}
POST /api/catalog/objects                register an object           (auth)
GET  /api/catalog/objects?type=X         query by type
GET  /api/catalog/objects/{*path}        latest + every version
POST /api/catalog/attributes             set an attribute             (auth)
GET  /api/catalog/attributes/{*path}
GET  /api/catalog/by-attribute?name=N    query by attribute   (reverse)
POST /api/catalog/relations              assert an edge               (auth)
GET  /api/catalog/relations/{*path}      query by relation
GET  /api/catalog/relations-to/{*path}   query by reverse relation
POST /api/catalog/tick                   EHDB lifecycle               (auth)
GET  /metrics
```

Writes and the lifecycle tick require the internal bearer token, mirroring
`noetl/server`'s `/api/internal/*` guard: **503** when the token is unconfigured (a
privileged surface gets no permissive default) and **403** on a missing, malformed or
mismatched header. Reads are open — the catalog's inventory is the platform's own object
list, and it is the *mutation* that is privileged.

⚠ **Every read returns the full set with its count, never a page.** A paginated default
is how a partial answer passes for a complete one. The folds behind these endpoints
returned `1 of 49`, `0 of 48`, `1 of 53` and `1 of 40` before they were keyed correctly —
answers that looked successful. The `0 of 48` reported *"nobody uses this credential"*
while 48 objects did.

## Four datasets, and that number does not move

`c1` entities · `c2` relations · `c3` attributes · `c4` types. **AC3 asserts the
`Dataset` impl count is exactly four**, and it is the invariant the whole design rests on:
adding an object type must not add storage shape.

Three *reverse* indexes exist and none of them added a dataset. Each lives **inside** an
existing dataset as a second row kind, keyed by a control-character sentinel, which works
because `ehdb-l0` matches the index key by exact string equality:

| dataset | forward key | synthetic key | answers |
| :-- | :-- | :-- | :-- |
| `c1` | `path` | `\u{1}type/<kind>` | every object of type X |
| `c2` | `from_path` | `\u{1}to/<path>` | **what references X** |
| `c3` | `path` | `\u{1}attr/<name>` | which objects carry attribute N |

The sentinel is a control character because a synthetic key must be impossible to collide
with a real `metadata.path`; a forward write whose path intrudes is **refused**.

## Reference, not template

The relational and EAV patterns are informed by the
[adiona data model](https://github.com/adiona/adiona-datamodel/tree/master/mysqldb) —
**inspiration only**. Its schema is **not** mapped in, its tables are not catalog entities,
and the acceptance proof is not an adiona slice.

What was taken: the one-polymorphic-identity fix for adiona's "a new table per entity
type" flaw, the EAV collapse, the self-referencing taxonomy (`supertype`), and the typed
value union its stored procedures reveal.

### Consciously dropped

| dropped | why |
| :-- | :-- |
| a SQL **DDL parser** (`catalog-schema`) | out of scope — the catalog has no SQL surface, and parsing a foreign schema is not what a catalog of noetl's own objects needs |
| the **adiona round-trip** acceptance proof | the generality that matters is over noetl's own object types; acceptance is set-equality over those |
| **localization as a worked feature** | it was driven by adiona's 24 `_translate` tables. The `lang` dimension exists and is **inert for noetl objects** (defaults to `None`; the neutral read excludes translations), and is not built on further. No concrete noetl need was found — flagged, not built |

## Layout

```
crates/catalog-model/    the object/attribute/relation model, and noetl's six types
crates/catalog-store/    the four EHDB datasets, the folds, the three reverse indexes
crates/catalog-extract/  reads a document for the references and facts it declares
crates/catalog-ingest/   walks a source (a dir, or a git ref) and registers what it finds
crates/catalog-api/      /api/catalog/* — the only interface
design/catalog-model.md  the design spec
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
