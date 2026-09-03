use std::sync::Arc;

use airicode::{
    Result,
    core::{CoreBuilder, project_from_path},
    plugins::{
        OpenAiProviderPlugin, PersistencePlugin, ToolGrepPlugin, ToolPatchHashlinePlugin,
        ToolPatchPlugin, ToolReadPlugin,
    },
};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn openai_provider_is_created_by_config_read() -> Result<()> {
    let environment_name = "AIRICODE_TEST_OPENAI_KEY";
    unsafe { std::env::set_var(environment_name, "test-key") };
    let plugin = Arc::new(OpenAiProviderPlugin::new());
    assert!(plugin.provider().is_none());

    let core = CoreBuilder::new()
        .config(json!({
            "plugins": {
                "provider_openai": {
                    "api_key_env": environment_name,
                    "base_url": "https://example.test/v1/"
                }
            }
        }))
        .plugin(plugin.clone())
        .build()
        .await;
    unsafe { std::env::remove_var(environment_name) };
    let core = core?;

    assert!(plugin.provider().is_some());
    assert!(core.registry().provider(plugin.provider_id()).is_some());
    Ok(())
}

#[tokio::test]
async fn persistence_store_is_created_by_open_project() -> Result<()> {
    let project_directory = tempdir()?;
    let project = project_from_path(project_directory.path().to_path_buf());
    let plugin = Arc::new(PersistencePlugin::new());

    let core = CoreBuilder::new()
        .project(project.clone())
        .plugin(plugin.clone())
        .build()
        .await?;

    assert_eq!(core.project()?, project);
    assert!(plugin.store().is_some());
    assert!(core.registry().session_store().is_some());
    assert!(plugin.discover().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn hashline_config_selects_hashline_tools() -> Result<()> {
    let core = CoreBuilder::new()
        .config(json!({ "tool": { "enable_hashline": true } }))
        .plugin(Arc::new(ToolReadPlugin::new()))
        .plugin(Arc::new(ToolGrepPlugin::new()))
        .plugin(Arc::new(ToolPatchPlugin::new()))
        .plugin(Arc::new(ToolPatchHashlinePlugin::new()))
        .build()
        .await?;
    let names = core
        .registry()
        .tools()
        .into_iter()
        .map(|tool| tool.definition().name)
        .collect::<Vec<_>>();
    assert!(names.contains(&"read".into()));
    assert!(names.contains(&"grep".into()));
    assert!(names.contains(&"patch_hashline".into()));
    assert!(!names.contains(&"patch".into()));
    Ok(())
}
