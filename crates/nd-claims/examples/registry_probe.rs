//! Offline fixture runner only. See tests/cli/generate.py; no default user paths.
use nd_claims::*;
use std::{path::PathBuf, sync::Arc};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let root = PathBuf::from(&args[1]);
    let pid: u32 = args[2].parse()?;
    let bs = BackendSessionId::claude(&args[3]);
    let duplicate = args[4] == "duplicate";
    let identity = Identity::read(pid).map_err(|e| e.to_string())?;
    let store = Arc::new(nd_store::Store::open(
        root.join(format!("probe-{}.sqlite", args[4])),
        2,
    )?);
    let commands = Arc::new(PinnedCli {
        executable: "/cli".into(),
        env: std::env::vars().collect(),
        timeout: std::time::Duration::from_secs(5),
    });
    let claims =
        Exclusivity::open_with_commands(store, RegistryConfig::new(root.join("config")), commands)?;
    let write = Act::Write {
        session: "fixture".into(),
        bs: bs.clone(),
    };
    assert_eq!(claims.peek(&write)?, Admit::Wait(Obstacle::Recovering));
    claims.observe(Observed::Recovered)?;
    claims.refresh()?;
    assert!(matches!(
        claims.peek(&write)?,
        Admit::Wait(Obstacle::ExternalWriter(_))
    ));
    claims.observe(Observed::Up {
        run: "fixture-run".into(),
        identity,
        generation: 1,
        kind: BackendKind::Claude,
    })?;
    claims.observe(Observed::Holding {
        run: "fixture-run".into(),
        generation: 1,
        at: 1,
        now: vec![Held {
            bs,
            session: "fixture".into(),
            last_leaf: None,
        }],
    })?;
    let _list = claims.want_list();
    claims.refresh()?;
    assert!(claims.externals()?.is_empty());
    if duplicate {
        assert!(matches!(
            claims.peek(&write)?,
            Admit::Wait(Obstacle::ExternalWriter(_))
        ));
    } else {
        assert_eq!(
            claims.peek(&write)?,
            Admit::Go(Pass {
                route: Route::Live("fixture-run".into())
            })
        );
    }
    println!("{}: PASS", args[4]);
    Ok(())
}
