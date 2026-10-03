fn main() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let v: String = conn
        .query_row("SELECT json_object('a', 1, 'b', 'x')", [], |r| r.get(0))
        .unwrap();
    println!("json_object works: {v}");
    let v2: Result<String, _> = conn.query_row(
        "SELECT json_object('conversation_id', id, 'subject', s) FROM (SELECT 5 AS id, 'has \"quotes\"' AS s)",
        [], |r| r.get(0),
    );
    println!("nested: {:?}", v2);
}
