fn main() -> anyhow::Result<()> {
    compile_gresources()?;

    Ok(())
}

fn compile_gresources() -> anyhow::Result<()> {
    glib_build_tools::compile_resources(
        &["data"],
        "data/resources.gresource.xml",
        "compiled.gresource",
    );
    Ok(())
}
