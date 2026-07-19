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
struct SessionNameArgs {
    #[schemars(description = "Human-readable name for the current browser automation session")]
    name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct VisibilityArgs {
    #[schemars(description = "Set true to show or false to hide; omit to read current visibility")]
    visible: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ViewportArgs {
    #[schemars(description = "Viewport width in CSS pixels; required unless reset is true")]
    width: Option<u32>,
    #[schemars(description = "Viewport height in CSS pixels; required unless reset is true")]
    height: Option<u32>,
    #[serde(default)]
    #[schemars(description = "Reset the explicit viewport override")]
    reset: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TabActionArgs {
    #[schemars(description = "One of back, forward, reload, or close")]
    action: String,
    #[schemars(description = "Optional numeric in-app browser tab id")]
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
struct DragArgs {
    #[schemars(description = "CSS selector for the drag start; use this or from_x/from_y")]
    from_selector: Option<String>,
    #[schemars(description = "Drag start viewport x coordinate; requires from_y")]
    from_x: Option<f64>,
    #[schemars(description = "Drag start viewport y coordinate; requires from_x")]
    from_y: Option<f64>,
    #[schemars(description = "CSS selector for the drag end; use this or to_x/to_y")]
    to_selector: Option<String>,
    #[schemars(description = "Drag end viewport x coordinate; requires to_y")]
    to_x: Option<f64>,
    #[schemars(description = "Drag end viewport y coordinate; requires to_x")]
    to_y: Option<f64>,
    #[serde(default = "default_drag_steps")]
    #[schemars(description = "Number of visible cursor steps; defaults to 12 and is capped at 60")]
    steps: usize,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

fn default_drag_steps() -> usize {
    12
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KeypressArgs {
    #[schemars(description = "Key combinations such as Enter, Ctrl+A, Shift+Tab, or ArrowDown")]
    keys: Vec<String>,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ElementInfoArgs {
    #[schemars(description = "CSS selector to inspect")]
    selector: String,
    #[serde(default)]
    #[schemars(description = "Specific HTML attribute names to return")]
    attribute_names: Vec<String>,
    #[serde(default = "default_element_limit")]
    #[schemars(
        description = "Maximum matching elements to return; defaults to 20 and is capped at 100"
    )]
    limit: usize,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

fn default_element_limit() -> usize {
    20
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EvaluateArgs {
    #[schemars(
        description = "JavaScript expression used only to read page state; side effects are blocked by V8"
    )]
    expression: String,
    #[schemars(description = "Optional numeric in-app browser tab id")]
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WaitPageArgs {
    #[schemars(description = "Optional substring that must appear in the current URL")]
    url_contains: Option<String>,
    #[schemars(description = "Optional document state: loading, interactive, or complete")]
    load_state: Option<String>,
    #[serde(default = "default_timeout")]
    #[schemars(
        description = "Maximum wait in milliseconds; defaults to 15000 and is capped at 60000"
    )]
    timeout_ms: u64,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

fn default_timeout() -> u64 {
    15_000
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ScreenshotArgs {
    #[schemars(description = "Optional filename without directories; .png is added when missing")]
    filename: Option<String>,
    #[serde(default)]
    #[schemars(description = "Capture the entire scrollable page instead of the current viewport")]
    full_page: bool,
    #[schemars(
        description = "Optional CSS selector to capture just one element; incompatible with full_page"
    )]
    selector: Option<String>,
    #[schemars(description = "Optional target id from list_tabs")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClipboardArgs {
    #[schemars(description = "One of read or write")]
    action: String,
    #[schemars(description = "Text to write; required for write and omitted for read")]
    text: Option<String>,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ConsoleLogsArgs {
    #[schemars(description = "One of start, read, or clear")]
    action: String,
    #[serde(default)]
    #[schemars(
        description = "For read, optionally capture for up to 10000 milliseconds before returning"
    )]
    wait_ms: u64,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DialogArgs {
    #[schemars(description = "One of accept or dismiss")]
    action: String,
    #[schemars(description = "Optional prompt response when accepting a prompt dialog")]
    prompt_text: Option<String>,
    #[schemars(
        description = "Optional CSS selector to click before handling the dialog; use this or trigger_text"
    )]
    trigger_selector: Option<String>,
    #[schemars(
        description = "Optional visible text to click before handling the dialog; use this or trigger_selector"
    )]
    trigger_text: Option<String>,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UploadArgs {
    #[schemars(description = "CSS selector resolving to input[type=file]")]
    selector: String,
    #[schemars(description = "Absolute paths of local files explicitly selected for upload")]
    files: Vec<String>,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExportArgs {
    #[schemars(description = "Export format: html, text, markdown, or pdf")]
    format: String,
    #[schemars(description = "Optional basename; the expected extension is added")]
    filename: Option<String>,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PageAssetsArgs {
    #[schemars(description = "One of list or bundle")]
    action: String,
    #[schemars(description = "Inventory id returned by list; required for bundle")]
    inventory_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional asset ids to bundle")]
    asset_ids: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Optional bundle kinds: font, image, stylesheet, video")]
    kinds: Vec<String>,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CdpCommandArgs {
    #[schemars(description = "Chrome DevTools Protocol Domain.method name")]
    method: String,
    #[serde(default = "empty_object")]
    #[schemars(description = "JSON object containing CDP command parameters")]
    params: serde_json::Value,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CdpEventsArgs {
    #[schemars(description = "Exact CDP event methods to capture")]
    methods: Vec<String>,
    #[serde(default = "default_cdp_event_timeout")]
    #[schemars(
        description = "Capture duration in milliseconds; defaults to 2000 and is capped at 30000"
    )]
    timeout_ms: u64,
    #[serde(default = "default_cdp_event_limit")]
    #[schemars(description = "Maximum events; defaults to 100 and is capped at 1000")]
    limit: usize,
    #[schemars(description = "Optional numeric in-app browser tab id")]
    target_id: Option<String>,
}

fn default_cdp_event_timeout() -> u64 {
    2_000
}

fn default_cdp_event_limit() -> usize {
    100
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

    #[tool(
        description = "Give the current Codex side-pane browser automation session a readable name"
    )]
    async fn name_session(
        &self,
        Parameters(args): Parameters<SessionNameArgs>,
    ) -> Result<String, String> {
        self.controller
            .name_session(&args.name)
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Read, show, or hide the Codex in-app Browser side pane")]
    async fn browser_visibility(
        &self,
        Parameters(args): Parameters<VisibilityArgs>,
    ) -> Result<String, String> {
        self.controller
            .visibility(args.visible)
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Set or reset an explicit side-pane viewport for responsive testing")]
    async fn browser_viewport(
        &self,
        Parameters(args): Parameters<ViewportArgs>,
    ) -> Result<String, String> {
        self.controller
            .viewport(args.width, args.height, args.reset)
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

    #[tool(description = "Navigate a side-pane tab back or forward, reload it, or close it")]
    async fn tab_action(
        &self,
        Parameters(args): Parameters<TabActionArgs>,
    ) -> Result<String, String> {
        self.controller
            .tab_action(&args.action, args.target_id.as_deref())
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

    #[tool(description = "Inspect bounded text, state, attributes, and geometry for a CSS locator")]
    async fn element_info(
        &self,
        Parameters(args): Parameters<ElementInfoArgs>,
    ) -> Result<String, String> {
        self.controller
            .element_info(
                &args.selector,
                &args.attribute_names,
                args.limit,
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Evaluate a bounded read-only JavaScript expression with V8 side effects blocked"
    )]
    async fn evaluate_readonly(
        &self,
        Parameters(args): Parameters<EvaluateArgs>,
    ) -> Result<String, String> {
        self.controller
            .evaluate_readonly(&args.expression, args.target_id.as_deref())
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

    #[tool(
        description = "Move the visible Codex cursor and double-click by CSS selector or visible text"
    )]
    async fn double_click(
        &self,
        Parameters(args): Parameters<ClickArgs>,
    ) -> Result<String, String> {
        self.controller
            .double_click(
                args.selector.as_deref(),
                args.text.as_deref(),
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Drag visibly between selectors or viewport points using the Codex cursor"
    )]
    async fn drag(&self, Parameters(args): Parameters<DragArgs>) -> Result<String, String> {
        self.controller
            .drag(
                args.from_selector.as_deref(),
                args.from_x,
                args.from_y,
                args.to_selector.as_deref(),
                args.to_x,
                args.to_y,
                args.steps,
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

    #[tool(description = "Press keyboard combinations at the focused page element")]
    async fn keypress(&self, Parameters(args): Parameters<KeypressArgs>) -> Result<String, String> {
        self.controller
            .keypress(&args.keys, args.target_id.as_deref())
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

    #[tool(description = "Wait for a URL substring, document load state, or both")]
    async fn wait_for_page(
        &self,
        Parameters(args): Parameters<WaitPageArgs>,
    ) -> Result<String, String> {
        self.controller
            .wait_for_page(
                args.url_contains.as_deref(),
                args.load_state.as_deref(),
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
            .screenshot(
                args.filename.as_deref(),
                args.full_page,
                args.selector.as_deref(),
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Read or write text in the current side-pane browser clipboard")]
    async fn clipboard(
        &self,
        Parameters(args): Parameters<ClipboardArgs>,
    ) -> Result<String, String> {
        self.controller
            .clipboard(
                &args.action,
                args.text.as_deref(),
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Start, read, or clear a bounded page console and runtime-error capture")]
    async fn console_logs(
        &self,
        Parameters(args): Parameters<ConsoleLogsArgs>,
    ) -> Result<String, String> {
        self.controller
            .console_logs(&args.action, args.wait_ms, args.target_id.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Click an optional trigger and accept or dismiss its JavaScript dialog on the same browser connection"
    )]
    async fn handle_dialog(
        &self,
        Parameters(args): Parameters<DialogArgs>,
    ) -> Result<String, String> {
        self.controller
            .handle_dialog(
                &args.action,
                args.prompt_text.as_deref(),
                args.trigger_selector.as_deref(),
                args.trigger_text.as_deref(),
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Set explicitly authorized absolute local files on an input[type=file]")]
    async fn upload_files(
        &self,
        Parameters(args): Parameters<UploadArgs>,
    ) -> Result<String, String> {
        self.controller
            .upload_files(&args.selector, &args.files, args.target_id.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Export the current page as HTML, text, Markdown, or PDF")]
    async fn export_page(
        &self,
        Parameters(args): Parameters<ExportArgs>,
    ) -> Result<String, String> {
        self.controller
            .export_page(
                &args.format,
                args.filename.as_deref(),
                args.target_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "List rendered page assets or bundle selected image, font, stylesheet, and video assets"
    )]
    async fn page_assets(
        &self,
        Parameters(args): Parameters<PageAssetsArgs>,
    ) -> Result<String, String> {
        match args.action.as_str() {
            "list" => self
                .controller
                .page_assets_list(args.target_id.as_deref())
                .await
                .map_err(|error| error.to_string()),
            "bundle" => self
                .controller
                .page_assets_bundle(
                    args.inventory_id
                        .as_deref()
                        .ok_or_else(|| "page_assets bundle requires inventory_id".to_owned())?,
                    &args.asset_ids,
                    &args.kinds,
                    args.target_id.as_deref(),
                )
                .await
                .map_err(|error| error.to_string()),
            _ => Err("page_assets action must be list or bundle".to_owned()),
        }
    }

    #[tool(
        description = "Send a raw Chrome DevTools Protocol command when explicit developer mode is enabled"
    )]
    async fn cdp_command(
        &self,
        Parameters(args): Parameters<CdpCommandArgs>,
    ) -> Result<String, String> {
        self.controller
            .cdp_command(&args.method, args.params, args.target_id.as_deref())
            .await
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Capture selected raw CDP events when explicit developer mode is enabled")]
    async fn cdp_events(
        &self,
        Parameters(args): Parameters<CdpEventsArgs>,
    ) -> Result<String, String> {
        self.controller
            .cdp_events(
                &args.methods,
                args.timeout_ms.min(30_000),
                args.limit.min(1000),
                args.target_id.as_deref(),
            )
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
            "Controls this Codex task's in-app side-pane browser through the app's browser-use socket. Start with browser_status or launch_browser and inspect with snapshot or element_info. Pointer actions use the visible Codex cursor. Core tools cover tab lifecycle, pointer and keyboard input, locators, waits, screenshots, clipboard, dialogs, uploads, verified downloads, console capture, exports, page assets, visibility, and viewport overrides. evaluate_readonly blocks V8 side effects. Raw CDP commands and events require the explicit RUST_BROWSER_ENABLE_RAW_CDP developer flag. Navigation is limited to the configured host allowlist; login stays in the visible side pane and secrets remain user-controlled.",
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
