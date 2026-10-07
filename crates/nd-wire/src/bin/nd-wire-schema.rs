fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or("protocol".into()));
    std::fs::create_dir_all(&directory)?;
    for (name, schema) in [
        (
            "configure-session.schema.json",
            schemars::schema_for!(nd_wire::ConfigureSession),
        ),
        (
            "rename-session.schema.json",
            schemars::schema_for!(nd_wire::RenameSession),
        ),
        (
            "settings-expected.schema.json",
            schemars::schema_for!(nd_wire::SettingsExpected),
        ),
        (
            "title-expected.schema.json",
            schemars::schema_for!(nd_wire::TitleExpected),
        ),
        ("page.schema.json", schemars::schema_for!(nd_wire::Page)),
        ("draft.schema.json", schemars::schema_for!(nd_wire::Draft)),
        (
            "draft-update.schema.json",
            schemars::schema_for!(nd_wire::DraftUpdate),
        ),
        (
            "draft-expected.schema.json",
            schemars::schema_for!(nd_wire::DraftExpected),
        ),
        (
            "draft-updated.schema.json",
            schemars::schema_for!(nd_wire::DraftUpdated),
        ),
        (
            "shell-args.schema.json",
            schemars::schema_for!(nd_wire::ShellArgs),
        ),
        (
            "subtask-args.schema.json",
            schemars::schema_for!(nd_wire::SubtaskArgs),
        ),
        (
            "compact-args.schema.json",
            schemars::schema_for!(nd_wire::CompactArgs),
        ),
        (
            "invoked.schema.json",
            schemars::schema_for!(nd_wire::Invoked),
        ),
        (
            "attachments.schema.json",
            schemars::schema_for!(Vec<nd_wire::Attachment>),
        ),
        (
            "models.schema.json",
            schemars::schema_for!(Vec<nd_wire::Model>),
        ),
        (
            "request.schema.json",
            schemars::schema_for!(nd_wire::Request),
        ),
        (
            "response.schema.json",
            schemars::schema_for!(nd_wire::Response),
        ),
    ] {
        std::fs::write(
            directory.join(name),
            format!("{}\n", serde_json::to_string_pretty(&schema)?),
        )?;
    }
    Ok(())
}
