//! 两个 mod 过钉住 CLI 的静态检查（`claude plugin validate`），且动作 mod 只挂 session.start。
#![cfg(feature = "scenarios")]
use nd_testkit::{Program, Scenario, ScenarioOptions};
use std::time::Duration;

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "types" {
                copy_dir(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn both_mods_pass_the_pinned_cli_static_check_with_their_multi_file_layout() {
    let mut scenario = Scenario::start(ScenarioOptions::new(
        "mod-validate",
        std::env::var_os("ND_TEST_DAEMON").unwrap(),
    ))
    .await
    .unwrap();
    let mods = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods");
    let mut listed = vec![];
    for module in ["new-desktop", "new-desktop-actions"] {
        copy_dir(
            &mods.join(module),
            &scenario.root().join("mods").join(module),
        );
        let process = scenario
            .spawn(
                &format!("validate-{}", module.len()),
                Program::claude().args(["plugin", "validate", &format!("/sandbox/mods/{module}")]),
            )
            .unwrap();
        let code = process.wait(Duration::from_secs(60)).await.unwrap();
        let out = process.stdout().unwrap();
        assert_eq!(code, 0, "{module}: {out}{}", process.stderr().unwrap());
        assert!(out.contains("Validation passed"), "{module}: {out}");
        assert!(!out.contains("warning"), "{module}: {out}");
        listed.push(out);
    }
    let hooks = |out: &str| {
        out.lines()
            .find_map(|l| {
                l.split_once("register.ts hooks: ")
                    .map(|(_, h)| h.trim().to_owned())
            })
            .unwrap_or_default()
            .split(", ")
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        hooks(&listed[0]),
        ["classic.SessionStart", "session.end", "session.start"]
            .map(str::to_owned)
            .into()
    );
    assert_eq!(hooks(&listed[1]), ["session.start".to_owned()].into());
    scenario.close().unwrap();
}

/// N5 的边界：钩子可以从本 mod 的其他文件导入，但 `$` 不能跨文件传；
/// 所以每个能力文件自带用 `$` 的辅助函数，共享状态文件里不碰 `$`。
#[tokio::test]
async fn the_static_check_refuses_passing_dollar_across_an_import() {
    let mut scenario = Scenario::start(ScenarioOptions::new(
        "mod-dollar",
        std::env::var_os("ND_TEST_DAEMON").unwrap(),
    ))
    .await
    .unwrap();
    let probe = scenario.root().join("mods/probe");
    copy_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/new-desktop"),
        &probe,
    );
    std::fs::write(
        probe.join("hooks/helper.ts"),
        "export async function helper($: any) { await $.session.id() }\n",
    )
    .unwrap();
    std::fs::write(
        probe.join("hooks/register.ts"),
        "import { helper } from './helper.ts'\nexport function register(on: any) {\n  on('session.start', async ($: any, e: any, next: any) => { await helper($); return next(e) })\n}\n",
    )
    .unwrap();
    let process = scenario
        .spawn(
            "validate-probe",
            Program::claude().args(["plugin", "validate", "/sandbox/mods/probe"]),
        )
        .unwrap();
    assert_ne!(process.wait(Duration::from_secs(60)).await.unwrap(), 0);
    let out = process.stdout().unwrap();
    assert!(out.contains("never across an import"), "{out}");
    scenario.close().unwrap();
}
