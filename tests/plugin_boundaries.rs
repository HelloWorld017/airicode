use std::{fs, path::Path};

fn production_source(path: &Path) -> String {
    let source = fs::read_to_string(path).unwrap();
    source
        .split("#[cfg(test)]")
        .next()
        .unwrap_or(&source)
        .to_owned()
}

#[test]
fn core_never_imports_concrete_features() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for entry in fs::read_dir(root.join("src/core")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = production_source(&path);
        assert!(
            !source.contains("crate::plugins") && !source.contains("crate::providers"),
            "core module {} imports a concrete feature",
            path.display()
        );
    }
}

#[test]
fn feature_plugins_do_not_import_each_other() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for entry in fs::read_dir(root.join("src/plugins")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().and_then(|value| value.to_str()) == Some("mod.rs")
            || path.extension().and_then(|value| value.to_str()) != Some("rs")
        {
            continue;
        }
        let source = production_source(&path);
        assert!(
            !source.contains("crate::plugins") && !source.contains("super::"),
            "plugin module {} imports another concrete plugin",
            path.display()
        );
        assert!(
            source.contains("impl Plugin for"),
            "feature module {} does not implement Plugin",
            path.display()
        );
        assert!(
            source.contains("PluginRegistrar"),
            "feature module {} does not register through Plugin::init",
            path.display()
        );
    }
}

#[tokio::test]
async fn empty_core_has_no_plugin_features() {
    let core = airicode::Core::new().build().await.unwrap();
    assert!(core.plugins().ids().is_empty());
    assert!(core.providers().ids().is_empty());
    assert!(core.tools().ids().is_empty());
    assert!(core.workdir_layers().ids().is_empty());
}
