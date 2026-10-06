//! 会话外查询模型目录：短命辅助进程留在守护进程 cgroup，不发送人类提示。
use crate::{Claude, Open, Start, launch_spec};
use nd_claims::{BackendKind, Exclusivity, GoneHow, Identity, Observed};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub(crate) async fn query(
    claude: &Claude,
    claims: &Exclusivity,
    generation: u64,
    cwd: PathBuf,
) -> crate::Result<Vec<nd_wire::Model>> {
    let session = uuid::Uuid::new_v4().to_string();
    let run = format!("models-{session}");
    let spec = launch_spec(
        claude.config(),
        &run,
        &Open {
            start: Start::Fresh {
                session: session.clone(),
            },
            cwd,
            model: None,
            permission_mode: None,
        },
    );
    claude.channel().register(&run, &session);
    let result = async {
        let mut child = tokio::process::Command::new(&spec.argv[0])
            .args(&spec.argv[1..])
            .arg("--no-session-persistence")
            .env_clear()
            .envs(&spec.env)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let identity = Identity::read(child.id().ok_or("helper already exited")?)?;
        let result = async {
            claims.observe(Observed::Up {
                run: run.clone(),
                identity: identity.clone(),
                generation,
                kind: BackendKind::Claude,
            })?;
            claude
                .channel()
                .wait_binding(&run, claude.config().hello_timeout, |b| b.mods.len() == 2)
                .await;
            let frame = json!({"type":"control_request", "request_id":"models", "request":{
                "subtype":"initialize", "perTaskStopAffordance":true, "forwardSubagentText":true
            }});
            let mut stdin = child.stdin.take().ok_or("helper stdin missing")?;
            stdin.write_all(format!("{frame}\n").as_bytes()).await?;
            let stdout = child.stdout.take().ok_or("helper stdout missing")?;
            let mut lines = BufReader::new(stdout.take(4 * 1024 * 1024)).lines();
            while let Some(line) = lines.next_line().await? {
                let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if frame["type"] != "control_response"
                    || frame["response"]["request_id"] != "models"
                {
                    continue;
                }
                if frame["response"]["subtype"] != "success" {
                    return Err("模型目录初始化失败".into());
                }
                let response = &frame["response"]["response"];
                let values = response["models"].as_array().ok_or("后端未提供模型列表")?;
                let mut models = Vec::new();
                for (values, unavailable) in [
                    (Some(values), false),
                    (response["unavailable_models"].as_array(), true),
                ] {
                    for model in values.into_iter().flatten() {
                        let value = model["value"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or("后端模型缺少 value")?;
                        models.push(nd_wire::Model {
                            value: value.into(),
                            label: model["displayName"].as_str().unwrap_or(value).into(),
                            description: model["description"].as_str().unwrap_or_default().into(),
                            disabled: unavailable || model["disabled"].as_bool().unwrap_or(false),
                        });
                    }
                }
                return Ok(models);
            }
            Err("辅助进程没有返回模型列表".into())
        };
        let result = tokio::time::timeout(
            claude.config().hello_timeout + claude.config().init_timeout,
            result,
        )
        .await;
        // 只杀这次创建的辅助进程。没有提示、工具或会话写入责任。
        child.kill().await?;
        claims.observe(Observed::Gone {
            run: run.clone(),
            identity: Some(identity),
            how: GoneHow::Exited,
        })?;
        result.map_err(|_| "模型列表查询超时")?
    }
    .await;
    claude.channel().unregister(&run);
    result
}
