# noetl/catalog

**Last refreshed:** 2026-10-06 (repo created; design spec and P1 model types landed)

A generalized, event-sourced catalog of **NoETL's own internal resources** —
playbooks first — persisted through [EHDB](https://github.com/noetl/ehdb).

| | |
| :-- | :-- |
| **Design spec** | [`design/catalog-model.md`](https://github.com/noetl/catalog/blob/main/design/catalog-model.md) — the reference; read it before adding a resource type |
| **Tracking** | [noetl/ai-meta#427](https://github.com/noetl/ai-meta/issues/427) · [Umbrella page](https://github.com/noetl/ai-meta/wiki/Umbrella-Catalog-Model) |
| **Storage** | `ehdb-l0` + `ehdb-core`, git tag `v0.4.5`. Nothing else. |
| **Toolchain** | 1.99.0, pinned in `rust-toolchain.toml` |
| **CI** | fmt · source hygiene · tests · `clippy --all-targets -- -D warnings`. Required check `rust`, `strict`. No required review. |

## Storage is EHDB — the one non-negotiable

**The catalog is a model and an API. It is not a datastore.**

- A catalog write becomes an **event appended to an EHDB dataset**.
- Every read is served from a **fold** over that log.
- There is no second database, no engine of our own, no external datastore.

This follows ai-meta's `self-sufficiency.md`: *self-sufficient means NoETL owns its
own state; it does not mean no dependencies.* `ehdb-l0` is a crate that compiles into
the binary — allowed and preferred. A separate thing to deploy, size, quorum and
recover is what is forbidden.

The boundary is load-bearing, not stylistic. A catalog that owned storage would be a
second source of truth for facts the event log already holds, and the two would
disagree.

## Layout

```
crates/catalog-model/   the entity/relation model  (P1, landed)
design/                 the design specification
```

## The model

```
ResourceType  ──<  Entity  >──  Attribute
                     │
                     └──<  Relation  >──  Entity
```

| type | what it is |
| :-- | :-- |
| `ResourceType` | the type registry. `executable`, `catalogued`, optional `attribute_schema`, optional `supertype` |
| `Entity` / `EntityRef` | a catalogued resource at one version; identity is `(resource_type, path, version)` |
| `Attribute` / `AttributeValue` | typed metadata — `Text` `Integer` `Measure` `Flag` `Timestamp` `Ref` `Json` |
| `Relation` | a directed edge with a `RelationKind` and a `Provenance` |

### Invariants the types hold

These are the ones worth knowing before changing anything here; each has its
reasoning in a doc comment at the definition.

- **`ResourceType::new` lowercases the name.** Not cosmetic: `noetl.catalog.kind`
  holds `'Playbook'` **875** times and `'playbook'` **650** times on prod, so a
  case-sensitive comparison returns a *partial* set and reads as a working query.
  Normalizing at construction makes that state unrepresentable.
- **`catalogued` is independent of `executable`.** `credential` is a known resource
  type that is deliberately **not** catalogued — it is diverted to
  `noetl.credential`. One field would either lose the type or invite credentials in.
- **`Entity::version` is `u32`.** Today's column is `SMALLSERIAL` → `i16`, a ceiling
  of **32,767** per path, and `catalog load` re-versions every entry on each run, so
  the ceiling is reachable by routine operation.
- **`content` is `Option` while `content_sha256` is not**, so identity survives a
  body-less listing. `None` means *not fetched*, never *empty*.
- **An unpinned `EntityRef` stays unpinned.** Every `kind: playbook` step today names
  a path and no version, so that genuinely *is* unpinned; defaulting it would
  fabricate a claim the source never made.
- **`Provenance` keeps `Declared` / `Extracted` / `Observed` distinct**, and only
  `Observed` is evidence of execution. `Extracted` says *the source says so*;
  `Observed` says *it actually happened*.
- **`AttributeValue::Json` is not queryable.** A filter over an opaque blob is a
  schema nobody declared, and it is unindexable.

### Adding a resource type

Append one `ResourceType` record. That is the whole procedure — **no DDL, no new
dataset, no migration, no deploy.**

#### ⭐ Demonstrated on a real second type, not only asserted

`subscription` is supported, and it shares nothing structurally with a playbook:

| | playbook | subscription |
| :-- | :-- | :-- |
| content | a `workflow:` tree of steps | a `spec:`, **no `workflow:` at all** |
| reference location | `workflow[].tool[].path` | `spec.dispatch.playbook` |
| credential dependency | — | `spec.auth` → `RelationKind::Requires` |
| nesting | arbitrary (`iterator`, `task_sequence`) | flat |

Measured on the `noetl/e2e` corpus: **9 of 9** `kind: Subscription` fixtures carry a
`spec.dispatch.playbook`, 6 of 9 carry a `spec.auth`, and the spec scalars appear at
`source` 9/9, `mode` 9/9, `activation` 7/9, `stream` 6/9, `consumer` 6/9. The playbooks
they dispatch sit in the same directory, so the cross-type graph is closed and checkable.

**Supporting it added zero datasets.** AC3's count is still exactly four.

⚠ `spec.auth` names a **credential** — a resource type the catalog deliberately does
*not* hold, because the CLI diverts credentials to `noetl.credential`. The edge is
recorded anyway: *"this subscription needs alias X"* is a real dependency, and a dangling
one is worth knowing about even when the target lives elsewhere.

#### How a new type is added in practice

1. Append a `ResourceType` record (`c4`).
2. If its references live somewhere new in the document, add an arm to
   `catalog_extract::find_references`, which dispatches on the document's own `kind`.

⚠ An **unknown** kind yields an empty reference list rather than an error, deliberately:
the catalog must be able to hold a resource type whose references nobody has taught it to
read yet. Erroring there would make such a type unregisterable.

⚠ This is enforced, not merely intended.
`tests/adding_a_resource_type_costs_no_schema.rs` exercises a type the crate has
never heard of and asserts the `Dataset` impl count **does not change**. Today that
count is 0; when P2 lands the four `c1..c4` datasets it becomes exactly 4 and must
**stay 4** however many resource types exist. If a resource type ever needs its own
dataset, the model has regressed to the per-type-table shape this repo exists to
remove.

## Reads: the three indexes, and why none of them is a fifth dataset

`ehdb-l0`'s `Dataset::index_key` returns **one** `&str`, so one dataset indexes one
dimension. Three queries needed a second dimension, and AC3 forbids adding a dataset to
get it. The resolution: a **second row kind inside the existing dataset**, keyed by a
control-character sentinel. Sound because `read_index_after` matches the index key by
**exact string equality** (`ehdb-l0` `engine.rs:1489`), so the two key spaces are
disjoint.

| dataset | forward key | synthetic key | reverse query |
| :-- | :-- | :-- | :-- |
| `c1` | `path` | `\u{1}type/<kind>` | `resources_of_type` — every resource of type X |
| `c2` | `from_path` | `\u{1}to/<path>` | `relations_to` — **who calls X** |
| `c3` | `path` | `\u{1}attr/<name>` | `resources_with_attribute` — who uses credential X |

The sentinel is a control character because a reverse key must be impossible to collide
with a real `metadata.path`: a path reading `attr/uses_tool.postgres` would otherwise
silently answer a reverse query. A forward write whose path intrudes is **refused** —
the path comes from a document, so it is untrusted input, not a programming mistake.

### ⚠⚠ Every one of these REDs was a partial answer, never an empty one

Many paths share one synthetic key, which makes these the datasets' worst case for the
`.last()` idiom. Measured, with `.last()` planted:

| query | reading | truth |
| :-- | --: | --: |
| who uses `uses_credential.adiona_actor` | **1** | 49 |
| …after one resource unsets it | **0** | 48 |
| every `playbook` | **1** | 53 |
| who calls `shared/dep` | **1** | 40 |

The second row is the one to remember: once any resource unsets the attribute, the
latest op under the shared key is a **tombstone**, so `.last()` reports *"nobody uses
this credential"* while 48 do. That reading would green-light a rotation that breaks all
48. An empty answer gets investigated; a plausible list of 1 gets acted on.

So every assertion in these tests is **set equality**, never a count — a count of 49 can
still be the wrong 49 — and all of them are checked against ground truth derived
independently from git.

### Tombstones are required, not an optimisation

Each reverse row carries a `live` flag. Without it an unset/retract/archive would leave
the resource in the reverse answer forever and the index would only grow. For
`relations_to` the consequence inverts and gets worse: a caller list that only grows
argues **against** deleting something that is in fact unused.

### ⚠ `partition()` is derived from `index_key()`, never from the path

They are halves of one contract — a reader picks the shard with `read_partition(key)`
then matches `index_key` inside it. A row partitioned by one value and indexed by another
sends the reader to the **wrong shard**, which returns *nothing* rather than erroring.

### ⚠ Synthetic rows are excluded from a forward fold's INPUT, not its output

A reverse attribute row's `name()` equals a real attribute name and would **shadow** it —
a wrong value, not a missing one. Type rows carry `version: 0` and would create a phantom
version. Dropping them before the fold makes the worst case a missing reverse row.

## Observability

`catalog_tick_total{outcome}`, `catalog_ingest_total{outcome}`,
`catalog_ingest_skipped_total{reason}`, `catalog_build_info{version}` — 11 series,
rendered by `catalog metrics`.

⚠⚠ **Every closed label set is pinned at 0, unconditionally.** `Registry::gather` prunes
metric families with no children, so a labelled metric is absent from a scrape until
something increments it — registering is not enough. With the pins removed the whole
scrape is **155 bytes containing only `build_info`**, which is the shape in which the
prod gateway once served a 200 with zero bytes.

The pinning is not inside a config branch: `noetl/server#315` pinned its publish-skip
reasons behind `if event_bus_mode.publishes_ehdb()`, leaving them absent on exactly the
configuration whose reason someone would be reading.

⚠ The counters are **per-process**, and the CLI exits immediately — so a standalone
`catalog metrics` is all-zero *by construction*, not a measurement. The subcommand says
so, because otherwise a reader cannot tell "nothing happened" from "nothing could have
happened".

## Acceptance criteria: 9 cited, 1 open, audited in code

The spec's AC table is not the authority — a ticked box is a copy of reality. Each
criterion names a test function in `tests/spec_acceptance_traceability.rs`, and the audit
asserts the function **exists in the file it claims**, so a renamed or deleted test fails
loudly instead of rotting into a tick.

⚠⚠ **AC10 is not met**: the existing `/api/catalog` wire shapes cannot be exercised from
here, because nothing in `noetl/server` links `catalog-store` — the crate has **no
consumer on any serving path**. It is recorded as `Open` with that reason, and the audit
prints it every run rather than failing, because a check that fails on a known gap gets
disabled.

## Guards, and what each exists to prevent

A guard whose purpose is unrecorded is a guard someone deletes as noise.

| guard | prevents |
| :-- | :-- |
| `every_rust_source_on_disk_is_git_tracked` | A source file present locally and absent from a clean checkout. `git add` skips an ignored file silently and `git status` never lists it, so **CI is the first place the absence is observable** — by which point it looks like an unrelated compile error. |
| `a_brand_new_resource_type_needs_no_new_storage_shape` | The design premise eroding one reasonable-looking change at a time, until "add a type" means "add a dataset". |
| `the_name_is_lowercased_at_construction` / `two_spellings_of_one_type_are_the_same_value` | Reintroducing prod's mixed-case state into the model. |
| `a_version_above_the_smallint_ceiling_round_trips` | A narrowing of `version` back to `i16`. The test does not compile against `i16`. |
| `absent_content_is_distinguishable_from_empty_content` | Conflating *not fetched* with *no body* — which would make a projected listing indistinguishable from a catalog of empty playbooks. |
| `extracted_and_observed_stay_distinguishable_after_a_round_trip` | Collapsing provenance, i.e. treating a declared edge as an executed one. |
| `only_json_is_unqueryable` | A new `AttributeValue` variant slipping in without a decision about filterability. A total match, so it fails to compile rather than silently shrinking. |
| `a_subscription_is_not_read_as_a_playbook` | The dispatch arm firing for every kind. Asserts the dispatch **is** found *and* that the same document read as a playbook yields nothing — without the second half the first is incidental, since a subscription has no `workflow:` for the playbook walker to find. |
| `a_subscription_without_an_auth_alias_yields_only_its_dispatch_edge` | The auth arm firing on something other than `spec.auth`. If both subscription fixtures yielded two edges, the auth edge would prove nothing. |
| `a_playbook_and_a_subscription_coexist_without_contaminating_each_other` | A partition or index-key mistake making one type readable under the other's path. Both types share the same four logs, so this is the test that the sharing is correct rather than coincidental. |

### Two guards assert their own extraction first

`every_rust_source_on_disk_is_git_tracked` and
`a_brand_new_resource_type_needs_no_new_storage_shape` both print the population they
examined and fail on an implausibly small one.

⚠ The reason is specific: **a scan that walks the wrong directory finds zero
violations and reports a clean pass**, which is indistinguishable from the property
actually holding. The hygiene guard demonstrated this on its very first run — before
anything was committed it refused to compare against an empty `git ls-files`, rather
than reporting that every file was fine.

## ⚠⚠ The EHDB fold trap, for whoever writes P2

**EHDB has no `Projection` or `Fold` trait** — 23 public traits workspace-wide, none
of them a fold. The pattern is: implement `Dataset`, then hand-write a `…Store` whose
fold is

```rust
self.engine.read_index_after(key, 0)?.into_iter().next_back()
```

i.e. **latest-op-wins per index key**. Copy it for the entity log, where register →
archive → restore genuinely is last-write-wins on one `path+version`.

**Do not copy it for attributes or relations.** Many distinct names and edges coexist
under one index key there, so latest-wins keeps exactly one and *returns a
plausible-looking row*. Those folds must accumulate into a map and apply set/unset in
`op_seq` order.

Write AC4's two-attribute positive control **before** the fold, not after. The broken
version does not error.

## Operational requirements EHDB imposes quietly

| requirement | why it is not optional |
| :-- | :-- |
| set `seal_max_age` **and** run a timer driving `seal_aged_parts()` | The default `None` leaves the durability window **unbounded in time** — a shard that appends a few records and goes quiet never seals, so those records never reach the substrate. ⚠ Setting the field is *necessary but not sufficient*; without a timer the flag is inert on exactly the shard it protects. **The catalog is almost always idle, which makes it the worst case.** |
| `FlushPolicy::EveryAppend` | `CallerDriven` is only for a writer that owns its commit points. Catalog writes are rare and matter individually. |
| leave `manifest_retain` at its default of 32 | `0` means unbounded; that default produced 6,770 snapshots / 19.4 GB behind 71.8 MB of data on prod and stopped every append. |
| pin the same ehdb tag as any process sharing `local_root` | `FORMAT_VERSION` is written, matched, or **refused**. Two versions over one layout is the corruption vector it blocks. |
| single writer, `shard_count = 1` | `L0Engine` takes `&mut self` and has **no built-in mutual exclusion**. The only election/fencing code is the shadow-mode, non-authoritative pair in `ehdb-reference`, which this crate does not depend on. |

## Relationship to the existing `/api/catalog` surface

The live surface is **subsumed, not duplicated incompatibly**. `register`,
`register/batch`, `list`, `resource`, `delete`, `restore` and `ui_schema` keep their
wire shapes, including explicitly-emitted nulls (clients depend on them).

⚠ There is a predecessor in `noetl/server`: `catalog_log.rs` already writes
`CatalogRecord` to its own `StoreTier::Catalog` in shadow, the event types
`catalog.registered` / `.archived` / `.restored` already exist, and
`CatalogRelation` is a pure fold **nothing calls on a read path**. This repo finishes
and generalizes that line; it does not replace it. The RFCs those files cite are
missing from disk, which is part of why the spec lives here.

## Development

```bash
cargo fmt --all --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Every CI step can fail the build; none is suffixed `|| true`. Each gate was proven to
fail on a planted defect before being relied on — **4 of 4**.
