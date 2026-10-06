//! 重新生成 `protocol/mod.schema.json` 与两个 mod 的 `hooks/proto.ts`。参数：仓库根目录。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or(".".into()));
    std::fs::create_dir_all(root.join("protocol"))?;
    std::fs::write(
        root.join("protocol/mod.schema.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&nd_mod_proto::schema())?
        ),
    )?;
    for module in nd_mod_proto::ModName::ALL {
        let hooks = root.join("mods").join(module.as_str()).join("hooks");
        std::fs::create_dir_all(&hooks)?;
        std::fs::write(hooks.join("proto.ts"), nd_mod_proto::typescript_module())?;
    }
    Ok(())
}
