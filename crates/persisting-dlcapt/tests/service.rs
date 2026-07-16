use persisting_dlcapt::config::{ExportConfig, ModelRoute, ProxyConfig, StorageConfig};

fn config(public: &str, admin: &str) -> ProxyConfig {
    ProxyConfig {
        listen: public.into(),
        admin_listen: admin.into(),
        store_dir: tempfile::tempdir().unwrap().keep().display().to_string(),
        agent_id: "service-test".into(),
        session_header: "x-persisting-session-id".into(),
        session_header_aliases: vec![],
        default_session_id: "default".into(),
        preserve_raw: false,
        base_session_path: "/v1/sessions".into(),
        storage: StorageConfig::default(),
        export: ExportConfig::default(),
        models: vec![ModelRoute {
            name: "*".into(),
            display_name: None,
            provider: "openai".into(),
            upstream_base_url: "https://example.invalid/v1".into(),
            api_key: Some(String::new()),
        }],
    }
}

#[tokio::test]
async fn serve_returns_error_when_admin_address_is_already_bound() {
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = occupied.local_addr().unwrap().to_string();
    let error = persisting_dlcapt::serve(config("127.0.0.1:0", &address))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("failed binding admin listener"));
}
