# Design spec — a generalized catalog of NoETL internal resources, on EHDB

| | |
| :-- | :-- |
| **Status** | `draft` — open questions resolved by stated assumption, see [§12](#12-assumptions-made-without-the-user) |
| **Created** | 2026-10-06 |
| **Repo** | `noetl/catalog` |
| **Tracking** | [noetl/ai-meta#427](https://github.com/noetl/ai-meta/issues/427) |
| **Storage** | EHDB (`ehdb-l0` + `ehdb-core`, git tag `v0.4.5`) — nothing else |

---

## 1. Scope — what "catalog" means here

NoETL registers internal resources by **path** and **version** and then references
them by name: a playbook to execute, a subscription to listen on. Today that
lives in one Postgres table, `noetl.catalog`, whose shape is specific to what was
needed when it was written.

**In scope.** The catalog of NoETL's *own internal* resources — the things the
platform registers and references internally. Playbooks are the primary, worked
case; the model generalizes so the other internal types fit without reshaping the
store.

**Out of scope.** Business data of any kind. Tenant domain data. Anything a
playbook *operates on* rather than *is*. The catalog describes NoETL's own
furniture, not the work.

### 1.1 The resource types that actually exist

Discovered from the codebase, not invented. `noetl.resource` is a lookup table
whose `name` column is the FK target of `noetl.catalog.kind`, so it is the
authoritative permitted set:

| kind | seeded where | registered in practice | verdict |
| :-- | :-- | :-- | :-- |
| `playbook` | `schema_ddl.sql:3-16` | **171** fixture YAMLs carry `kind: Playbook`; CLI default; dashboard counts it | **live, primary** |
| `subscription` | `server/src/db/queries/catalog.rs:18-30` (startup seed) | **9** YAMLs; has its own registration-time schema validation; scanned by `ingress.rs` | **live** |
| `credential` | `schema_ddl.sql` seed | **0** files. CLI *deliberately diverts* these to `POST /api/credentials` (`cli/src/lib.rs:5064-5070`) | seeded, **deliberately not catalogued** |
| `mcp` | `schema_ddl.sql` seed | **0** files, 0 code paths | **aspirational, never registered** |
| `agent` | `schema_ddl.sql` seed | **0** files, 0 code paths | **aspirational, never registered** |
| `memory` | `schema_ddl.sql` seed | **0** files, 0 code paths | **aspirational, never registered** |

A second, separate registry also exists: `noetl.registry`, reached via
`/api/internal/registry/*`, with a closed kind list `["model", "dataset", "eval",
"release"]` (`server/src/services/registry.rs:44`).

⚠ **Its own source comment records that extending `noetl.catalog` plus its
`noetl.resource(kind)` FK was considered and rejected** in favour of a separate
table (`server/src/db/queries/registry.rs:19-21`). That decision is prior art
this spec must answer rather than silently repeat — see [§9](#9-the-noetlregistry-question).

So the honest count is **two kinds in real use** (`playbook`, `subscription`),
four seeded-but-unused, and four more living in a parallel table. A model that
only serves `playbook` would be under-built; one that invents twelve speculative
types would be fiction. The model below is sized to the two live kinds and adds
new ones as **data**.

---

## 2. The non-negotiable: storage is EHDB

**The catalog is a model and an API. It is not a datastore.**

- A catalog write becomes an **event appended to an EHDB dataset**.
- Every catalog read is served from an **EHDB-derived projection** — a fold over
  that log.
- There is no second database, no engine of our own, no external datastore.

This follows `agents/rules/self-sufficiency.md`: *self-sufficient means NoETL owns
its own state; it does not mean no dependencies.* `ehdb-l0` is a crate that
compiles into the binary. That is allowed and preferred. A separate thing to
deploy, size, quorum and recover is what is forbidden.

The reason this is load-bearing and not stylistic: a catalog that owned storage
would be a second source of truth for facts the log already holds, and the two
would disagree. `agents/rules/representation-drift.md` is a list of times that
has already happened here.

### 2.1 Where this starts from — not a blank page

⚠ **An event-sourced catalog already exists in shadow.** This is the single most
important finding of the audit, and it changes this project from "build" to
"finish and generalize":

| component | file | state |
| :-- | :-- | :-- |
| `CatalogRecord` — the event shape | `server/src/handlers/catalog_log.rs:89-109` | written to its own `StoreTier::Catalog`, env-gated `off` (default) \| `shadow` |
| event types | `server/src/handlers/catalog_relation.rs:21-24` | `catalog.registered` / `catalog.archived` / `catalog.restored` |
| `CatalogRelation` — the fold | `server/src/handlers/catalog_relation.rs` | pure, IO-free, and **nothing calls it on a read path** (`:15-18`) |
| read-source ladder | `server/src/handlers/catalog_read.rs:1-31` | `postgres` (default) \| `verify` \| `tier` |

`catalog_log.rs:1-22` states the current position plainly: *"`POST
/api/catalog/register` performs a direct `INSERT` and there is no emit site
anywhere in the catalog service or handler. A relation is by definition a fold of
a log, so the catalog relation had nothing to fold."*

Prod scale: **1,601 records**, paged because the tier-service frame caps at 1 MiB
(`catalog_read.rs:21-22`).

This spec therefore has a named predecessor. Two consequences:

1. **The event vocabulary is not invented here.** `catalog.registered` /
   `.archived` / `.restored` already exist and are already written in shadow. The
   generalized model extends that vocabulary; it does not replace it.
2. **The known hazard is already documented and must be carried forward.** A
   cached fold is *"stale by construction for up to the TTL, which is exactly the
   read-your-writes problem the RFC names (register-then-run)"*
   (`catalog_read.rs`). Register-then-immediately-execute is the common path, so
   this is the central correctness problem, not an edge case. See
   [§7](#7-read-your-writes).

⚠ The RFCs those files cite — `repos/server/docs/rfc/ehdb-catalog-relation.md` and
`…-step3.md` — **do not exist on disk.** The directory is absent. Design
rationale that was written down has been lost; this document is partly a
reconstruction, and that is a reason to write it here rather than in the server
repo.

---

## 3. The reference model, and the one thing to change about it

The relational patterns are informed by the
[adiona data model](https://github.com/adiona/adiona-datamodel/tree/master/mysqldb).
Read for structure, not for schema. Its relevant core:

| adiona table | pattern |
| :-- | :-- |
| `attributes` | attribute **definitions**, scoped by a category — an attribute registry, i.e. a type system for metadata |
| `item_attributes`, `trip_attributes` | **EAV value rows** — `(entity_id, attribute_id, attribute_value)` |
| `item_attribute_content`, `trip_attribute_content` | long/localized content hung off an attribute value |
| `category_types`, `categories` | a **self-referencing taxonomy** (`master_category_type_id`, `master_category_id`) — hierarchy without new tables |
| `trip_category` | an M:N **relation** table |

Its stored procedures also reveal a typed value union — the `OUT` parameters carry
`attribute_value`, `attribute_text`, `attribute_measure`, `attribute_flag`,
`attribute_timestamp`.

### 3.1 The flaw to fix

**adiona needs a new table per entity type.** `item_attributes` and
`trip_attributes` are the same table twice; `trip_category` would need an
`item_category` sibling. Adding an entity type means DDL.

That is precisely the property this project exists to remove. The fix:

> **One polymorphic entity identity.** An entity is
> `(resource_type, path, version)`. Attributes and relations reference that
> identity, not a per-type table. Adding a resource type writes **rows**, never
> schema.

This is also why the mapping onto an event-sourced store is natural rather than
forced: in EHDB every "table" is already a projection of a log, so "no new table
per type" and "no new dataset per type" are the same requirement.

⚠ **Not ported:** MySQL triggers, stored procedures, `AUTO_INCREMENT`,
`lang_code` localization, and the business tables (`trips`, `cars`, `tours`,
`currencies`, `orders`). Localization is deliberately dropped — NoETL internal
resources have no translated names, and an unused `lang_code` on every row is a
column that will drift.

---

## 4. The model

Four entities. The whole model.

```
ResourceType  ──<  Entity  >──  Attribute
                     │
                     └──<  Relation  >──  Entity
```

### 4.1 `ResourceType` — the type registry

What `noetl.resource` is today, kept and generalized. A resource type is **data**.

| field | type | notes |
| :-- | :-- | :-- |
| `name` | `String` | lowercase, the identity. `playbook`, `subscription`, … |
| `executable` | `bool` | can `/api/execute` take it? today in `meta.executable` |
| `catalogued` | `bool` | today's `meta.catalog`. `credential` would be `false` — it is seeded but deliberately stored elsewhere |
| `attribute_schema` | `Option<JsonValue>` | optional declared shape for this type's attributes. Absent = unconstrained |
| `supertype` | `Option<String>` | self-reference, from adiona's `master_category_type_id`. Lets `agent` be a specialization of `playbook` without a new table |

**Adding a resource type is one appended event.** That is the generalization
requirement, discharged here.

### 4.2 `Entity` — a catalogued resource

| field | type | notes |
| :-- | :-- | :-- |
| `resource_type` | `String` | FK → `ResourceType.name` |
| `path` | `String` | the logical identity, e.g. `muno/playbooks/itinerary-planner` |
| `version` | `u32` | **`u32`, not `i16`** — see below |
| `entity_id` | `i64` | snowflake, stable per `(path, version)` |
| `content` | `Option<String>` | the raw source (YAML). Optional in projections: it is 97.4% of a listing response, measured on prod (`server/src/db/models/catalog.rs:139-188`) |
| `content_sha256` | `String` | always present, even when `content` is projected away — so identity is verifiable without the body |
| `archived_at` | `Option<Timestamp>` | soft delete |

⚠ **`version` widens from `i16` to `u32`.** Today's column is `SMALLSERIAL` → Rust
`i16`, a ceiling of **32,767 versions per path** already called out as a concern
(`server/src/handlers/catalog_relation.rs:33-36`). `noetl catalog load` creates a
new version of *every* entry on each run, so the ceiling is reachable by routine
operation, not by abuse. Widening is free in an event-sourced store and cannot be
done cheaply in the Postgres one.

⚠ The i16-ness is a live decode hazard in the current code — using `i32` shipped a
decode failure. Any bridge that reads today's table must keep `i16` at the SQL
boundary and widen after.

### 4.3 `Attribute` — typed metadata, the EAV generalization

adiona's `attributes` + `<entity>_attributes`, collapsed to one shape.

| field | type | notes |
| :-- | :-- | :-- |
| `entity_id` | `i64` | the entity this describes |
| `name` | `String` | `labels.team`, `exposed_in_ui`, … |
| `value` | `AttributeValue` | the typed union below |

```rust
pub enum AttributeValue {
    Text(String),
    Integer(i64),
    Measure(f64),
    Flag(bool),
    Timestamp(i64),     // epoch micros, UTC
    Ref(EntityRef),     // a typed pointer to another catalogued entity
    Json(JsonValue),    // escape hatch; see below
}
```

The first five mirror adiona's typed OUT parameters. `Ref` is added because a
NoETL catalog's most valuable attribute *is* a pointer to another entity.

⚠ **`Json` is an escape hatch and will be abused.** It exists so today's
`meta`/`payload`/`layout` JSONB columns can be carried across without loss. The
rule: anything queried or filtered gets a real variant; `Json` is for opaque
bodies only. A guard will assert `Json` does not appear in any indexed attribute
name.

Where today's columns land:

| today | becomes |
| :-- | :-- |
| `meta` JSONB (from `metadata.labels`) | one `Attribute` per label, properly typed |
| `payload` JSONB (from `workload:`) | one `Json` attribute, opaque by nature |
| `layout` JSONB (from `workflow:`) | one `Json` attribute, opaque by nature |
| `credential_id INTEGER` | **dropped.** No FK, no reader, no writer found anywhere in five repos — it is dead |

### 4.4 `Relation` — the thing that does not exist today

| field | type | notes |
| :-- | :-- | :-- |
| `from_entity` | `EntityRef` | |
| `to_entity` | `EntityRef` | |
| `kind` | `RelationKind` | |
| `discovered_by` | `Provenance` | **how we know** — see below |

```rust
pub enum RelationKind {
    Invokes,      // a playbook step with `kind: playbook` names another by path
    DerivesFrom,  // lineage; generalizes noetl.registry's `lineage` JSONB
    Supersedes,   // version N replaces version N-1 at the same path
    Requires,     // a declared dependency (credential alias, tool kind)
    Annotates,    // a memory/doc entity describing another
}

pub enum Provenance {
    Declared,                  // stated in the resource's own source
    Extracted { at: i64 },     // parsed out of `content` at registration
    Observed { execution_id: i64 },  // inferred from a real execution
}
```

**This is the largest genuinely new capability.** Today there is **no stored
relation between catalog entries at all** — a playbook names a child by path
string inside a step's `tool:` block, and registration never reads it:

```yaml
tool:
- name: save_profile
  kind: playbook
  path: fixtures/playbooks/playbook_composition/user_profile_scorer
```

Registration extracts exactly `path`, `kind`, `payload`, `layout`, `meta`; it
walks the `workflow:` tree *only* to validate `tool.kind` strings and discards the
pairs afterwards (`server/src/services/catalog.rs:477-502`). So a playbook can be
registered referencing a child that does not exist and nothing says so until the
step runs.

The only relation recorded anywhere is **execution-level**:
`noetl.execution.parent_execution_id` (nullable, no FK, no index of its own). It
answers *"which runs were children of this run"* but not *"which playbooks
reference this playbook"* — that needs re-parsing every `content` blob.

`Provenance` exists because those two are different claims.
`Extracted` says the source says so; `Observed` says it actually happened. Merging
them would recreate the existence-vs-reachability conflation that
`agents/rules/representation-drift.md` is largely about.

---

## 5. The EHDB mapping

### 5.1 Dependency

```toml
ehdb-l0   = { git = "https://github.com/noetl/ehdb", tag = "v0.4.5" }
ehdb-core = { git = "https://github.com/noetl/ehdb", tag = "v0.4.5" }
```

`ehdb-l0` is the live engine and is what `noetl-server` itself depends on.

⚠ **Not `ehdb-reference`.** It is the older parallel lineage (`EventLogDriver`,
`ProjectionDriver`, …) and is not what the server links. Its `election.rs` /
`fencing.rs` are declared without `pub use` re-exports and both self-describe as
non-authoritative: *"⚠⚠ Wired, but NOT authoritative … Nothing here decides who
writes"* and *"⚠⚠ Shipped in SHADOW mode — this refuses nothing yet."*

⚠ **Both pins move together.** They share `ehdb-l0`; two tags put two `ehdb-l0`
versions in one dependency graph, which the server's own `Cargo.toml:107-113`
records as a real incident.

### 5.2 Datasets — a new namespace, deliberately

`ehdb-l0` has **D1–D10, all slots taken**, and `dataset.rs:5-6` states *"Adding a
dataset is a deliberate compiled-in change here, never a runtime operation."*
That sentence governs **ehdb's own** D-numbered datasets.

⚠ **`d7_catalog` already exists** — `CatalogDataset` / `CatalogStore` /
`CatalogOp`, sort key `op_seq`, index dimension `path`, described as *"noetl.catalog
— versioned playbook/tool/resource registry."* It mirrors today's table closely.

**Decision: this repo defines its own `Dataset` impls and does not take a D-number
and does not reuse D7.**

Rationale, in order of weight:

1. `Dataset` is a public trait and `L0Engine<D>` is generic over it, so a
   downstream crate can implement it without modifying `ehdb-l0`.
   `ehdb-l0/tests/generic_dataset.rs` is a 30-line proof on a non-D1 schema
   (`AuditDataset`, `const NAME = "test_audit"`) covering append → seal →
   replicate → merge → read-by-index → cold-load.
2. **The D-number space is ehdb's.** Taking `D11` would make this repo's release
   cadence a constraint on ehdb's, and ehdb's `lib.rs:14` doc header enumerates
   the fixed set D1–D10 — a downstream addition would make that sentence wrong
   from outside the repo that owns it.
3. **D7 is the wrong shape.** `CatalogOp` is keyed by `path` and mirrors one flat
   row. The generalized model needs entity + relation + attribute, which is three
   logs with different partition keys.

So: dataset names prefixed `c` for catalog, in a namespace nothing else uses.

| name | record | sort key | partition / index dim | holds |
| :-- | :-- | :-- | :-- | :-- |
| `c1_catalog_entity` | `EntityOp` | `op_seq` | `path` | register / archive / restore |
| `c2_catalog_relation` | `RelationOp` | `op_seq` | `from_path` | relation assert / retract |
| `c3_catalog_attribute` | `AttributeOp` | `op_seq` | `path` | attribute set / unset |
| `c4_catalog_type` | `TypeOp` | `op_seq` | `name` | resource-type declarations |

Three logs rather than one because the partition key differs: entity and attribute
reads are by `path`, relation reads are by endpoint. One log keyed by `path` would
make "what references X" a full scan.

⚠ `c4` is tiny — single digits of records. It is separate anyway because its
read pattern is "load all, cache" while the others are "index lookup", and
because a type declaration must be readable before the entities that reference it.

### 5.3 The fold

⚠ **There is no `Projection` or `Fold` trait in EHDB.** Confirmed: 23 public
traits workspace-wide, none of them a fold abstraction. The pattern is:
*implement `Dataset` for an op-log record, then hand-write a `…Store` whose fold
is `read_index_after(key, 0).last()`.*

Three in-tree instances to copy: `ProjectionStore` (D3), `RuntimeStore` (D8),
`CatalogStore` (D7). The reference fold body is literally:

```rust
self.engine.read_index_after(execution_id, 0)?.into_iter().next_back()
```

i.e. **latest-op-wins per index key**. That is correct for the entity log
(register → archive → restore is last-write-wins on one path+version) but **not**
for attributes or relations, where many distinct names/edges coexist under one
index key. Those folds accumulate into a map keyed by `(name)` / `(to, kind)` and
apply set/unset in `op_seq` order.

⚠ This is the one place a naive copy of `ProjectionStore` would be silently
wrong: it would keep only the most recent attribute and drop the rest. A guard
will plant two attributes on one entity and assert both survive — a positive
control, because the broken version returns one row and *looks* fine.

### 5.4 Record shapes

Every record carries a `u64` sort key and a `String` index dimension, per the
`Dataset` contract. `#[serde(deny_unknown_fields)]` **on all four**, matching
every ehdb record except `EventRecord`.

⚠ `EventRecord` deliberately omits `deny_unknown_fields` so a rollback can read
newer writes. That posture is correct for D1, which must be readable by two
process versions at once. These four are read only by this crate, so strict is
right — but if that ever stops being true, this decision has to be revisited
*before* the second reader ships, not after.

---

## 6. Operational requirements EHDB imposes

These are not optional. Each is a documented way `ehdb-l0` fails quietly.

### 6.1 ⚠⚠ `seal_max_age` must be set AND a timer must drive sealing

`L0Config::seal_max_age` defaults to `None`, and `engine.rs:85-95` says what that
means: *"today's behavior leaves the durability window **unbounded in time**: a
shard that appends a few records and goes quiet never seals, so those records
never reach the substrate."*

And the sting: *"⚠ Setting this is necessary but not sufficient. An idle shard
takes no appends, so something must drive `L0Engine::seal_aged_parts` on a timer;
the flag alone is inert on exactly the shard it protects."*

EHDB's own spec requires a 5 s default plus a timer. **Nothing in `ehdb-l0/src/`
sets it today** — only an example does.

This matters more for the catalog than for the event log, because **the catalog is
almost always idle.** 1,601 records in total; registration is a human-paced act.
The catalog is the worst case for this bug: the durability window is unbounded
precisely because nothing is happening. A registered playbook could sit unsealed
indefinitely and be lost on pod replacement.

→ **Requirement.** Set `seal_max_age` explicitly and run a sealer tick. A test
must assert the tick exists and fires, not merely that the field is set — a set
field with no timer is the exact inert-flag shape this codebase keeps producing.

### 6.2 `FlushPolicy` chosen deliberately

`EveryAppend` (fsync per append) for a bare `L0Engine`. `CallerDriven` is only for
a writer that owns its commit points; `FeedWriter::new` silently flips the engine
to it, so prod does not run the documented default. `Buffered` is for derived
tiers and never for a log of record.

→ Catalog writes are rare and matter individually. **`EveryAppend`.** The fsync
cost is irrelevant at this volume and the failure mode of being wrong is losing a
registration.

### 6.3 `manifest_retain` must not be 0

`0` means unbounded, which on prod produced **6,770 snapshots / 19.4 GB behind
71.8 MB of data and stopped every append**. Default is 32.

→ Leave the default. Do not make it configurable without a stated reason.

### 6.4 `FORMAT_VERSION` pins the tag across processes

`open` writes it if absent, matches it, or **refuses**. Two processes at different
ehdb versions over one `local_root` is the corruption vector this blocks.

→ Any process sharing the catalog's `local_root` pins the same ehdb tag. Since the
server already pins `v0.4.5`, this crate must too — and they must move together.

### 6.5 Single writer per partition

`L0Engine` takes `&mut self` for appends and has **no built-in mutual exclusion**;
*"the caller is the shard owner."* The only election/fencing code is the
shadow-mode non-authoritative pair in `ehdb-reference`.

→ **Assumption:** the catalog runs with `shard_count = 1` and a single writer — the
server process that owns catalog registration today. This is not a limitation at
1,601 records. Multi-writer is explicitly out of scope and would need the
election work EHDB has not finished.

---

## 7. Read-your-writes

The predecessor named this as the open hazard: a cached fold is *"stale by
construction for up to the TTL, which is exactly the read-your-writes problem the
RFC names (register-then-run)."*

It is the common path: `noetl catalog register` immediately followed by
`noetl execute --path …`. The e2e suite does exactly this — register, then execute,
in one loop body.

**This is why the fold must not be served from a TTL cache.**

EHDB makes that easy and the predecessor's framing slightly pessimistic: L0 reads
already **merge the active (unsealed) part with local sealed parts and
substrate-replica parts**. A read after an append sees the append with no flush
and no cache invalidation. The staleness was a property of the caching layer, not
of EHDB.

→ **Requirement.** Reads go to `read_index_after` directly. If a cache is ever
added it must be invalidated by `op_seq`, not by time. A guard: append, then read
in the same test without flushing, and assert the write is visible. That test
fails on a TTL cache.

---

## 8. Compatibility with the live surface

The existing `/api/catalog` surface is driven by the CLI, the gateway UI, and the
e2e suite. **It is subsumed, not duplicated incompatibly.**

| route | status |
| :-- | :-- |
| `POST /api/catalog/register` | kept, wire-compatible. Gains relation extraction |
| `POST /api/catalog/register/batch` | kept (1000-item cap, partial failure already first-class) |
| `POST /api/catalog/list` | kept. **Fixes the case bug** — see below |
| `POST /api/catalog/resource` | kept |
| `POST /api/catalog/delete` / `restore` | kept, internal-token gated |
| `GET /api/catalog/{path}/ui_schema` | kept |

Response shapes stay as they are, including explicitly-emitted nulls (they match a
retired pydantic wire shape and clients depend on it).

### 8.1 ⚠ A live bug this model must not inherit

Registration lowercases `kind` unconditionally
(`server/src/services/catalog.rs:80-84`), but the listing predicate binds the
caller's string verbatim (`server/src/db/queries/catalog.rs:250`):

```rust
let kind_pred = if kind.is_some() { "kind = $1" } else { "1 = 1" };
```

So **`POST /api/catalog/list {"resource_type":"Playbook"}` matches zero rows**
while `"playbook"` works. Affected live callers send the capitalised form:

- `noetl catalog list Playbook` — the CLI's own help text says `Playbook`
- the gateway UI: `apiRequest('POST', '/catalog/list', { resource_type: 'Playbook' })`
- the handler's own rustdoc documents the capitalised body

`grep -rn "resource_type" server/tests/` → **0 matches.** No test covers it.

The subscription scan got it right (`WHERE LOWER(kind) = 'subscription'`); the
catalog listing did not.

→ In this model `ResourceType.name` is lowercase by construction and lookup
normalizes at the boundary, so the bug is unrepresentable. **Tracked separately as
a fix to the current server** — it is a live bug affecting CLI users today and
should not wait for this project.

### 8.2 Indexing

`noetl.catalog` has **no indexes beyond PK and `UNIQUE (path, version)`** — none
on `kind`, none partial on `archived_at IS NULL`, despite every `list?resource_type=`
filter and the subscription scan using `kind`.

In EHDB the equivalent is the `Dataset::index_key` bloom dimension, chosen per
dataset in §5.2. `kind` is **not** an index dimension — it is an attribute of the
type, and a kind-filtered listing folds `c4` first (tiny, cacheable) then reads by
path. Making `kind` a partition key would hot-spot every playbook into one shard.

---

## 9. The `noetl.registry` question

`noetl.registry` exists with kinds `model`, `dataset`, `eval`, `release`, a
`lineage JSONB` column, and a source comment recording that **extending
`noetl.catalog` was considered and rejected.**

The rejection reasons are not recorded in a form I can read — the cited RFCs are
missing from disk. What is observable:

- `noetl.registry` **does** retry on the version race (3 attempts);
  `noetl.catalog` does not.
- `noetl.registry` **has** a lineage concept; `noetl.catalog` has no relations.
- `lineage` is **unvalidated, unindexed, and has no reader that traverses it.**

So the earlier decision bought a retry loop and a lineage column that nothing
reads, at the cost of a second table with its own kind list.

→ **Assumption (stated, reversible):** this model is a superset of both.
`RelationKind::DerivesFrom` generalizes `lineage`, and `model`/`dataset`/`eval`/
`release` are ordinary `ResourceType` rows. **But `noetl.registry` is not
migrated by this spec.** Unifying them is a follow-up with its own evidence, and
doing it here would make this project's scope the union of two half-built things.
The model is designed so the merge is possible later; it is not performed now.

---

## 10. What a new resource type costs

The acceptance test of the whole design. To add `mcp`:

1. Append one `TypeOp` to `c4_catalog_type`:
   `{ name: "mcp", executable: false, catalogued: true, supertype: None }`.
2. Register entities with `resource_type: "mcp"`.

**That is the entire procedure.** No DDL. No new dataset. No new table. No
migration. No deploy.

Contrast: today it needs a row in `noetl.resource` (DDL seed or startup code) and
the FK permits nothing else. Contrast adiona: a new `mcp_attributes` table and an
`mcp_category` table.

→ **A guard will enforce this.** A test adds a synthetic resource type, writes an
entity with two attributes and one relation, reads them all back, and asserts the
count of `Dataset` impls in the crate is **unchanged**. That last assertion is the
one that matters — it fails if someone "adds a type" by adding a dataset.

---

## 11. Acceptance criteria

Checkable, per `agents/rules/spec-driven-development.md`.

| # | criterion | how it is checked |
| :-- | :-- | :-- |
| AC1 | A catalog write is an EHDB append; no other store is written | guard: no `sqlx`/SQL string in the crate; dependency list has no DB driver |
| AC2 | A read after an append sees it without a flush | test: append then `read_index_after`, no flush between. Fails on a TTL cache |
| AC3 | Adding a resource type adds no `Dataset` impl | test: add a type end-to-end, assert the `Dataset` impl count is unchanged |
| AC4 | Multiple attributes on one entity all survive the fold | test: two attributes, assert both. **Positive control** — the naive latest-wins fold returns one and looks fine |
| AC5 | A relation records its provenance | test: `Extracted` and `Observed` are distinguishable after a round trip |
| AC6 | `version` exceeds 32,767 | test: version 40,000 round-trips. Fails on `i16` |
| AC7 | `seal_max_age` is set **and** a timer drives `seal_aged_parts` | test asserts the tick fires, not that the field is set |
| AC8 | A kind filter is case-insensitive | test: `"Playbook"` and `"playbook"` return the same set |
| AC9 | `content` can be projected away while identity survives | test: listing without content still carries `path`, `version`, `content_sha256` |
| AC10 | The existing `/api/catalog` wire shapes are unchanged | the e2e register→execute loop passes against the new path |

⚠ AC4 and AC7 are the two that protect against *silent* wrongness. The others
fail loudly. These two would otherwise pass while being broken, which is why each
specifies a positive control.

### Status, as audited in code

**9 of 10 cited, 1 deliberately open, 0 broken citations.** The citations are not in
this table — a ticked box here is a copy of reality, true only while someone keeps it
true, and this repo has three recorded instances of shipped work with unticked boxes
(#194 T0–T5, ehdb#241 phases 6–10, #201). So each criterion names a test function in
`crates/catalog-store/tests/spec_acceptance_traceability.rs`, and the audit asserts
the function **exists in the file it claims**. A citation to a renamed or deleted test
fails there rather than rotting into a confident tick — proven by mutation: renaming
one cited test and pointing another at a missing file produced `2 broken citations`.

⚠⚠ **AC10 is NOT met**, and is recorded as `Open` with its reason rather than ticked.
It requires the real `noetl-server`, and nothing in `noetl/server` links
`catalog-store` — the crate has **no consumer on any serving path**. Discharging it
means a server integration, which is a separate change with its own owner gate. The
audit prints this every run and deliberately does **not** fail on it: a check that
fails on a known gap gets disabled, and one that prints it keeps it visible.

⚠ **AC8 was covered but untraceable.** A sweep for `AC8` across the crate found
**zero** mentions, while the behaviour and its test already existed (the `c1` type
index lowercases its key). Covered-but-uncited is how a criterion quietly loses its
proof, which is the gap the audit closed.

---

## 12. Assumptions made without the user

Recorded as assumptions, not decisions, so they can be overturned cheaply.

1. **"Internal resources" excludes business data.** The catalog describes NoETL's
   own furniture. ([§1](#1-scope--what-catalog-means-here))
2. **Two live kinds, not twelve.** Sized to `playbook` + `subscription`; the rest
   are added as data when real.
3. **`credential` stays out.** It is seeded but deliberately routed to
   `noetl.credential`, and credentials have handling rules the catalog should not
   acquire.
4. **New dataset namespace `c1..c4`, not `D11`, and not reusing `D7`.** ([§5.2](#52-datasets--a-new-namespace-deliberately))
5. **Single writer, `shard_count = 1`.** EHDB has no authoritative election.
   ([§6.5](#65-single-writer-per-partition))
6. **`noetl.registry` is not migrated here.** ([§9](#9-the-noetlregistry-question))
7. **`version` widens to `u32`.** ([§4.2](#42-entity--a-catalogued-resource))
8. ~~**Localization is dropped** from the adiona pattern.~~ **REVERSED
   2026-10-06.** Dropping it would make adiona a *false* worked example: **24 of its
   58 tables are `_translate` / `_content`**, so nearly half the reference schema
   would be inexpressible and the catalog would not be a generalization of the model
   it claims to generalize. Localization is now a real `lang` dimension on
   `Attribute`, folded by `(name, lang)`.

   ⚠ The rejected alternative was encoding the language into the attribute name
   (`category_name@de`). A probe confirmed it "works" and it is wrong: the language
   becomes unqueryable, `attributes()` reports one entry per language as though they
   were different attributes, and nothing can ask which languages exist or fall back
   to a default.

   ⚠ `None` is **not** `Some("en")`. A playbook's `uses_tool.postgres` has no
   language; an English label is a translation that happens to be English.
   Conflating them would make every noetl attribute pretend to be English, so the
   neutral read (`attributes`) deliberately excludes translations.
9. **`credential_id` is dropped** as dead — no FK, no reader, no writer in five
   repos.
10. **The existing shadow catalog log is the predecessor to extend**, not replace.
    Its three event types are kept. ([§2.1](#21-where-this-starts-from--not-a-blank-page))

## 13. Open questions — flagged, not blocking

1. **Does `noetl.registry` merge in?** [§9](#9-the-noetlregistry-question) assumes
   not-now. Needs the missing RFCs or a decision.
2. **Who owns the DDL during transition?** Two byte-identical copies of
   `schema_ddl.sql` exist and must be edited together; ownership transfer to
   `repos/server` is gated on owner approval.
3. **Is `Observed` provenance worth the write volume?** Recording a relation per
   execution is a write per run. Possibly sampled, possibly derived on read.
4. **Does the catalog get its own `local_root`/PVC or share the server's?** Shared
   means `FORMAT_VERSION` couples the deploys ([§6.4](#64-format_version-pins-the-tag-across-processes)).
   Separate means another volume to size. Leaning shared, since the writer is the
   server process.

## 14. Plan

Incremental, flag-gated, each step reversible.

| phase | content | gate |
| :-- | :-- | :-- |
| **P0** | this spec + repo + CI | ✅ done |
| **P1** | `catalog-model`: the four types, `AttributeValue`, no storage. Pure, fully testable | none needed |
| **P2** | `catalog-store`: the four `Dataset` impls + folds. AC2, AC4, AC6 | off by default |
| **P3** | relation extraction from playbook YAML (`Extracted` provenance). AC5 | off by default |
| **P4** | read path behind `NOETL_CATALOG_MODEL=shadow`, compared against `noetl.catalog`. Report **coverage**, not just divergence | shadow |
| **P5** | serve reads when shadow parity holds over a window **sized to the registration period**, not to a sample count | flag flip, reversible |

⚠ P4's comparator must publish **coverage** — how many entities it compared — not
only divergence. A comparator that compares nothing reports perfect agreement.
That exact reading closed an issue wrongly before
(`agents/rules/representation-drift.md`, "coverage was ~0 by construction").

⚠ P5's window is sized by the **period of registration events**, not by how many
comparisons fit in it. A clean window shorter than the period between
registrations is a non-result. Volume is not duration.
