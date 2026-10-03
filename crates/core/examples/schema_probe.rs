// Introspect the port's actual DB schema after all migrations.
use spp_core::db;
use std::path::Path;

fn main() {
    let dir = std::env::temp_dir().join("spp-schema-probe");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("probe.db");
    let _ = std::fs::remove_file(&path);
    let mut conn = db::open(&path).unwrap();
    db::ensure_migrations_table(&conn).unwrap();
    spp_core::migrations::run_all(&mut conn).unwrap();
    // The same chain the boot sites run.
    let _ = spp_core::search::apply_fts_migration(&conn);
    let _ = spp_core::activity::apply_m003(&conn);
    let _ = spp_core::ticket_states::apply_m004(&conn);
    let _ = spp_core::notifications::apply_m005(&conn);
    let _ = spp_core::side_threads::apply_m006(&conn);
    let _ = spp_core::automation::apply_m007(&conn);
    let _ = spp_core::embeddings::apply_m008(&conn);
    let _ = spp_core::ai_center::apply_m009(&conn);
    let _ = spp_core::ai_analysis::apply_m010(&conn);
    let _ = spp_core::ai_features::apply_m011_to_m013(&conn);
    let _ = spp_core::intelligence::apply_m014(&conn);
    let _ = spp_core::intelligence_features::apply_m015_to_m019(&conn);
    let _ = spp_core::reports::apply_m020_to_m022(&conn);
    let _ = spp_core::outreach::apply_m023_to_m025(&conn);
    let _ = spp_core::data_tools::apply_m026_to_m027(&conn);
    let _ = spp_core::inbox::apply_m028(&conn);
    let _ = spp_core::sync_schema::apply_m029(&conn);
    let _ = spp_core::conversation_ops::apply_m030(&conn);
    let _ = spp_core::outreach::apply_m031(&conn);
    let _ = spp_core::ticket_states::apply_m032(&conn);
    let _ = spp_core::ai_attributes::apply_m033(&conn);
    let _ = spp_core::reports::apply_m034(&conn);
    let _ = spp_core::intelligence_features::apply_m035(&conn);
    let _ = spp_core::customer_events::apply_m036(&conn);
    let _ = spp_core::maintenance::apply_m037(&conn);
    let _ = spp_core::connectors::apply_m038(&conn);
    let _ = spp_core::mirror_tables::apply_m039(&conn);
    let _ = spp_core::db_breadth::apply_m040(&conn);

    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    println!("TABLES ({}):", tables.len());
    for t in &tables {
        println!("  {t}");
    }
    println!();
    for t in &[
        "conversations",
        "conversation_threads",
        "customers",
        "ratings",
        "outreach_recipients",
        "incident_conversations",
        "custom_object_links",
        "custom_objects",
        "issue_clusters",
        "issue_cluster_conversations",
        "known_issue_conversations",
        "conversation_tags",
        "tags",
        "incidents",
    ] {
        if let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({t})")) {
            let cols: Vec<String> = stmt
                .query_map([], |r| {
                    Ok(format!(
                        "{} {}",
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?
                    ))
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            println!("{t}: {}", cols.join(", "));
        } else {
            println!("{t}: MISSING");
        }
    }
    let _ = Path::new(&path);
}
