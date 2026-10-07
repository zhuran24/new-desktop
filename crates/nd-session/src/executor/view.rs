use super::*;

/// `invoke/<命令 id>` 条目：种类、内容、状态，再并上结果字段。
pub(super) fn invoke_shown(invoke: &Invoke, state: &str, extra: Value) -> Shown {
    let mut data = match &invoke.invocation {
        Invocation::Shell { command } => json!({"command": command}),
        Invocation::ForkAgent { prompt } => json!({"prompt": prompt}),
        Invocation::Compact { scope, .. } => json!({
            "scope": match scope { CompactScope::From => "from", CompactScope::UpTo => "up_to" },
            "message": invoke.message,
        }),
        // 改标题、生成标题不是收据等结果的命令，不进这里。
        Invocation::Title { .. } | Invocation::GenerateTitle { .. } => json!({}),
    };
    data["invoke"] = json!(invoke.id);
    data["state"] = json!(state);
    if let Value::Object(extra) = extra {
        for (k, v) in extra {
            if !v.is_null() {
                data[k] = v;
            }
        }
    }
    Shown::Invoke {
        id: invoke.id.clone(),
        kind: invoke.kind().into(),
        data,
    }
}

pub(super) fn intent_name(intent: Intent) -> &'static str {
    match intent {
        Intent::Fold => "fold",
        Intent::AfterTurn => "after_turn",
        Intent::Interrupting => "interrupting",
    }
}

pub(crate) fn header(core: &Core) -> Value {
    let meta = core.meta();
    let carrier = core
        .current_carrier()
        .or_else(|| core.carriers.values().next());
    json!({
        "session": meta.id,
        "status": meta.status.as_str(),
        "created_by": meta.created_by,
        "cwd": meta.cwd,
        "backend": format!("{:?}", meta.kind).to_lowercase(),
        "model": meta.model,
        "title":meta.title.text,
        "title_source":meta.title.source,
        "title_revision":meta.title.revision,
        "settings_revision":meta.settings_revision,
        "permission_mode": meta.permission_mode,
        "pending_setting": core.ops.values().find(|op| matches!(op.spec,OpSpec::Configure(_))).map(|op| if matches!(&op.spec, OpSpec::Configure(op) if matches!(op.setting, nd_wire::LiveSetting::Model(_))) && carrier.is_some_and(|c| c.turn_running) {"模型将在本回合结束后生效"} else {"正在应用设置"}),
        "settings": meta.settings,
        "caps": nd_backend::session_capabilities(&meta.kind, &meta.settings.caps),
        "note": meta.note,
        "irreversible": meta.irreversible,
        "process": carrier.map(|c| json!({
            "carrier": c.id,
            "backend_session": c.bs.id,
            "run": c.run,
            "alive": c.alive,
            "readiness": c.readiness,
            "turn_running": c.turn_running,
            "turn": c.turn,
            "interrupt_scope": "中断前核对回合；CLI 没有原子期望回合参数，核对与写入之间仍有竞态",
            "drain": c.drain,
            "features": c.features,
        })),
        "interaction": carrier.map(|c| &c.interaction),
        // 只能聊天的降级进程：会话头写明原因和这时用不了的功能（端口能力表给的名字）。
        "degraded": carrier.and_then(|c| match &c.readiness {
            Some(nd_backend::Readiness::ChatOnly { why }) => Some(json!({
                "why": why,
                "unavailable": c.features.iter().filter(|f| !f.available).map(|f| f.label.clone()).collect::<Vec<_>>(),
            })),
            _ => None,
        }),
        "op": core.ops.values().next().map(|op| json!({"op": op.id, "kind": op.spec.kind(), "phase": op.phase.name()})),
    })
}
