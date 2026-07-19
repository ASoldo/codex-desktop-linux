use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    env,
    ffi::OsStr,
    hash::{Hash, Hasher},
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::Mutex,
    time::{sleep, timeout},
};
use url::Url;

const MAX_BACKEND_FRAME_BYTES: usize = 64 * 1024 * 1024;
const BROWSER_ID: &str = "rust-browser-control";
const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;
const MAX_ASSET_BUNDLE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct BrowserConfig {
    socket_dir: PathBuf,
    sessions_root: PathBuf,
    session_id: String,
    download_dir: PathBuf,
    screenshot_dir: PathBuf,
    export_dir: PathBuf,
    asset_dir: PathBuf,
    allowed_hosts: Vec<String>,
    raw_cdp_enabled: bool,
}

impl BrowserConfig {
    pub fn from_env() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set"))?;
        let pictures_root = env::var_os("XDG_PICTURES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("Pictures"));
        let session_id = env::var("RUST_BROWSER_SESSION_ID")
            .or_else(|_| env::var("CODEX_THREAD_ID"))
            .context(
                "missing Codex task id; pass CODEX_THREAD_ID through the MCP environment or set RUST_BROWSER_SESSION_ID",
            )?;
        let socket_dir = env::var_os("RUST_BROWSER_SOCKET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/codex-browser-use"));
        let sessions_root = env::var_os("RUST_BROWSER_SESSIONS_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex/sessions"));
        let download_dir = env::var_os("RUST_BROWSER_DOWNLOAD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("Downloads/RustBrowserControl"));
        let screenshot_dir = pictures_root.join("RustBrowserControl");
        let export_dir = download_dir.join("exports");
        let asset_dir = download_dir.join("page-assets");
        let hosts = env::var("RUST_BROWSER_ALLOWED_HOSTS").unwrap_or_else(|_| "*".to_owned());
        let allowed_hosts = hosts
            .split(',')
            .map(|host| host.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect::<Vec<_>>();
        if allowed_hosts.is_empty() {
            bail!("RUST_BROWSER_ALLOWED_HOSTS cannot be empty");
        }

        Ok(Self {
            socket_dir,
            sessions_root,
            session_id,
            download_dir,
            screenshot_dir,
            export_dir,
            asset_dir,
            allowed_hosts,
            raw_cdp_enabled: env_flag("RUST_BROWSER_ENABLE_RAW_CDP"),
        })
    }

    fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.download_dir).with_context(|| {
            format!("create download directory {}", self.download_dir.display())
        })?;
        std::fs::create_dir_all(&self.screenshot_dir).with_context(|| {
            format!(
                "create screenshot directory {}",
                self.screenshot_dir.display()
            )
        })?;
        std::fs::create_dir_all(&self.export_dir)
            .with_context(|| format!("create export directory {}", self.export_dir.display()))?;
        std::fs::create_dir_all(&self.asset_dir)
            .with_context(|| format!("create asset directory {}", self.asset_dir.display()))?;
        Ok(())
    }

    fn validate_url(&self, input: &str) -> Result<Url> {
        let url = Url::parse(input).with_context(|| format!("invalid URL: {input}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("only HTTP(S) navigation is allowed");
        }
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("URL has no host"))?
            .to_ascii_lowercase();
        let allowed = self
            .allowed_hosts
            .iter()
            .any(|base| base == "*" || host == *base || host.ends_with(&format!(".{base}")));
        if !allowed {
            bail!(
                "host {host} is not allowed; configured hosts: {}",
                self.allowed_hosts.join(", ")
            );
        }
        Ok(url)
    }

    fn current_turn_id(&self) -> String {
        env::var("RUST_BROWSER_TURN_ID")
            .or_else(|_| env::var("CODEX_TURN_ID"))
            .ok()
            .or_else(|| latest_turn_id(&self.sessions_root, &self.session_id))
            .unwrap_or_else(|| "rust-browser-control".to_owned())
    }
}

#[derive(Clone)]
pub struct BrowserController {
    config: BrowserConfig,
    state: std::sync::Arc<Mutex<BrowserState>>,
    action_lock: std::sync::Arc<Mutex<()>>,
}

#[derive(Default)]
struct BrowserState {
    preferred_tab: Option<u64>,
    asset_inventories: HashMap<String, AssetInventory>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AssetInventory {
    id: String,
    page_url: String,
    assets: Vec<PageAsset>,
    inline_svgs: Vec<InlineSvg>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PageAsset {
    id: String,
    kind: String,
    name: String,
    url: String,
    sources: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct InlineSvg {
    id: String,
    name: String,
    markup: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Tab {
    id: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    active: bool,
}

struct IabClient {
    stream: UnixStream,
    next_id: u64,
    session_id: String,
    turn_id: String,
    socket_path: PathBuf,
    events: VecDeque<Value>,
}

impl IabClient {
    async fn connect(path: &Path, session_id: &str, turn_id: &str) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .await
            .with_context(|| format!("connect to {}", path.display()))?;
        Ok(Self {
            stream,
            next_id: 1,
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            socket_path: path.to_owned(),
            events: VecDeque::new(),
        })
    }

    async fn read_message(&mut self) -> Result<Value> {
        let mut header = [0_u8; 4];
        self.stream.read_exact(&mut header).await?;
        let length = u32::from_le_bytes(header) as usize;
        if length > MAX_BACKEND_FRAME_BYTES {
            bail!("browser backend response frame is too large: {length} bytes");
        }
        let mut body = vec![0_u8; length];
        self.stream.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body)?)
    }

    async fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        let mut params = match params {
            Value::Object(map) => map,
            _ => bail!("browser backend request params must be an object"),
        };
        params.insert(
            "session_id".to_owned(),
            Value::String(self.session_id.clone()),
        );
        params.insert("turn_id".to_owned(), Value::String(self.turn_id.clone()));
        params.insert(
            "session_context".to_owned(),
            Value::String("live".to_owned()),
        );
        let id = self.next_id;
        self.next_id += 1;
        let payload = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        if payload.len() > MAX_BACKEND_FRAME_BYTES {
            bail!("browser backend request frame is too large");
        }
        self.stream
            .write_all(&(payload.len() as u32).to_le_bytes())
            .await?;
        self.stream.write_all(&payload).await?;
        self.stream.flush().await?;
        Ok(id)
    }

    async fn wait_response(&mut self, id: u64, method: &str) -> Result<Value> {
        loop {
            let response = self.read_message().await?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                self.events.push_back(response);
                continue;
            }
            if let Some(error) = response.get("error") {
                bail!("browser backend {method} failed: {error}");
            }
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send_request(method, params).await?;
        self.wait_response(id, method).await
    }

    async fn next_event(&mut self, wait: Duration) -> Result<Option<Value>> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        match timeout(wait, self.read_message()).await {
            Ok(result) => Ok(Some(result?)),
            Err(_) => Ok(None),
        }
    }

    async fn get_info(&mut self) -> Result<Value> {
        self.request("getInfo", json!({})).await
    }

    async fn tabs(&mut self) -> Result<Vec<Tab>> {
        Ok(serde_json::from_value(
            self.request("getTabs", json!({})).await?,
        )?)
    }

    async fn attach(&mut self, tab_id: u64) -> Result<()> {
        self.request("attach", json!({ "tabId": tab_id })).await?;
        Ok(())
    }

    async fn cdp(&mut self, tab_id: u64, method: &str, command_params: Value) -> Result<Value> {
        self.request(
            "executeCdp",
            json!({
                "target": { "tabId": tab_id },
                "method": method,
                "commandParams": command_params,
            }),
        )
        .await
    }

    async fn send_cdp(&mut self, tab_id: u64, method: &str, command_params: Value) -> Result<u64> {
        self.send_request(
            "executeCdp",
            json!({
                "target": { "tabId": tab_id },
                "method": method,
                "commandParams": command_params,
            }),
        )
        .await
    }

    async fn move_mouse(&mut self, tab_id: u64, x: f64, y: f64) -> Result<()> {
        self.request(
            "moveMouse",
            json!({ "tabId": tab_id, "x": x, "y": y, "waitForArrival": true }),
        )
        .await?;
        self.cdp(
            tab_id,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseMoved",
                "x": x,
                "y": y,
                "button": "none",
                "buttons": 0,
            }),
        )
        .await?;
        Ok(())
    }

    async fn visible_click(&mut self, tab_id: u64, x: f64, y: f64) -> Result<()> {
        self.move_mouse(tab_id, x, y).await?;
        self.cdp(
            tab_id,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mousePressed",
                "x": x,
                "y": y,
                "button": "left",
                "buttons": 1,
                "clickCount": 1,
            }),
        )
        .await?;
        self.cdp(
            tab_id,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseReleased",
                "x": x,
                "y": y,
                "button": "left",
                "buttons": 0,
                "clickCount": 1,
            }),
        )
        .await?;
        Ok(())
    }

    async fn visible_double_click(&mut self, tab_id: u64, x: f64, y: f64) -> Result<()> {
        self.move_mouse(tab_id, x, y).await?;
        for click_count in 1..=2 {
            self.cdp(
                tab_id,
                "Input.dispatchMouseEvent",
                json!({
                    "type": "mousePressed",
                    "x": x,
                    "y": y,
                    "button": "left",
                    "buttons": 1,
                    "clickCount": click_count,
                }),
            )
            .await?;
            self.cdp(
                tab_id,
                "Input.dispatchMouseEvent",
                json!({
                    "type": "mouseReleased",
                    "x": x,
                    "y": y,
                    "button": "left",
                    "buttons": 0,
                    "clickCount": click_count,
                }),
            )
            .await?;
        }
        Ok(())
    }
}

impl BrowserController {
    pub fn new(config: BrowserConfig) -> Self {
        Self {
            config,
            state: std::sync::Arc::new(Mutex::new(BrowserState::default())),
            action_lock: std::sync::Arc::new(Mutex::new(())),
        }
    }

    pub async fn launch(&self, initial_url: Option<&str>) -> Result<String> {
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, None).await?;
        if let Some(initial_url) = initial_url {
            let url = self.config.validate_url(initial_url)?;
            client
                .cdp(tab_id, "Page.navigate", json!({ "url": url.as_str() }))
                .await?;
            self.wait_ready(&mut client, tab_id, 20_000).await?;
        }
        self.configure_downloads(&mut client, tab_id).await;
        let identity = self.page_identity(&mut client, tab_id).await?;
        Ok(format!(
            "Codex in-app browser ready\nsocket: {}\ntask: {}\ntab: {tab_id}\ndownloads: {}\npage: {}",
            client.socket_path.display(),
            self.config.session_id,
            self.config.download_dir.display(),
            serde_json::to_string(&identity)?
        ))
    }

    pub async fn status(&self) -> Result<String> {
        let mut client = self.connect().await?;
        let info = client.get_info().await?;
        let tabs = client.tabs().await?;
        Ok(format!(
            "connected: true\nbackend: {}\nbackend version: {}\nsocket: {}\ntask: {}\nturn: {}\ntabs: {}\ndownloads: {}\nscreenshots: {}\nexports: {}\nraw CDP: {}\ncapabilities: {}\nallowed hosts: {}",
            info.get("name")
                .and_then(Value::as_str)
                .unwrap_or("Codex in-app browser"),
            info.get("version")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            client.socket_path.display(),
            self.config.session_id,
            client.turn_id,
            tabs.len(),
            self.config.download_dir.display(),
            self.config.screenshot_dir.display(),
            self.config.export_dir.display(),
            self.config.raw_cdp_enabled,
            serde_json::to_string(info.get("capabilities").unwrap_or(&Value::Null))?,
            self.config.allowed_hosts.join(", ")
        ))
    }

    pub async fn list_tabs(&self) -> Result<String> {
        let mut client = self.connect().await?;
        Ok(serde_json::to_string_pretty(&client.tabs().await?)?)
    }

    pub async fn name_session(&self, name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 120 {
            bail!("session name must contain 1 to 120 characters");
        }
        let mut client = self.connect().await?;
        client
            .request("nameSession", json!({ "name": name }))
            .await?;
        Ok(format!("named browser session: {name}"))
    }

    pub async fn visibility(&self, visible: Option<bool>) -> Result<String> {
        let mut client = self.connect().await?;
        if let Some(visible) = visible {
            client
                .request(
                    "executeUnhandledCommand",
                    json!({
                        "type": "browser_visibility_set",
                        "browser_id": BROWSER_ID,
                        "visible": visible,
                    }),
                )
                .await?;
        }
        let result = client
            .request(
                "executeUnhandledCommand",
                json!({
                    "type": "browser_visibility_get",
                    "browser_id": BROWSER_ID,
                }),
            )
            .await?;
        Ok(serde_json::to_string_pretty(&result)?)
    }

    pub async fn viewport(
        &self,
        width: Option<u32>,
        height: Option<u32>,
        reset: bool,
    ) -> Result<String> {
        let mut client = self.connect().await?;
        if reset {
            if width.is_some() || height.is_some() {
                bail!("viewport reset cannot be combined with width or height");
            }
            client
                .request(
                    "executeUnhandledCommand",
                    json!({
                        "type": "browser_viewport_reset",
                        "browser_id": BROWSER_ID,
                    }),
                )
                .await?;
            return Ok("browser viewport override reset".to_owned());
        }
        let width = width.context("viewport width is required unless reset is true")?;
        let height = height.context("viewport height is required unless reset is true")?;
        if !(240..=7680).contains(&width) || !(240..=4320).contains(&height) {
            bail!("viewport must be between 240x240 and 7680x4320");
        }
        client
            .request(
                "executeUnhandledCommand",
                json!({
                    "type": "browser_viewport_set",
                    "browser_id": BROWSER_ID,
                    "width": width,
                    "height": height,
                }),
            )
            .await?;
        Ok(format!("browser viewport override set to {width}x{height}"))
    }

    pub async fn tab_action(&self, action: &str, tab_id: Option<&str>) -> Result<String> {
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        match action {
            "back" | "forward" => {
                let history = client
                    .cdp(tab_id, "Page.getNavigationHistory", json!({}))
                    .await?;
                let current = history
                    .get("currentIndex")
                    .and_then(Value::as_i64)
                    .context("navigation history has no current index")?;
                let next = if action == "back" {
                    current - 1
                } else {
                    current + 1
                };
                let entries = history
                    .get("entries")
                    .and_then(Value::as_array)
                    .context("navigation history has no entries")?;
                if next < 0 || next as usize >= entries.len() {
                    bail!("cannot navigate {action}; no history entry is available");
                }
                let entry_id = entries[next as usize]
                    .get("id")
                    .and_then(Value::as_u64)
                    .context("history entry has no id")?;
                client
                    .cdp(
                        tab_id,
                        "Page.navigateToHistoryEntry",
                        json!({ "entryId": entry_id }),
                    )
                    .await?;
                self.wait_ready(&mut client, tab_id, 20_000).await?;
            }
            "reload" => {
                client.cdp(tab_id, "Page.reload", json!({})).await?;
                self.wait_ready(&mut client, tab_id, 20_000).await?;
            }
            "close" => {
                client.cdp(tab_id, "Page.close", json!({})).await?;
                let mut state = self.state.lock().await;
                if state.preferred_tab == Some(tab_id) {
                    state.preferred_tab = None;
                }
                return Ok(format!("closed side-pane tab {tab_id}"));
            }
            _ => bail!("unsupported tab action {action}; use back, forward, reload, or close"),
        }
        let identity = self.page_identity(&mut client, tab_id).await?;
        Ok(serde_json::to_string_pretty(&json!({
            "action": action,
            "tabId": tab_id,
            "page": identity,
        }))?)
    }

    pub async fn open_tab(&self, input: &str) -> Result<String> {
        let url = self.config.validate_url(input)?;
        let mut client = self.connect().await?;
        let created = client.request("createTab", json!({})).await?;
        let tab_id = value_tab_id(&created).context("createTab returned no tab id")?;
        self.state.lock().await.preferred_tab = Some(tab_id);
        client.attach(tab_id).await?;
        client
            .cdp(tab_id, "Page.navigate", json!({ "url": url.as_str() }))
            .await?;
        self.wait_ready(&mut client, tab_id, 20_000).await?;
        Ok(serde_json::to_string_pretty(
            &self.page_identity(&mut client, tab_id).await?,
        )?)
    }

    pub async fn navigate(&self, input: &str, tab_id: Option<&str>) -> Result<String> {
        let url = self.config.validate_url(input)?;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        client
            .cdp(tab_id, "Page.navigate", json!({ "url": url.as_str() }))
            .await?;
        self.wait_ready(&mut client, tab_id, 20_000).await?;
        Ok(serde_json::to_string_pretty(
            &self.page_identity(&mut client, tab_id).await?,
        )?)
    }

    pub async fn snapshot(&self, tab_id: Option<&str>) -> Result<String> {
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let expression = r#"(() => {
          const esc = value => CSS.escape(String(value));
          const selectorFor = el => {
            if (el.id) return `#${esc(el.id)}`;
            const testid = el.getAttribute('data-testid');
            if (testid) return `[data-testid="${String(testid).replace(/"/g, '\\"')}"]`;
            const name = el.getAttribute('name');
            if (name) return `${el.tagName.toLowerCase()}[name="${String(name).replace(/"/g, '\\"')}"]`;
            const classes = [...el.classList].filter(Boolean).slice(0, 3);
            if (classes.length) return `${el.tagName.toLowerCase()}.${classes.map(esc).join('.')}`;
            const parts = [];
            let node = el;
            while (node && node.nodeType === 1 && parts.length < 5) {
              let part = node.tagName.toLowerCase();
              const siblings = node.parentElement ? [...node.parentElement.children].filter(x => x.tagName === node.tagName) : [];
              if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(node) + 1})`;
              parts.unshift(part);
              node = node.parentElement;
            }
            return parts.join(' > ');
          };
          const visible = el => {
            const s = getComputedStyle(el);
            const r = el.getBoundingClientRect();
            return s.visibility !== 'hidden' && s.display !== 'none' && r.width > 0 && r.height > 0 && r.bottom > 0 && r.right > 0;
          };
          const nodes = [...document.querySelectorAll('a,button,input,textarea,select,[role="button"],[role="link"],[tabindex],.product-animation')]
            .filter(visible).slice(0, 300);
          return {
            tabId: TAB_ID,
            url: location.href,
            title: document.title,
            text: (document.body?.innerText || '').replace(/\n{3,}/g, '\n\n').slice(0, 16000),
            interactive: nodes.map((el, index) => {
              const r = el.getBoundingClientRect();
              return {
                ref: `e${index + 1}`,
                tag: el.tagName.toLowerCase(),
                role: el.getAttribute('role') || null,
                name: el.matches('input[type="password"]')
                  ? '[password field]'
                  : (el.getAttribute('aria-label') || el.innerText || el.value || el.getAttribute('title') || '').trim().replace(/\s+/g, ' ').slice(0, 180),
                selector: selectorFor(el),
                center: {x: r.left + r.width / 2, y: r.top + r.height / 2},
                disabled: Boolean(el.disabled || el.getAttribute('aria-disabled') === 'true')
              };
            })
          };
        })()"#
            .replace("TAB_ID", &tab_id.to_string());
        let value = evaluate(&mut client, tab_id, &expression).await?;
        Ok(serde_json::to_string_pretty(&value)?)
    }

    pub async fn element_info(
        &self,
        selector: &str,
        attribute_names: &[String],
        limit: usize,
        tab_id: Option<&str>,
    ) -> Result<String> {
        if selector.trim().is_empty() {
            bail!("selector cannot be empty");
        }
        let limit = limit.clamp(1, 100);
        let selector_json = serde_json::to_string(selector)?;
        let attributes_json = serde_json::to_string(attribute_names)?;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let expression = format!(
            r#"(() => {{
              const selector = {selector_json};
              const requestedAttributes = {attributes_json}.slice(0, 50);
              const all = [...document.querySelectorAll(selector)];
              const normalize = value => String(value || '').trim().replace(/\s+/g, ' ');
              const items = all.slice(0, {limit}).map((el, index) => {{
                const style = getComputedStyle(el);
                const rect = el.getBoundingClientRect();
                const attributes = Object.fromEntries(requestedAttributes.map(name => [name, el.getAttribute(name)]));
                const password = el.matches('input[type="password"]');
                return {{
                  index,
                  tag: el.tagName.toLowerCase(),
                  role: el.getAttribute('role'),
                  accessibleName: normalize(el.getAttribute('aria-label') || el.innerText || el.getAttribute('title')).slice(0, 1000),
                  text: normalize(el.innerText || el.textContent).slice(0, 1000),
                  value: password ? '[password field]' : ('value' in el ? String(el.value).slice(0, 1000) : null),
                  visible: style.visibility !== 'hidden' && style.display !== 'none' && rect.width > 0 && rect.height > 0,
                  enabled: !(el.disabled || el.getAttribute('aria-disabled') === 'true'),
                  checked: 'checked' in el ? Boolean(el.checked) : null,
                  selected: 'selected' in el ? Boolean(el.selected) : null,
                  attributes,
                  rect: {{x: rect.x, y: rect.y, width: rect.width, height: rect.height}}
                }};
              }});
              return {{selector, count: all.length, returned: items.length, items}};
            }})()"#
        );
        let value = evaluate(&mut client, tab_id, &expression).await?;
        Ok(serde_json::to_string_pretty(&value)?)
    }

    pub async fn evaluate_readonly(
        &self,
        expression: &str,
        tab_id: Option<&str>,
    ) -> Result<String> {
        if expression.trim().is_empty() {
            bail!("expression cannot be empty");
        }
        if expression.len() > 32_000 {
            bail!("expression is too large; maximum length is 32000 bytes");
        }
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let result = client
            .cdp(
                tab_id,
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": false,
                    "throwOnSideEffect": true,
                    "disableBreaks": true,
                    "timeout": 5000,
                }),
            )
            .await?;
        if let Some(details) = result.get("exceptionDetails") {
            bail!("read-only evaluation failed or may have side effects: {details}");
        }
        Ok(serde_json::to_string_pretty(
            result.pointer("/result/value").unwrap_or(&Value::Null),
        )?)
    }

    pub async fn move_cursor(&self, x: f64, y: f64, tab_id: Option<&str>) -> Result<String> {
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        client.move_mouse(tab_id, x, y).await?;
        Ok(format!(
            "moved visible Codex cursor to ({x:.1}, {y:.1}) in tab {tab_id}"
        ))
    }

    pub async fn click(
        &self,
        selector: Option<&str>,
        text: Option<&str>,
        tab_id: Option<&str>,
    ) -> Result<String> {
        validate_locator(selector, text)?;
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let located = locate(&mut client, tab_id, selector, text).await?;
        let (x, y) = point_from_value(&located)?;
        client.visible_click(tab_id, x, y).await?;
        sleep(Duration::from_millis(500)).await;
        Ok(serde_json::to_string_pretty(&json!({
            "clicked": located,
            "tabId": tab_id,
            "visibleCursor": true,
        }))?)
    }

    pub async fn double_click(
        &self,
        selector: Option<&str>,
        text: Option<&str>,
        tab_id: Option<&str>,
    ) -> Result<String> {
        validate_locator(selector, text)?;
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let located = locate(&mut client, tab_id, selector, text).await?;
        let (x, y) = point_from_value(&located)?;
        client.visible_double_click(tab_id, x, y).await?;
        sleep(Duration::from_millis(350)).await;
        Ok(serde_json::to_string_pretty(&json!({
            "doubleClicked": located,
            "tabId": tab_id,
            "visibleCursor": true,
        }))?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn drag(
        &self,
        from_selector: Option<&str>,
        from_x: Option<f64>,
        from_y: Option<f64>,
        to_selector: Option<&str>,
        to_x: Option<f64>,
        to_y: Option<f64>,
        steps: usize,
        tab_id: Option<&str>,
    ) -> Result<String> {
        validate_point_or_selector("drag start", from_selector, from_x, from_y)?;
        validate_point_or_selector("drag end", to_selector, to_x, to_y)?;
        let steps = steps.clamp(2, 60);
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let start = if let Some(selector) = from_selector {
            point_from_value(&locate(&mut client, tab_id, Some(selector), None).await?)?
        } else {
            (from_x.unwrap_or_default(), from_y.unwrap_or_default())
        };
        let end = if let Some(selector) = to_selector {
            point_from_value(&locate(&mut client, tab_id, Some(selector), None).await?)?
        } else {
            (to_x.unwrap_or_default(), to_y.unwrap_or_default())
        };
        client.move_mouse(tab_id, start.0, start.1).await?;
        client
            .cdp(
                tab_id,
                "Input.dispatchMouseEvent",
                json!({
                    "type": "mousePressed",
                    "x": start.0,
                    "y": start.1,
                    "button": "left",
                    "buttons": 1,
                    "clickCount": 1,
                }),
            )
            .await?;
        for step in 1..=steps {
            let ratio = step as f64 / steps as f64;
            let x = start.0 + (end.0 - start.0) * ratio;
            let y = start.1 + (end.1 - start.1) * ratio;
            client
                .request(
                    "moveMouse",
                    json!({ "tabId": tab_id, "x": x, "y": y, "waitForArrival": true }),
                )
                .await?;
            client
                .cdp(
                    tab_id,
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": "mouseMoved",
                        "x": x,
                        "y": y,
                        "button": "left",
                        "buttons": 1,
                    }),
                )
                .await?;
        }
        client
            .cdp(
                tab_id,
                "Input.dispatchMouseEvent",
                json!({
                    "type": "mouseReleased",
                    "x": end.0,
                    "y": end.1,
                    "button": "left",
                    "buttons": 0,
                    "clickCount": 1,
                }),
            )
            .await?;
        Ok(serde_json::to_string_pretty(&json!({
            "tabId": tab_id,
            "from": {"x": start.0, "y": start.1},
            "to": {"x": end.0, "y": end.1},
            "steps": steps,
            "visibleCursor": true,
        }))?)
    }

    pub async fn type_text(
        &self,
        selector: &str,
        text: &str,
        clear: bool,
        submit: bool,
        tab_id: Option<&str>,
    ) -> Result<String> {
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let located = locate(&mut client, tab_id, Some(selector), None).await?;
        let (x, y) = point_from_value(&located)?;
        client.visible_click(tab_id, x, y).await?;
        let selector_json = serde_json::to_string(selector)?;
        let text_json = serde_json::to_string(text)?;
        let typed = evaluate(
            &mut client,
            tab_id,
            &format!(
                r#"(() => {{
                  const el = document.querySelector({selector_json});
                  if (!el) return {{ok:false, reason:'not found'}};
                  el.focus();
                  const next = {text_json};
                  if ('value' in el) {{
                    const prototype = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
                    const setter = Object.getOwnPropertyDescriptor(prototype, 'value')?.set;
                    const value = {clear} ? next : String(el.value || '') + next;
                    if (setter) setter.call(el, value); else el.value = value;
                    el.dispatchEvent(new InputEvent('input', {{bubbles:true, inputType:'insertText', data:next}}));
                    el.dispatchEvent(new Event('change', {{bubbles:true}}));
                    return {{ok:true, value:el.value}};
                  }}
                  if (el.isContentEditable) {{
                    el.textContent = {clear} ? next : String(el.textContent || '') + next;
                    el.dispatchEvent(new InputEvent('input', {{bubbles:true, inputType:'insertText', data:next}}));
                    return {{ok:true, value:el.textContent}};
                  }}
                  return {{ok:false, reason:'element is not editable', tag:el.tagName.toLowerCase()}};
                }})()"#
            ),
        )
        .await?;
        if !typed.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            bail!("could not type into {selector}: {typed}");
        }
        if submit {
            sleep(Duration::from_millis(50)).await;
            dispatch_key(&mut client, tab_id, "rawKeyDown", "Enter", "Enter", 13, 0).await?;
            dispatch_key(&mut client, tab_id, "keyUp", "Enter", "Enter", 13, 0).await?;
        }
        Ok(format!(
            "typed {} characters into {selector}; submitted: {submit}; visible cursor: true",
            text.chars().count()
        ))
    }

    pub async fn keypress(&self, keys: &[String], tab_id: Option<&str>) -> Result<String> {
        if keys.is_empty() || keys.len() > 50 {
            bail!("supply between 1 and 50 key combinations");
        }
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        for combination in keys {
            dispatch_key_combination(&mut client, tab_id, combination).await?;
            sleep(Duration::from_millis(20)).await;
        }
        Ok(serde_json::to_string_pretty(&json!({
            "tabId": tab_id,
            "keys": keys,
        }))?)
    }

    pub async fn select_option(
        &self,
        selector: &str,
        selector_index: usize,
        option: &str,
        tab_id: Option<&str>,
    ) -> Result<String> {
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let selector_json = serde_json::to_string(selector)?;
        let option_json = serde_json::to_string(option)?;
        let locate_expression = format!(
            r#"(() => {{
              const elements = [...document.querySelectorAll({selector_json})];
              const el = elements[{selector_index}];
              if (!el) return {{ok:false, reason:'selector index not found', matches:elements.length}};
              if (el.tagName !== 'SELECT') return {{ok:false, reason:'element is not a select', tag:el.tagName.toLowerCase()}};
              el.scrollIntoView({{block:'center', inline:'center'}});
              const rect = el.getBoundingClientRect();
              return {{ok:true, x:rect.left + rect.width / 2, y:rect.top + rect.height / 2, matches:elements.length}};
            }})()"#
        );
        let located = evaluate(&mut client, tab_id, &locate_expression).await?;
        if !located.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            bail!("select element not found: {located}");
        }
        let (x, y) = point_from_value(&located)?;
        client.visible_click(tab_id, x, y).await?;
        let select_expression = format!(
            r#"(() => {{
              const normalize = value => String(value || '').trim().replace(/\s+/g, ' ').toLowerCase();
              const elements = [...document.querySelectorAll({selector_json})];
              const el = elements[{selector_index}];
              if (!el) return {{ok:false, reason:'selector index not found', matches:elements.length}};
              const wanted = normalize({option_json});
              const choice = [...el.options].find(item => normalize(item.textContent) === wanted || normalize(item.value) === wanted)
                || [...el.options].find(item => normalize(item.textContent).includes(wanted));
              if (!choice) return {{ok:false, reason:'option not found', options:[...el.options].map(item => item.textContent.trim())}};
              el.value = choice.value;
              el.dispatchEvent(new Event('input', {{bubbles:true}}));
              el.dispatchEvent(new Event('change', {{bubbles:true}}));
              return {{ok:true, value:el.value, text:choice.textContent.trim(), selectorIndex:{selector_index}, matches:elements.length}};
            }})()"#
        );
        let selected = evaluate(&mut client, tab_id, &select_expression).await?;
        if !selected.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            bail!("could not select option: {selected}");
        }
        Ok(serde_json::to_string_pretty(&json!({
            "selected": selected,
            "tabId": tab_id,
            "visibleCursor": true,
        }))?)
    }

    pub async fn set_checked(
        &self,
        selector: &str,
        checked: bool,
        tab_id: Option<&str>,
    ) -> Result<String> {
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let selector_json = serde_json::to_string(selector)?;
        let state_expression = format!(
            r#"(() => {{
              const el = document.querySelector({selector_json});
              if (!el) return {{ok:false, reason:'not found'}};
              if (el.type !== 'checkbox' && el.type !== 'radio') return {{ok:false, reason:'element is not checkable', type:el.type}};
              el.scrollIntoView({{block:'center', inline:'center'}});
              const rect = el.getBoundingClientRect();
              return {{ok:true, checked:Boolean(el.checked), x:rect.left + rect.width / 2, y:rect.top + rect.height / 2}};
            }})()"#
        );
        let before = evaluate(&mut client, tab_id, &state_expression).await?;
        if !before.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            bail!("checkable element not found: {before}");
        }
        if before.get("checked").and_then(Value::as_bool) != Some(checked) {
            let (x, y) = point_from_value(&before)?;
            client.visible_click(tab_id, x, y).await?;
            sleep(Duration::from_millis(100)).await;
        } else {
            let (x, y) = point_from_value(&before)?;
            client.move_mouse(tab_id, x, y).await?;
        }
        let after = evaluate(&mut client, tab_id, &state_expression).await?;
        if after.get("checked").and_then(Value::as_bool) != Some(checked) {
            bail!("checkable element did not reach requested state: {after}");
        }
        Ok(serde_json::to_string_pretty(&json!({
            "checked": checked,
            "selector": selector,
            "tabId": tab_id,
            "visibleCursor": true,
        }))?)
    }

    pub async fn scroll(
        &self,
        scroll_x: f64,
        scroll_y: f64,
        selector: Option<&str>,
        x: Option<f64>,
        y: Option<f64>,
        tab_id: Option<&str>,
    ) -> Result<String> {
        if (x.is_some() && y.is_none()) || (x.is_none() && y.is_some()) {
            bail!("supply both x and y, or neither");
        }
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let point = if let Some(selector) = selector {
            locate(&mut client, tab_id, Some(selector), None).await?
        } else if let (Some(x), Some(y)) = (x, y) {
            json!({ "x": x, "y": y })
        } else {
            let metrics = client
                .cdp(tab_id, "Page.getLayoutMetrics", json!({}))
                .await?;
            let viewport = metrics
                .get("cssVisualViewport")
                .ok_or_else(|| anyhow!("Page.getLayoutMetrics returned no cssVisualViewport"))?;
            json!({
                "x": viewport.get("clientWidth").and_then(Value::as_f64).unwrap_or(1280.0) / 2.0,
                "y": viewport.get("clientHeight").and_then(Value::as_f64).unwrap_or(720.0) / 2.0,
            })
        };
        let (x, y) = point_from_value(&point)?;
        client.move_mouse(tab_id, x, y).await?;
        client
            .cdp(
                tab_id,
                "Input.synthesizeScrollGesture",
                json!({
                    "x": x,
                    "y": y,
                    "xDistance": -scroll_x,
                    "yDistance": -scroll_y,
                    "gestureSourceType": "mouse",
                    "preventFling": true,
                    "speed": 8000,
                }),
            )
            .await?;
        sleep(Duration::from_millis(250)).await;
        Ok(serde_json::to_string_pretty(&json!({
            "tabId": tab_id,
            "point": { "x": x, "y": y },
            "scrollX": scroll_x,
            "scrollY": scroll_y,
            "visibleCursor": true,
        }))?)
    }

    pub async fn wait_for(
        &self,
        selector: Option<&str>,
        text: Option<&str>,
        timeout_ms: u64,
        tab_id: Option<&str>,
    ) -> Result<String> {
        validate_locator(selector, text)?;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let expression = presence_expression(selector, text)?;
        loop {
            let result = evaluate(&mut client, tab_id, &expression).await?;
            if result
                .get("found")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(serde_json::to_string_pretty(&result)?);
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("timed out after {timeout_ms} ms waiting for element");
            }
            sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn wait_for_page(
        &self,
        url_contains: Option<&str>,
        load_state: Option<&str>,
        timeout_ms: u64,
        tab_id: Option<&str>,
    ) -> Result<String> {
        if url_contains.is_none() && load_state.is_none() {
            bail!("supply url_contains, load_state, or both");
        }
        let requested_state = load_state.unwrap_or("complete");
        if !matches!(requested_state, "loading" | "interactive" | "complete") {
            bail!("load_state must be loading, interactive, or complete");
        }
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let state = self.page_identity_with_state(&mut client, tab_id).await?;
            let url_ready = url_contains.is_none_or(|wanted| {
                state
                    .get("url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| url.contains(wanted))
            });
            let load_ready = load_state.is_none_or(|_| {
                state.get("readyState").and_then(Value::as_str) == Some(requested_state)
            });
            if url_ready && load_ready {
                return Ok(serde_json::to_string_pretty(&state)?);
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "timed out after {timeout_ms} ms waiting for URL/load state; last state: {state}"
                );
            }
            sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn clipboard(
        &self,
        action: &str,
        text: Option<&str>,
        tab_id: Option<&str>,
    ) -> Result<String> {
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        match action {
            "read" => {
                if text.is_some() {
                    bail!("clipboard read does not accept text");
                }
                let value = evaluate(&mut client, tab_id, "navigator.clipboard.readText()")
                    .await
                    .context(
                        "browser clipboard read failed; make the side pane visible and focused",
                    )?;
                Ok(serde_json::to_string_pretty(&json!({ "text": value }))?)
            }
            "write" => {
                let text = text.context("clipboard write requires text")?;
                let text_json = serde_json::to_string(text)?;
                evaluate(
                    &mut client,
                    tab_id,
                    &format!("navigator.clipboard.writeText({text_json}).then(() => true)"),
                )
                .await
                .context(
                    "browser clipboard write failed; make the side pane visible and focused",
                )?;
                Ok(format!(
                    "wrote {} characters to the browser clipboard",
                    text.chars().count()
                ))
            }
            _ => bail!("clipboard action must be read or write"),
        }
    }

    pub async fn console_logs(
        &self,
        action: &str,
        wait_ms: u64,
        tab_id: Option<&str>,
    ) -> Result<String> {
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let install = r#"(() => {
          if (globalThis.__rustBrowserLogCapture) return true;
          const entries = [];
          const push = entry => { entries.push({...entry, timestamp: Date.now()}); if (entries.length > 1000) entries.splice(0, entries.length - 1000); };
          const originals = {};
          for (const level of ['debug', 'info', 'log', 'warn', 'error']) {
            originals[level] = console[level].bind(console);
            console[level] = (...args) => {
              push({type: 'console', level, text: args.map(value => { try { return typeof value === 'string' ? value : JSON.stringify(value); } catch { return String(value); } }).join(' ').slice(0, 8000)});
              return originals[level](...args);
            };
          }
          addEventListener('error', event => push({type: 'error', level: 'error', text: String(event.message || event.error || 'page error').slice(0, 8000)}));
          addEventListener('unhandledrejection', event => push({type: 'unhandledrejection', level: 'error', text: String(event.reason || 'unhandled rejection').slice(0, 8000)}));
          globalThis.__rustBrowserLogCapture = {entries, originals};
          return true;
        })()"#;
        evaluate(&mut client, tab_id, install).await?;
        match action {
            "start" => Ok("page console capture is active; reproduce the issue, then call console_logs with action read".to_owned()),
            "clear" => {
                evaluate(&mut client, tab_id, "(__rustBrowserLogCapture.entries.length = 0, true)").await?;
                Ok("cleared captured page console logs".to_owned())
            }
            "read" => {
                if wait_ms > 0 {
                    sleep(Duration::from_millis(wait_ms.min(10_000))).await;
                }
                let logs = evaluate(&mut client, tab_id, "__rustBrowserLogCapture.entries.slice(-500)").await?;
                Ok(serde_json::to_string_pretty(&logs)?)
            }
            _ => bail!("console_logs action must be start, read, or clear"),
        }
    }

    pub async fn handle_dialog(
        &self,
        action: &str,
        prompt_text: Option<&str>,
        trigger_selector: Option<&str>,
        trigger_text: Option<&str>,
        tab_id: Option<&str>,
    ) -> Result<String> {
        if !matches!(action, "accept" | "dismiss") {
            bail!("dialog action must be accept or dismiss");
        }
        if action == "dismiss" && prompt_text.is_some() {
            bail!("prompt_text is valid only when accepting a prompt dialog");
        }
        let has_trigger = trigger_selector.is_some() || trigger_text.is_some();
        if has_trigger {
            validate_locator(trigger_selector, trigger_text)?;
        }
        let _action = self.action_lock.lock().await;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        client.cdp(tab_id, "Page.enable", json!({})).await?;
        let mut params = json!({ "accept": action == "accept" });
        if let Some(prompt_text) = prompt_text {
            params["promptText"] = Value::String(prompt_text.to_owned());
        }
        if has_trigger {
            let located = locate(&mut client, tab_id, trigger_selector, trigger_text).await?;
            let (x, y) = point_from_value(&located)?;
            client.move_mouse(tab_id, x, y).await?;
            client
                .cdp(
                    tab_id,
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": "mousePressed",
                        "x": x,
                        "y": y,
                        "button": "left",
                        "buttons": 1,
                        "clickCount": 1,
                    }),
                )
                .await?;
            let _release_id = client
                .send_cdp(
                    tab_id,
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": "mouseReleased",
                        "x": x,
                        "y": y,
                        "button": "left",
                        "buttons": 0,
                        "clickCount": 1,
                    }),
                )
                .await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while tokio::time::Instant::now() < deadline {
                let Some(event) = client.next_event(Duration::from_millis(250)).await? else {
                    continue;
                };
                if event.get("method").and_then(Value::as_str) == Some("onCDPEvent")
                    && event.pointer("/params/method").and_then(Value::as_str)
                        == Some("Page.javascriptDialogOpening")
                    && event
                        .pointer("/params/source/tabId")
                        .and_then(Value::as_u64)
                        == Some(tab_id)
                {
                    let dialog = event
                        .pointer("/params/params")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let response_id = client
                        .send_cdp(tab_id, "Page.handleJavaScriptDialog", params)
                        .await?;
                    client
                        .wait_response(response_id, "executeCdp(Page.handleJavaScriptDialog)")
                        .await?;
                    return Ok(serde_json::to_string_pretty(&json!({
                        "action": action,
                        "dialog": dialog,
                        "tabId": tab_id,
                        "trigger": located,
                        "visibleCursor": true,
                    }))?);
                }
            }
            bail!("the trigger did not open a JavaScript dialog within 5000 ms");
        }
        client
            .cdp(tab_id, "Page.handleJavaScriptDialog", params)
            .await
            .context(
                "no active JavaScript dialog could be handled; pass its trigger selector/text so the click and dialog response share one connection",
            )?;
        Ok(format!(
            "{action}ed JavaScript dialog in tab {tab_id}; trigger clicked: {has_trigger}"
        ))
    }

    pub async fn upload_files(
        &self,
        selector: &str,
        files: &[String],
        tab_id: Option<&str>,
    ) -> Result<String> {
        if selector.trim().is_empty() || files.is_empty() || files.len() > 100 {
            bail!("supply a selector and between 1 and 100 files");
        }
        let mut canonical_files = Vec::with_capacity(files.len());
        for requested in files {
            let path = Path::new(requested);
            if !path.is_absolute() {
                bail!("upload paths must be absolute: {requested}");
            }
            let canonical = std::fs::canonicalize(path)
                .with_context(|| format!("resolve upload file {requested}"))?;
            let metadata = std::fs::metadata(&canonical)
                .with_context(|| format!("inspect upload file {}", canonical.display()))?;
            if !metadata.is_file() {
                bail!("upload path is not a regular file: {}", canonical.display());
            }
            canonical_files.push(canonical.to_string_lossy().into_owned());
        }
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let selector_json = serde_json::to_string(selector)?;
        let input = evaluate(
            &mut client,
            tab_id,
            &format!(
                "(() => {{ const el = document.querySelector({selector_json}); return el ? {{tag:el.tagName.toLowerCase(), type:el.type, multiple:Boolean(el.multiple)}} : null; }})()"
            ),
        )
        .await?;
        if input.is_null()
            || input.get("tag").and_then(Value::as_str) != Some("input")
            || input.get("type").and_then(Value::as_str) != Some("file")
        {
            bail!("selector must resolve to an input[type=file]; found: {input}");
        }
        if canonical_files.len() > 1 && input.get("multiple").and_then(Value::as_bool) != Some(true)
        {
            bail!("the selected file input does not accept multiple files");
        }
        let document = client
            .cdp(tab_id, "DOM.getDocument", json!({ "depth": 0 }))
            .await?;
        let root_id = document
            .pointer("/root/nodeId")
            .and_then(Value::as_u64)
            .context("DOM.getDocument returned no root node id")?;
        let node = client
            .cdp(
                tab_id,
                "DOM.querySelector",
                json!({ "nodeId": root_id, "selector": selector }),
            )
            .await?;
        let node_id = node
            .get("nodeId")
            .and_then(Value::as_u64)
            .filter(|id| *id != 0)
            .context("file input disappeared before upload")?;
        client
            .cdp(
                tab_id,
                "DOM.setFileInputFiles",
                json!({ "files": canonical_files, "nodeId": node_id }),
            )
            .await?;
        Ok(serde_json::to_string_pretty(&json!({
            "selector": selector,
            "files": canonical_files,
            "tabId": tab_id,
        }))?)
    }

    pub async fn screenshot(
        &self,
        filename: Option<&str>,
        full_page: bool,
        selector: Option<&str>,
        tab_id: Option<&str>,
    ) -> Result<String> {
        if full_page && selector.is_some() {
            bail!("full_page and selector cannot be combined");
        }
        self.config.ensure_dirs()?;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let clip = if let Some(selector) = selector {
            let selector_json = serde_json::to_string(selector)?;
            let rect = evaluate(
                &mut client,
                tab_id,
                &format!(
                    r#"(() => {{
                      const el = document.querySelector({selector_json});
                      if (!el) return null;
                      const r = el.getBoundingClientRect();
                      return {{x:r.left + scrollX, y:r.top + scrollY, width:r.width, height:r.height, scale:1}};
                    }})()"#
                ),
            )
            .await?;
            if rect.is_null()
                || rect.get("width").and_then(Value::as_f64).unwrap_or(0.0) <= 0.0
                || rect.get("height").and_then(Value::as_f64).unwrap_or(0.0) <= 0.0
            {
                bail!("screenshot selector was not found or has no visible bounds");
            }
            Some(rect)
        } else if full_page {
            let metrics = client
                .cdp(tab_id, "Page.getLayoutMetrics", json!({}))
                .await?;
            let size = metrics
                .get("cssContentSize")
                .context("Page.getLayoutMetrics returned no content size")?;
            Some(json!({
                "x": 0,
                "y": 0,
                "width": size.get("width").and_then(Value::as_f64).unwrap_or(1280.0),
                "height": size.get("height").and_then(Value::as_f64).unwrap_or(720.0),
                "scale": 1,
            }))
        } else {
            None
        };
        let mut options = json!({ "format": "png", "fromSurface": true, "captureBeyondViewport": full_page || selector.is_some() });
        if let Some(clip) = clip {
            options["clip"] = clip;
        }
        let result = client
            .cdp(tab_id, "Page.captureScreenshot", options)
            .await?;
        let encoded = result
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("CDP screenshot returned no data"))?;
        let bytes = STANDARD.decode(encoded).context("decode screenshot")?;
        let filename = safe_png_filename(filename);
        let path = self.config.screenshot_dir.join(filename);
        tokio::fs::write(&path, &bytes)
            .await
            .with_context(|| format!("write {}", path.display()))?;
        Ok(format!(
            "screenshot: {}\nbytes: {}",
            path.display(),
            bytes.len()
        ))
    }

    pub async fn export_page(
        &self,
        format: &str,
        filename: Option<&str>,
        tab_id: Option<&str>,
    ) -> Result<String> {
        if !matches!(format, "html" | "text" | "markdown" | "pdf") {
            bail!("export format must be html, text, markdown, or pdf");
        }
        self.config.ensure_dirs()?;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let extension = if format == "markdown" { "md" } else { format };
        let filename = safe_export_filename(filename, extension);
        let path = self.config.export_dir.join(filename);
        let bytes = if format == "pdf" {
            let result = client
                .cdp(
                    tab_id,
                    "Page.printToPDF",
                    json!({
                        "printBackground": true,
                        "preferCSSPageSize": true,
                    }),
                )
                .await?;
            let encoded = result
                .get("data")
                .and_then(Value::as_str)
                .context("PDF export returned no data")?;
            STANDARD.decode(encoded).context("decode PDF export")?
        } else {
            let expression = match format {
                "html" => "document.documentElement.outerHTML",
                "text" => "document.body?.innerText || ''",
                "markdown" => {
                    "`# ${document.title || 'Export'}\\n\\nSource: ${location.href}\\n\\n${document.body?.innerText || ''}`"
                }
                _ => unreachable!(),
            };
            let value = evaluate(&mut client, tab_id, expression).await?;
            value
                .as_str()
                .context("page export did not return text")?
                .as_bytes()
                .to_vec()
        };
        tokio::fs::write(&path, &bytes)
            .await
            .with_context(|| format!("write page export {}", path.display()))?;
        Ok(format!(
            "page export: {}\nformat: {format}\nbytes: {}",
            path.display(),
            bytes.len()
        ))
    }

    pub async fn page_assets_list(&self, tab_id: Option<&str>) -> Result<String> {
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let raw = evaluate(
            &mut client,
            tab_id,
            r#"(() => {
              const classify = (url, hint = '') => {
                const clean = String(url || '').split(/[?#]/)[0].toLowerCase();
                const type = String(hint || '').toLowerCase();
                if (/\.(png|jpe?g|gif|webp|avif|svg|ico|bmp)$/.test(clean) || type === 'img' || type === 'image') return 'image';
                if (/\.(woff2?|ttf|otf|eot)$/.test(clean) || type === 'font') return 'font';
                if (/\.css$/.test(clean) || type === 'link' || type === 'css') return 'stylesheet';
                if (/\.(mp4|webm|mov|m4v|ogv)$/.test(clean) || type === 'video') return 'video';
                if (/\.js$/.test(clean) || type === 'script') return 'script';
                return 'other';
              };
              const rows = new Map();
              const add = (url, hint, source) => {
                try {
                  const absolute = new URL(url, location.href).href;
                  if (!/^https?:/.test(absolute)) return;
                  const existing = rows.get(absolute) || {url:absolute, kind:classify(absolute, hint), sources:[]};
                  existing.sources.push(source);
                  rows.set(absolute, existing);
                } catch {}
              };
              for (const [selector, attribute, hint] of [
                ['img[src]', 'src', 'image'], ['img[srcset]', 'srcset', 'image'],
                ['source[src]', 'src', 'video'], ['video[src]', 'src', 'video'],
                ['link[href]', 'href', 'link'], ['script[src]', 'src', 'script']
              ]) {
                for (const node of document.querySelectorAll(selector)) {
                  const value = node.getAttribute(attribute);
                  if (attribute === 'srcset') {
                    for (const part of String(value || '').split(',')) add(part.trim().split(/\s+/)[0], hint, {kind:'attribute', property:attribute});
                  } else add(value, hint, {kind:'attribute', property:attribute});
                }
              }
              for (const entry of performance.getEntriesByType('resource')) add(entry.name, entry.initiatorType, {kind:'resource'});
              const inlineSvgs = [...document.querySelectorAll('svg')].slice(0, 500).map((svg, index) => ({name:`inline-${index + 1}.svg`, markup:svg.outerHTML.slice(0, 200000)}));
              return {pageUrl:location.href, assets:[...rows.values()].slice(0, 5000), inlineSvgs};
            })()"#,
        )
        .await?;
        let page_url = raw
            .get("pageUrl")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let inventory_id = format!(
            "inv-{}-{:x}",
            now_millis(),
            stable_hash(&(page_url.clone(), tab_id))
        );
        let mut assets = Vec::new();
        for (index, raw_asset) in raw
            .get("assets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let url = raw_asset
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if self.config.validate_url(&url).is_err() {
                continue;
            }
            let kind = raw_asset
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("other")
                .to_owned();
            assets.push(PageAsset {
                id: format!("asset-{}-{:x}", index + 1, stable_hash(&url)),
                name: asset_name_from_url(&url, index + 1),
                url,
                kind,
                sources: raw_asset
                    .get("sources")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        let inline_svgs = raw
            .get("inlineSvgs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, svg)| InlineSvg {
                id: format!("svg-{}", index + 1),
                name: svg
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("inline.svg")
                    .to_owned(),
                markup: svg
                    .get("markup")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
            .collect::<Vec<_>>();
        let inventory = AssetInventory {
            id: inventory_id.clone(),
            page_url: page_url.clone(),
            assets,
            inline_svgs,
        };
        let summary = asset_summary(&inventory.assets);
        let response = json!({
            "id": inventory.id,
            "pageUrl": inventory.page_url,
            "assets": inventory.assets,
            "inlineSvgs": inventory.inline_svgs,
            "summary": {
                "byKind": summary,
                "inlineSvgCount": inventory.inline_svgs.len(),
                "totalCount": inventory.assets.len(),
            }
        });
        let mut state = self.state.lock().await;
        if state.asset_inventories.len() >= 16
            && let Some(oldest) = state.asset_inventories.keys().min().cloned()
        {
            state.asset_inventories.remove(&oldest);
        }
        state.asset_inventories.insert(inventory_id, inventory);
        Ok(serde_json::to_string_pretty(&response)?)
    }

    pub async fn page_assets_bundle(
        &self,
        inventory_id: &str,
        asset_ids: &[String],
        kinds: &[String],
        tab_id: Option<&str>,
    ) -> Result<String> {
        let inventory = self
            .state
            .lock()
            .await
            .asset_inventories
            .get(inventory_id)
            .cloned()
            .with_context(|| {
                format!(
                    "unknown or expired asset inventory {inventory_id}; call page_assets list again"
                )
            })?;
        for kind in kinds {
            if !matches!(kind.as_str(), "font" | "image" | "stylesheet" | "video") {
                bail!("bundle kind must be font, image, stylesheet, or video: {kind}");
            }
        }
        let requested = inventory
            .assets
            .iter()
            .filter(|asset| {
                (asset_ids.is_empty() || asset_ids.contains(&asset.id))
                    && (kinds.is_empty() || kinds.contains(&asset.kind))
                    && matches!(
                        asset.kind.as_str(),
                        "font" | "image" | "stylesheet" | "video"
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        if requested.is_empty() {
            bail!("no downloadable assets matched this inventory and filter");
        }
        self.config.ensure_dirs()?;
        let directory = self.config.asset_dir.join(safe_component(inventory_id));
        tokio::fs::create_dir_all(&directory).await?;
        let started = tokio::time::Instant::now();
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let mut successes = Vec::new();
        let mut failures = Vec::new();
        let mut total_bytes = 0_usize;
        for asset in &requested {
            if total_bytes >= MAX_ASSET_BUNDLE_BYTES {
                failures.push(json!({
                    "id": asset.id, "name": asset.name, "url": asset.url,
                    "contentType": Value::Null, "reason": "bundle byte limit reached"
                }));
                continue;
            }
            let url_json = serde_json::to_string(&asset.url)?;
            let expression = format!(
                r#"fetch({url_json}, {{credentials:'include'}}).then(async response => {{
                  if (!response.ok) return {{ok:false, reason:`HTTP ${{response.status}}`, contentType:response.headers.get('content-type')}};
                  const bytes = new Uint8Array(await response.arrayBuffer());
                  if (bytes.byteLength > {MAX_ASSET_BYTES}) return {{ok:false, reason:`asset exceeds {MAX_ASSET_BYTES} byte limit`, contentType:response.headers.get('content-type')}};
                  let binary = '';
                  for (let offset = 0; offset < bytes.length; offset += 32768) binary += String.fromCharCode(...bytes.subarray(offset, offset + 32768));
                  return {{ok:true, data:btoa(binary), size:bytes.byteLength, contentType:response.headers.get('content-type')}};
                }}).catch(error => ({{ok:false, reason:String(error), contentType:null}}))"#
            );
            let fetched = evaluate(&mut client, tab_id, &expression).await?;
            if fetched.get("ok").and_then(Value::as_bool) != Some(true) {
                failures.push(json!({
                    "id": asset.id,
                    "name": asset.name,
                    "url": asset.url,
                    "contentType": fetched.get("contentType").cloned().unwrap_or(Value::Null),
                    "reason": fetched.get("reason").and_then(Value::as_str).unwrap_or("fetch failed"),
                }));
                continue;
            }
            let encoded = fetched
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let bytes = STANDARD
                .decode(encoded)
                .context("decode bundled page asset")?;
            if total_bytes.saturating_add(bytes.len()) > MAX_ASSET_BUNDLE_BYTES {
                failures.push(json!({
                    "id": asset.id, "name": asset.name, "url": asset.url,
                    "contentType": fetched.get("contentType").cloned().unwrap_or(Value::Null),
                    "reason": "bundle byte limit would be exceeded"
                }));
                continue;
            }
            total_bytes += bytes.len();
            let filename = format!(
                "{}-{}",
                safe_component(&asset.id),
                safe_component(&asset.name)
            );
            let path = directory.join(filename);
            tokio::fs::write(&path, &bytes).await?;
            successes.push(json!({
                "id": asset.id,
                "kind": asset.kind,
                "name": asset.name,
                "path": path,
                "url": asset.url,
                "contentType": fetched.get("contentType").cloned().unwrap_or(Value::Null),
            }));
        }
        let manifest_path = directory.join("manifest.json");
        let result = json!({
            "assets": successes,
            "directoryPath": directory,
            "failures": failures,
            "manifestPath": manifest_path,
            "summary": {
                "downloadedCount": successes.len(),
                "elapsedMs": started.elapsed().as_millis(),
                "failedCount": failures.len(),
                "requestedCount": requested.len(),
                "bytes": total_bytes,
            }
        });
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&result)?).await?;
        Ok(serde_json::to_string_pretty(&result)?)
    }

    pub async fn cdp_command(
        &self,
        method: &str,
        params: Value,
        tab_id: Option<&str>,
    ) -> Result<String> {
        if !self.config.raw_cdp_enabled {
            bail!(
                "raw CDP is disabled; set RUST_BROWSER_ENABLE_RAW_CDP=1 before launching Codex Desktop to enable developer-mode commands"
            );
        }
        if method.trim().is_empty() || method.len() > 200 || !method.contains('.') {
            bail!("CDP method must be a non-empty Domain.method name");
        }
        if !params.is_object() {
            bail!("CDP params must be a JSON object");
        }
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let result = client.cdp(tab_id, method, params).await?;
        Ok(serde_json::to_string_pretty(&result)?)
    }

    pub async fn cdp_events(
        &self,
        methods: &[String],
        timeout_ms: u64,
        limit: usize,
        tab_id: Option<&str>,
    ) -> Result<String> {
        if !self.config.raw_cdp_enabled {
            bail!(
                "raw CDP events are disabled; set RUST_BROWSER_ENABLE_RAW_CDP=1 before launching Codex Desktop"
            );
        }
        if methods.is_empty() || methods.len() > 100 {
            bail!("supply between 1 and 100 CDP event method names");
        }
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        for domain in methods.iter().filter_map(|method| method.split('.').next()) {
            if matches!(domain, "Runtime" | "Log" | "Network" | "Page") {
                let _ = client
                    .cdp(tab_id, &format!("{domain}.enable"), json!({}))
                    .await;
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms.min(30_000));
        let mut events = Vec::new();
        while tokio::time::Instant::now() < deadline && events.len() < limit.clamp(1, 1000) {
            let Some(event) = client.next_event(Duration::from_millis(250)).await? else {
                continue;
            };
            if event.get("method").and_then(Value::as_str) != Some("onCDPEvent") {
                continue;
            }
            let event_method = event.pointer("/params/method").and_then(Value::as_str);
            if event_method.is_some_and(|method| methods.iter().any(|wanted| wanted == method)) {
                events.push(event.pointer("/params").cloned().unwrap_or(event));
            }
        }
        Ok(serde_json::to_string_pretty(&json!({
            "tabId": tab_id,
            "events": events,
            "count": events.len(),
        }))?)
    }

    pub async fn click_and_wait_for_download(
        &self,
        selector: Option<&str>,
        text: Option<&str>,
        timeout_ms: u64,
        tab_id: Option<&str>,
    ) -> Result<String> {
        validate_locator(selector, text)?;
        let _action = self.action_lock.lock().await;
        self.config.ensure_dirs()?;
        let before = directory_state(&self.config.download_dir)?;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        self.configure_downloads(&mut client, tab_id).await;
        client
            .cdp(
                tab_id,
                "Fetch.enable",
                json!({
                    "patterns": [{
                        "requestStage": "Response",
                        "resourceType": "Document"
                    }]
                }),
            )
            .await?;
        let located = locate(&mut client, tab_id, selector, text).await?;
        let (x, y) = point_from_value(&located)?;
        client.visible_click(tab_id, x, y).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut allowed_url = None::<String>;
        let mut backend_filename = None::<String>;
        loop {
            if let Some((path, size)) = completed_download(&self.config.download_dir, &before)? {
                let _ = client.cdp(tab_id, "Fetch.disable", json!({})).await;
                return Ok(format!(
                    "download complete: {}\nbytes: {size}\nsource URL authorized: {}",
                    path.display(),
                    allowed_url.is_some()
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = client.cdp(tab_id, "Fetch.disable", json!({})).await;
                bail!(
                    "no completed new download appeared in {} within {timeout_ms} ms; source URL authorized: {}; backend filename: {}",
                    self.config.download_dir.display(),
                    allowed_url.is_some(),
                    backend_filename.as_deref().unwrap_or("none")
                );
            }
            let Some(event) = client.next_event(Duration::from_millis(250)).await? else {
                continue;
            };
            let event_method = event.get("method").and_then(Value::as_str);
            if event_method == Some("onCDPEvent")
                && event.pointer("/params/method").and_then(Value::as_str)
                    == Some("Fetch.requestPaused")
                && event
                    .pointer("/params/source/tabId")
                    .and_then(Value::as_u64)
                    == Some(tab_id)
            {
                let paused = event
                    .pointer("/params/params")
                    .ok_or_else(|| anyhow!("Fetch.requestPaused event had no params: {event}"))?;
                let request_id = paused
                    .get("requestId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Fetch.requestPaused had no requestId: {event}"))?;
                let url = paused
                    .pointer("/request/url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Fetch.requestPaused had no request URL: {event}"))?;
                self.config
                    .validate_url(url)
                    .with_context(|| format!("download source URL is not allowed: {url}"))?;
                client
                    .request("allowDownload", json!({ "tabId": tab_id, "url": url }))
                    .await?;
                client
                    .cdp(
                        tab_id,
                        "Fetch.continueResponse",
                        json!({ "requestId": request_id }),
                    )
                    .await?;
                allowed_url = Some(url.to_owned());
            } else if event_method == Some("onDownloadChange") {
                if let Some(filename) = event.pointer("/params/filename").and_then(Value::as_str) {
                    backend_filename = Some(filename.to_owned());
                }
                if event.pointer("/params/status").and_then(Value::as_str) == Some("complete")
                    && let Some(filename) = backend_filename.as_deref()
                {
                    let path = PathBuf::from(filename);
                    if let Ok(metadata) = std::fs::metadata(&path) {
                        let _ = client.cdp(tab_id, "Fetch.disable", json!({})).await;
                        return Ok(format!(
                            "download complete: {}\nbytes: {}\nsource URL authorized: {}",
                            path.display(),
                            metadata.len(),
                            allowed_url.is_some()
                        ));
                    }
                }
            }
        }
    }

    async fn connect(&self) -> Result<IabClient> {
        let turn_id = self.config.current_turn_id();
        let mut entries = tokio::fs::read_dir(&self.config.socket_dir)
            .await
            .with_context(|| format!("read {}", self.config.socket_dir.display()))?;
        let mut failures = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;
            if !file_type.is_socket() || path.extension().and_then(OsStr::to_str) != Some("sock") {
                continue;
            }
            let mut client =
                match IabClient::connect(&path, &self.config.session_id, &turn_id).await {
                    Ok(client) => client,
                    Err(error) => {
                        failures.push(format!("{}: {error:#}", path.display()));
                        continue;
                    }
                };
            match client.get_info().await {
                Ok(info)
                    if info.get("type").and_then(Value::as_str) == Some("iab")
                        && info
                            .pointer("/metadata/codexSessionId")
                            .and_then(Value::as_str)
                            == Some(self.config.session_id.as_str()) =>
                {
                    return Ok(client);
                }
                Ok(_) => {}
                Err(error) => failures.push(format!("{}: {error:#}", path.display())),
            }
        }
        bail!(
            "no Codex in-app browser socket matched task {} in {}; open the side-pane Browser for this task and retry. probes: {}",
            self.config.session_id,
            self.config.socket_dir.display(),
            failures.into_iter().take(4).collect::<Vec<_>>().join(" | ")
        )
    }

    async fn resolve_tab(&self, client: &mut IabClient, requested: Option<&str>) -> Result<u64> {
        let tabs = client.tabs().await?;
        let requested = requested.map(parse_tab_id).transpose()?;
        let preferred = self.state.lock().await.preferred_tab;
        let chosen = requested
            .and_then(|id| tabs.iter().find(|tab| tab.id == id))
            .or_else(|| preferred.and_then(|id| tabs.iter().find(|tab| tab.id == id)))
            .or_else(|| tabs.iter().find(|tab| tab.active))
            .or_else(|| tabs.first())
            .map(|tab| tab.id);
        let tab_id = if let Some(id) = chosen {
            id
        } else {
            let user_tabs = client.request("getUserTabs", json!({})).await.ok();
            if let Some(id) = user_tabs
                .as_ref()
                .and_then(Value::as_array)
                .and_then(|tabs| {
                    tabs.iter()
                        .find(|tab| tab.get("active").and_then(Value::as_bool) == Some(true))
                        .or_else(|| tabs.first())
                })
                .and_then(value_tab_id)
            {
                client
                    .request("claimUserTab", json!({ "tabId": id }))
                    .await?;
                id
            } else {
                let created = client.request("createTab", json!({})).await?;
                value_tab_id(&created).context("createTab returned no tab id")?
            }
        };
        client.attach(tab_id).await?;
        self.state.lock().await.preferred_tab = Some(tab_id);
        Ok(tab_id)
    }

    async fn wait_ready(&self, client: &mut IabClient, tab_id: u64, timeout_ms: u64) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if evaluate(client, tab_id, "document.readyState")
                .await
                .ok()
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .as_deref()
                == Some("complete")
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("page did not reach readyState=complete within {timeout_ms} ms");
            }
            sleep(Duration::from_millis(200)).await;
        }
    }

    async fn page_identity(&self, client: &mut IabClient, tab_id: u64) -> Result<Value> {
        evaluate(
            client,
            tab_id,
            "({url: location.href, title: document.title})",
        )
        .await
    }

    async fn page_identity_with_state(&self, client: &mut IabClient, tab_id: u64) -> Result<Value> {
        evaluate(
            client,
            tab_id,
            "({url: location.href, title: document.title, readyState: document.readyState})",
        )
        .await
    }

    async fn configure_downloads(&self, client: &mut IabClient, tab_id: u64) {
        if self.config.ensure_dirs().is_err() {
            return;
        }
        let _ = client
            .cdp(
                tab_id,
                "Browser.setDownloadBehavior",
                json!({
                    "behavior": "allow",
                    "downloadPath": self.config.download_dir.to_string_lossy(),
                    "eventsEnabled": true,
                }),
            )
            .await;
    }
}

async fn evaluate(client: &mut IabClient, tab_id: u64, expression: &str) -> Result<Value> {
    let result = client
        .cdp(
            tab_id,
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
                "userGesture": true,
            }),
        )
        .await?;
    if let Some(details) = result.get("exceptionDetails") {
        bail!("JavaScript evaluation failed: {details}");
    }
    Ok(result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null))
}

async fn locate(
    client: &mut IabClient,
    tab_id: u64,
    selector: Option<&str>,
    text: Option<&str>,
) -> Result<Value> {
    let expression = locate_expression(selector, text)?;
    let result = evaluate(client, tab_id, &expression).await?;
    if !result.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        bail!("element not found or not clickable: {result}");
    }
    Ok(result)
}

fn locate_expression(selector: Option<&str>, text: Option<&str>) -> Result<String> {
    validate_locator(selector, text)?;
    let selector_json = serde_json::to_string(&selector)?;
    let text_json = serde_json::to_string(&text)?;
    Ok(format!(
        r#"(() => {{
          const selector = {selector_json};
          const wanted = {text_json};
          const normalize = value => String(value || '').trim().replace(/\s+/g, ' ');
          let el = selector ? document.querySelector(selector) : null;
          if (!el && wanted) {{
            const candidates = [...document.querySelectorAll('button,a,input,[role="button"],[role="link"],[tabindex],.product-animation,body *')];
            el = candidates.find(node => normalize(node.innerText || node.value || node.getAttribute('aria-label') || node.getAttribute('title')) === normalize(wanted))
              || candidates.find(node => normalize(node.innerText || node.value || node.getAttribute('aria-label') || node.getAttribute('title')).includes(normalize(wanted)));
          }}
          if (!el) return {{ok:false, reason:'not found'}};
          if (el.disabled || el.getAttribute('aria-disabled') === 'true') return {{ok:false, reason:'disabled'}};
          el.scrollIntoView({{block:'center', inline:'center'}});
          const rect = el.getBoundingClientRect();
          if (!rect.width || !rect.height) return {{ok:false, reason:'not visible'}};
          return {{
            ok:true,
            tag:el.tagName.toLowerCase(),
            text:normalize(el.innerText || el.value || el.getAttribute('aria-label')).slice(0, 200),
            x:Math.max(1, Math.min(innerWidth - 1, rect.left + rect.width / 2)),
            y:Math.max(1, Math.min(innerHeight - 1, rect.top + rect.height / 2))
          }};
        }})()"#,
    ))
}

fn presence_expression(selector: Option<&str>, text: Option<&str>) -> Result<String> {
    validate_locator(selector, text)?;
    let selector_json = serde_json::to_string(&selector)?;
    let text_json = serde_json::to_string(&text)?;
    Ok(format!(
        r#"(() => {{
          const selector = {selector_json};
          const wanted = {text_json};
          const normalize = value => String(value || '').trim().replace(/\s+/g, ' ');
          let el = selector ? document.querySelector(selector) : null;
          if (!el && wanted) el = [...document.querySelectorAll('body *')].find(node => normalize(node.innerText) === normalize(wanted));
          return el ? {{found:true, tag:el.tagName.toLowerCase(), text:normalize(el.innerText || el.value || el.getAttribute('aria-label')).slice(0, 200)}} : {{found:false}};
        }})()"#,
    ))
}

async fn dispatch_key(
    client: &mut IabClient,
    tab_id: u64,
    event_type: &str,
    key: &str,
    code: &str,
    virtual_key: u16,
    modifiers: u8,
) -> Result<()> {
    client
        .cdp(
            tab_id,
            "Input.dispatchKeyEvent",
            json!({
                "type": event_type,
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": virtual_key,
                "nativeVirtualKeyCode": virtual_key,
                "modifiers": modifiers,
            }),
        )
        .await?;
    Ok(())
}

async fn dispatch_key_combination(
    client: &mut IabClient,
    tab_id: u64,
    combination: &str,
) -> Result<()> {
    let parts = combination
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        bail!("key combination cannot be empty");
    }
    let key_name = parts.last().copied().unwrap_or_default();
    let mut modifiers = 0_u8;
    for modifier in &parts[..parts.len() - 1] {
        match modifier.to_ascii_lowercase().as_str() {
            "alt" | "option" => modifiers |= 1,
            "ctrl" | "control" => modifiers |= 2,
            "meta" | "cmd" | "command" | "super" => modifiers |= 4,
            "shift" => modifiers |= 8,
            unknown => bail!("unknown key modifier {unknown} in {combination}"),
        }
    }
    let normalized = key_name.to_ascii_lowercase();
    let (key, code, virtual_key, text) = match normalized.as_str() {
        "enter" | "return" => ("Enter".to_owned(), "Enter".to_owned(), 13, None),
        "tab" => ("Tab".to_owned(), "Tab".to_owned(), 9, None),
        "escape" | "esc" => ("Escape".to_owned(), "Escape".to_owned(), 27, None),
        "backspace" => ("Backspace".to_owned(), "Backspace".to_owned(), 8, None),
        "delete" | "del" => ("Delete".to_owned(), "Delete".to_owned(), 46, None),
        "arrowup" | "up" => ("ArrowUp".to_owned(), "ArrowUp".to_owned(), 38, None),
        "arrowdown" | "down" => ("ArrowDown".to_owned(), "ArrowDown".to_owned(), 40, None),
        "arrowleft" | "left" => ("ArrowLeft".to_owned(), "ArrowLeft".to_owned(), 37, None),
        "arrowright" | "right" => ("ArrowRight".to_owned(), "ArrowRight".to_owned(), 39, None),
        "pageup" => ("PageUp".to_owned(), "PageUp".to_owned(), 33, None),
        "pagedown" => ("PageDown".to_owned(), "PageDown".to_owned(), 34, None),
        "home" => ("Home".to_owned(), "Home".to_owned(), 36, None),
        "end" => ("End".to_owned(), "End".to_owned(), 35, None),
        "space" => (" ".to_owned(), "Space".to_owned(), 32, Some(" ".to_owned())),
        _ if key_name.chars().count() == 1 => {
            let character = key_name.chars().next().unwrap_or_default();
            let upper = character.to_ascii_uppercase();
            let rendered = if modifiers & 8 != 0 {
                upper.to_string()
            } else {
                character.to_string()
            };
            (
                rendered.clone(),
                format!("Key{upper}"),
                upper as u16,
                (modifiers & 7 == 0).then_some(rendered),
            )
        }
        _ => bail!("unsupported key name {key_name}"),
    };
    let mut key_down = json!({
        "type": "rawKeyDown",
        "key": key,
        "code": code,
        "windowsVirtualKeyCode": virtual_key,
        "nativeVirtualKeyCode": virtual_key,
        "modifiers": modifiers,
    });
    if let Some(text) = text {
        key_down["text"] = Value::String(text);
    }
    client
        .cdp(tab_id, "Input.dispatchKeyEvent", key_down)
        .await?;
    client
        .cdp(
            tab_id,
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": virtual_key,
                "nativeVirtualKeyCode": virtual_key,
                "modifiers": modifiers,
            }),
        )
        .await?;
    Ok(())
}

fn point_from_value(value: &Value) -> Result<(f64, f64)> {
    let x = value
        .get("x")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("point has no numeric x coordinate: {value}"))?;
    let y = value
        .get("y")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("point has no numeric y coordinate: {value}"))?;
    Ok((x, y))
}

fn value_tab_id(value: &Value) -> Option<u64> {
    value
        .get("id")
        .or_else(|| value.get("tabId"))
        .and_then(|id| {
            id.as_u64()
                .or_else(|| id.as_str().and_then(|raw| raw.parse().ok()))
        })
        .or_else(|| value.as_u64())
}

fn parse_tab_id(raw: &str) -> Result<u64> {
    raw.parse::<u64>()
        .with_context(|| format!("tab id must be a positive integer: {raw}"))
}

fn validate_locator(selector: Option<&str>, text: Option<&str>) -> Result<()> {
    match (
        selector.filter(|value| !value.is_empty()),
        text.filter(|value| !value.is_empty()),
    ) {
        (Some(_), None) | (None, Some(_)) => Ok(()),
        _ => bail!("supply exactly one non-empty locator: selector or text"),
    }
}

fn validate_point_or_selector(
    label: &str,
    selector: Option<&str>,
    x: Option<f64>,
    y: Option<f64>,
) -> Result<()> {
    match (selector.filter(|value| !value.is_empty()), x, y) {
        (Some(_), None, None) | (None, Some(_), Some(_)) => Ok(()),
        _ => bail!("{label} requires either a non-empty selector or both x and y coordinates"),
    }
}

fn env_flag(name: &str) -> bool {
    env::var(name).ok().is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn stable_hash<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn safe_component(raw: &str) -> String {
    let clean = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .take(180)
        .collect::<String>();
    if clean.is_empty() {
        "item".to_owned()
    } else {
        clean
    }
}

fn safe_export_filename(requested: Option<&str>, extension: &str) -> String {
    let fallback = format!("page-export-{}.{}", now_millis(), extension);
    let Some(raw) = requested else {
        return fallback;
    };
    let basename = Path::new(raw)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    let mut clean = safe_component(basename);
    let expected = format!(".{extension}");
    if !clean.to_ascii_lowercase().ends_with(&expected) {
        clean.push_str(&expected);
    }
    clean
}

fn asset_name_from_url(raw: &str, index: usize) -> String {
    Url::parse(raw)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|name| !name.is_empty())
                .map(safe_component)
        })
        .unwrap_or_else(|| format!("asset-{index}"))
}

fn asset_summary(assets: &[PageAsset]) -> BTreeMap<String, usize> {
    let mut summary = BTreeMap::new();
    for asset in assets {
        *summary.entry(asset.kind.clone()).or_default() += 1;
    }
    summary
}

fn latest_turn_id(sessions_root: &Path, session_id: &str) -> Option<String> {
    let mut candidates = Vec::new();
    collect_rollouts(sessions_root, session_id, &mut candidates);
    candidates.sort_by_key(|path| {
        path.metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH)
    });
    let path = candidates.pop()?;
    let content = std::fs::read_to_string(path).ok()?;
    content.lines().rev().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        if value.get("type").and_then(Value::as_str) != Some("turn_context") {
            return None;
        }
        value
            .pointer("/payload/turn_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn collect_rollouts(root: &Path, session_id: &str, output: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rollouts(&path, session_id, output);
        } else if path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.ends_with(".jsonl") && name.contains(session_id))
        {
            output.push(path);
        }
    }
}

fn safe_png_filename(requested: Option<&str>) -> String {
    let fallback = format!(
        "screenshot-{}.png",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let Some(raw) = requested else {
        return fallback;
    };
    let basename = Path::new(raw)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    let clean = basename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if clean.is_empty() {
        fallback
    } else if clean.to_ascii_lowercase().ends_with(".png") {
        clean
    } else {
        format!("{clean}.png")
    }
}

type FileState = BTreeMap<PathBuf, (u64, Option<SystemTime>)>;

fn directory_state(directory: &Path) -> Result<FileState> {
    let mut state = BTreeMap::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            state.insert(entry.path(), (metadata.len(), metadata.modified().ok()));
        }
    }
    Ok(state)
}

fn completed_download(directory: &Path, before: &FileState) -> Result<Option<(PathBuf, u64)>> {
    let now = directory_state(directory)?;
    let has_partial = now
        .keys()
        .any(|path| path.extension().and_then(OsStr::to_str) == Some("crdownload"));
    if has_partial {
        return Ok(None);
    }
    for (path, (size, modified)) in now.into_iter().rev() {
        let changed = before.get(&path).is_none_or(|old| old != &(size, modified));
        if changed && size > 0 {
            return Ok(Some((path, size)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BrowserConfig {
        BrowserConfig {
            socket_dir: PathBuf::from("/tmp/codex-browser-use"),
            sessions_root: PathBuf::from("sessions"),
            session_id: "test-task".to_owned(),
            download_dir: PathBuf::from("downloads"),
            screenshot_dir: PathBuf::from("screenshots"),
            export_dir: PathBuf::from("exports"),
            asset_dir: PathBuf::from("assets"),
            allowed_hosts: vec!["example.com".to_owned()],
            raw_cdp_enabled: false,
        }
    }

    #[test]
    fn validates_subdomains_without_suffix_confusion() {
        let config = test_config();
        assert!(config.validate_url("https://www.example.com/").is_ok());
        assert!(config.validate_url("https://evil-example.com/").is_err());
        assert!(config.validate_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn wildcard_allows_http_but_not_local_file_urls() {
        let mut config = test_config();
        config.allowed_hosts = vec!["*".to_owned()];
        assert!(config.validate_url("https://openai.com/").is_ok());
        assert!(config.validate_url("http://127.0.0.1:3000/").is_ok());
        assert!(config.validate_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn sanitizes_screenshot_names() {
        assert_eq!(
            safe_png_filename(Some("../../browser capture")),
            "browser_capture.png"
        );
        assert_eq!(safe_png_filename(Some("frame.png")), "frame.png");
    }

    #[test]
    fn sanitizes_exports_and_asset_names() {
        assert_eq!(
            safe_export_filename(Some("../../report final"), "pdf"),
            "report_final.pdf"
        );
        assert_eq!(
            asset_name_from_url("https://example.com/media/photo.webp?size=2", 1),
            "photo.webp"
        );
    }

    #[test]
    fn locator_requires_exactly_one_choice() {
        assert!(validate_locator(Some("button"), None).is_ok());
        assert!(validate_locator(None, Some("Download")).is_ok());
        assert!(validate_locator(None, None).is_err());
        assert!(validate_locator(Some("button"), Some("Download")).is_err());
    }

    #[test]
    fn drag_endpoint_requires_one_addressing_mode() {
        assert!(validate_point_or_selector("start", Some("#item"), None, None).is_ok());
        assert!(validate_point_or_selector("start", None, Some(1.0), Some(2.0)).is_ok());
        assert!(validate_point_or_selector("start", Some("#item"), Some(1.0), Some(2.0)).is_err());
        assert!(validate_point_or_selector("start", None, Some(1.0), None).is_err());
    }

    #[test]
    fn reads_latest_turn_context() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rollout-test-task.jsonl");
        std::fs::write(
            path,
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"old\"}}\n{\"type\":\"event_msg\"}\n{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"new\"}}\n",
        )
        .unwrap();
        assert_eq!(
            latest_turn_id(temp.path(), "test-task").as_deref(),
            Some("new")
        );
    }
}
