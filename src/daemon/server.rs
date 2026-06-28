use crate::daemon::handlers::LspHandlers;
use crate::daemon::lifecycle::DaemonLifecycle;
use crate::daemon::state::ReadOnlyStorage;
use std::sync::Arc;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};
use tracing::info;

pub struct Backend {
    pub client: Client,
    pub lifecycle: Arc<DaemonLifecycle>,
    pub storage: Arc<ReadOnlyStorage>,
    pub handlers: Arc<LspHandlers>,
}

impl Backend {
    pub fn new(client: Client, lifecycle: DaemonLifecycle, storage: ReadOnlyStorage) -> Self {
        let lifecycle = Arc::new(lifecycle);
        let storage = Arc::new(storage);
        let handlers = Arc::new(LspHandlers::new(client.clone(), storage.clone()));
        Self {
            client,
            lifecycle,
            storage,
            handlers,
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(
        &self,
        _: InitializeParams,
    ) -> tower_lsp_server::jsonrpc::Result<InitializeResult> {
        info!("LSP Server Initializing");
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        info!("LSP Server Initialized");
        self.client
            .log_message(MessageType::INFO, "Ledgerful LSP server initialized")
            .await;

        let lifecycle = self.lifecycle.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                if !lifecycle.check_parent_alive() {
                    tracing::error!("Parent process died. LSP Server self-terminating.");
                    let _ = lifecycle.cleanup();
                    std::process::exit(0);
                }
            }
        });
    }

    async fn shutdown(&self) -> tower_lsp_server::jsonrpc::Result<()> {
        info!("LSP Server Shutting Down");
        let _ = self.lifecycle.cleanup();
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.handlers.on_open(params).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        self.handlers.on_change(params).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.handlers.on_save(params).await;
    }

    async fn hover(&self, params: HoverParams) -> tower_lsp_server::jsonrpc::Result<Option<Hover>> {
        self.handlers.on_hover(params).await
    }

    async fn code_lens(
        &self,
        params: CodeLensParams,
    ) -> tower_lsp_server::jsonrpc::Result<Option<Vec<CodeLens>>> {
        self.handlers.on_code_lens(params).await
    }
}
