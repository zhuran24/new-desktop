//! Indexed open reservations and exited run identities; writes need neither.
use super::*;
use nd_store::OptionalExtension;

#[derive(Serialize, Deserialize)]
pub(super) struct Reservation {
    pub act: Act,
    pub grant: Option<Grant>,
}

pub(super) fn migrate(tx: &Tx<'_>, state: &State) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS claims_open (cause TEXT PRIMARY KEY, via TEXT NOT NULL, body TEXT NOT NULL); CREATE INDEX IF NOT EXISTS claims_open_run ON claims_open(via); CREATE TABLE IF NOT EXISTS claims_gone (run TEXT PRIMARY KEY);")?;
    for (cause, act) in &state.causes {
        if let Act::Open { via, .. } = act {
            let row = Reservation {
                act: act.clone(),
                grant: state.grants.get(cause).cloned(),
            };
            tx.execute(
                "INSERT OR IGNORE INTO claims_open VALUES (?1,?2,?3)",
                params![cause, via, serde_json::to_string(&row).map_err(error)?],
            )?;
        }
    }
    for run in &state.gone {
        gone(tx, run)?;
    }
    Ok(())
}
pub(super) fn get(tx: &Tx<'_>, cause: &str) -> Result<Option<Reservation>> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM claims_open WHERE cause=?1",
            [cause],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|body| serde_json::from_str(&body).map_err(error))
        .transpose()
}
pub(super) fn put(tx: &Tx<'_>, cause: &str, act: &Act, grant: Option<Grant>) -> Result<()> {
    let Act::Open { via, .. } = act else {
        return Ok(());
    };
    let body = serde_json::to_string(&Reservation {
        act: act.clone(),
        grant,
    })
    .map_err(error)?;
    tx.execute("INSERT INTO claims_open VALUES (?1,?2,?3) ON CONFLICT(cause) DO UPDATE SET body=excluded.body", params![cause,via,body])?;
    Ok(())
}
pub(super) fn remove(tx: &Tx<'_>, cause: &str) -> Result<()> {
    tx.execute("DELETE FROM claims_open WHERE cause=?1", [cause])?;
    Ok(())
}
pub(super) fn is_gone(tx: &Tx<'_>, run: &str) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM claims_gone WHERE run=?1)",
        [run],
        |r| r.get(0),
    )?)
}
pub(super) fn gone(tx: &Tx<'_>, run: &str) -> Result<()> {
    tx.execute("INSERT OR IGNORE INTO claims_gone VALUES (?1)", [run])?;
    tx.execute("DELETE FROM claims_open WHERE via=?1", [run])?;
    Ok(())
}
