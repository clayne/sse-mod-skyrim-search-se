use win_dbg_logger::output_debug_string;
use std::sync::Mutex;
use anyhow::Context;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref DB: Mutex<rusqlite::Connection> = {
        match init_db().context("init_db error") {
            Ok(db) => Mutex::new(db),
            Err(err) => {
                output_debug_string(format!("{:#}", err).as_str());
                panic!(format!("{:#}", err));
            }
        }
    };
}

const DEBUG: bool = true;

fn init_db() -> anyhow::Result<rusqlite::Connection> {
    let conn = if DEBUG {
        rusqlite::Connection::open("skyrim_search_se.db").context("open error")?
    } else {
        rusqlite::Connection::open("").context("open error")?
    };

    conn.execute_batch(r#"
        PRAGMA mmap_size=268435456;
        PRAGMA synchronous=OFF;
        PRAGMA journal_mode=OFF;
        DROP TABLE IF EXISTS npc;
        CREATE TABLE npc (
            id integer primary key not null,
            edid text unique collate nocase,
            name text collate nocase
        );
        CREATE INDEX npc_edid ON npc (edid);
        CREATE INDEX npc_name ON npc (name);
        "#,
    ).context("init_schema error")?;
    Ok(conn)
}
