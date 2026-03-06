/// Pre-extracts the bundled GraphQLite extension to the shared temp directory.
///
/// Nextest runs each test as a separate process. Without pre-extraction, multiple
/// processes race to write the `.so`/`.dylib` to `/tmp/graphqlite/`, causing
/// "file too short" errors when one process loads a partially-written file.
///
/// Running this as a nextest setup script ensures the extension is fully written
/// before any test process tries to load it.
fn main() {
    let conn = graphqlite::Connection::open_in_memory()
        .expect("failed to pre-extract GraphQLite extension");

    // Verify the Cypher extension is actually functional, not just the SQLite connection.
    // This catches silent extraction failures where open_in_memory succeeds but the
    // extension was not properly loaded.
    conn.cypher("CREATE (n:_Probe {ok: 1})")
        .expect("GraphQLite Cypher extension not functional after pre-extraction");

    println!("graphqlite extension pre-extracted and verified");
}
