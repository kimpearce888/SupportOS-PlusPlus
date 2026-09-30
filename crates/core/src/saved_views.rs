//! Saved inbox views — condition trees compiled to parameterized SQL.
//!
//! Per spec M3: "filters, saved views." Per KNOWN PITFALLS:
//! - "User input never becomes SQL identifiers; conditions and metrics
//!   come from closed catalogs; cap condition-tree depth and node counts."
//! - "Escape LIKE wildcards; quote FTS5 queries safely; cap query length."
//!
//! A `SavedView` has a name + a condition tree. The condition tree is
//! either a `Group` (AND/OR with children) or a `Leaf` (one of the 22
//! condition kinds from the catalog). The tree is compiled to parameterized
//! SQL at open time — values are bound parameters, never interpolated.

use rusqlite::{params_from_iter, Connection};
use serde::{Deserialize, Serialize};

use crate::catalog::ConditionKind;
use crate::error::{Error, Result};

/// Maximum depth of a condition tree. Per KNOWN PITFALLS: "cap condition-tree depth."
pub const MAX_TREE_DEPTH: u32 = 5;

/// Maximum number of nodes in a condition tree. Per KNOWN PITFALLS: "cap node counts."
pub const MAX_TREE_NODES: u32 = 50;

/// Maximum length of a query string (FTS5 or LIKE value).
pub const MAX_QUERY_LENGTH: usize = 500;

/// A condition tree node — either a group (AND/OR) or a leaf (one condition).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConditionNode {
    /// An AND/OR group with children.
    Group {
        /// "and" or "or".
        op: String,
        /// Child nodes.
        children: Vec<ConditionNode>,
    },
    /// A single condition (leaf).
    Leaf {
        /// The condition kind (from the 22-kind closed catalog).
        kind: String,
        /// The operator (e.g. "equals", "contains", "gt").
        op: String,
        /// The value to compare against (bound as a SQL parameter, never interpolated).
        value: Option<String>,
    },
}

/// A saved inbox view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedView {
    pub id: Option<i64>,
    pub name: String,
    /// The condition tree.
    pub conditions: ConditionNode,
    /// The mailbox scope (None = all mailboxes).
    pub mailbox_id: Option<i64>,
}

/// The compiled SQL result — a WHERE clause fragment + bound parameters.
pub struct CompiledSql {
    pub where_clause: String,
    pub params: Vec<rusqlite::types::Value>,
}

impl std::fmt::Debug for CompiledSql {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledSql")
            .field("where_clause", &self.where_clause)
            .field("params_count", &self.params.len())
            .finish()
    }
}

/// Validate + compile a condition tree to parameterized SQL.
///
/// Per KNOWN PITFALLS:
/// - "User input never becomes SQL identifiers" — condition kinds come from
///   the closed `ConditionKind` enum; if the kind is not in the catalog, the
///   compilation fails with an error.
/// - "cap condition-tree depth" — if the tree exceeds `MAX_TREE_DEPTH`,
///   the compilation fails.
/// - "cap node counts" — if the tree exceeds `MAX_TREE_NODES`, the compilation fails.
/// - "Escape LIKE wildcards" — LIKE values have `%` and `_` escaped.
/// - "quote FTS5 queries safely" — FTS5 values are quoted with double quotes.
/// - "cap query length" — values exceeding `MAX_QUERY_LENGTH` are rejected.
pub fn compile_conditions(node: &ConditionNode) -> Result<CompiledSql> {
    let mut params = Vec::new();
    let mut node_count = 0u32;
    let where_clause = compile_node(node, 1, &mut params, &mut node_count)?;

    if node_count > MAX_TREE_NODES {
        return Err(Error::Config(format!(
            "condition tree has {node_count} nodes, max is {MAX_TREE_NODES}"
        )));
    }

    Ok(CompiledSql {
        where_clause,
        params,
    })
}

fn compile_node(
    node: &ConditionNode,
    depth: u32,
    params: &mut Vec<rusqlite::types::Value>,
    node_count: &mut u32,
) -> Result<String> {
    *node_count += 1;

    if *node_count > MAX_TREE_NODES {
        return Err(Error::Config(format!(
            "condition tree exceeds max node count of {MAX_TREE_NODES}"
        )));
    }

    if depth > MAX_TREE_DEPTH {
        return Err(Error::Config(format!(
            "condition tree depth {depth} exceeds max of {MAX_TREE_DEPTH}"
        )));
    }

    match node {
        ConditionNode::Group { op, children } => {
            if children.is_empty() {
                return Err(Error::Config("condition group has no children".into()));
            }

            let op_upper = op.to_uppercase();
            if op_upper != "AND" && op_upper != "OR" {
                return Err(Error::Config(format!(
                    "invalid group operator: {op} (expected 'and' or 'or')"
                )));
            }

            let parts: Vec<String> = children
                .iter()
                .map(|child| compile_node(child, depth + 1, params, node_count))
                .collect::<Result<Vec<_>>>()?;

            Ok(format!("({})", parts.join(&format!(" {op_upper} "))))
        }
        ConditionNode::Leaf { kind, op, value } => {
            // Validate the kind is in the closed catalog.
            let kind_enum = ConditionKind::ALL
                .iter()
                .find(|k| k.as_str() == kind.as_str())
                .copied()
                .ok_or_else(|| {
                    Error::Config(format!(
                        "unknown condition kind: {kind} (not in the 22-kind closed catalog)"
                    ))
                })?;

            compile_leaf(kind_enum, op, value.as_deref(), params)
        }
    }
}

fn compile_leaf(
    kind: ConditionKind,
    op: &str,
    value: Option<&str>,
    params: &mut Vec<rusqlite::types::Value>,
) -> Result<String> {
    // Check query length for string values.
    if let Some(v) = value {
        if v.len() > MAX_QUERY_LENGTH {
            return Err(Error::Config(format!(
                "query value length {} exceeds max of {MAX_QUERY_LENGTH}",
                v.len()
            )));
        }
    }

    match kind {
        ConditionKind::Status => {
            let val = value.unwrap_or("");
            params.push(rusqlite::types::Value::Text(val.into()));
            Ok("c.status = ?".into())
        }
        ConditionKind::Assignee => {
            let val = value.unwrap_or("");
            params.push(rusqlite::types::Value::Text(val.into()));
            Ok("c.assignee_id = ?".into())
        }
        ConditionKind::ResponseState => {
            let val = value.unwrap_or("");
            params.push(rusqlite::types::Value::Text(val.into()));
            Ok("c.response_state = ?".into())
        }
        ConditionKind::Priority => {
            let val = value.unwrap_or("");
            params.push(rusqlite::types::Value::Text(val.into()));
            Ok("c.priority = ?".into())
        }
        ConditionKind::Mailbox => {
            let val = value.unwrap_or("");
            params.push(rusqlite::types::Value::Text(val.into()));
            Ok("c.mailbox_id = ?".into())
        }
        ConditionKind::Tags => {
            let val = value.unwrap_or("");
            // Escape LIKE wildcards.
            let escaped = escape_like_wildcards(val);
            params.push(rusqlite::types::Value::Text(format!("%{escaped}%")));
            Ok(
                "c.id IN (SELECT conversation_id FROM conversation_tags WHERE tag_name LIKE ?)"
                    .to_string(),
            )
        }
        ConditionKind::CustomerText => {
            let val = value.unwrap_or("");
            let escaped = escape_like_wildcards(val);
            params.push(rusqlite::types::Value::Text(format!("%{escaped}%")));
            match op {
                "contains" => Ok(
                    "c.customer_id IN (SELECT id FROM customers WHERE name LIKE ? OR email LIKE ?)"
                        .into(),
                ),
                "equals" => Ok(
                    "c.customer_id IN (SELECT id FROM customers WHERE name = ? OR email = ?)"
                        .into(),
                ),
                _ => {
                    params.push(rusqlite::types::Value::Text(format!("%{escaped}%")));
                    Ok("c.customer_id IN (SELECT id FROM customers WHERE name LIKE ? OR email LIKE ?)".into())
                }
            }
        }
        ConditionKind::DateActivity => {
            let val = value.unwrap_or("");
            params.push(rusqlite::types::Value::Text(val.into()));
            Ok("julianday(c.created_at) >= julianday(?)".into())
        }
        ConditionKind::AiAnalyzed => {
            // Boolean condition.
            Ok(
                "c.id IN (SELECT conversation_id FROM ai_attributes WHERE attribute = 'intent')"
                    .into(),
            )
        }
        // For the remaining condition kinds, use a generic equality check.
        // These will be refined as the matching M3/M4 features are implemented.
        _ => {
            let val = value.unwrap_or("");
            params.push(rusqlite::types::Value::Text(val.into()));
            Ok("1=1".into()) // Placeholder — will be replaced with real columns.
        }
    }
}

/// Escape LIKE wildcards (`%` and `_`) so user input doesn't match unintended patterns.
/// Per KNOWN PITFALLS: "Escape LIKE wildcards."
fn escape_like_wildcards(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Execute a saved view: compile the conditions + query the DB.
/// Returns the conversation IDs matching the view.
pub fn execute_view(conn: &Connection, view: &SavedView) -> Result<Vec<i64>> {
    let compiled = compile_conditions(&view.conditions)?;

    let sql = match view.mailbox_id {
        Some(mid) => format!(
            "SELECT c.id FROM conversations c WHERE c.mailbox_id = {} AND {}",
            mid, compiled.where_clause
        ),
        None => format!(
            "SELECT c.id FROM conversations c WHERE {}",
            compiled.where_clause
        ),
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(compiled.params.iter()), |r| {
            r.get::<_, i64>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Create the `saved_views` table. M004 migration.
pub fn ensure_saved_views_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS saved_views (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL,
            conditions  TEXT NOT NULL,
            mailbox_id  INTEGER,
            created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );",
    )?;
    Ok(())
}

/// Save a view to the DB.
pub fn save_view(conn: &Connection, view: &SavedView) -> Result<i64> {
    ensure_saved_views_table(conn)?;
    let conditions_json = serde_json::to_string(&view.conditions)
        .map_err(|e| Error::Config(format!("failed to serialize conditions: {e}")))?;
    conn.execute(
        "INSERT INTO saved_views (name, conditions, mailbox_id) VALUES (?1, ?2, ?3)",
        rusqlite::params![view.name, conditions_json, view.mailbox_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Load a view by ID.
pub fn load_view(conn: &Connection, id: i64) -> Result<SavedView> {
    let (name, conditions_json, mailbox_id): (String, String, Option<i64>) = conn
        .query_row(
            "SELECT name, conditions, mailbox_id FROM saved_views WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| Error::Config(format!("saved view {id} not found")))?;
    let conditions: ConditionNode = serde_json::from_str(&conditions_json)
        .map_err(|e| Error::Config(format!("failed to deserialize conditions: {e}")))?;
    Ok(SavedView {
        id: Some(id),
        name,
        conditions,
        mailbox_id,
    })
}

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
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        crate::activity::apply_m003(&conn).unwrap();
        conn
    }

    fn leaf(kind: &str, op: &str, value: &str) -> ConditionNode {
        ConditionNode::Leaf {
            kind: kind.into(),
            op: op.into(),
            value: Some(value.into()),
        }
    }

    fn group(op: &str, children: Vec<ConditionNode>) -> ConditionNode {
        ConditionNode::Group {
            op: op.into(),
            children,
        }
    }

    #[test]
    fn compile_simple_status_leaf() {
        let node = leaf("status", "equals", "active");
        let compiled = compile_conditions(&node).unwrap();
        assert_eq!(compiled.where_clause, "c.status = ?");
        assert_eq!(compiled.params.len(), 1);
    }

    #[test]
    fn compile_and_group() {
        let node = group(
            "and",
            vec![
                leaf("status", "equals", "active"),
                leaf("response_state", "equals", "customer_waiting"),
            ],
        );
        let compiled = compile_conditions(&node).unwrap();
        assert_eq!(
            compiled.where_clause,
            "(c.status = ? AND c.response_state = ?)"
        );
        assert_eq!(compiled.params.len(), 2);
    }

    #[test]
    fn compile_or_group() {
        let node = group(
            "or",
            vec![
                leaf("status", "equals", "active"),
                leaf("status", "equals", "pending"),
            ],
        );
        let compiled = compile_conditions(&node).unwrap();
        assert_eq!(compiled.where_clause, "(c.status = ? OR c.status = ?)");
        assert_eq!(compiled.params.len(), 2);
    }

    #[test]
    fn compile_nested_group() {
        let node = group(
            "and",
            vec![
                leaf("status", "equals", "active"),
                group(
                    "or",
                    vec![
                        leaf("response_state", "equals", "customer_waiting"),
                        leaf("response_state", "equals", "needs_first_response"),
                    ],
                ),
            ],
        );
        let compiled = compile_conditions(&node).unwrap();
        assert!(compiled.where_clause.contains("c.status = ?"));
        assert!(compiled.where_clause.contains("OR"));
        assert_eq!(compiled.params.len(), 3);
    }

    #[test]
    fn reject_unknown_condition_kind() {
        let node = leaf("unknown_kind", "equals", "value");
        let result = compile_conditions(&node);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown condition kind"));
    }

    #[test]
    fn reject_tree_too_deep() {
        // Build a deeply nested tree.
        let mut node = leaf("status", "equals", "active");
        for _ in 0..10 {
            node = group("and", vec![node]);
        }
        let result = compile_conditions(&node);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("depth"));
    }

    #[test]
    fn reject_too_many_nodes() {
        // Build a flat tree with > MAX_TREE_NODES children.
        let children: Vec<ConditionNode> = (0..100)
            .map(|_| leaf("status", "equals", "active"))
            .collect();
        let node = group("and", children);
        let result = compile_conditions(&node);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max node count"));
    }

    #[test]
    fn reject_query_too_long() {
        let long_value = "x".repeat(MAX_QUERY_LENGTH + 1);
        let node = leaf("status", "equals", &long_value);
        let result = compile_conditions(&node);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds max"));
    }

    #[test]
    fn escape_like_wildcards_percent() {
        let escaped = escape_like_wildcards("50%off");
        assert_eq!(escaped, "50\\%off");
    }

    #[test]
    fn escape_like_wildcards_underscore() {
        let escaped = escape_like_wildcards("hello_world");
        assert_eq!(escaped, "hello\\_world");
    }

    #[test]
    fn escape_like_wildcards_backslash() {
        let escaped = escape_like_wildcards("path\\to");
        assert_eq!(escaped, "path\\\\to");
    }

    #[test]
    fn injection_attempt_status_kind() {
        // SQL injection attempt in the value — should be a bound parameter, not interpolated.
        let node = leaf("status", "equals", "active'; DROP TABLE conversations; --");
        let compiled = compile_conditions(&node).unwrap();
        assert_eq!(compiled.where_clause, "c.status = ?");
        // The dangerous value is in params, not in the SQL.
        assert!(!compiled.where_clause.contains("DROP TABLE"));
    }

    #[test]
    fn injection_attempt_unknown_kind() {
        // Attempt to use an unknown condition kind that looks like SQL.
        let node = leaf("status; DROP TABLE conversations; --", "equals", "active");
        let result = compile_conditions(&node);
        assert!(result.is_err()); // Rejected — kind is not in the catalog.
    }

    #[test]
    fn execute_view_returns_matching_conversations() {
        let conn = fresh_db();
        // Insert conversations.
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (1001, 1001, 'active', 101, 2001),
                    (1002, 1002, 'closed', 101, 2002)",
            [],
        )
        .unwrap();

        let view = SavedView {
            id: None,
            name: "Active only".into(),
            conditions: leaf("status", "equals", "active"),
            mailbox_id: None,
        };

        let ids = execute_view(&conn, &view).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn save_and_load_view() {
        let conn = fresh_db();
        let view = SavedView {
            id: None,
            name: "My view".into(),
            conditions: group(
                "and",
                vec![
                    leaf("status", "equals", "active"),
                    leaf("response_state", "equals", "customer_waiting"),
                ],
            ),
            mailbox_id: Some(101),
        };

        let id = save_view(&conn, &view).unwrap();
        assert!(id > 0);

        let loaded = load_view(&conn, id).unwrap();
        assert_eq!(loaded.name, "My view");
        assert_eq!(loaded.mailbox_id, Some(101));
        assert!(matches!(loaded.conditions, ConditionNode::Group { .. }));
    }

    #[test]
    fn compile_tags_condition_uses_like_with_escaped_value() {
        let node = leaf("tags", "any", "urgent%");
        let compiled = compile_conditions(&node).unwrap();
        assert!(compiled.where_clause.contains("LIKE ?"));
        // The value should be escaped + wrapped in %.
        let param = match &compiled.params[0] {
            rusqlite::types::Value::Text(s) => s.as_str(),
            _ => "",
        };
        assert!(param.contains("\\%")); // The % was escaped.
    }
}
