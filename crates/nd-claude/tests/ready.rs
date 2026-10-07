//! 真 CLI 配伪端点：先两个 hello，再 initialize；就绪含 hello 报的后端会话 id。
#![cfg(feature = "scenarios")]
mod support;
use nd_claude::{InitOptions, Readiness};
use nd_mod_proto::{HelloCause, ModName};
use nd_watchdog_proto::Event;
use support::*;

#[tokio::test]
async fn pinned_cli_is_ready_after_both_mods_report_the_preset_session_before_initialize() {
    let fx = Fixture::start("claude-ready").await;
    let claude = fx.claude(fx.config());
    let session = session_id();
    let run = claude
        .open("ready", fx.fresh(&session), InitOptions::default())
        .await
        .unwrap();
    let ready = run.ready();
    assert_eq!(ready.caps.readiness, Readiness::Full);
    assert_eq!(ready.backend_session_id, session);
    for module in ModName::ALL {
        let hello = &ready.hellos[&module];
        assert_eq!(hello.backend_session_id, session, "{module:?}");
        assert_eq!(hello.cause, HelloCause::Start);
        assert_eq!(hello.cli_version, "2.1.289");
    }
    // stdin 上第一行、也是唯一一行是 initialize，写在两个 hello 都到之后。
    let records = fx
        .scenario
        .watchdogs()
        .unwrap()
        .records("ready", 0, 1000)
        .unwrap();
    let inputs: Vec<_> = records
        .iter()
        .filter_map(|r| match &r.event {
            Event::In { line, .. } => Some((
                r.ms,
                serde_json::from_str::<serde_json::Value>(line).unwrap(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(inputs.len(), 1, "{inputs:?}");
    assert_eq!(inputs[0].1["request"]["subtype"], "initialize");
    let hellos_done = run
        .recording()
        .iter()
        .filter(|r| matches!(r.event, nd_claude::ModEvent::Hello { .. }))
        .map(|r| r.ms)
        .max()
        .unwrap();
    assert!(
        inputs[0].0 >= hellos_done,
        "initialize {} before hello {}",
        inputs[0].0,
        hellos_done
    );
    // 回应来自这个 CLI 进程本身，拉起和就绪都不发模型请求。
    assert_eq!(ready.initialize["pid"], ready.identity.pid);
    assert!(
        ready.initialize["models"]
            .as_array()
            .is_some_and(|m| !m.is_empty())
    );
    assert!(fx.scenario.endpoint().requests().is_empty());
    drop(run);
    drop(claude);
    fx.close();
}

fn skills_mod(root: &std::path::Path, name: &str) {
    let dir = root.join("claude/skills").join(name);
    std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(dir.join("hooks")).unwrap();
    std::fs::write(
        dir.join(".claude-plugin/plugin.json"),
        format!(r#"{{"name":"{name}","version":"0.0.1","description":"skills-dir probe","author":{{"name":"test"}}}}"#),
    )
    .unwrap();
    std::fs::write(
        dir.join("hooks/hooks.json"),
        r#"{"modules":["./register.ts"]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("hooks/register.ts"),
        format!("export function register(on: any) {{\n  on('session.start', async ($: any, e: any, next: any) => {{ await $.fs.write('/sandbox/out/loaded-{name}', 'loaded'); return next(e) }})\n}}\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn backend_gets_the_spec_template_without_preload_and_with_the_four_old_mods_disabled() {
    let fx = Fixture::start("claude-template").await;
    let root = fx.scenario.root().to_owned();
    for name in ["codex-direct", "sendnow", "cc-quota", "ultracode-toggle"]
        .iter()
        .chain(&["keep-me", "owner-off"])
    {
        skills_mod(&root, name);
    }
    // owner 自己在用户设置里关掉的 mod 仍然关着：flag 层的 enabledPlugins 按键合并，不整体覆盖。
    std::fs::write(
        root.join("claude/settings.json"),
        r#"{"enabledPlugins":{"owner-off@skills-dir":false}}"#,
    )
    .unwrap();
    let mut config = fx.config();
    // 继承来的预加载必须去掉；留着的话 Bun 找不到脚本，CLI 根本起不来。
    config.env.insert(
        "BUN_OPTIONS".into(),
        "--preload /sandbox/missing-preload.js".into(),
    );
    config
        .env
        .insert("CLAUDE_CODE_PLUGIN_DIR_WATCH".into(), "1".into());
    let claude = fx.claude(config);
    let session = session_id();
    let run = claude
        .open("template", fx.fresh(&session), InitOptions::default())
        .await
        .unwrap();
    let pid = run.ready().identity.pid;
    let cmdline: Vec<String> = std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap()
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let environ: std::collections::BTreeMap<String, String> =
        std::fs::read(format!("/proc/{pid}/environ"))
            .unwrap()
            .split(|b| *b == 0)
            .filter_map(|s| {
                let s = String::from_utf8_lossy(s);
                s.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned()))
            })
            .collect();
    for flag in [
        "--output-format",
        "--input-format",
        "--verbose",
        "--permission-prompt-tool",
        "--replay-user-messages",
        "--include-partial-messages",
        "--session-id",
    ] {
        assert!(
            cmdline.iter().any(|a| a == flag),
            "missing {flag}: {cmdline:?}"
        );
    }
    for absent in [
        "--await-initialize",
        "--setting-sources",
        "--dangerously-skip-permissions",
    ] {
        assert!(!cmdline.iter().any(|a| a == absent), "unexpected {absent}");
    }
    let value_of = |flag: &str| {
        cmdline
            .iter()
            .position(|a| a == flag)
            .map(|i| cmdline[i + 1].clone())
            .unwrap()
    };
    assert_eq!(value_of("--permission-prompt-tool"), "stdio");
    assert_eq!(value_of("--session-id"), session);
    assert_eq!(
        cmdline.iter().filter(|a| *a == "--plugin-dir").count(),
        2,
        "{cmdline:?}"
    );
    let settings: serde_json::Value = serde_json::from_str(&value_of("--settings")).unwrap();
    for module in ["new-desktop", "new-desktop-actions"] {
        assert_eq!(
            settings["pluginConfigs"][module]["options"]["run"],
            "template"
        );
        assert_eq!(
            settings["pluginConfigs"][module]["options"]["sock"],
            root.join("runtime/mod.sock").to_string_lossy().as_ref()
        );
    }
    for (key, value) in [
        ("CLAUDE_CODE_ENABLE_FUNCTION_HOOKS", "1"),
        ("CLAUDE_CODE_FORK_SUBAGENT", "1"),
        ("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", "true"),
        ("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1"),
        ("DISABLE_UPDATES", "1"),
        ("CLAUDE_CODE_PLUGIN_DIR_WATCH", "0"),
    ] {
        assert_eq!(environ.get(key).map(String::as_str), Some(value), "{key}");
    }
    assert!(!environ.contains_key("BUN_OPTIONS"));
    // 旧 mod 和 owner 关掉的都没装上；别的 skills-dir mod 照常装载。
    let loaded = |name: &str| root.join("out").join(format!("loaded-{name}")).exists();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !loaded("keep-me") {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("an unrelated skills-dir mod still loads");
    for name in ["codex-direct", "sendnow", "cc-quota", "ultracode-toggle"]
        .iter()
        .chain(&["owner-off"])
    {
        assert!(!loaded(name), "{name} loaded");
    }
    drop(run);
    drop(claude);
    fx.close();
}

#[tokio::test]
async fn hello_timeout_still_launches_but_only_for_chat_and_without_hook_routed_agents() {
    use nd_claude::{Availability, Feature};
    use nd_testkit::{ModelReply, Route};
    let fx = Fixture::start("claude-chat-only").await;
    // 过不了静态检查的钩子 mod：引擎整个拒载，它永远不会报到。
    std::fs::write(
        fx.scenario.root().join("mods/new-desktop/hooks/register.ts"),
        "export function register(on: any) { on('session.start', async ($: any, e: any, next: any) => { await import('./state.ts'); return next(e) }) }\n",
    )
    .unwrap();
    let mut config = fx.config();
    config.hello_timeout = std::time::Duration::from_secs(3);
    let claude = fx.claude(config);
    let mut init = InitOptions::default();
    init.hook_agents.insert(
        "nd-codex-probe".into(),
        serde_json::json!({"description":"routed by the hook mod","prompt":"never declared without it","model":"nd-codex/gpt"}),
    );
    let session = session_id();
    let mut run = claude
        .open("chat-only", fx.fresh(&session), init)
        .await
        .unwrap();
    let caps = &run.ready().caps;
    let Readiness::ChatOnly { why } = &caps.readiness else {
        panic!("expected chat-only: {:?}", caps.readiness)
    };
    assert!(
        why.contains("new-desktop") && !why.contains("new-desktop-actions"),
        "{why}"
    );
    for feature in Feature::ALL {
        assert!(
            matches!(caps.features[&feature], Availability::Unsupported { .. }),
            "{feature:?}"
        );
    }
    assert!(!run.ready().hellos.contains_key(&ModName::Hook));
    let records = fx
        .scenario
        .watchdogs()
        .unwrap()
        .records("chat-only", 0, 1000)
        .unwrap();
    let init_line = records
        .iter()
        .find_map(|r| match &r.event {
            Event::In { line, .. } => {
                Some(serde_json::from_str::<serde_json::Value>(line).unwrap())
            }
            _ => None,
        })
        .unwrap();
    assert!(init_line["request"].get("agents").is_none(), "{init_line}");
    assert!(
        !run.ready().initialize["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["name"] == "nd-codex-probe")
    );
    // 仍能聊天：一条普通消息得到伪端点的回答。
    fx.scenario
        .endpoint()
        .enqueue(Route::new(None, MODEL), ModelReply::text("CHAT_ONLY_OK"));
    run.write(&serde_json::json!({"type":"user","message":{"role":"user","content":"hi"},"parent_tool_use_id":null,"session_id":session}))
        .await
        .unwrap();
    let frames = run
        .wait_frame(std::time::Duration::from_secs(30), |f| {
            f["type"] == "result"
        })
        .await
        .unwrap();
    assert_eq!(frames.last().unwrap()["result"], "CHAT_ONLY_OK");
    drop(run);
    drop(claude);
    fx.close();
}

#[tokio::test]
async fn a_resumed_backend_is_ready_when_both_mods_report_the_resumed_session() {
    use nd_claude::{Open, Start};
    use nd_testkit::{ModelReply, Route};
    let fx = Fixture::start("claude-resume").await;
    let claude = fx.claude(fx.config());
    let session = session_id();
    {
        let mut first = claude
            .open("first", fx.fresh(&session), InitOptions::default())
            .await
            .unwrap();
        fx.scenario
            .endpoint()
            .enqueue(Route::new(None, MODEL), ModelReply::text("FIRST_TURN"));
        first
            .write(&serde_json::json!({"type":"user","message":{"role":"user","content":"remember this"},"parent_tool_use_id":null,"session_id":session}))
            .await
            .unwrap();
        first
            .wait_frame(std::time::Duration::from_secs(30), |f| {
                f["type"] == "result"
            })
            .await
            .unwrap();
    }
    // 结束第一个后端进程（关 stdin），再用 --resume 拉起同一个后端会话。
    let runs = fx.scenario.watchdogs().unwrap();
    runs.link("first")
        .await
        .unwrap()
        .finish(nd_watchdog_proto::Finish::CloseStdin)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while runs
            .inspect()
            .unwrap()
            .iter()
            .any(|f| f.run == "first" && f.state == "Up")
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let open = Open {
        start: Start::Resume {
            session: session.clone(),
        },
        ..fx.fresh(&session)
    };
    let resumed = claude
        .open("second", open, InitOptions::default())
        .await
        .unwrap();
    assert_eq!(resumed.ready().caps.readiness, Readiness::Full);
    for module in ModName::ALL {
        assert_eq!(resumed.ready().hellos[&module].backend_session_id, session);
    }
    drop(resumed);
    drop(claude);
    fx.close();
}
