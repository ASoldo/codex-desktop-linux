mod browser;

use browser::{BrowserConfig, BrowserController};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;

#[derive(Clone)]
struct BrowserServer {
    controller: BrowserController,
    tool_router: ToolRouter<Self>,
}

impl BrowserServer {
    fn new(config: BrowserConfig) -> Self {
        Self {
            controller: BrowserController::new(config),
            tool_router: Self::tool_router(),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LaunchArgs {
    #[schemars(
        description = "Optional initial HTTP(S) URL. When omitted, attach to the current side-pane tab without navigating"
    )]
    initial_url: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TargetArgs {
    #[schemars(
        description = "Optional numeric in-app browser tab id from list_tabs; the active tab is used when omitted"
    )]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NavigateArgs {
    #[schemars(description = "HTTP(S) URL whose host is in the configured allowlist")]
    url: String,
    #[schemars(description = "Optional target id from list_tabs")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClickArgs {
    #[schemars(description = "CSS selector. Supply selector or text, not both")]
    selector: Option<String>,
    #[schemars(description = "Visible text of the element. Supply selector or text, not both")]
    text: Option<String>,
    #[schemars(description = "Optional target id from list_tabs")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CursorArgs {
    #[schemars(description = "Viewport x coordinate")]
    x: f64,
    #[schemars(description = "Viewport y coordinate")]
    y: f64,
    #[schemars(description = "Optional numeric in-app browser tab id from list_tabs")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ScrollArgs {
    #[serde(default)]
    #[schemars(description = "Horizontal scroll amount in pixels; positive scrolls right")]
    scroll_x: f64,
    #[serde(default)]
    #[schemars(description = "Vertical scroll amount in pixels; positive scrolls down")]
    scroll_y: f64,
    #[schemars(description = "Optional CSS selector whose center receives the wheel gesture")]
    selector: Option<String>,
    #[schemars(description = "Optional viewport x coordinate; requires y")]
    x: Option<f64>,
    #[schemars(description = "Optional viewport y coordinate; requires x")]
    y: Option<f64>,
    #[schemars(description = "Optional numeric in-app browser tab id from list_tabs")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SelectOptionArgs {
    #[schemars(description = "CSS selector matching one or more select elements")]
    selector: String,
    #[serde(default)]
    #[schemars(description = "Zero-based index when the selector matches multiple selects")]
    selector_index: usize,
    #[schemars(description = "Option label or value to select")]
    option: String,
    #[schemars(description = "Optional numeric in-app browser tab id from list_tabs")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetCheckedArgs {
    #[schemars(description = "CSS selector matching a checkbox or radio input")]
    selector: String,
    #[schemars(description = "Requested checked state")]
    checked: bool,
    #[schemars(description = "Optional numeric in-app browser tab id from list_tabs")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TypeArgs {
    #[schemars(description = "CSS selector for an input, textarea, or contenteditable element")]
    selector: String,
    #[schemars(description = "Text to enter")]
    text: String,
    #[serde(default = "default_true")]
    #[schemars(description = "Clear the existing value first; defaults to true")]
    clear: bool,
    #[serde(default)]
    #[schemars(description = "Press Enter after typing; defaults to false")]
    submit: bool,
    #[schemars(description = "Optional target id from list_tabs")]
    target_id: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WaitArgs {
    #[schemars(description = "CSS selector. Supply selector or text, not both")]
    selector: Option<String>,
    #[schemars(description = "Visible text. Supply selector or text, not both")]
    text: Option<String>,
    #[serde(default = "default_timeout")]
    #[schemars(
        description = "Maximum wait in milliseconds; defaults to 15000 and is capped at 60000"
    )]
    timeout_ms: u64,
    #[schemars(description = "Optional target id from list_tabs")]
    target_id: Option<String>,
}

fn default_timeout() -> u64 {
    15_000
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ScreenshotArgs {
    #[schemars(description = "Optional filename without directories; .png is added when missing")]
    filename: Option<String>,
    #[schemars(description = "Optional target id from list_tabs")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DownloadArgs {
    #[schemars(description = "CSS selector. Supply selector or text, not both")]
    selector: Option<String>,
    #[schemars(
        description = "Visible text of the download trigger. Supply selector or text, not both"
    )]
    text: Option<String>,
    #[serde(default = "default_download_timeout")]
    #[schemars(
        description = "Maximum wait in milliseconds; defaults to 60000 and is capped at 120000"
    )]
    timeout_ms: u64,
    #[schemars(description = "Optional target id from list_tabs")]
    target_id: Option<String>,
}

fn default_download_timeout() -> u64 {
    60_000
}

#[tool_router]
impl BrowserServer {
    #[tool(
        description = "Attach to this task's visible Codex side-pane browser, optionally opening an allowlisted URL"
    )]
    async fn launch_browser(
        &self,
        Parameters(args): Parameters<LaunchArgs>,
    ) -> Result<String, String> {
        self.controller
            .launch(args.initial_url.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Report the current task's Codex in-app browser socket, tabs, and directories"
    )]
    async fn browser_status(&self) -> Result<String, String> {
        self.controller
            .status()
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "List tabs owned by this task's Codex side-pane browser")]
    async fn list_tabs(&self) -> Result<String, String> {
        self.controller
            .list_tabs()
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Open an allowlisted URL in a new Codex side-pane tab")]
    async fn open_tab(&self, Parameters(args): Parameters<NavigateArgs>) -> Result<String, String> {
        self.controller
            .open_tab(&args.url)
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Navigate a tab to an allowlisted URL and wait for the document to settle"
    )]
    async fn navigate(&self, Parameters(args): Parameters<NavigateArgs>) -> Result<String, String> {
        self.controller
            .navigate(&args.url, args.target_id.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Read a compact page snapshot with visible text and interactive CSS selectors"
    )]
    async fn snapshot(&self, Parameters(args): Parameters<TargetArgs>) -> Result<String, String> {
        self.controller
            .snapshot(args.target_id.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Move the visible Codex cursor and click an element by CSS selector or visible text"
    )]
    async fn click(&self, Parameters(args): Parameters<ClickArgs>) -> Result<String, String> {
        self.controller
            .click(
                args.selector.as_deref(),
                args.text.as_deref(),
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Move the visible Codex cursor to viewport coordinates without clicking")]
    async fn move_cursor(
        &self,
        Parameters(args): Parameters<CursorArgs>,
    ) -> Result<String, String> {
        self.controller
            .move_cursor(args.x, args.y, args.target_id.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Move the visible Codex cursor and perform a mouse-wheel scroll gesture")]
    async fn scroll(&self, Parameters(args): Parameters<ScrollArgs>) -> Result<String, String> {
        self.controller
            .scroll(
                args.scroll_x,
                args.scroll_y,
                args.selector.as_deref(),
                args.x,
                args.y,
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Click visibly, then type into an input and optionally clear or submit")]
    async fn type_text(&self, Parameters(args): Parameters<TypeArgs>) -> Result<String, String> {
        self.controller
            .type_text(
                &args.selector,
                &args.text,
                args.clear,
                args.submit,
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Move the visible Codex cursor to a select field and choose an option")]
    async fn select_option(
        &self,
        Parameters(args): Parameters<SelectOptionArgs>,
    ) -> Result<String, String> {
        self.controller
            .select_option(
                &args.selector,
                args.selector_index,
                &args.option,
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Move the visible Codex cursor and set a checkbox or radio state")]
    async fn set_checked(
        &self,
        Parameters(args): Parameters<SetCheckedArgs>,
    ) -> Result<String, String> {
        self.controller
            .set_checked(&args.selector, args.checked, args.target_id.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Wait until a CSS selector or visible text appears")]
    async fn wait_for(&self, Parameters(args): Parameters<WaitArgs>) -> Result<String, String> {
        self.controller
            .wait_for(
                args.selector.as_deref(),
                args.text.as_deref(),
                args.timeout_ms.min(60_000),
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Capture the current tab to a PNG in the controller screenshot directory")]
    async fn screenshot(
        &self,
        Parameters(args): Parameters<ScreenshotArgs>,
    ) -> Result<String, String> {
        self.controller
            .screenshot(args.filename.as_deref(), args.target_id.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Click a download trigger and return only after a new file is complete on disk"
    )]
    async fn click_and_wait_for_download(
        &self,
        Parameters(args): Parameters<DownloadArgs>,
    ) -> Result<String, String> {
        self.controller
            .click_and_wait_for_download(
                args.selector.as_deref(),
                args.text.as_deref(),
                args.timeout_ms.min(120_000),
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BrowserServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Controls this Codex task's in-app side-pane browser through the app's browser-use socket. Pointer actions use the visible Codex cursor. Start with browser_status or launch_browser, inspect with snapshot, use click/scroll/type_text/select_option/set_checked for interaction, and use click_and_wait_for_download for downloads. Navigation is limited to the configured host allowlist.",
        )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = BrowserConfig::from_env()?;
    let server = BrowserServer::new(config);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
