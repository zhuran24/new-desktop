fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or("protocol".into()));
    std::fs::create_dir_all(&directory)?;
    for (name, schema) in [
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
