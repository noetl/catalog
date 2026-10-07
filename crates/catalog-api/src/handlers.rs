//! Every handler. Each read returns the **full** set its query names, never a page —
//! see the note on [`by_attribute`].

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::Json;
use catalog_model::{Attribute, AttributeValue, Entity, EntityRef, Provenance, Relation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::auth::require_internal_token;
use crate::ApiState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn store_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// ⚠ A poisoned mutex means a previous writer panicked mid-append. Returning 500 is
/// correct: the store's ordering invariant may be broken, and serving reads as though
/// nothing happened would be the silent-wrong-answer shape.
fn lock(state: &ApiState) -> ApiResult<std::sync::MutexGuard<'_, catalog_store::CatalogStore>> {
    state.store.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "catalog store lock poisoned — a previous write panicked; the store's \
             ordering invariant may be broken"
                .to_string(),
        )
    })
}

// ---------------------------------------------------------------------------
// health + metrics
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
    /// Sealed parts per dataset, so health is a measurement rather than a constant.
    pub parts: BTreeMap<String, usize>,
}

pub async fn health(State(state): State<ApiState>) -> ApiResult<Json<Health>> {
    let pc = lock(&state)?.part_counts();
    let mut parts = BTreeMap::new();
    parts.insert("entities".into(), pc.entities);
    parts.insert("attributes".into(), pc.attributes);
    parts.insert("relations".into(), pc.relations);
    parts.insert("types".into(), pc.types);
    Ok(Json(Health {
        status: "ok",
        version: catalog_store::metrics::version(),
        parts,
    }))
}

pub async fn metrics() -> String {
    catalog_store::metrics::init();
    catalog_store::metrics::render()
}

// ---------------------------------------------------------------------------
// object types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DeclareType {
    pub name: String,
    #[serde(default = "yes")]
    pub executable: bool,
    #[serde(default = "yes")]
    pub catalogued: bool,
}
fn yes() -> bool {
    true
}

#[derive(Serialize)]
pub struct Declared {
    pub name: String,
    pub op_seq: u64,
    /// Whether this is one of noetl's six known internal object types.
    ///
    /// ⚠ Reported, never enforced. A catalog that admits only a known list is not a
    /// generic catalog — this field exists so an operator sees a typo, not so the API
    /// can refuse one.
    pub known_noetl_type: bool,
}

pub async fn declare_type(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<DeclareType>,
) -> ApiResult<Json<Declared>> {
    require_internal_token(&state, &headers)?;
    let name = req.name.trim().to_lowercase();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name must not be empty".into()));
    }
    // Where it is one of noetl's own, the declared flags come from noetl's seed rather
    // than the request: the catalog should not contradict the platform about whether a
    // `credential` is executable.
    let declared = catalog_model::noetl_resource_types()
        .into_iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| catalog_model::ResourceType::new(&name, req.executable, req.catalogued));
    let op_seq = lock(&state)?.declare_type(declared).map_err(store_err)?;
    Ok(Json(Declared {
        known_noetl_type: catalog_model::is_known_noetl_type(&name),
        name,
        op_seq,
    }))
}

/// noetl's known internal object types, and whether each is declared in THIS store.
///
/// ⚠ Both numbers are reported. "6 known" alone would not say which the store actually
/// holds, and "4 declared" alone would not say what is missing.
#[derive(Serialize)]
pub struct TypeListing {
    pub known_noetl_types: Vec<String>,
    pub declared_here: Vec<String>,
}

pub async fn list_types(State(state): State<ApiState>) -> ApiResult<Json<TypeListing>> {
    let s = lock(&state)?;
    let known: Vec<String> = catalog_model::noetl_resource_types()
        .into_iter()
        .map(|t| t.name)
        .collect();
    let mut declared = Vec::new();
    for n in &known {
        if s.resource_type(n).map_err(store_err)?.is_some() {
            declared.push(n.clone());
        }
    }
    Ok(Json(TypeListing {
        known_noetl_types: known,
        declared_here: declared,
    }))
}

pub async fn get_type(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let t = lock(&state)?.resource_type(&name).map_err(store_err)?;
    match t {
        Some(t) => Ok(Json(serde_json::to_value(t).map_err(store_err)?)),
        None => Err((
            StatusCode::NOT_FOUND,
            format!("type {name:?} is not declared in this store"),
        )),
    }
}

// ---------------------------------------------------------------------------
// objects
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RegisterObject {
    pub resource_type: String,
    pub path: String,
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub entity_id: i64,
    #[serde(default)]
    pub content: Option<String>,
    /// When present, references and attributes are extracted from it, exactly as the
    /// ingest path does.
    #[serde(default)]
    pub extract: bool,
}
fn one() -> u32 {
    1
}

#[derive(Serialize)]
pub struct Registered {
    pub path: String,
    pub op_seq: u64,
    pub relations: usize,
    pub attributes: usize,
}

pub async fn register_object(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<RegisterObject>,
) -> ApiResult<Json<Registered>> {
    require_internal_token(&state, &headers)?;
    let body = req.content.clone().unwrap_or_default();
    let entity = Entity {
        resource_type: req.resource_type.trim().to_lowercase(),
        path: req.path.clone(),
        version: req.version,
        entity_id: req.entity_id,
        content_sha256: catalog_ingest::sha256_hex(body.as_bytes()),
        content: req.content,
        archived_at: None,
    };
    let mut s = lock(&state)?;
    if req.extract && !body.is_empty() {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let r = s
            .register_from_source(entity, &body, at)
            .map_err(store_err)?;
        return Ok(Json(Registered {
            path: req.path,
            op_seq: r.op_seq,
            relations: r.relations,
            attributes: r.attributes,
        }));
    }
    let op_seq = s.register(entity).map_err(store_err)?;
    Ok(Json(Registered {
        path: req.path,
        op_seq,
        relations: 0,
        attributes: 0,
    }))
}

#[derive(Deserialize)]
pub struct ListQuery {
    /// The object type to list. Required — there is deliberately no "list everything".
    pub r#type: String,
}

/// **Query by type.** Every live object of one type.
///
/// ⚠ Returns the FULL set and a `count`, not a page. A paginated default is how a
/// partial answer passes for a complete one, which is the failure this catalog's folds
/// were built against: a reverse lookup that returned 1 of 49 *looked* successful.
#[derive(Serialize)]
pub struct Listing {
    pub r#type: String,
    pub count: usize,
    pub paths: Vec<String>,
}

pub async fn list_objects(
    State(state): State<ApiState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Listing>> {
    let paths = lock(&state)?
        .resources_of_type(&q.r#type)
        .map_err(store_err)?;
    Ok(Json(Listing {
        r#type: q.r#type,
        count: paths.len(),
        paths,
    }))
}

#[derive(Serialize)]
pub struct ObjectView {
    pub path: String,
    pub latest: Option<Entity>,
    pub versions: Vec<Entity>,
}

pub async fn get_object(
    State(state): State<ApiState>,
    Path(path): Path<String>,
) -> ApiResult<Json<ObjectView>> {
    let s = lock(&state)?;
    let versions = s.versions(&path).map_err(store_err)?;
    let latest = s.latest(&path).map_err(store_err)?;
    if versions.is_empty() && latest.is_none() {
        return Err((StatusCode::NOT_FOUND, format!("no object at {path:?}")));
    }
    Ok(Json(ObjectView {
        path,
        latest,
        versions,
    }))
}

// ---------------------------------------------------------------------------
// attributes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SetAttribute {
    pub path: String,
    pub name: String,
    pub value: serde_json::Value,
    #[serde(default)]
    pub entity_id: i64,
}

pub async fn set_attribute(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<SetAttribute>,
) -> ApiResult<Json<serde_json::Value>> {
    require_internal_token(&state, &headers)?;
    let value = json_to_attribute_value(&req.value);
    let op_seq = lock(&state)?
        .set_attribute(
            &req.path,
            Attribute::new(req.entity_id, req.name.clone(), value),
        )
        .map_err(store_err)?;
    Ok(Json(serde_json::json!({ "op_seq": op_seq })))
}

/// JSON → the typed attribute union. The ordering matters: bool before number before
/// string, so `true` does not land as `Text("true")`.
fn json_to_attribute_value(v: &serde_json::Value) -> AttributeValue {
    match v {
        serde_json::Value::Bool(b) => AttributeValue::Flag(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                AttributeValue::Integer(i)
            } else {
                AttributeValue::Measure(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => AttributeValue::Text(s.clone()),
        other => AttributeValue::Json(other.clone()),
    }
}

pub async fn get_attributes(
    State(state): State<ApiState>,
    Path(path): Path<String>,
) -> ApiResult<Json<BTreeMap<String, Attribute>>> {
    Ok(Json(lock(&state)?.attributes(&path).map_err(store_err)?))
}

#[derive(Deserialize)]
pub struct ByAttributeQuery {
    pub name: String,
}

/// **Query by attribute — the reverse lookup.** Every object carrying attribute `name`.
///
/// The query this catalog exists for: `uses_credential.<alias>` answers "which objects
/// break if I rotate this credential".
///
/// ⚠⚠ Returns the FULL set with its count. A naive fold returned **1 of 49** here and
/// **0 of 48** once any object unset the attribute — partial answers that looked
/// successful, and the second would have green-lit a rotation breaking 48 objects.
#[derive(Serialize)]
pub struct ByAttribute {
    pub name: String,
    pub count: usize,
    pub paths: Vec<String>,
}

pub async fn by_attribute(
    State(state): State<ApiState>,
    Query(q): Query<ByAttributeQuery>,
) -> ApiResult<Json<ByAttribute>> {
    let paths = lock(&state)?
        .resources_with_attribute(&q.name)
        .map_err(store_err)?;
    Ok(Json(ByAttribute {
        name: q.name,
        count: paths.len(),
        paths,
    }))
}

// ---------------------------------------------------------------------------
// relations
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AssertRelation {
    pub from_type: String,
    pub from_path: String,
    pub to_type: String,
    pub to_path: String,
    /// The relation kind label — `invokes`, `requires`, `references`, `derives_from`,
    /// `supersedes`, `annotates`.
    pub kind: String,
    #[serde(default)]
    pub to_version: Option<u32>,
}

pub async fn assert_relation(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<AssertRelation>,
) -> ApiResult<Json<serde_json::Value>> {
    require_internal_token(&state, &headers)?;
    // ⚠ An unknown kind is REFUSED rather than coerced. Accepting it would write an
    // edge whose label nothing can query and whose retraction cannot match — the
    // silent-no-op shape `retract_relation` already guards.
    let kind = kind_from_label(&req.kind).ok_or_else(|| {
        let known: Vec<&str> = catalog_model::RelationKind::all()
            .iter()
            .map(|k| k.discriminant())
            .collect();
        (
            StatusCode::BAD_REQUEST,
            format!("unknown relation kind {:?}; known: {known:?}", req.kind),
        )
    })?;
    let rel = Relation {
        from_entity: EntityRef {
            resource_type: req.from_type.trim().to_lowercase(),
            path: req.from_path,
            version: None,
        },
        to_entity: EntityRef {
            resource_type: req.to_type.trim().to_lowercase(),
            path: req.to_path,
            version: req.to_version,
        },
        kind,
        discovered_by: Provenance::Declared,
    };
    let op_seq = lock(&state)?.assert_relation(rel).map_err(store_err)?;
    Ok(Json(serde_json::json!({ "op_seq": op_seq })))
}

fn kind_from_label(label: &str) -> Option<catalog_model::RelationKind> {
    let want = label.trim().to_lowercase();
    catalog_model::RelationKind::all()
        .into_iter()
        .find(|k| k.discriminant() == want)
}

/// **Query by relation.** Every live outgoing edge from `path` — what it invokes,
/// requires, references.
#[derive(Serialize)]
pub struct Edges {
    pub path: String,
    pub count: usize,
    pub edges: Vec<EdgeView>,
}

#[derive(Serialize)]
pub struct EdgeView {
    pub to_path: String,
    pub to_type: String,
    pub kind: String,
}

pub async fn relations_from(
    State(state): State<ApiState>,
    Path(path): Path<String>,
) -> ApiResult<Json<Edges>> {
    let rels = lock(&state)?.relations_from(&path).map_err(store_err)?;
    let edges: Vec<EdgeView> = rels
        .iter()
        .map(|r| EdgeView {
            to_path: r.to_entity.path.clone(),
            to_type: r.to_entity.resource_type.clone(),
            kind: r.kind.discriminant().to_string(),
        })
        .collect();
    Ok(Json(Edges {
        path,
        count: edges.len(),
        edges,
    }))
}

/// **Query by reverse relation.** Every live object that points at `path`.
///
/// ⚠⚠ The direction that decides whether an object can be retired. A naive fold
/// returned **1 of 40** callers for a shared dependency — and "1 caller" is the answer
/// that gets a dependency deleted.
#[derive(Serialize)]
pub struct Callers {
    pub path: String,
    pub count: usize,
    pub callers: Vec<CallerView>,
}

#[derive(Serialize)]
pub struct CallerView {
    pub from_path: String,
    pub kind: String,
}

pub async fn relations_to(
    State(state): State<ApiState>,
    Path(path): Path<String>,
) -> ApiResult<Json<Callers>> {
    let pairs = lock(&state)?.relations_to(&path).map_err(store_err)?;
    let callers: Vec<CallerView> = pairs
        .into_iter()
        .map(|(from_path, kind)| CallerView { from_path, kind })
        .collect();
    Ok(Json(Callers {
        path,
        count: callers.len(),
        callers,
    }))
}

// ---------------------------------------------------------------------------
// bulk ingest — walk a source over the API
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct IngestRequest {
    /// `dir:/path` or `git:/repo@ref`. The git form reads the **ref**, not the working
    /// tree — a stale checkout is the most reliable way to produce a confident zero.
    pub source: String,
    #[serde(default)]
    pub subpath: String,
}

#[derive(Serialize)]
pub struct IngestResult {
    pub source: String,
    /// **The denominator.** Files the walker considered.
    pub scanned: usize,
    pub registered: usize,
    pub relations: usize,
    pub attributes: usize,
    pub skipped: Vec<SkipView>,
    pub by_kind: BTreeMap<String, usize>,
    /// Types this run catalogued that are not among noetl's six known ones. Reported,
    /// never refused — a typo must be visible, not rejected.
    pub new_types: Vec<String>,
    /// Whether `scanned == registered + skipped`. A walk that drops files otherwise
    /// reports a clean run.
    pub accounts_for_every_file: bool,
}

#[derive(Serialize)]
pub struct SkipView {
    pub origin: String,
    pub reason: String,
}

/// **Ingest a whole source over the API.** Before this, ingestion over HTTP was one object
/// at a time and the walker was reachable only from the CLI — which contradicted
/// "API-only" for the one path that matters most.
pub async fn ingest(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<IngestRequest>,
) -> ApiResult<Json<IngestResult>> {
    require_internal_token(&state, &headers)?;
    let source = parse_source(&req.source).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!(
                "source {:?} must be `dir:<path>` or `git:<repo>@<ref>`",
                req.source
            ),
        )
    })?;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let res = {
        let mut s = lock(&state)?;
        catalog_ingest::ingest(&mut s, &source, &req.subpath, at)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("ingest: {e}")))?
    };

    // ⚠ The accounting is reported, not asserted away. A caller must be able to see that
    // the walk balanced, because `scanned=N registered=N skipped=0` is also what a run
    // that silently dropped files looks like.
    Ok(Json(IngestResult {
        source: source.label(),
        scanned: res.scanned,
        registered: res.registered,
        relations: res.relations,
        attributes: res.attributes,
        skipped: res
            .skipped
            .iter()
            .map(|(p, r)| SkipView {
                origin: p.display().to_string(),
                reason: r.to_string(),
            })
            .collect(),
        by_kind: res.by_kind.clone(),
        new_types: res.new_types.iter().cloned().collect(),
        accounts_for_every_file: res.accounts_for_every_file(),
    }))
}

fn parse_source(spec: &str) -> Option<catalog_ingest::Source> {
    if let Some(rest) = spec.strip_prefix("git:") {
        let at = rest.rfind('@')?;
        return Some(catalog_ingest::Source::GitRef {
            repo: std::path::PathBuf::from(&rest[..at]),
            reference: rest[at + 1..].to_string(),
        });
    }
    if let Some(p) = spec.strip_prefix("dir:") {
        return Some(catalog_ingest::Source::Dir(std::path::PathBuf::from(p)));
    }
    None
}

/// The attribute constraints this catalog enforces, and their allowed values.
///
/// ⚠ Published deliberately. A caller refused by a constraint it cannot see has no way to
/// comply — the same reason `noetl/server` quotes its valid set in a rejection.
#[derive(Serialize)]
pub struct ConstraintsView {
    pub enforced_on: &'static str,
    pub not_enforced_on: &'static str,
    pub constraints: Vec<ConstraintEntry>,
}

#[derive(Serialize)]
pub struct ConstraintEntry {
    pub attribute: String,
    pub allowed: Vec<String>,
}

pub async fn constraints() -> Json<ConstraintsView> {
    Json(ConstraintsView {
        enforced_on: "POST /api/catalog/attributes — an explicit write, where a caller \
                      asserts a fact",
        not_enforced_on: "extraction during registration — a document declaring a value \
                          noetl rejects is still catalogued, because recording it is how \
                          anyone finds it",
        constraints: catalog_model::constraints::described_constraints()
            .into_iter()
            .map(|(attribute, allowed)| ConstraintEntry {
                attribute: attribute.to_string(),
                allowed: allowed.into_iter().map(String::from).collect(),
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// lifecycle
// ---------------------------------------------------------------------------

/// Drive the EHDB lifecycle: seal aged parts, run pending merges, reclaim what the
/// merges superseded. Returns all three counts separately — a single number cannot show
/// a merge that costs storage.
pub async fn tick(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_internal_token(&state, &headers)?;
    let t = lock(&state)?.tick().map_err(store_err)?;
    Ok(Json(serde_json::json!({
        "sealed": t.sealed,
        "merged": t.merged,
        "reclaimed": t.reclaimed,
    })))
}
