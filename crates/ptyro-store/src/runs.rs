//! Runs (M7.1), the message ledger (M7.7) and leases (M7.13). The store stays ignorant of the
//! run engine's types: events and the projection arrive as JSON.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Result, Store};

/// A run's row: identity, the pinned recipe and the current projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRow {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub recipe_src: Option<String>,
    pub recipe_sha: Option<String>,
    pub status: String,
    pub state: serde_json::Value,
    pub project: Option<String>,
    pub lead: Option<String>,
    pub forked_from: Option<String>,
    pub fork_step: Option<String>,
    pub created_ms: u64,
    pub ended_ms: Option<u64>,
}

/// One delivery row: a message as one recipient gets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRow {
    pub id: String,
    pub run: Option<String>,
    pub at_ms: u64,
    pub sender: String,
    pub recipient: String,
    pub address: String,
    pub kind: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub priority: String,
    pub hop: u32,
    pub expires_ms: Option<u64>,
    pub delivered_ms: Option<u64>,
    pub seen_ms: Option<u64>,
    pub acked_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRow {
    pub id: String,
    pub holder: String,
    pub pattern: String,
    pub project: Option<String>,
    pub exclusive: bool,
    pub reason: Option<String>,
    pub expires_ms: u64,
    pub created_ms: u64,
}

fn ms(v: Option<i64>) -> Option<u64> {
    v.map(|v| v as u64)
}

fn run_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    let state: String = r.get(6)?;
    Ok(RunRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        name: r.get(2)?,
        recipe_src: r.get(3)?,
        recipe_sha: r.get(4)?,
        status: r.get(5)?,
        state: serde_json::from_str(&state).unwrap_or_default(),
        project: r.get(7)?,
        lead: r.get(8)?,
        forked_from: r.get(9)?,
        fork_step: r.get(10)?,
        created_ms: r.get::<_, i64>(11)? as u64,
        ended_ms: ms(r.get(12)?),
    })
}

const RUN_COLS: &str = "id, kind, name, recipe_src, recipe_sha, status, state, project_id, lead_session, forked_from, fork_step, created_at, ended_at";

fn message_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: r.get(0)?,
        run: r.get(1)?,
        at_ms: r.get::<_, i64>(2)? as u64,
        sender: r.get(3)?,
        recipient: r.get(4)?,
        address: r.get(5)?,
        kind: r.get(6)?,
        body: r.get(7)?,
        in_reply_to: r.get(8)?,
        priority: r.get(9)?,
        hop: r.get::<_, i64>(10)? as u32,
        expires_ms: ms(r.get(11)?),
        delivered_ms: ms(r.get(12)?),
        seen_ms: ms(r.get(13)?),
        acked_ms: ms(r.get(14)?),
    })
}

const MSG_COLS: &str = "id, run_id, at, sender, recipient, address, kind, body, in_reply_to, priority, hop, expires_at, delivered_at, seen_at, acked_at";

impl Store {
    /// Append `events` (`(seq, at, kind, step, payload)`) and write the projection, in one
    /// transaction: the log and the state can never disagree.
    pub fn run_append(
        &self,
        row: RunRow,
        events: Vec<(u64, u64, String, Option<String>, serde_json::Value)>,
    ) {
        self.spawn(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                &format!(
                    "INSERT INTO run ({RUN_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(id) DO UPDATE SET status = ?6, state = ?7, lead_session = ?9, ended_at = ?13"
                ),
                params![
                    row.id,
                    row.kind,
                    row.name,
                    row.recipe_src,
                    row.recipe_sha,
                    row.status,
                    row.state.to_string(),
                    row.project,
                    row.lead,
                    row.forked_from,
                    row.fork_step,
                    row.created_ms as i64,
                    row.ended_ms.map(|m| m as i64),
                ],
            )?;
            for (seq, at, kind, step, payload) in events {
                tx.execute(
                    "INSERT OR IGNORE INTO run_event (run_id, seq, at, kind, step_id, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![row.id, seq as i64, at as i64, kind, step, payload.to_string()],
                )?;
            }
            tx.commit()?;
            Ok(())
        });
    }

    /// Runs, newest first (`active`: only running or waiting ones).
    pub async fn runs(&self, active: bool, limit: usize) -> Result<Vec<RunRow>> {
        self.call(move |c| {
            let mut st = c.prepare(&format!(
                "SELECT {RUN_COLS} FROM run WHERE ?1 = 0 OR status IN ('running', 'waiting')
                 ORDER BY created_at DESC LIMIT ?2"
            ))?;
            Ok(st
                .query_map(params![i64::from(active), limit as i64], run_row)?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub async fn run(&self, id: String) -> Result<Option<RunRow>> {
        self.call(move |c| {
            Ok(c.query_row(
                &format!("SELECT {RUN_COLS} FROM run WHERE id = ?1"),
                params![id],
                run_row,
            )
            .optional()?)
        })
        .await
    }

    /// A run's events in order: `(seq, at, payload)`.
    pub async fn run_events(&self, id: String) -> Result<Vec<(u64, u64, serde_json::Value)>> {
        self.call(move |c| {
            let mut st =
                c.prepare("SELECT seq, at, payload FROM run_event WHERE run_id = ?1 ORDER BY seq")?;
            let rows = st
                .query_map(params![id], |r| {
                    let p: String = r.get(2)?;
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get::<_, i64>(1)? as u64,
                        serde_json::from_str(&p).unwrap_or_default(),
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
    }

    pub fn message_put(&self, rows: Vec<MessageRow>) {
        self.spawn(move |c| {
            let tx = c.transaction()?;
            for m in rows {
                tx.execute(
                    &format!(
                        "INSERT OR REPLACE INTO run_message ({MSG_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
                    ),
                    params![
                        m.id,
                        m.run,
                        m.at_ms as i64,
                        m.sender,
                        m.recipient,
                        m.address,
                        m.kind,
                        m.body,
                        m.in_reply_to,
                        m.priority,
                        i64::from(m.hop),
                        m.expires_ms.map(|v| v as i64),
                        m.delivered_ms.map(|v| v as i64),
                        m.seen_ms.map(|v| v as i64),
                        m.acked_ms.map(|v| v as i64),
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        });
    }

    /// Messages not acknowledged and not expired, oldest first (the daemon's working set).
    pub async fn messages_open(&self) -> Result<Vec<MessageRow>> {
        self.call(|c| {
            let now = ptyro_core::now_ms() as i64;
            let mut st = c.prepare(&format!(
                "SELECT {MSG_COLS} FROM run_message WHERE acked_at IS NULL AND (expires_at IS NULL OR expires_at > ?1)
                 ORDER BY at LIMIT 5000"
            ))?;
            Ok(st.query_map(params![now], message_row)?.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// The ledger of a run (or every message), oldest first.
    pub async fn messages(&self, run: Option<String>, limit: usize) -> Result<Vec<MessageRow>> {
        self.call(move |c| {
            let mut st = c.prepare(&format!(
                "SELECT {MSG_COLS} FROM run_message WHERE ?1 IS NULL OR run_id = ?1 ORDER BY at DESC LIMIT ?2"
            ))?;
            let mut v: Vec<MessageRow> =
                st.query_map(params![run, limit as i64], message_row)?.collect::<rusqlite::Result<_>>()?;
            v.reverse();
            Ok(v)
        })
        .await
    }

    pub fn lease_save(&self, l: LeaseRow) {
        self.spawn(move |c| {
            c.execute(
                "INSERT OR REPLACE INTO lease (id, holder, pattern, project_id, exclusive, reason, expires_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    l.id,
                    l.holder,
                    l.pattern,
                    l.project,
                    i64::from(l.exclusive),
                    l.reason,
                    l.expires_ms as i64,
                    l.created_ms as i64
                ],
            )?;
            Ok(())
        });
    }

    pub fn lease_delete(&self, id: String) {
        self.spawn(move |c| {
            c.execute("DELETE FROM lease WHERE id = ?1", params![id])?;
            Ok(())
        });
    }

    /// Leases that haven't expired.
    pub async fn leases(&self) -> Result<Vec<LeaseRow>> {
        self.call(|c| {
            let now = ptyro_core::now_ms() as i64;
            c.execute("DELETE FROM lease WHERE expires_at <= ?1", params![now])?;
            let mut st = c.prepare(
                "SELECT id, holder, pattern, project_id, exclusive, reason, expires_at, created_at FROM lease ORDER BY created_at",
            )?;
            Ok(st
                .query_map([], |r| {
                    Ok(LeaseRow {
                        id: r.get(0)?,
                        holder: r.get(1)?,
                        pattern: r.get(2)?,
                        project: r.get(3)?,
                        exclusive: r.get::<_, i64>(4)? != 0,
                        reason: r.get(5)?,
                        expires_ms: r.get::<_, i64>(6)? as u64,
                        created_ms: r.get::<_, i64>(7)? as u64,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_events_messages_and_leases_round_trip() {
        let s = Store::open_in_memory().unwrap();
        let row = RunRow {
            id: "r-1".into(),
            kind: "recipe".into(),
            name: "x".into(),
            recipe_src: Some("name = \"x\"".into()),
            recipe_sha: Some("abc".into()),
            status: "running".into(),
            state: serde_json::json!({"seq": 1}),
            project: None,
            lead: None,
            forked_from: None,
            fork_step: None,
            created_ms: 1,
            ended_ms: None,
        };
        s.run_append(
            row.clone(),
            vec![(1, 1, "created".into(), None, serde_json::json!({"e": "created"}))],
        );
        let mut done = row.clone();
        done.status = "succeeded".into();
        done.ended_ms = Some(5);
        s.run_append(done, vec![(2, 5, "ended".into(), None, serde_json::json!({"e": "ended"}))]);
        assert_eq!(s.runs(true, 10).await.unwrap().len(), 0);
        let all = s.runs(false, 10).await.unwrap();
        assert_eq!(all[0].status, "succeeded");
        assert_eq!(s.run_events("r-1".into()).await.unwrap().len(), 2);
        assert!(s.run("r-1".into()).await.unwrap().is_some());

        let m = MessageRow {
            id: "01".into(),
            run: Some("r-1".into()),
            at_ms: 1,
            sender: "@user".into(),
            recipient: "s1".into(),
            address: "@all".into(),
            kind: "note".into(),
            body: "hi".into(),
            in_reply_to: None,
            priority: "normal".into(),
            hop: 0,
            expires_ms: None,
            delivered_ms: None,
            seen_ms: None,
            acked_ms: None,
        };
        s.message_put(vec![m.clone()]);
        assert_eq!(s.messages_open().await.unwrap(), vec![m.clone()]);
        s.message_put(vec![MessageRow { acked_ms: Some(2), ..m }]);
        assert!(s.messages_open().await.unwrap().is_empty());
        assert_eq!(s.messages(Some("r-1".into()), 10).await.unwrap().len(), 1);

        let now = ptyro_core::now_ms();
        s.lease_save(LeaseRow {
            id: "l1".into(),
            holder: "s1".into(),
            pattern: "src/**".into(),
            project: None,
            exclusive: true,
            reason: None,
            expires_ms: now + 60_000,
            created_ms: now,
        });
        s.lease_save(LeaseRow {
            id: "l2".into(),
            holder: "s1".into(),
            pattern: "old".into(),
            project: None,
            exclusive: true,
            reason: None,
            expires_ms: 1,
            created_ms: 1,
        });
        let l = s.leases().await.unwrap();
        assert_eq!(l.len(), 1, "expired leases are dropped");
        s.lease_delete("l1".into());
        assert!(s.leases().await.unwrap().is_empty());
    }
}
