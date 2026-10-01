use std::path::Path;

pub(crate) fn setup(path: &Path) -> std::io::Result<()> {
    let script = std::fs::read_to_string(path)?;
    let patched = script.replace(
        r#"printf '%s\n' '{"answer":"default response"}'"#,
        "sleep 30",
    );
    std::fs::write(path, patched)
}

pub(crate) fn seed_telemetry_test(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, "printf '%s\\n' '{\"answer\":\"Default response\"}'\n")?;
    setup(path)
}
