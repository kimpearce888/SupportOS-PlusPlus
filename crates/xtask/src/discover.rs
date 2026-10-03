//! `discover` — scans a local checkout of the reference repo and emits inventories.
//!
//! Extracts API routes, tables, migrations, settings keys,
//! env vars, closed vocabularies, UI pages, and scripts. Writes a machine-readable
//! `target/discovery/inventory.json` and prints a human-readable summary to stdout.
//!
//! Cross-checks the 8 canonical counts against the spec values; exits non-zero on mismatch
//! so CI catches reference drift.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// The full inventory of the reference repo at a given HEAD.
#[derive(Debug, Clone, Serialize)]
pub struct Inventory {
    /// The reference repo HEAD this inventory was taken from.
    pub reference_head: String,
    /// Path that was scanned.
    pub scanned_path: String,
    /// Counts grouped by surface area.
    pub surfaces: BTreeMap<String, usize>,
    /// The 8 canonical counts (label → values).
    pub canonical_counts: Vec<CanonicalVocab>,
    /// Lists of values for closed vocabularies (so future runs can detect additions).
    pub vocabularies: BTreeMap<String, Vec<String>>,
}

/// One canonical vocabulary check.
#[derive(Debug, Clone, Serialize)]
pub struct CanonicalVocab {
    pub label: String,
    pub expected: usize,
    pub actual: usize,
    pub source_file: String,
    pub source_pattern: String,
    pub matches: bool,
}

/// Run discovery against `reference_path` and write the inventory to `out_path`.
///
/// Returns the inventory so the caller can print a summary.
pub fn run(reference_path: &Path, out_path: &Path) -> anyhow::Result<Inventory> {
    anyhow::ensure!(
        reference_path.exists(),
        "reference path does not exist: {}",
        reference_path.display()
    );
    anyhow::ensure!(
        !is_inside_workspace(reference_path, &workspace_root()),
        "the reference checkout must live OUTSIDE the SupportOS++ repo (spec A7)"
    );

    let reference_head = read_reference_head(reference_path).unwrap_or_else(|_| "unknown".into());

    let mut surfaces: BTreeMap<String, usize> = BTreeMap::new();

    // --- Surface-area counts ---
    surfaces.insert(
        "api_route_files".into(),
        count_files(
            &reference_path.join("src/server/routes"),
            "ts",
            &["index", "helpers"],
        ),
    );
    surfaces.insert(
        "api_endpoint_handlers".into(),
        count_route_handlers(&reference_path.join("src/server/routes"))?,
    );
    surfaces.insert(
        "database_migrations".into(),
        count_files(
            &reference_path.join("src/server/database/migrations"),
            "ts",
            &["index"],
        ),
    );
    surfaces.insert(
        "database_tables".into(),
        count_create_table(&reference_path.join("src/server/database/migrations"))?,
    );
    surfaces.insert(
        "database_repositories".into(),
        count_files(
            &reference_path.join("src/server/database/repositories"),
            "ts",
            &["helpers", "types"],
        ),
    );
    surfaces.insert(
        "client_pages".into(),
        count_files(&reference_path.join("src/client/pages"), "tsx", &[]),
    );
    surfaces.insert(
        "shared_modules".into(),
        count_files(&reference_path.join("src/shared"), "ts", &[]),
    );
    surfaces.insert(
        "server_module_dirs".into(),
        count_dirs(&reference_path.join("src/server")),
    );
    surfaces.insert(
        "scripts".into(),
        count_files(&reference_path.join("scripts"), "", &[]),
    );
    surfaces.insert(
        "env_vars".into(),
        count_env_vars(&reference_path.join(".env.example"))?,
    );

    // --- Closed vocabularies (8 canonical counts from spec A7) ---
    let shared = reference_path.join("src/shared");
    let (tiles_count, tile_keys) =
        collect_string_array(&shared.join("collaboration.ts"), "OPERATIONS_TILE_KEYS")?;
    let (notif_count, notif_types) =
        collect_string_array(&shared.join("collaboration.ts"), "NOTIFICATION_TYPES")?;
    let (af_count, activity_fields) =
        collect_string_array(&shared.join("activity.ts"), "ACTIVITY_FIELDS")?;
    let (_cal_count, calendar_modes) =
        collect_string_array(&shared.join("activity.ts"), "CALENDAR_DATE_MODES")?;
    let (_roll_count, rolling_modes) =
        collect_string_array(&shared.join("activity.ts"), "ROLLING_DATE_MODES")?;
    let (_exact_count, exact_modes) =
        collect_string_array(&shared.join("activity.ts"), "EXACT_DATE_MODES")?;
    let mut date_modes = calendar_modes;
    date_modes.extend(rolling_modes);
    date_modes.extend(exact_modes);
    let (rm_count, report_metrics) =
        collect_keyed_objects(&shared.join("reporting.ts"), "REPORT_METRICS")?;
    let (rd_count, report_dimensions) =
        collect_keyed_objects(&shared.join("reporting.ts"), "REPORT_DIMENSIONS")?;
    let (g_count, graph_kinds) =
        collect_string_array(&shared.join("graph.ts"), "GRAPH_NODE_KINDS")?;
    let (ai_count, ai_attrs) =
        collect_keyed_objects(&shared.join("constants.ts"), "AI_ATTRIBUTE_CATALOG")?;
    let (cop_count, copilot_tools) =
        collect_copilot_tools(&reference_path.join("src/server/ai/tools.ts"))?;
    let (ck_count, condition_kinds) = collect_condition_kinds(&shared.join("activity.ts"))?;

    let date_modes_count = date_modes.len();
    let mut vocabularies: BTreeMap<String, Vec<String>> = BTreeMap::new();
    vocabularies.insert("operations_tile_keys".into(), tile_keys);
    vocabularies.insert("notification_types".into(), notif_types);
    vocabularies.insert("activity_fields".into(), activity_fields);
    vocabularies.insert("date_modes".into(), date_modes);
    vocabularies.insert("report_metrics".into(), report_metrics);
    vocabularies.insert("report_dimensions".into(), report_dimensions);
    vocabularies.insert("graph_node_kinds".into(), graph_kinds);
    vocabularies.insert("ai_attribute_keys".into(), ai_attrs);
    vocabularies.insert("copilot_tools".into(), copilot_tools);
    vocabularies.insert("condition_kinds".into(), condition_kinds);

    let canonical_counts = vec![
        CanonicalVocab {
            label: "operations_tiles".into(),
            expected: 16,
            actual: tiles_count,
            source_file: "src/shared/collaboration.ts".into(),
            source_pattern: "OPERATIONS_TILE_KEYS string array".into(),
            matches: tiles_count == 16,
        },
        CanonicalVocab {
            label: "notification_types".into(),
            expected: 15,
            actual: notif_count,
            source_file: "src/shared/collaboration.ts".into(),
            source_pattern: "NOTIFICATION_TYPES string array".into(),
            matches: notif_count == 15,
        },
        CanonicalVocab {
            label: "condition_kinds".into(),
            expected: 22,
            actual: ck_count,
            source_file: "src/shared/activity.ts".into(),
            source_pattern: "kind: z.literal('...') occurrences".into(),
            matches: ck_count == 22,
        },
        CanonicalVocab {
            label: "activity_fields".into(),
            expected: 14,
            actual: af_count,
            source_file: "src/shared/activity.ts".into(),
            source_pattern: "ACTIVITY_FIELDS string array".into(),
            matches: af_count == 14,
        },
        CanonicalVocab {
            label: "date_modes".into(),
            expected: 15,
            actual: date_modes_count,
            source_file: "src/shared/activity.ts".into(),
            source_pattern: "CALENDAR_DATE_MODES + ROLLING_DATE_MODES + EXACT_DATE_MODES".into(),
            matches: date_modes_count == 15,
        },
        CanonicalVocab {
            label: "report_metrics".into(),
            expected: 21,
            actual: rm_count,
            source_file: "src/shared/reporting.ts".into(),
            source_pattern: "REPORT_METRICS key: '...'".into(),
            matches: rm_count == 21,
        },
        CanonicalVocab {
            label: "report_dimensions".into(),
            expected: 14,
            actual: rd_count,
            source_file: "src/shared/reporting.ts".into(),
            source_pattern: "REPORT_DIMENSIONS key: '...'".into(),
            matches: rd_count == 14,
        },
        CanonicalVocab {
            label: "graph_node_kinds".into(),
            expected: 12,
            actual: g_count,
            source_file: "src/shared/graph.ts".into(),
            source_pattern: "GRAPH_NODE_KINDS string array".into(),
            matches: g_count == 12,
        },
        CanonicalVocab {
            label: "ai_attribute_keys".into(),
            expected: 14,
            actual: ai_count,
            source_file: "src/shared/constants.ts".into(),
            source_pattern: "AI_ATTRIBUTE_CATALOG key: '...'".into(),
            matches: ai_count == 14,
        },
        CanonicalVocab {
            label: "copilot_tools".into(),
            expected: 22,
            actual: cop_count,
            source_file: "src/server/ai/tools.ts".into(),
            source_pattern: "name: '...' literals".into(),
            matches: cop_count == 22,
        },
    ];

    let inventory = Inventory {
        reference_head,
        scanned_path: reference_path.display().to_string(),
        surfaces,
        canonical_counts,
        vocabularies,
    };

    // Write the JSON inventory.
    fs::create_dir_all(out_path.parent().unwrap_or(Path::new(".")))?;
    let json = serde_json::to_string_pretty(&inventory)?;
    fs::write(out_path, json)?;

    Ok(inventory)
}

// ----------------------------------------------------------------------------
// Surface-area helpers
// ----------------------------------------------------------------------------

/// Count files in `dir` with extension `ext` (use "" for any), skipping any
/// file whose stem is in `exclude_stems`.
fn count_files(dir: &Path, ext: &str, exclude_stems: &[&str]) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !ext.is_empty() {
            match path.extension().and_then(|e| e.to_str()) {
                Some(e) if e == ext => {}
                _ => continue,
            }
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if exclude_stems.contains(&stem) {
            continue;
        }
        n += 1;
    }
    n
}

/// Count immediate subdirectories of `dir`.
fn count_dirs(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0;
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            n += 1;
        }
    }
    n
}

/// Count HTTP route handler definitions across all `.ts` files in `routes_dir`.
/// Matches `app.<verb>(...)` and `router.<verb>(...)` for the standard HTTP verbs.
fn count_route_handlers(routes_dir: &Path) -> anyhow::Result<usize> {
    let mut total = 0;
    let pattern = regex_simple_route_verb();
    let Ok(entries) = fs::read_dir(routes_dir) else {
        return Ok(0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("ts") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        total += count_pattern(&text, &pattern);
    }
    Ok(total)
}

/// Count `CREATE TABLE` statements across all migration files.
fn count_create_table(migrations_dir: &Path) -> anyhow::Result<usize> {
    let mut total = 0;
    let Ok(entries) = fs::read_dir(migrations_dir) else {
        return Ok(0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("ts") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        // Count case-insensitive "CREATE TABLE" occurrences (case-insensitive via lowercase copy).
        total += text.to_lowercase().matches("create table").count();
    }
    Ok(total)
}

/// Count non-comment, non-blank `KEY=value` or `KEY=` lines in `.env.example`.
fn count_env_vars(env_path: &Path) -> anyhow::Result<usize> {
    let text = fs::read_to_string(env_path).unwrap_or_default();
    let mut n = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Only lines that start with an uppercase A-Z key.
        if trimmed
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
            && trimmed.contains('=')
        {
            n += 1;
        }
    }
    Ok(n)
}

// ----------------------------------------------------------------------------
// Closed-vocabulary helpers
// ----------------------------------------------------------------------------

/// Collect the string literals inside a `const NAME = [ ... ]` array.
/// Returns `(count, values)`.
fn collect_string_array(
    file_path: &Path,
    const_name: &str,
) -> anyhow::Result<(usize, Vec<String>)> {
    let text = fs::read_to_string(file_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", file_path.display()))?;
    let block = extract_array_block(&text, const_name).unwrap_or_default();
    let values = extract_single_quoted_strings(&block);
    Ok((values.len(), values))
}

/// Collect the `key: '...'` values inside a `const NAME = [ ... ]` array of objects.
/// Returns `(count, values)`.
fn collect_keyed_objects(
    file_path: &Path,
    const_name: &str,
) -> anyhow::Result<(usize, Vec<String>)> {
    let text = fs::read_to_string(file_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", file_path.display()))?;
    let block = extract_array_block(&text, const_name).unwrap_or_default();
    let values = extract_key_single_quotes(&block);
    Ok((values.len(), values))
}

/// Collect Copilot tool `name: '...'` literals.
fn collect_copilot_tools(tools_path: &Path) -> anyhow::Result<(usize, Vec<String>)> {
    let text = fs::read_to_string(tools_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", tools_path.display()))?;
    let values = extract_key_single_quotes(&text);
    Ok((values.len(), values))
}

/// Count `kind: z.literal('...')` occurrences (saved-view condition kinds).
/// Excludes the `group` literal, which is the recursive AND/OR container node —
/// not a leaf condition kind. Spec counts 22 leaf kinds.
fn collect_condition_kinds(activity_path: &Path) -> anyhow::Result<(usize, Vec<String>)> {
    let text = fs::read_to_string(activity_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", activity_path.display()))?;
    let pattern = "kind: z.literal('";
    let mut values = Vec::new();
    let mut start = 0usize;
    while let Some(idx) = text[start..].find(pattern) {
        let abs = start + idx;
        let val_start = abs + pattern.len();
        if let Some(end) = text[val_start..].find('\'') {
            let val = &text[val_start..val_start + end];
            // Skip the recursive AND/OR container — it's not a leaf condition kind.
            if val != "group" && !values.contains(&val.to_string()) {
                values.push(val.to_string());
            }
            start = val_start + end + 1;
        } else {
            break;
        }
    }
    Ok((values.len(), values))
}

// ----------------------------------------------------------------------------
// Low-level text helpers
// ----------------------------------------------------------------------------

/// Extract the contents of `const NAME = [ ... ];` (the text between the first `[`
/// after the const declaration and the first `]` after that). Returns the raw block text.
///
/// Looks for `= [` rather than just `[` so type annotations like
/// `: readonly Foo[] =` don't match the wrong bracket. Stops at the first `]` rather
/// than `];` so this also handles arrays declared with `] as const;`, `] as const,`, etc.
fn extract_array_block(text: &str, const_name: &str) -> Option<String> {
    let needle = format!("export const {const_name}");
    let start = text.find(&needle)?;
    let rest = &text[start..];
    // Prefer `= [` (the array literal); fall back to `[` if no `=` is present.
    let bracket_open = rest.find("= [").map(|i| i + 2).or_else(|| rest.find('['))?;
    let after_open = &rest[bracket_open + 1..];
    let close = after_open.find(']')?;
    Some(after_open[..close].to_string())
}

/// Extract all single-quoted string literals from `text`.
fn extract_single_quoted_strings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            // Read until the next unescaped `'`.
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == b'\'' {
                    break;
                }
                j += 1;
            }
            if j <= bytes.len() {
                if let Ok(s) = std::str::from_utf8(&bytes[start..j]) {
                    out.push(s.to_string());
                }
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Extract the value of the FIRST `key: '...'` literal on each line (used for
/// REPORT_METRICS / REPORT_DIMENSIONS / AI_ATTRIBUTE_CATALOG / Copilot tools).
fn extract_key_single_quotes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(idx) = line.find("key: '").or_else(|| line.find("name: '")) {
            let after = &line[idx..];
            if let Some(q1) = after.find('\'') {
                let rest = &after[q1 + 1..];
                if let Some(q2) = rest.find('\'') {
                    out.push(rest[..q2].to_string());
                }
            }
        }
    }
    out
}

/// A hand-rolled alternation pattern matching `(app|router)\.(get|post|put|patch|delete|head)\(`.
/// Returns the regex (kept simple and dependency-free).
fn regex_simple_route_verb() -> Vec<String> {
    // We avoid pulling in the regex crate; the matcher below just counts substrings.
    vec![
        "app.get(".into(),
        "app.post(".into(),
        "app.put(".into(),
        "app.patch(".into(),
        "app.delete(".into(),
        "app.head(".into(),
        "router.get(".into(),
        "router.post(".into(),
        "router.put(".into(),
        "router.patch(".into(),
        "router.delete(".into(),
        "router.head(".into(),
    ]
}

/// Count occurrences of any of `patterns` in `text`. Overlapping matches are
/// impossible because every pattern ends in `(` which is unique enough at the
/// match position; the simple `matches().count()` is correct.
fn count_pattern(text: &str, patterns: &[String]) -> usize {
    let mut n = 0;
    for p in patterns {
        n += text.matches(p.as_str()).count();
    }
    n
}

// ----------------------------------------------------------------------------
// Misc helpers
// ----------------------------------------------------------------------------

/// Run `git rev-parse HEAD` in the reference checkout. Returns the SHA or an error.
fn read_reference_head(reference_path: &Path) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(reference_path)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("git rev-parse HEAD failed in {}", reference_path.display());
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(sha)
}

/// Returns true if `child` is inside `parent` (lexically, after canonicalization).
fn is_inside_workspace(child: &Path, parent: &Path) -> bool {
    let Ok(child) = fs::canonicalize(child) else {
        return false;
    };
    let Ok(parent) = fs::canonicalize(parent) else {
        return false;
    };
    child.starts_with(&parent)
}

/// Locate the SupportOS++ workspace root. In `xtask` this is two ancestors above
/// CARGO_MANIFEST_DIR (which is `crates/xtask`).
fn workspace_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string());
    PathBuf::from(manifest)
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Print a human-readable summary of the inventory to stdout, including the
/// canonical-count cross-check table.
pub fn print_summary(inv: &Inventory) {
    println!(
        "SupportOS++ discovery — reference HEAD: {}",
        inv.reference_head
    );
    println!("Scanned: {}", inv.scanned_path);
    println!();
    println!("== Surface areas ==");
    let mut max_label = 0;
    for k in inv.surfaces.keys() {
        max_label = max_label.max(k.len());
    }
    for (k, v) in &inv.surfaces {
        println!("{k:<max_label$}  {v:>6}");
    }
    println!();
    println!("== Canonical counts (A7 cross-check) ==");
    println!(
        "  {:<22}  {:>8}  {:>6}  {:>7}  source",
        "label", "expected", "actual", "match"
    );
    for c in &inv.canonical_counts {
        println!(
            "  {:<22}  {:>8}  {:>6}  {:>7}  {}",
            c.label,
            c.expected,
            c.actual,
            if c.matches { "OK" } else { "DIFF" },
            c.source_file
        );
    }
    println!();
    let all_match = inv.canonical_counts.iter().all(|c| c.matches);
    if all_match {
        println!(
            "All canonical counts match the spec. Inventory written to target/discovery/inventory.json"
        );
    } else {
        let mismatches: Vec<_> = inv.canonical_counts.iter().filter(|c| !c.matches).collect();
        println!(
            "WARNING: {} canonical count(s) differ from the spec. See docs/DEVIATIONS.md.",
            mismatches.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_array_block_finds_simple_const() {
        let src = "export const X = ['a', 'b', 'c'];";
        let block = extract_array_block(src, "X").unwrap();
        assert!(block.contains("'a'"));
        assert!(block.contains("'c'"));
    }

    #[test]
    fn extract_array_block_handles_type_annotation() {
        // The reference repo declares AI_ATTRIBUTE_CATALOG with a type annotation
        // whose `[]` would otherwise be mistaken for the array literal.
        let src = "export const AI_ATTRIBUTE_CATALOG: readonly AiAttributeDefinition[] = [\n  { key: 'intent' },\n  { key: 'product' }\n];";
        let block = extract_array_block(src, "AI_ATTRIBUTE_CATALOG").unwrap();
        assert!(block.contains("'intent'"));
        assert!(block.contains("'product'"));
    }

    #[test]
    fn extract_array_block_handles_as_const() {
        let src = "export const CALENDAR_DATE_MODES = ['today','yesterday'] as const;";
        let block = extract_array_block(src, "CALENDAR_DATE_MODES").unwrap();
        assert!(block.contains("'today'"));
        assert!(block.contains("'yesterday'"));
    }

    #[test]
    fn extract_array_block_returns_none_for_missing_const() {
        let src = "export const Y = ['a'];";
        assert!(extract_array_block(src, "X").is_none());
    }

    #[test]
    fn extract_single_quoted_strings_handles_escapes() {
        let s = "['a', 'b', 'c\\'s', 'd']";
        let vals = extract_single_quoted_strings(s);
        assert_eq!(vals, vec!["a", "b", "c\\'s", "d"]);
    }

    #[test]
    fn extract_key_single_quotes_picks_key_field() {
        let s = "  { key: 'intent', label: 'Intent' },\n  { key: 'product', label: 'Product' },";
        let vals = extract_key_single_quotes(s);
        assert_eq!(vals, vec!["intent", "product"]);
    }

    #[test]
    fn extract_key_single_quotes_picks_name_field() {
        let s = "  name: 'search_conversations',\n  name: 'get_conversation',";
        let vals = extract_key_single_quotes(s);
        assert_eq!(vals, vec!["search_conversations", "get_conversation"]);
    }

    #[test]
    fn collect_condition_kinds_dedupes() {
        // Two z.literal('status') on different lines should count once.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            tmp.path(),
            "z.object({ kind: z.literal('status') })\nz.object({ kind: z.literal('status') })\n",
        )
        .unwrap();
        let (n, vals) = collect_condition_kinds(tmp.path()).unwrap();
        assert_eq!(n, 1);
        assert_eq!(vals, vec!["status".to_string()]);
    }

    #[test]
    fn count_env_vars_skips_comments_and_blanks() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            tmp.path(),
            "# a comment\n\nFOO=bar\nBAZ=\n# another\nQUX=1\n",
        )
        .unwrap();
        assert_eq!(count_env_vars(tmp.path()).unwrap(), 3);
    }

    #[test]
    fn count_route_handlers_counts_all_verbs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("a.ts"),
            "app.get('/x', h);\napp.post('/y', h);\nrouter.delete('/z', h);\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("b.ts"),
            "app.get('/a', h);\napp.patch('/b', h);\n",
        )
        .unwrap();
        assert_eq!(count_route_handlers(tmp.path()).unwrap(), 5);
    }

    #[test]
    fn count_create_table_is_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("001.ts"),
            "CREATE TABLE a (id INTEGER);\ncreate table b (id INTEGER);\n",
        )
        .unwrap();
        assert_eq!(count_create_table(tmp.path()).unwrap(), 2);
    }
}
