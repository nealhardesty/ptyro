//! Schema migrations, counted by `PRAGMA user_version` (SPEC §14.2): migration `n` brings the
//! schema to version `n + 1`. Every pending one runs in one transaction with the version it
//! reaches, so a failure leaves the database as it was.

use rusqlite::Connection;

use crate::{Result, StoreError};

/// One step of a schema's history: SQL run once, in order.
pub struct M(&'static str);

impl M {
    pub const fn up(sql: &'static str) -> Self {
        Self(sql)
    }
}

/// Bring `conn`'s schema up to the last of `migrations`. A schema newer than they know is
/// refused untouched (callers say so first, with the file's path).
pub fn to_latest(conn: &mut Connection, migrations: &[M]) -> Result<()> {
    let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let known = migrations.len();
    let Ok(from) = usize::try_from(found) else {
        return Err(StoreError::TooNew { path: "the database".into(), found, known });
    };
    if from > known {
        return Err(StoreError::TooNew { path: "the database".into(), found, known });
    }
    if from == known {
        return Ok(());
    }
    let tx = conn.transaction()?;
    for m in &migrations[from..] {
        tx.execute_batch(m.0)?;
    }
    tx.pragma_update(None, "user_version", known as i64)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &[M] = &[M::up("CREATE TABLE a (x);"), M::up("ALTER TABLE a ADD COLUMN y;")];

    fn version(c: &Connection) -> i64 {
        c.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap()
    }

    #[test]
    fn each_migration_runs_once_in_order() {
        let mut c = Connection::open_in_memory().unwrap();
        to_latest(&mut c, &TWO[..1]).unwrap();
        assert_eq!(version(&c), 1);
        to_latest(&mut c, TWO).unwrap();
        to_latest(&mut c, TWO).unwrap();
        assert_eq!(version(&c), 2);
        c.execute("INSERT INTO a (x, y) VALUES (1, 2)", []).unwrap();
    }

    #[test]
    fn a_failure_changes_nothing_and_a_newer_schema_is_refused() {
        let mut c = Connection::open_in_memory().unwrap();
        let bad = &[M::up("CREATE TABLE a (x);"), M::up("NOT SQL;")];
        assert!(to_latest(&mut c, bad).is_err());
        assert_eq!(version(&c), 0);
        assert!(c.prepare("SELECT x FROM a").is_err(), "the first one was rolled back too");
        c.pragma_update(None, "user_version", 5).unwrap();
        assert!(matches!(to_latest(&mut c, TWO), Err(StoreError::TooNew { found: 5, .. })));
    }
}
