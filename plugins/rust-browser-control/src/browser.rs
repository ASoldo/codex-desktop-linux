use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    env,
    ffi::OsStr,
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

#[derive(Clone, Debug)]
pub struct BrowserConfig {
    socket_dir: PathBuf,
    sessions_root: PathBuf,
    session_id: String,
    download_dir: PathBuf,
    screenshot_dir: PathBuf,
    allowed_hosts: Vec<String>,
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
            allowed_hosts,
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

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
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
            "connected: true\nbackend: {}\nsocket: {}\ntask: {}\nturn: {}\ntabs: {}\ndownloads: {}\nscreenshots: {}\nallowed hosts: {}",
            info.get("name")
                .and_then(Value::as_str)
                .unwrap_or("Codex in-app browser"),
            client.socket_path.display(),
            self.config.session_id,
            client.turn_id,
            tabs.len(),
            self.config.download_dir.display(),
            self.config.screenshot_dir.display(),
            self.config.allowed_hosts.join(", ")
        ))
    }

    pub async fn list_tabs(&self) -> Result<String> {
        let mut client = self.connect().await?;
        Ok(serde_json::to_string_pretty(&client.tabs().await?)?)
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

    pub async fn screenshot(&self, filename: Option<&str>, tab_id: Option<&str>) -> Result<String> {
        self.config.ensure_dirs()?;
        let mut client = self.connect().await?;
        let tab_id = self.resolve_tab(&mut client, tab_id).await?;
        let result = client
            .cdp(
                tab_id,
                "Page.captureScreenshot",
                json!({ "format": "png", "fromSurface": true }),
            )
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
                if event.pointer("/params/status").and_then(Value::as_str) == Some("complete") {
                    if let Some(filename) = backend_filename.as_deref() {
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
            allowed_hosts: vec!["example.com".to_owned()],
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
    fn locator_requires_exactly_one_choice() {
        assert!(validate_locator(Some("button"), None).is_ok());
        assert!(validate_locator(None, Some("Download")).is_ok());
        assert!(validate_locator(None, None).is_err());
        assert!(validate_locator(Some("button"), Some("Download")).is_err());
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
