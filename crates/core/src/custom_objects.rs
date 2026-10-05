//! Custom objects store (P5 part 5) — the port of the reference
//! `src/server/customobjects/customObjectsRepo.ts` (plan Phase 21).
//!
//! The v1.x routes carried three pure stubs (`POST/DELETE …/links` answered
//! fake-success ignoring the body, `GET /for/:kind/:id` answered a hardcoded
//! empty list) and the rest of the surface skipped the reference's core
//! safety model entirely: no slug/type validation, no field-key rules, no
//! dynamic value validation, no FTS indexing, no pagination/search and no
//! per-type report — while the M040-aligned tables (`custom_object_types`,
//! `custom_object_fields` with `label`/`sort_order`, `custom_objects` with
//! `search_text`/`provenance`, `custom_object_links` with `note`,
//! `fts_custom_objects`) and the customer-events derivation for
//! `custom_object_event` timeline rows already existed underneath.
//!
//! ## The safety model (reference doc, verbatim intent)
//!
//! Types define typed fields; VALUES are stored as JSON and validated by a
//! schema built from the type's own field definitions at every write, so
//! user-defined data never becomes SQL. Field keys are whitelisted
//! identifiers; filtering compiles to fixed, parameterized statements.
//! Core Help Scout entities are untouched — relationships are edge rows in
//! `custom_object_links`, with target existence validated at write time (a
//! 422 in the route, never an FK 500).
//!
//! ## Column mapping (documented adaptation)
//!
//! The reference's `custom_object_fields.key` is the port's legacy `name`
//! column (M026); the reference's `custom_objects.properties` is the port's
//! legacy `data_json` column (M036). Both are exposed under their reference
//! names on the wire. New writes populate the legacy columns so older
//! readers (demo seeding, e2e snapshots) keep working.

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

use crate::error::{Error, Result};

/// The closed target-kind vocabulary (reference `CUSTOM_OBJECT_LINK_TARGETS`).
pub const LINK_TARGET_KINDS: [&str; 6] = [
    "customer",
    "organization",
    "conversation",
    "known_issue",
    "incident",
    "campaign",
];

/// The closed field-type vocabulary (reference `CUSTOM_FIELD_TYPES`).
pub const FIELD_TYPES: [&str; 6] = ["text", "long_text", "number", "date", "boolean", "select"];

/// Reference `FIELD_KEY_REGEX = /^[a-z][a-z0-9_]{0,58}$/`.
fn valid_field_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    let rest: usize = chars.count();
    rest <= 58
        && key[1..]
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Reference slug generation: lowercase, non-alphanumerics collapse to `-`,
/// trimmed of leading/trailing dashes, `type` when everything strips away.
pub fn slugify(name: &str) -> String {
    let lowered = name.trim().to_lowercase();
    let mut slug = String::new();
    let mut pending_dash = false;
    for c in lowered.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c);
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        "type".to_string()
    } else {
        slug
    }
}

/// One parsed field definition (reference `customFieldDefSchema`).
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub required: bool,
    pub options: Option<Vec<String>>,
}

/// A raw field as it arrives in a create/patch request body, already
/// zod-checked by the route (400 on shape failures) but not yet
/// store-validated (field-key rules, select options, type immutability).
#[derive(Debug, Clone)]
pub struct FieldInput {
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub required: bool,
    pub options: Option<Vec<String>>,
}

impl FieldInput {
    /// Store-level validation shared by create and patch: the key must be a
    /// lowercase snake_case identifier and select fields need options.
    fn validate(&self) -> Result<()> {
        if !valid_field_key(&self.key) {
            return Err(Error::Validation(format!(
                "Field key \"{}\" must be lowercase snake_case",
                self.key
            )));
        }
        if self.field_type == "select" && self.options.as_ref().is_none_or(Vec::is_empty) {
            return Err(Error::Validation(format!(
                "Select field \"{}\" needs at least one option",
                self.key
            )));
        }
        Ok(())
    }
}

/// One link as it arrives in a request body (target existence is validated
/// by the store, not the route).
#[derive(Debug, Clone)]
pub struct LinkInput {
    pub target_kind: String,
    pub target_local_id: i64,
    pub note: Option<String>,
}

// ─── Types ─────────────────────────────────────────────────────────────────

/// Reference `createType`: slug from the name, duplicate-slug refusal, field
/// rules enforced inside one transaction. Returns the new type's id + slug.
pub fn create_type(
    conn: &Connection,
    name: &str,
    description: Option<&str>,
    fields: &[FieldInput],
) -> Result<(i64, String)> {
    let slug = slugify(name);
    // Reference: the slug must survive the identifier shape (letters, digits,
    // dashes) — refuse pathological names instead of mangling them.
    let slug_ok = {
        let mut chars = slug.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && slug.len() <= 59
    };
    if !slug_ok {
        return Err(Error::Validation(
            "Type name must start with a letter and contain only letters, digits and spaces"
                .to_string(),
        ));
    }
    let dup: Option<i64> = conn
        .query_row(
            "SELECT id FROM custom_object_types WHERE slug = ?1 AND deleted_at IS NULL",
            params![slug],
            |r| r.get(0),
        )
        .ok();
    if dup.is_some() {
        return Err(Error::Validation(format!(
            "A type with slug \"{slug}\" already exists"
        )));
    }
    let mut keys = std::collections::HashSet::new();
    for f in fields {
        f.validate()?;
        if !keys.insert(f.key.clone()) {
            return Err(Error::Validation(format!(
                "Duplicate field key \"{}\"",
                f.key
            )));
        }
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO custom_object_types (name, slug, description) VALUES (?1, ?2, ?3)",
        params![name.trim(), slug, description],
    )?;
    let type_id = tx.last_insert_rowid();
    for (order, f) in fields.iter().enumerate() {
        insert_field(&tx, type_id, f, order)?;
    }
    tx.commit()?;
    Ok((type_id, slug))
}

/// Reference `patchType`: label/option/required changes and NEW fields only —
/// re-typing a field that already stored values would corrupt data.
///
/// `description` is two-level to mirror the reference's nullish semantics:
/// outer `None` = absent from the request (untouched unless a name is also
/// present — the reference's `input.description ?? null` quirk), outer
/// `Some(None)` = explicit null (cleared), `Some(Some(_))` = a new value.
pub fn patch_type(
    conn: &Connection,
    type_id: i64,
    name: Option<&str>,
    description: Option<Option<&str>>,
    fields: Option<&[FieldInput]>,
) -> Result<()> {
    let Some(existing) = get_type(conn, type_id)? else {
        return Err(Error::Validation("Type not found".to_string()));
    };
    if let Some(fields) = fields {
        let current: std::collections::HashMap<String, FieldDef> = existing
            .fields
            .iter()
            .map(|f| (f.key.clone(), f.clone()))
            .collect();
        let mut keys = std::collections::HashSet::new();
        for f in fields {
            f.validate()?;
            if let Some(c) = current.get(&f.key) {
                if c.field_type != f.field_type {
                    return Err(Error::Validation(format!(
                        "Field \"{}\" already exists as {}; field types are immutable (existing objects store values)",
                        f.key, c.field_type
                    )));
                }
            }
            if !keys.insert(f.key.clone()) {
                return Err(Error::Validation(format!(
                    "Duplicate field key \"{}\"",
                    f.key
                )));
            }
        }
    }
    let tx = conn.unchecked_transaction()?;
    if name.is_some() || description.is_some() {
        // Reference: name COALESCEs to the existing value; description is
        // always rewritten in this branch (explicit null — or the reference's
        // `?? null` on an absent field alongside a name — clears it).
        tx.execute(
            "UPDATE custom_object_types
                SET name = COALESCE(?1, name), description = ?2,
                    updated_at = datetime('now')
              WHERE id = ?3",
            params![name, description.flatten(), type_id],
        )?;
    }
    if let Some(fields) = fields {
        // Replace the field set. Existing objects keep their stored JSON;
        // re-validation happens on the next edit (reference comment).
        tx.execute(
            "DELETE FROM custom_object_fields WHERE type_id = ?1",
            params![type_id],
        )?;
        for (order, f) in fields.iter().enumerate() {
            insert_field(&tx, type_id, f, order)?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Insert one field row — the legacy `name` column carries the reference's
/// `key` (documented adaptation) so older readers keep working.
fn insert_field(conn: &Connection, type_id: i64, f: &FieldInput, order: usize) -> Result<()> {
    let options = f
        .options
        .as_ref()
        .map(|o| serde_json::to_string(o).unwrap_or_else(|_| "[]".to_string()));
    conn.execute(
        "INSERT INTO custom_object_fields
            (type_id, name, label, field_type, required, options_json, sort_order)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            type_id,
            f.key,
            f.label,
            f.field_type,
            i64::from(f.required),
            options,
            (order + 1) as i64
        ],
    )?;
    Ok(())
}

/// The parsed detail of one type: the row plus its fields (in sort order)
/// and live object count. Reference `CustomObjectTypeDetail`.
#[derive(Debug, Clone)]
pub struct TypeDetail {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub provenance: String,
    pub fields: Vec<FieldDef>,
    pub object_count: i64,
}

impl TypeDetail {
    /// The wire shape (reference listTypes/getType rows).
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "slug": self.slug,
            "description": self.description,
            "deleted_at": null,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "provenance": self.provenance,
            "fields": self.fields.iter().map(field_def_json).collect::<Vec<_>>(),
            "object_count": self.object_count,
        })
    }
}

fn field_def_json(f: &FieldDef) -> Value {
    json!({
        "key": f.key,
        "label": f.label,
        "fieldType": f.field_type,
        "required": f.required,
        "options": f.options,
    })
}

fn load_fields(conn: &Connection, type_id: i64) -> Result<Vec<FieldDef>> {
    let mut stmt = conn.prepare(
        "SELECT name, label, field_type, required, options_json
           FROM custom_object_fields
          WHERE type_id = ?1
          ORDER BY sort_order, id",
    )?;
    let fields = stmt
        .query_map(params![type_id], |r| {
            let key: String = r.get(0)?;
            let label: String = r.get(1)?;
            let field_type: String = r.get(2)?;
            let required = r.get::<_, i64>(3)? != 0;
            let options: Option<String> = r.get(4)?;
            let options = options.and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok());
            Ok(FieldDef {
                key,
                label,
                field_type,
                required,
                options,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(fields)
}

/// One raw `custom_object_types` row before detail assembly (the field
/// list + object count join in [`row_to_type_detail`]).
struct TypeRow {
    id: i64,
    name: String,
    slug: String,
    description: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    provenance: String,
}

fn row_to_type_detail(conn: &Connection, row: TypeRow) -> Result<TypeDetail> {
    let fields = load_fields(conn, row.id)?;
    let object_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM custom_objects WHERE type_id = ?1 AND deleted_at IS NULL",
        params![row.id],
        |r| r.get(0),
    )?;
    Ok(TypeDetail {
        id: row.id,
        name: row.name,
        slug: row.slug,
        description: row.description,
        created_at: row.created_at,
        updated_at: row.updated_at,
        provenance: row.provenance,
        fields,
        object_count,
    })
}

/// Reference `listTypes`: every non-deleted type with fields + counts,
/// ordered by name.
pub fn list_types(conn: &Connection) -> Result<Vec<TypeDetail>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, slug, description, created_at, updated_at, provenance
           FROM custom_object_types
          WHERE deleted_at IS NULL
          ORDER BY name",
    )?;
    let rows: Vec<TypeRow> = stmt
        .query_map([], |r| {
            Ok(TypeRow {
                id: r.get(0)?,
                name: r.get(1)?,
                slug: r.get(2)?,
                description: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
                provenance: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|row| row_to_type_detail(conn, row))
        .collect()
}

/// Reference `getType`: one non-deleted type, or None when unknown.
pub fn get_type(conn: &Connection, type_id: i64) -> Result<Option<TypeDetail>> {
    let row = conn
        .query_row(
            "SELECT id, name, slug, description, created_at, updated_at, provenance
               FROM custom_object_types
              WHERE id = ?1 AND deleted_at IS NULL",
            params![type_id],
            |r| {
                Ok(TypeRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    slug: r.get(2)?,
                    description: r.get(3)?,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                    provenance: r.get(6)?,
                })
            },
        )
        .ok();
    match row {
        Some(row) => row_to_type_detail(conn, row).map(Some),
        None => Ok(None),
    }
}

/// Reference `getTypeBySlug`.
pub fn get_type_by_slug(conn: &Connection, slug: &str) -> Result<Option<TypeDetail>> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM custom_object_types WHERE slug = ?1 AND deleted_at IS NULL",
            params![slug],
            |r| r.get(0),
        )
        .ok();
    match id {
        Some(id) => get_type(conn, id),
        None => Ok(None),
    }
}

/// Reference `deleteType`: refuses while objects exist (the route answers
/// 409 with the message). Returns false when the type is unknown.
pub fn delete_type(conn: &Connection, type_id: i64) -> Result<bool> {
    let Some(existing) = get_type(conn, type_id)? else {
        return Ok(false);
    };
    if existing.object_count > 0 {
        return Err(Error::Validation(format!(
            "Type \"{}\" still has {} object(s); delete or move them first",
            existing.name, existing.object_count
        )));
    }
    conn.execute(
        "DELETE FROM custom_object_types WHERE id = ?1",
        params![type_id],
    )?;
    Ok(true)
}

// ─── Dynamic value validation (the reference's `buildSchema`) ───────────────

/// Validate `input` against the type's field definitions — the port of the
/// reference's dynamic Zod schema:
/// - strict: unrecognized keys are refused (user data never widens the model)
/// - `text` ≤ 300 chars, `long_text` ≤ 5000, `number` finite, `date` ISO
///   (YYYY-MM-DD with an optional time), `boolean`, `select` one of options
/// - required fields must be present and non-null; optional fields may be
///   absent or null (both strip to nothing, mirroring `.nullish()`)
///
/// Returns the clean (null-stripped) property map.
pub fn validate_properties(
    fields: &[FieldDef],
    input: &Map<String, Value>,
) -> std::result::Result<Map<String, Value>, String> {
    let mut clean = Map::new();
    let defs: std::collections::HashMap<&str, &FieldDef> =
        fields.iter().map(|f| (f.key.as_str(), f)).collect();
    for (key, value) in input {
        let Some(def) = defs.get(key.as_str()) else {
            return Err(format!("Unrecognized key: \"{key}\""));
        };
        let value = match value {
            Value::Null => {
                if def.required {
                    return Err(format!("{key}: Required"));
                }
                continue;
            }
            v => v,
        };
        let ok = match def.field_type.as_str() {
            "text" => value.as_str().is_some_and(|s| s.chars().count() <= 300),
            "long_text" => value.as_str().is_some_and(|s| s.chars().count() <= 5000),
            "number" => value.as_f64().is_some_and(f64::is_finite),
            "date" => value.as_str().is_some_and(valid_iso_date),
            "boolean" => value.is_boolean(),
            "select" => value.as_str().is_some_and(|s| {
                def.options
                    .as_ref()
                    .is_some_and(|o| o.iter().any(|opt| opt == s))
            }),
            other => return Err(format!("{key}: Unknown field type {other}")),
        };
        if !ok {
            return Err(format!("{key}: Invalid {}", def.field_type));
        }
        clean.insert(key.clone(), value.clone());
    }
    for def in fields {
        if def.required && !clean.contains_key(&def.key) {
            return Err(format!("{}: Required", def.key));
        }
    }
    Ok(clean)
}

/// Reference date regex: `^\d{4}-\d{2}-\d{2}(T[\d:.]+(Z|[+-]\d{2}:?\d{2})?)?$`.
fn valid_iso_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return false;
    }
    let date = &s[..10];
    let mut parts = date.split('-');
    let (y, m, d) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    if parts.next().is_some() {
        return false;
    }
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return false;
    }
    if !y.bytes().all(|b| b.is_ascii_digit())
        || !m.bytes().all(|b| b.is_ascii_digit())
        || !d.bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    let m_v: u32 = m.parse().unwrap_or(0);
    let d_v: u32 = d.parse().unwrap_or(0);
    if !(1..=12).contains(&m_v) || !(1..=31).contains(&d_v) {
        return false;
    }
    if s.len() == 10 {
        return true;
    }
    let rest = &s[10..];
    let rest = match rest.strip_prefix('T') {
        Some(r) => r,
        None => return false,
    };
    // Time part: digits/colons/dots, then Z or ±HH:MM / ±HHMM at the end.
    // (The time body admits no +, - or Z, so the LAST zone char is the
    // boundary — `str::rfind` with a char pattern.)
    if rest.is_empty() {
        return false;
    }
    let (time, zone) = match rest.rfind(['Z', '+', '-']) {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if time.is_empty()
        || !time
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b':' || b == b'.')
    {
        return false;
    }
    if zone == "Z" {
        return true;
    }
    if zone.is_empty() {
        return false;
    }
    let zone_body = &zone[1..];
    let digits = zone_body.replace(':', "");
    (zone_body.len() == 5 && zone_body.as_bytes()[2] == b':')
        || (digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Reference `buildSearchText`: title + every scalar property value, capped
/// at 5000 chars.
fn build_search_text(title: &str, properties: &Map<String, Value>) -> String {
    let mut parts = vec![title.to_string()];
    for v in properties.values() {
        match v {
            Value::String(s) => parts.push(s.clone()),
            Value::Number(n) => parts.push(n.to_string()),
            Value::Bool(b) => parts.push(b.to_string()),
            _ => {}
        }
    }
    let joined = parts.join(" ");
    joined.chars().take(5000).collect()
}

// ─── Objects ───────────────────────────────────────────────────────────────

/// Reference `createObject`: validate against the type's live field defs,
/// index into FTS, create links (target existence validated). Returns the
/// new object's id.
pub fn create_object(
    conn: &Connection,
    type_id: i64,
    title: &str,
    properties: &Map<String, Value>,
    links: &[LinkInput],
) -> Result<i64> {
    let type_detail =
        get_type(conn, type_id)?.ok_or_else(|| Error::Validation("Type not found".to_string()))?;
    let clean = validate_properties(&type_detail.fields, properties).map_err(Error::Validation)?;
    let search_text = build_search_text(title, &clean);
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO custom_objects (type_id, title, data_json, search_text)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            type_id,
            title,
            serde_json::to_string(&Value::Object(clean)).unwrap_or_else(|_| "{}".into()),
            search_text
        ],
    )?;
    let object_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO fts_custom_objects (title, search_text, object_id) VALUES (?1, ?2, ?3)",
        params![title, search_text, object_id],
    )?;
    for link in links {
        insert_link(&tx, object_id, link)?;
    }
    tx.commit()?;
    Ok(object_id)
}

/// Reference `patchObject`: partial update — properties MERGE with the
/// existing values before re-validation, links replace the set when
/// provided. Returns the updated detail, or None when the object is
/// unknown/deleted.
pub fn patch_object(
    conn: &Connection,
    object_id: i64,
    title: Option<&str>,
    properties: Option<&Map<String, Value>>,
    links: Option<&[LinkInput]>,
) -> Result<Option<Value>> {
    let Some(existing) = get_object(conn, object_id)? else {
        return Ok(None);
    };
    let type_detail = get_type(conn, existing["type_id"].as_i64().unwrap_or(0))?
        .ok_or_else(|| Error::Validation("Type not found".to_string()))?;
    let clean = match properties {
        Some(input) => {
            let mut merged = existing["properties"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for (k, v) in input {
                merged.insert(k.clone(), v.clone());
            }
            validate_properties(&type_detail.fields, &merged).map_err(Error::Validation)?
        }
        None => existing["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    let title = title
        .map(str::to_string)
        .unwrap_or_else(|| existing["title"].as_str().unwrap_or("").to_string());
    let search_text = build_search_text(&title, &clean);
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE custom_objects
            SET title = ?1, data_json = ?2, search_text = ?3, updated_at = datetime('now')
          WHERE id = ?4",
        params![
            title,
            serde_json::to_string(&Value::Object(clean)).unwrap_or_else(|_| "{}".into()),
            search_text,
            object_id
        ],
    )?;
    tx.execute(
        "DELETE FROM fts_custom_objects WHERE object_id = ?1",
        params![object_id],
    )?;
    tx.execute(
        "INSERT INTO fts_custom_objects (title, search_text, object_id) VALUES (?1, ?2, ?3)",
        params![title, search_text, object_id],
    )?;
    if let Some(links) = links {
        tx.execute(
            "DELETE FROM custom_object_links WHERE object_id = ?1",
            params![object_id],
        )?;
        for link in links {
            insert_link(&tx, object_id, link)?;
        }
    }
    tx.commit()?;
    get_object(conn, object_id)
}

/// Reference `deleteObject`: soft delete + FTS removal. False when unknown.
pub fn delete_object(conn: &Connection, object_id: i64) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE custom_objects SET deleted_at = datetime('now')
          WHERE id = ?1 AND deleted_at IS NULL",
        params![object_id],
    )?;
    tx.execute(
        "DELETE FROM fts_custom_objects WHERE object_id = ?1",
        params![object_id],
    )?;
    tx.commit()?;
    Ok(changed > 0)
}

/// Parse the stored `data_json` column defensively (malformed legacy values
/// degrade to an empty object, never a 500 — reference `JSON.parse` guard).
fn parse_properties(stored: Option<String>) -> Map<String, Value> {
    stored
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Reference `getObject`: the object + type names + links (with target
/// labels). None when unknown or soft-deleted.
pub fn get_object(conn: &Connection, object_id: i64) -> Result<Option<Value>> {
    let row = conn
        .query_row(
            "SELECT o.id, o.type_id, o.title, o.data_json, o.created_at, o.updated_at,
                    o.provenance, t.name, t.slug
               FROM custom_objects o
               JOIN custom_object_types t ON t.id = o.type_id
              WHERE o.id = ?1 AND o.deleted_at IS NULL",
            params![object_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                ))
            },
        )
        .ok();
    let Some((
        id,
        type_id,
        title,
        data_json,
        created_at,
        updated_at,
        provenance,
        type_name,
        type_slug,
    )) = row
    else {
        return Ok(None);
    };
    let properties = parse_properties(data_json);
    let links = list_links(conn, id)?;
    Ok(Some(json!({
        "id": id,
        "type_id": type_id,
        "title": title,
        "properties": properties,
        "created_at": created_at,
        "updated_at": updated_at,
        "provenance": provenance,
        "type_name": type_name,
        "type_slug": type_slug,
        "links": links,
    })))
}

/// The link rows for one object with resolved target labels (reference
/// `getObject` link projection).
fn list_links(conn: &Connection, object_id: i64) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT target_kind, target_local_id, note, linked_at
           FROM custom_object_links
          WHERE object_id = ?1
          ORDER BY id",
    )?;
    let rows: Vec<(String, i64, Option<String>, Option<String>)> = stmt
        .query_map(params![object_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(kind, id, note, linked_at)| {
            Ok(json!({
                "target_kind": kind,
                "target_local_id": id,
                "target_label": link_label(conn, &kind, id)?,
                "note": note,
                "linked_at": linked_at,
            }))
        })
        .collect()
}

/// Reference `listObjects`: type filter, LIKE search over title/search_text,
/// LIMIT/OFFSET pagination and the matching total.
pub fn list_objects(
    conn: &Connection,
    type_id: Option<i64>,
    query: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<(Vec<Value>, i64)> {
    let limit = limit.clamp(1, 200) as i64;
    let offset = offset as i64;
    let trimmed = query.map(str::trim).filter(|q| !q.is_empty());
    let mut where_sql = String::from("o.deleted_at IS NULL");
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(tid) = type_id.filter(|v| *v > 0) {
        where_sql.push_str(" AND o.type_id = ?");
        binds.push(Box::new(tid));
    }
    let like = trimmed.map(|q| {
        format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    if let Some(l) = &like {
        where_sql.push_str(" AND (o.title LIKE ? ESCAPE '\\' OR o.search_text LIKE ? ESCAPE '\\')");
        binds.push(Box::new(l.clone()));
        binds.push(Box::new(l.clone()));
    }
    let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let select_sql = format!(
        "SELECT o.id, o.type_id, o.title, o.data_json, o.created_at, o.updated_at, o.provenance
           FROM custom_objects o
          WHERE {where_sql}
          ORDER BY o.updated_at DESC, o.id DESC
          LIMIT {limit} OFFSET {offset}"
    );
    let to_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<Value> {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "type_id": r.get::<_, i64>(1)?,
            "title": r.get::<_, String>(2)?,
            "properties": parse_properties(r.get(3)?),
            "created_at": r.get::<_, Option<String>>(4)?,
            "updated_at": r.get::<_, Option<String>>(5)?,
            "provenance": r.get::<_, String>(6)?,
        }))
    };
    let objects: Vec<Value> = conn
        .prepare(&select_sql)?
        .query_map(rusqlite::params_from_iter(refs.iter()), to_row)?
        .filter_map(|r| r.ok())
        .collect();
    let count_sql = format!("SELECT COUNT(*) FROM custom_objects o WHERE {where_sql}");
    let total: i64 = conn.query_row(&count_sql, rusqlite::params_from_iter(refs.iter()), |r| {
        r.get(0)
    })?;
    Ok((objects, total))
}

/// Reference `searchObjects`: FTS search with quoted-prefix terms (the
/// knowledge-search safety pattern — hostile characters are neutralized).
pub fn search_objects(
    conn: &Connection,
    query: &str,
    type_id: Option<i64>,
    limit: u32,
) -> Result<Vec<Value>> {
    let tokens: String = query
        .replace(['"', '*', '(', ')'], " ")
        .split_whitespace()
        .filter(|t| t.chars().count() > 1)
        .take(6)
        .map(|t| format!("\"{}\"*", t.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ");
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.clamp(1, 50) as i64;
    let sql = match type_id {
        Some(tid) if tid > 0 => format!(
            "SELECT o.id, o.type_id, o.title, o.data_json, o.created_at, o.updated_at, o.provenance,
                    t.name AS type_name,
                    snippet(fts_custom_objects, 1, '[', ']', '...', 12) AS snippet
               FROM fts_custom_objects f
               JOIN custom_objects o ON o.id = f.object_id AND o.deleted_at IS NULL
               JOIN custom_object_types t ON t.id = o.type_id
              WHERE fts_custom_objects MATCH ?1 AND o.type_id = {tid}
              ORDER BY rank LIMIT {limit}"
        ),
        _ => format!(
            "SELECT o.id, o.type_id, o.title, o.data_json, o.created_at, o.updated_at, o.provenance,
                    t.name AS type_name,
                    snippet(fts_custom_objects, 1, '[', ']', '...', 12) AS snippet
               FROM fts_custom_objects f
               JOIN custom_objects o ON o.id = f.object_id AND o.deleted_at IS NULL
               JOIN custom_object_types t ON t.id = o.type_id
              WHERE fts_custom_objects MATCH ?1
              ORDER BY rank LIMIT {limit}"
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<Value> = stmt
        .query_map(params![tokens], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "type_id": r.get::<_, i64>(1)?,
                "title": r.get::<_, String>(2)?,
                "properties": parse_properties(r.get(3)?),
                "created_at": r.get::<_, Option<String>>(4)?,
                "updated_at": r.get::<_, Option<String>>(5)?,
                "provenance": r.get::<_, String>(6)?,
                "type_name": r.get::<_, String>(7)?,
                "snippet": r.get::<_, String>(8)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// ─── Links ─────────────────────────────────────────────────────────────────

/// Reference `insertLink`: validate target existence first (a 422 in the
/// route, never an FK 500), then INSERT OR IGNORE.
fn insert_link(conn: &Connection, object_id: i64, link: &LinkInput) -> Result<()> {
    if !link_target_exists(conn, &link.target_kind, link.target_local_id)? {
        return Err(Error::Validation(format!(
            "{} #{} does not exist",
            link.target_kind, link.target_local_id
        )));
    }
    conn.execute(
        "INSERT OR IGNORE INTO custom_object_links
            (object_id, target_kind, target_local_id, note)
         VALUES (?1, ?2, ?3, ?4)",
        params![object_id, link.target_kind, link.target_local_id, link.note],
    )?;
    Ok(())
}

/// Reference `addLink`: the object must exist, then the link inserts.
pub fn add_link(conn: &Connection, object_id: i64, link: &LinkInput) -> Result<()> {
    if get_object(conn, object_id)?.is_none() {
        return Err(Error::Validation("Object not found".to_string()));
    }
    insert_link(conn, object_id, link)
}

/// Reference `removeLink`: true when a row was actually removed.
pub fn remove_link(
    conn: &Connection,
    object_id: i64,
    target_kind: &str,
    target_local_id: i64,
) -> Result<bool> {
    let removed = conn.execute(
        "DELETE FROM custom_object_links
          WHERE object_id = ?1 AND target_kind = ?2 AND target_local_id = ?3",
        params![object_id, target_kind, target_local_id],
    )?;
    Ok(removed > 0)
}

/// Reference `objectsForTarget`: reverse lookup — objects linked to a
/// target, newest-updated first, capped at 100.
pub fn objects_for_target(
    conn: &Connection,
    target_kind: &str,
    target_local_id: i64,
    limit: u32,
) -> Result<Vec<Value>> {
    let limit = limit.clamp(1, 100) as i64;
    let mut stmt = conn.prepare(
        "SELECT o.id, o.type_id, o.title, o.data_json, o.created_at, o.updated_at, o.provenance,
                t.name AS type_name
           FROM custom_object_links l
           JOIN custom_objects o ON o.id = l.object_id AND o.deleted_at IS NULL
           JOIN custom_object_types t ON t.id = o.type_id
          WHERE l.target_kind = ?1 AND l.target_local_id = ?2
          ORDER BY o.updated_at DESC
          LIMIT ?3",
    )?;
    let rows: Vec<Value> = stmt
        .query_map(params![target_kind, target_local_id, limit], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "type_id": r.get::<_, i64>(1)?,
                "title": r.get::<_, String>(2)?,
                "properties": parse_properties(r.get(3)?),
                "created_at": r.get::<_, Option<String>>(4)?,
                "updated_at": r.get::<_, Option<String>>(5)?,
                "provenance": r.get::<_, String>(6)?,
                "type_name": r.get::<_, String>(7)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Reference `linkTargetExists`: closed table vocabulary per kind, with the
/// soft-delete filter where the mirror keeps one.
fn link_target_exists(conn: &Connection, kind: &str, id: i64) -> Result<bool> {
    let sql = match kind {
        "customer" => "SELECT 1 FROM customers WHERE id = ?1 AND deleted_at IS NULL",
        "organization" => "SELECT 1 FROM organizations WHERE id = ?1 AND deleted_at IS NULL",
        "conversation" => "SELECT 1 FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
        "known_issue" => "SELECT 1 FROM known_issues WHERE id = ?1",
        "incident" => "SELECT 1 FROM incidents WHERE id = ?1",
        "campaign" => "SELECT 1 FROM outreach_campaigns WHERE id = ?1",
        _ => return Ok(false),
    };
    Ok(conn.query_row(sql, params![id], |_| Ok(())).is_ok())
}

/// Reference `linkLabel`: a human label for the link target, with a stable
/// `kind #id` fallback when the row vanished.
fn link_label(conn: &Connection, kind: &str, id: i64) -> Result<Option<String>> {
    let label: Option<String> = match kind {
        "customer" => conn
            .query_row(
                "SELECT TRIM(COALESCE(first_name, '') || ' ' || COALESCE(last_name, ''))
                   FROM customers WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
            .filter(|s: &String| !s.trim().is_empty())
            .or_else(|| Some(format!("customer #{id}"))),
        "organization" => conn
            .query_row(
                "SELECT name FROM organizations WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .ok()
            .flatten()
            .or_else(|| Some(format!("organization #{id}"))),
        "conversation" => conn
            .query_row(
                "SELECT number FROM conversations WHERE id = ?1",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .map(|n| format!("#{n}"))
            .or_else(|| Some(format!("#{id}"))),
        "known_issue" => conn
            .query_row(
                "SELECT name FROM known_issues WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .ok()
            .flatten()
            .or_else(|| Some(format!("known issue #{id}"))),
        "incident" => conn
            .query_row(
                "SELECT code || ': ' || title FROM incidents WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .ok()
            .flatten()
            .or_else(|| Some(format!("incident #{id}"))),
        "campaign" => conn
            .query_row(
                "SELECT name FROM outreach_campaigns WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .ok()
            .flatten()
            .or_else(|| Some(format!("campaign #{id}"))),
        _ => None,
    };
    Ok(label)
}

// ─── Reporting ─────────────────────────────────────────────────────────────

/// Reference `report`: per-type object counts, per-target-kind link counts
/// and the totals.
pub fn report(conn: &Connection) -> Result<Value> {
    let types = list_types(conn)?;
    let mut out = Vec::with_capacity(types.len());
    for t in &types {
        let mut stmt = conn.prepare(
            "SELECT l.target_kind, COUNT(*)
               FROM custom_object_links l
               JOIN custom_objects o ON o.id = l.object_id AND o.deleted_at IS NULL
              WHERE o.type_id = ?1
              GROUP BY l.target_kind",
        )?;
        let rows: Vec<(String, i64)> = stmt
            .query_map(params![t.id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        let mut link_counts = Map::new();
        for (kind, n) in rows {
            link_counts.insert(kind, json!(n));
        }
        out.push(json!({
            "id": t.id,
            "name": t.name,
            "slug": t.slug,
            "object_count": t.object_count,
            "link_counts": Value::Object(link_counts),
        }));
    }
    let total_objects: i64 = conn.query_row(
        "SELECT COUNT(*) FROM custom_objects WHERE deleted_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    let total_links: i64 =
        conn.query_row("SELECT COUNT(*) FROM custom_object_links", [], |r| r.get(0))?;
    Ok(json!({
        "types": out,
        "total_objects": total_objects,
        "total_links": total_links,
    }))
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn text_field(key: &str, label: &str) -> FieldInput {
        FieldInput {
            key: key.to_string(),
            label: label.to_string(),
            field_type: "text".to_string(),
            required: false,
            options: None,
        }
    }

    fn select_field(key: &str, label: &str, options: &[&str]) -> FieldInput {
        FieldInput {
            key: key.to_string(),
            label: label.to_string(),
            field_type: "select".to_string(),
            required: false,
            options: Some(options.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn props(pairs: &[(&str, Value)]) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v.clone());
        }
        m
    }

    #[test]
    fn slugify_matches_reference() {
        assert_eq!(slugify("Account"), "account");
        assert_eq!(slugify("  My Cool Type  "), "my-cool-type");
        assert_eq!(slugify("---"), "type");
        assert_eq!(slugify("Ver 2.0!"), "ver-2-0");
    }

    #[test]
    fn create_type_round_trips_with_fields_and_counts() {
        let conn = fresh_db();
        let (id, slug) = create_type(
            &conn,
            "Account",
            Some("Commercial record"),
            &[
                select_field("plan_tier", "Plan tier", &["free", "growth", "enterprise"]),
                FieldInput {
                    key: "mrr".into(),
                    label: "MRR".into(),
                    field_type: "number".into(),
                    required: false,
                    options: None,
                },
                FieldInput {
                    key: "renewal_date".into(),
                    label: "Renewal".into(),
                    field_type: "date".into(),
                    required: false,
                    options: None,
                },
                FieldInput {
                    key: "vip".into(),
                    label: "VIP".into(),
                    field_type: "boolean".into(),
                    required: false,
                    options: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(slug, "account");
        let t = get_type(&conn, id).unwrap().unwrap();
        assert_eq!(t.fields.len(), 4);
        assert_eq!(
            t.fields[0].options.as_deref(),
            Some(
                &[
                    "free".to_string(),
                    "growth".to_string(),
                    "enterprise".to_string()
                ][..]
            )
        );
        assert_eq!(get_type_by_slug(&conn, "account").unwrap().unwrap().id, id);
        assert_eq!(t.object_count, 0);
        assert_eq!(t.description.as_deref(), Some("Commercial record"));
    }

    #[test]
    fn create_type_refuses_duplicates_bad_keys_and_missing_options() {
        let conn = fresh_db();
        create_type(&conn, "Account", None, &[text_field("x", "X")]).unwrap();
        assert!(create_type(&conn, "Account", None, &[text_field("y", "Y")]).is_err());
        assert!(create_type(&conn, "Bad Keys", None, &[text_field("Not-Snake", "X")]).is_err());
        assert!(create_type(&conn, "No Options", None, &[select_field("tier", "T", &[])]).is_err());
        assert!(create_type(
            &conn,
            "Dup Keys",
            None,
            &[text_field("a", "A"), text_field("a", "A2")]
        )
        .is_err());
    }

    #[test]
    fn patch_type_allows_label_changes_but_not_retyping() {
        let conn = fresh_db();
        let (id, _) = create_type(
            &conn,
            "Mutable",
            None,
            &[FieldInput {
                key: "amount".into(),
                label: "Amount".into(),
                field_type: "number".into(),
                required: false,
                options: None,
            }],
        )
        .unwrap();
        patch_type(
            &conn,
            id,
            None,
            None,
            Some(&[FieldInput {
                key: "amount".into(),
                label: "Amount USD".into(),
                field_type: "number".into(),
                required: true,
                options: None,
            }]),
        )
        .unwrap();
        let t = get_type(&conn, id).unwrap().unwrap();
        assert_eq!(t.fields[0].label, "Amount USD");
        assert!(t.fields[0].required);
        let err = patch_type(
            &conn,
            id,
            None,
            None,
            Some(&[text_field("amount", "Amount")]),
        );
        assert!(err.is_err());
        assert!(err
            .unwrap_err()
            .to_string()
            .contains("field types are immutable"));
    }

    #[test]
    fn delete_type_refuses_while_objects_exist() {
        let conn = fresh_db();
        let (occupied, _) = create_type(
            &conn,
            "Occupied",
            None,
            &[FieldInput {
                key: "name".into(),
                label: "Name".into(),
                field_type: "text".into(),
                required: true,
                options: None,
            }],
        )
        .unwrap();
        create_object(&conn, occupied, "One", &props(&[("name", json!("x"))]), &[]).unwrap();
        let err = delete_type(&conn, occupied).unwrap_err();
        assert!(err.to_string().contains("still has 1 object"));
        let (empty, _) = create_type(&conn, "Empty", None, &[text_field("name", "Name")]).unwrap();
        assert!(delete_type(&conn, empty).unwrap());
        assert!(!delete_type(&conn, empty).unwrap());
    }

    #[test]
    fn create_object_validates_against_dynamic_schema() {
        let conn = fresh_db();
        let (tid, _) = create_type(
            &conn,
            "Sub",
            None,
            &[
                select_field("tier", "Tier", &["a", "b"]),
                FieldInput {
                    key: "seats".into(),
                    label: "Seats".into(),
                    field_type: "number".into(),
                    required: false,
                    options: None,
                },
                FieldInput {
                    key: "expires".into(),
                    label: "Expires".into(),
                    field_type: "date".into(),
                    required: false,
                    options: None,
                },
            ],
        )
        .unwrap();
        // Make tier required via a patch (the full field set — a patch
        // REPLACES the type's fields).
        patch_type(
            &conn,
            tid,
            None,
            None,
            Some(&[
                FieldInput {
                    key: "tier".into(),
                    label: "Tier".into(),
                    field_type: "select".into(),
                    required: true,
                    options: Some(vec!["a".into(), "b".into()]),
                },
                FieldInput {
                    key: "seats".into(),
                    label: "Seats".into(),
                    field_type: "number".into(),
                    required: false,
                    options: None,
                },
                FieldInput {
                    key: "expires".into(),
                    label: "Expires".into(),
                    field_type: "date".into(),
                    required: false,
                    options: None,
                },
            ]),
        )
        .unwrap();
        let ok = create_object(
            &conn,
            tid,
            "Acme sub",
            &props(&[
                ("tier", json!("a")),
                ("seats", json!(12)),
                ("expires", json!("2026-01-01")),
            ]),
            &[],
        )
        .unwrap();
        let obj = get_object(&conn, ok).unwrap().unwrap();
        assert_eq!(obj["properties"]["tier"], json!("a"));
        // Missing required.
        assert!(create_object(&conn, tid, "x", &props(&[]), &[]).is_err());
        // Bad enum.
        assert!(create_object(&conn, tid, "x", &props(&[("tier", json!("c"))]), &[]).is_err());
        // Wrong type.
        assert!(create_object(
            &conn,
            tid,
            "x",
            &props(&[("tier", json!("a")), ("seats", json!("many"))]),
            &[]
        )
        .is_err());
        // Bad date.
        assert!(create_object(
            &conn,
            tid,
            "x",
            &props(&[("tier", json!("a")), ("expires", json!("tomorrow"))]),
            &[]
        )
        .is_err());
        // Unknown key (strict).
        assert!(create_object(
            &conn,
            tid,
            "x",
            &props(&[("tier", json!("a")), ("rogue", json!(1))]),
            &[]
        )
        .is_err());
        // Unknown type.
        assert!(create_object(&conn, 999, "x", &props(&[]), &[]).is_err());
    }

    #[test]
    fn optional_null_strips_and_required_null_rejects() {
        let conn = fresh_db();
        let (tid, _) = create_type(
            &conn,
            "T",
            None,
            &[
                FieldInput {
                    key: "req".into(),
                    label: "Req".into(),
                    field_type: "text".into(),
                    required: true,
                    options: None,
                },
                FieldInput {
                    key: "opt".into(),
                    label: "Opt".into(),
                    field_type: "number".into(),
                    required: false,
                    options: None,
                },
            ],
        )
        .unwrap();
        let id = create_object(
            &conn,
            tid,
            "o",
            &props(&[("req", json!("v")), ("opt", Value::Null)]),
            &[],
        )
        .unwrap();
        let obj = get_object(&conn, id).unwrap().unwrap();
        assert!(obj["properties"].as_object().unwrap().get("opt").is_none());
        assert!(create_object(&conn, tid, "o", &props(&[("req", Value::Null)]), &[]).is_err());
    }

    #[test]
    fn links_validate_targets_and_reverse_lookup_works() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO customers (remote_id, first_name) VALUES (9501, 'Ada')",
            [],
        )
        .unwrap();
        let customer_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO organizations (remote_id, name) VALUES (7501, 'Compute Inc')",
            [],
        )
        .unwrap();
        let org_id = conn.last_insert_rowid();
        let (tid, _) = create_type(
            &conn,
            "Linked",
            None,
            &[FieldInput {
                key: "name".into(),
                label: "Name".into(),
                field_type: "text".into(),
                required: true,
                options: None,
            }],
        )
        .unwrap();
        let obj_id = create_object(
            &conn,
            tid,
            "Compute account",
            &props(&[("name", json!("compute"))]),
            &[
                LinkInput {
                    target_kind: "customer".into(),
                    target_local_id: customer_id,
                    note: None,
                },
                LinkInput {
                    target_kind: "organization".into(),
                    target_local_id: org_id,
                    note: Some("primary".into()),
                },
            ],
        )
        .unwrap();
        let obj = get_object(&conn, obj_id).unwrap().unwrap();
        assert_eq!(obj["links"].as_array().unwrap().len(), 2);
        let customer_link = obj["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["target_kind"] == json!("customer"))
            .unwrap();
        assert!(customer_link["target_label"]
            .as_str()
            .unwrap()
            .contains("Ada"));
        // Unknown target -> Validation (a 422 in the route, never an FK 500).
        assert!(create_object(
            &conn,
            tid,
            "x",
            &props(&[("name", json!("x"))]),
            &[LinkInput {
                target_kind: "customer".into(),
                target_local_id: 999,
                note: None,
            }]
        )
        .is_err());
        // Reverse lookup.
        let for_customer = objects_for_target(&conn, "customer", customer_id, 50).unwrap();
        assert!(for_customer
            .iter()
            .any(|o| o["title"] == json!("Compute account")));
        // Remove + verify.
        assert!(remove_link(&conn, obj_id, "customer", customer_id).unwrap());
        assert!(!remove_link(&conn, obj_id, "customer", customer_id).unwrap());
        assert!(objects_for_target(&conn, "customer", customer_id, 50)
            .unwrap()
            .is_empty());
        // addLink on a missing object.
        assert!(add_link(
            &conn,
            999,
            &LinkInput {
                target_kind: "customer".into(),
                target_local_id: customer_id,
                note: None
            }
        )
        .is_err());
    }

    #[test]
    fn fts_indexing_and_search_work() {
        let conn = fresh_db();
        let (tid, _) =
            create_type(&conn, "Searchable", None, &[text_field("note", "Note")]).unwrap();
        create_object(
            &conn,
            tid,
            "Alpha deployment",
            &props(&[("note", json!("production cluster frankfurt"))]),
            &[],
        )
        .unwrap();
        create_object(
            &conn,
            tid,
            "Beta deployment",
            &props(&[("note", json!("staging cluster berlin"))]),
            &[],
        )
        .unwrap();
        let hits = search_objects(&conn, "frankfurt production", None, 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0]["title"], json!("Alpha deployment"));
        assert_eq!(hits[0]["type_name"], json!("Searchable"));
        assert!(hits[0]["snippet"].as_str().is_some());
        // Hostile query characters are neutralized (no panic, no error).
        assert!(search_objects(&conn, "\" * ( ) OR DROP", None, 5).is_ok());
    }

    #[test]
    fn soft_delete_removes_fts_but_keeps_rows() {
        let conn = fresh_db();
        let (tid, _) = create_type(
            &conn,
            "Deletable",
            None,
            &[FieldInput {
                key: "name".into(),
                label: "Name".into(),
                field_type: "text".into(),
                required: true,
                options: None,
            }],
        )
        .unwrap();
        let keep = create_object(
            &conn,
            tid,
            "Keep me",
            &props(&[("name", json!("keep"))]),
            &[],
        )
        .unwrap();
        let drop_id = create_object(
            &conn,
            tid,
            "Delete me searchable-token-xyz",
            &props(&[("name", json!("delete"))]),
            &[],
        )
        .unwrap();
        assert!(delete_object(&conn, drop_id).unwrap());
        assert!(get_object(&conn, drop_id).unwrap().is_none());
        assert_eq!(
            get_object(&conn, keep).unwrap().unwrap()["title"],
            json!("Keep me")
        );
        assert!(search_objects(&conn, "searchable-token-xyz", None, 5)
            .unwrap()
            .is_empty());
        // Idempotent on already-deleted.
        assert!(!delete_object(&conn, drop_id).unwrap());
    }

    #[test]
    fn patch_object_merges_properties_and_replaces_links() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO customers (remote_id, first_name) VALUES (1, 'Ada')",
            [],
        )
        .unwrap();
        let customer_id = conn.last_insert_rowid();
        let (tid, _) = create_type(
            &conn,
            "P",
            None,
            &[
                text_field("a", "A"),
                FieldInput {
                    key: "n".into(),
                    label: "N".into(),
                    field_type: "number".into(),
                    required: false,
                    options: None,
                },
            ],
        )
        .unwrap();
        let id = create_object(&conn, tid, "orig", &props(&[("a", json!("1"))]), &[]).unwrap();
        // Partial update: only n provided — a survives.
        let updated = patch_object(
            &conn,
            id,
            Some("renamed"),
            Some(&props(&[("n", json!(5))])),
            Some(&[LinkInput {
                target_kind: "customer".into(),
                target_local_id: customer_id,
                note: None,
            }]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(updated["title"], json!("renamed"));
        assert_eq!(updated["properties"]["a"], json!("1"));
        assert_eq!(updated["properties"]["n"], json!(5));
        assert_eq!(updated["links"].as_array().unwrap().len(), 1);
        // Title-only patch keeps properties + links.
        let again = patch_object(&conn, id, Some("again"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(again["properties"]["n"], json!(5));
        assert_eq!(again["links"].as_array().unwrap().len(), 1);
        // Unknown object -> None (the route answers 404).
        assert!(patch_object(&conn, 999, None, None, None)
            .unwrap()
            .is_none());
        // Invalid merge -> Validation (422).
        assert!(patch_object(
            &conn,
            id,
            None,
            Some(&props(&[("n", json!("not a number"))])),
            None
        )
        .is_err());
    }

    #[test]
    fn list_objects_filters_paginates_and_counts() {
        let conn = fresh_db();
        let (tid, _) = create_type(&conn, "List", None, &[text_field("k", "K")]).unwrap();
        for i in 0..5 {
            create_object(
                &conn,
                tid,
                &format!("obj-{i}"),
                &props(&[("k", json!(format!("value-{i}")))]),
                &[],
            )
            .unwrap();
        }
        let (all, total) = list_objects(&conn, Some(tid), None, 50, 0).unwrap();
        assert_eq!(total, 5);
        assert_eq!(all.len(), 5);
        let (page, total2) = list_objects(&conn, Some(tid), None, 2, 0).unwrap();
        assert_eq!(total2, 5);
        assert_eq!(page.len(), 2);
        let (none, total3) = list_objects(&conn, Some(999), None, 50, 0).unwrap();
        assert_eq!(total3, 0);
        assert!(none.is_empty());
        let (hits, _) = list_objects(&conn, None, Some("obj-3"), 50, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["title"], json!("obj-3"));
    }

    #[test]
    fn report_counts_objects_and_links() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO customers (remote_id, first_name) VALUES (1, 'Ada')",
            [],
        )
        .unwrap();
        let customer_id = conn.last_insert_rowid();
        let (tid, _) = create_type(
            &conn,
            "Reported",
            None,
            &[FieldInput {
                key: "name".into(),
                label: "Name".into(),
                field_type: "text".into(),
                required: true,
                options: None,
            }],
        )
        .unwrap();
        create_object(
            &conn,
            tid,
            "one",
            &props(&[("name", json!("x"))]),
            &[LinkInput {
                target_kind: "customer".into(),
                target_local_id: customer_id,
                note: None,
            }],
        )
        .unwrap();
        create_object(&conn, tid, "two", &props(&[("name", json!("y"))]), &[]).unwrap();
        let r = report(&conn).unwrap();
        assert_eq!(r["total_objects"], json!(2));
        assert_eq!(r["total_links"], json!(1));
        let entry = r["types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == json!("Reported"))
            .unwrap();
        assert_eq!(entry["object_count"], json!(2));
        assert_eq!(entry["link_counts"]["customer"], json!(1));
    }

    #[test]
    fn valid_iso_date_matrix() {
        assert!(valid_iso_date("2026-01-01"));
        assert!(valid_iso_date("2026-01-01T10:00:00Z"));
        assert!(valid_iso_date("2026-01-01T10:00:00.123Z"));
        assert!(valid_iso_date("2026-01-01T10:00:00+02:00"));
        assert!(valid_iso_date("2026-01-01T10:00:00+0200"));
        assert!(!valid_iso_date("tomorrow"));
        assert!(!valid_iso_date("2026-1-1"));
        assert!(!valid_iso_date("2026-13-01"));
        assert!(!valid_iso_date("2026-01-32"));
        assert!(!valid_iso_date("2026-01-01X"));
        assert!(!valid_iso_date(""));
    }

    #[test]
    fn field_key_rules_match_reference_regex() {
        assert!(valid_field_key("a"));
        assert!(valid_field_key("plan_tier"));
        // ^[a-z][a-z0-9_]{0,58}$ — max 59 chars total.
        let max = format!("a{}", "b".repeat(58));
        assert!(valid_field_key(&max));
        let too_long = format!("a{}", "b".repeat(59));
        assert!(!valid_field_key(&too_long));
        assert!(!valid_field_key("A"));
        assert!(!valid_field_key("1a"));
        assert!(!valid_field_key("Not-Snake"));
        assert!(!valid_field_key("has space"));
        assert!(!valid_field_key(""));
    }
}
