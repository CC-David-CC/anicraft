//! Same-origin transport for the WebOS account window.

use anicraft_engine::{PreparedSave, format};
use js_sys::{Array, ArrayBuffer, JSON, Object, Reflect, Uint8Array};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::VecDeque, path::PathBuf, rc::Rc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{MessageEvent, Window};

use crate::app::App;

const PREFIX: &str = "anicraft:account-";
const MAX_QUEUED_MESSAGES: usize = 8;
pub const MAX_BYTES: usize = 128 * 1024 * 1024;

/// The current browser document's request channel to its desktop parent.
pub struct AccountChannel {
    browser: Window,
    parent: Window,
    origin: String,
    generation: String,
    profile: String,
    inbox: Rc<RefCell<VecDeque<JsValue>>>,
    listener: Closure<dyn FnMut(MessageEvent)>,
}

impl AccountChannel {
    /// A standalone browser document uses its existing local commands and storage.
    pub fn connect(ctx: &egui::Context) -> Result<Option<Self>, String> {
        let browser = web_sys::window().ok_or(tr!("No browser window."))?;
        let address = browser.location().href().map_err(|_| tr!("The browser address is unavailable."))?;
        let address = web_sys::Url::new(&address).map_err(|_| tr!("Invalid browser address."))?;
        let params = address.search_params();
        if params.get("host").as_deref() != Some("webos-account") {
            return Ok(None);
        }
        let profile = params
            .get("profile")
            .filter(|value| {
                !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
            })
            .ok_or(tr!("AniCraft needs a valid account profile."))?;
        let parent = browser.parent().ok().flatten().ok_or(tr!("No parent desktop."))?;
        let origin = address.origin();
        if Object::is(browser.as_ref(), parent.as_ref()) || parent.location().origin().ok().as_ref() != Some(&origin) {
            return Err(tr!("AniCraft needs a same-origin WebOS account window.").into());
        }
        let mut entropy = [0u8; 16];
        browser
            .crypto()
            .and_then(|crypto| crypto.get_random_values_with_u8_array(&mut entropy))
            .map_err(|_| tr!("The native editor generation is unavailable."))?;
        let generation: String = entropy.iter().map(|byte| format!("{byte:02x}")).collect();
        let inbox = Rc::new(RefCell::new(VecDeque::new()));
        let pending = inbox.clone();
        let source = parent.clone();
        let expected_origin = origin.clone();
        let expected_generation = generation.clone();
        let ctx = ctx.clone();
        let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if event.origin() != expected_origin || !event.source().is_some_and(|window| Object::is(window.as_ref(), source.as_ref())) {
                return;
            }
            let packet = event.data();
            if get(&packet, "generation").as_string().as_deref() != Some(expected_generation.as_str()) {
                return;
            }
            if let Ok(mut queue) = pending.try_borrow_mut()
                && queue.len() < MAX_QUEUED_MESSAGES
                && string(&packet, "type", 64).is_some_and(|kind| kind.starts_with(PREFIX))
            {
                queue.push_back(packet);
                ctx.request_repaint();
            }
        });
        browser
            .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
            .map_err(|_| tr!("The desktop request listener could not start."))?;
        Ok(Some(Self { browser, parent, origin, generation, profile, inbox, listener }))
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn receive(&self) -> Option<JsValue> {
        self.inbox.try_borrow_mut().ok()?.pop_front()
    }

    pub fn send(&self, kind: &str, metadata: Value, bytes: Option<&[u8]>) -> Result<(), String> {
        if bytes.is_some_and(|bytes| bytes.len() > MAX_BYTES) {
            return Err(tr!("The AniCraft project exceeds the account editor's size limit.").into());
        }
        let packet = JSON::parse(&metadata.to_string()).map_err(|_| tr!("Could not encode the desktop message."))?;
        set(&packet, "type", format!("{PREFIX}{kind}"))?;
        set(&packet, "generation", self.generation.as_str())?;
        let transfers = Array::new();
        if let Some(bytes) = bytes {
            let buffer = Uint8Array::from(bytes).buffer();
            set(&packet, "bytes", buffer.clone())?;
            transfers.push(&buffer);
        }
        self.parent.post_message_with_transfer(&packet, &self.origin, &transfers).map_err(|_| tr!("Could not reach the WebOS desktop.").into())
    }
}

impl Drop for AccountChannel {
    fn drop(&mut self) {
        let _ = self.browser.remove_event_listener_with_callback("message", self.listener.as_ref().unchecked_ref());
    }
}

fn set(packet: &JsValue, name: &str, value: impl Into<JsValue>) -> Result<(), String> {
    if Reflect::set(packet, &name.into(), &value.into()).ok() == Some(true) {
        Ok(())
    } else {
        Err(tr!("Could not encode the desktop message field.").into())
    }
}

fn get(packet: &JsValue, name: &str) -> JsValue {
    Reflect::get(packet, &name.into()).unwrap_or(JsValue::UNDEFINED)
}

fn bytes(packet: &JsValue) -> Result<Option<Vec<u8>>, String> {
    let value = get(packet, "bytes");
    if value.is_null() {
        return Ok(None);
    }
    let buffer = value.dyn_into::<ArrayBuffer>().map_err(|_| tr!("Expected a native project buffer."))?;
    if buffer.byte_length() == 0 || buffer.byte_length() as usize > MAX_BYTES {
        return Err(tr!("Invalid AniCraft project size.").into());
    }
    Ok(Some(Uint8Array::new(&buffer).to_vec()))
}

fn string(packet: &JsValue, name: &str, limit: usize) -> Option<String> {
    get(packet, name).as_string().filter(|value| value.len() <= limit)
}

fn integer(packet: &JsValue, name: &str) -> Option<u64> {
    let value = string(packet, name, 20)?;
    let number: u64 = value.parse().ok()?;
    (number.to_string() == value).then_some(number)
}

fn nullable(packet: &JsValue, name: &str, limit: usize) -> Result<Option<String>, String> {
    let value = get(packet, name);
    if value.is_null() {
        return Ok(None);
    }
    value.as_string().filter(|value| value.len() <= limit).map(Some).ok_or_else(|| tr!("Invalid AniCraft {name}.", name = name))
}

fn valid_path(path: &str) -> bool {
    path.len() <= 2048
        && path.get(path.len().saturating_sub(".anicraft".len())..).is_some_and(|extension| extension.eq_ignore_ascii_case(".anicraft"))
        && !path.chars().any(|ch| ch.is_control() || ch == '\\')
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn request(packet: &JsValue) -> Result<Value, String> {
    let id = string(packet, "requestId", 36)
        .filter(|id| id.len() == 36 && id.bytes().all(|ch| ch.is_ascii_hexdigit() || ch == b'-'))
        .ok_or(tr!("Invalid AniCraft request ID."))?;
    Ok(json!({"requestId":id}))
}

fn response(response: &mut Value, result: Result<(), String>) {
    response["ok"] = json!(result.is_ok());
    if let Err(error) = result {
        response["message"] = json!(error);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    document: u64,
    revision: u64,
}

impl Target {
    fn current(app: &App) -> Self {
        Self { document: app.s.document_id(), revision: app.s.revision }
    }

    fn read(packet: &JsValue) -> Result<Self, String> {
        Ok(Self {
            document: integer(packet, "documentId").ok_or(tr!("Invalid AniCraft document ID."))?,
            revision: integer(packet, "revision").ok_or(tr!("Invalid AniCraft revision."))?,
        })
    }

    fn write(self, packet: &mut Value) {
        packet["documentId"] = json!(self.document.to_string());
        packet["revision"] = json!(self.revision.to_string());
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SaveKey {
    ticket: u64,
    target: Target,
    path: String,
}

impl SaveKey {
    fn read(packet: &JsValue) -> Result<Self, String> {
        Ok(Self {
            ticket: integer(packet, "ticket").ok_or(tr!("Invalid AniCraft save ticket."))?,
            target: Target::read(packet)?,
            path: string(packet, "path", 2048).ok_or(tr!("Invalid AniCraft save path."))?,
        })
    }

    fn write(&self, packet: &mut Value) {
        self.target.write(packet);
        packet["ticket"] = json!(self.ticket.to_string());
        packet["path"] = json!(self.path);
    }
}

struct PendingSave {
    key: SaveKey,
    prepared: PreparedSave,
}

struct Dialog {
    id: u64,
    destination_for: Option<Target>,
}

/// A hosted AniCraft window owns exactly one native session; the parent owns account I/O.
pub struct AccountHost {
    channel: AccountChannel,
    ready: bool,
    started: bool,
    dialog: Option<Dialog>,
    next_dialog: u64,
    pending: Option<PendingSave>,
    next_ticket: u64,
    lease: Option<String>,
    finalized: bool,
    last_state: Option<Value>,
}

impl AccountHost {
    pub fn connect(ctx: &egui::Context) -> Result<Option<Self>, String> {
        Ok(AccountChannel::connect(ctx)?.map(|channel| Self {
            channel,
            ready: false,
            started: false,
            dialog: None,
            next_dialog: 0,
            pending: None,
            next_ticket: 0,
            lease: None,
            finalized: false,
            last_state: None,
        }))
    }

    pub fn profile(&self) -> &str {
        self.channel.profile()
    }

    pub fn suspended(&self) -> bool {
        !self.started || self.lease.is_some() || self.finalized
    }

    fn pending(&self) -> bool {
        self.pending.is_some() || self.dialog.is_some()
    }

    fn path<'a>(&self, app: &'a App) -> Option<&'a str> {
        app.s.path.as_deref().and_then(std::path::Path::to_str)
    }

    fn state(&self, app: &App) -> Value {
        if !self.started {
            return json!({"available":false});
        }
        let unfinished = app.unfinished_account_edit();
        let mut state = json!({
            "available":true,
            "dirty":app.s.is_dirty(),
            "capturable":!unfinished,
            "autosaveEligible":!unfinished && self.path(app).is_some(),
            "operationPending":self.pending(),
            "unfinishedEdit":unfinished,
        });
        Target::current(app).write(&mut state);
        state
    }

    pub fn publish(&mut self, app: &mut App) {
        let state = self.state(app);
        if self.last_state.as_ref() != Some(&state) {
            match self.channel.send("project-state", state.clone(), None) {
                Ok(()) => self.last_state = Some(state),
                Err(error) => app.status_err(error),
            }
        }
    }

    pub fn poll(&mut self, app: &mut App) {
        if !self.ready {
            match self.channel.send("ready", json!({}), None) {
                Ok(()) => self.ready = true,
                Err(error) => app.status_err(error),
            }
        }
        while let Some(packet) = self.channel.receive() {
            if let Err(error) = self.receive(app, &packet) {
                app.status_err(error);
            }
        }
    }

    pub fn file_action(&mut self, app: &mut App, command: &str, params: &Value) -> Result<bool, String> {
        let mut dialog = match command {
            "host.close" => {
                self.channel.send("close-intent", json!({}), None)?;
                return Ok(true);
            }
            "file.recovery_list" | "file.recover" | "file.discard_recovery" => {
                app.status_ok(tr!("WebOS offers this account’s browser drafts in the desktop."));
                return Ok(true);
            }
            "file.new" | "file.new_sample" | "file.new_from_template" => {
                let settings = json!({"command":command,"params":params}).to_string();
                json!({"operation":"new","settings":settings})
            }
            "file.open" => json!({"operation":"open","path":null}),
            "file.open_recent" => {
                let entry =
                    params["index"].as_u64().and_then(|index| usize::try_from(index).ok()).and_then(|index| app.s.recent.entries().get(index));
                let path = params["path"]
                    .as_str()
                    .or_else(|| entry.and_then(|entry| entry.path.to_str()))
                    .filter(|path| valid_path(path))
                    .ok_or(tr!("The recent account file is unavailable."))?;
                json!({"operation":"open","path":path})
            }
            "file.save" | "file.save_as" => {
                if self.suspended() || self.pending() || app.unfinished_account_edit() || app.modal_open() {
                    return Err(tr!("Finish the current AniCraft operation before saving.").into());
                }
                if command == "file.save"
                    && let Some(path) = self.path(app).map(str::to_owned)
                {
                    let target = Target::current(app);
                    let (pending, bytes) = self.prepare(app, target, path)?;
                    self.send_save("save", json!({}), pending, &bytes)?;
                    return Ok(true);
                }
                let suggested =
                    app.s.path.as_deref().and_then(std::path::Path::file_name).and_then(std::ffi::OsStr::to_str).unwrap_or("animation.anicraft"); // i18n-exempt: default filename
                json!({"operation":"save","suggested":suggested})
            }
            _ => return Ok(false),
        };
        if self.suspended() || self.dialog.is_some() {
            return Err(tr!("AniCraft is already waiting for the desktop.").into());
        }
        let id = self.next_dialog.checked_add(1).ok_or(tr!("AniCraft dialog request limit reached."))?;
        dialog["requestId"] = json!(id.to_string());
        self.channel.send("dialog", dialog, None)?;
        self.next_dialog = id;
        self.dialog = Some(Dialog { id, destination_for: matches!(command, "file.save" | "file.save_as").then(|| Target::current(app)) });
        Ok(true)
    }

    fn receive(&mut self, app: &mut App, packet: &JsValue) -> Result<(), String> {
        let kind = string(packet, "type", 64).ok_or(tr!("Invalid AniCraft message."))?;
        let Some(kind) = kind.strip_prefix(PREFIX) else { return Ok(()) };
        if kind == "start" {
            return self.start(app, packet);
        }
        if self.suspended() && kind != "session" {
            return Ok(());
        }
        match kind {
            "dialog-result" => self.finish_dialog(app, packet),
            "save-result" => self.finish_save(app, packet),
            "autosave" | "session-save" => self.capture_save(app, packet, kind),
            "recovery-capture" => self.capture_recovery(app, packet),
            "operation" => self.operation(app, packet),
            "session" => self.session(app, packet),
            "close" => {
                let mut result = request(packet)?;
                result["approved"] = json!(!app.s.is_dirty() && !app.unfinished_account_edit() && !self.pending());
                self.channel.send("close-result", result, None)
            }
            _ => Ok(()),
        }
    }

    fn start(&mut self, app: &mut App, packet: &JsValue) -> Result<(), String> {
        let mut result = request(packet)?;
        response(&mut result, self.open_project(app, packet));
        if result["ok"] == true {
            let Value::Object(state) = self.state(app) else { return Err(tr!("Invalid AniCraft project state.").into()) };
            for (key, value) in state {
                result[key] = value;
            }
            result["path"] = json!(self.path(app));
        }
        self.channel.send("started", result, None)
    }

    fn open_project(&mut self, app: &mut App, packet: &JsValue) -> Result<(), String> {
        if self.started || self.finalized {
            return Err(tr!("This AniCraft editor already owns a project.").into());
        }
        let path = nullable(packet, "path", 2048)?;
        let settings = if get(packet, "newSettings").is_undefined() { None } else { nullable(packet, "newSettings", 4096)? };
        let import = nullable(packet, "importName", 255)?;
        let recover = get(packet, "recover").as_bool().ok_or(tr!("Invalid AniCraft recovery request."))?;
        if path.as_deref().is_some_and(|path| !valid_path(path))
            || import.as_deref().is_some_and(|name| !valid_path(name) || name.contains('/'))
            || (import.is_some() && (path.is_some() || recover))
        {
            return Err(tr!("Invalid AniCraft opening path.").into());
        }
        if let Some(bytes) = bytes(packet)? {
            if settings.is_some() || (path.is_none() && !recover && import.is_none()) {
                return Err(tr!("Invalid AniCraft project open.").into());
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| tr!("Invalid AniCraft project encoding."))?;
            let doc = format::from_json(text).map_err(|error| error.to_string())?;
            app.s.replace_document(doc, path.map(PathBuf::from));
            if recover || import.is_some() {
                app.s.mark_unsaved();
            } else if let Some(path) = app.s.path.as_deref() {
                app.s.recent.add(path);
            }
            app.announce("file.open", &json!({}), 0);
        } else {
            if path.is_some() || import.is_some() || recover {
                return Err(tr!("The AniCraft project data is missing.").into());
            }
            let (command, params) = new_project_command(settings.as_deref())?;
            let result = app.s.execute(command, &params).map_err(|error| error.to_string())?;
            app.announce(command, &result, 0);
        }
        self.started = true;
        Ok(())
    }

    fn prepare(&mut self, app: &App, target: Target, path: String) -> Result<(PendingSave, String), String> {
        if self.pending() || app.unfinished_account_edit() || Target::current(app) != target || !valid_path(&path) {
            return Err(tr!("The AniCraft save no longer identifies the reviewed project.").into());
        }
        let ticket = self.next_ticket.checked_add(1).ok_or(tr!("AniCraft save ticket limit reached."))?;
        let (prepared, bytes) = app.s.prepare_save(PathBuf::from(&path)).map_err(|error| error.to_string())?;
        self.next_ticket = ticket;
        Ok((PendingSave { key: SaveKey { ticket, target, path }, prepared }, bytes))
    }

    fn send_save(&mut self, kind: &str, mut result: Value, pending: PendingSave, bytes: &str) -> Result<(), String> {
        pending.key.write(&mut result);
        self.channel.send(kind, result, Some(bytes.as_bytes()))?;
        self.pending = Some(pending);
        Ok(())
    }

    fn finish_dialog(&mut self, app: &mut App, packet: &JsValue) -> Result<(), String> {
        if self.dialog.as_ref().map(|dialog| dialog.id) != integer(packet, "requestId") {
            return Ok(());
        }
        let Some(dialog) = self.dialog.take() else { return Ok(()) };
        let outcome = string(packet, "outcome", 16);
        if outcome.as_deref() == Some("failed") {
            return Err(string(packet, "message", 512).unwrap_or_else(|| tr!("The AniCraft dialog did not complete.").into()));
        }
        if outcome.as_deref() == Some("destination")
            && let Some(target) = dialog.destination_for
        {
            let path = string(packet, "path", 2048).ok_or(tr!("Invalid AniCraft destination."))?;
            let (pending, bytes) = self.prepare(app, target, path)?;
            self.send_save("save", json!({}), pending, &bytes)?;
        }
        Ok(())
    }

    fn finish_save(&mut self, app: &mut App, packet: &JsValue) -> Result<(), String> {
        let key = SaveKey::read(packet)?;
        if self.pending.as_ref().map(|pending| &pending.key) != Some(&key) {
            return Err(tr!("The AniCraft save receipt does not match the pending write.").into());
        }
        let outcome = string(packet, "outcome", 16);
        let adopted = match outcome.as_deref() {
            Some("unknown") => {
                app.status_err(tr!("The account save has not been confirmed."));
                return Ok(());
            }
            Some("written" | "failed") => {
                let Some(pending) = self.pending.take() else { return Ok(()) };
                if outcome.as_deref() == Some("written") {
                    app.s.confirm_save(pending.prepared).map_err(|error| error.to_string())?;
                    app.status_ok(tr!("Saved to account"));
                    true
                } else {
                    if let Some(message) = string(packet, "message", 512) {
                        app.status_err(message);
                    }
                    false
                }
            }
            _ => return Err(tr!("Invalid AniCraft save outcome.").into()),
        };
        let mut result = json!({"adopted":adopted,"projectState":self.state(app)});
        key.write(&mut result);
        self.channel.send("save-settled", result, None)
    }

    fn capture_save(&mut self, app: &App, packet: &JsValue, kind: &str) -> Result<(), String> {
        let mut result = request(packet)?;
        let target = Target::read(packet)?;
        let path = string(packet, "path", 2048).ok_or(tr!("Invalid AniCraft save destination."))?;
        let automatic = kind == "autosave";
        let reply = if automatic { "autosave-result" } else { "session-save-result" };
        result["reviewedRevision"] = json!(target.revision.to_string());
        result["outcome"] = json!("busy");
        let allowed = app.s.is_dirty()
            && !self.pending()
            && !app.unfinished_account_edit()
            && !app.modal_open()
            && Target::current(app) == target
            && (!automatic || self.path(app) == Some(path.as_str()));
        if allowed {
            match self.prepare(app, target, path.clone()) {
                Ok((pending, bytes)) => {
                    result["outcome"] = json!("captured");
                    if bytes.len() <= MAX_BYTES {
                        return self.send_save(reply, result, pending, &bytes);
                    }
                    result["outcome"] = json!("failed");
                    result["message"] = json!("The AniCraft project exceeds the account editor's size limit.");
                }
                Err(error) => {
                    result["outcome"] = json!("failed");
                    result["message"] = json!(error);
                }
            }
        }
        target.write(&mut result);
        result["path"] = json!(path);
        self.channel.send(reply, result, None)
    }

    fn capture_recovery(&self, app: &App, packet: &JsValue) -> Result<(), String> {
        let mut result = request(packet)?;
        let target = Target::read(packet)?;
        target.write(&mut result);
        if Target::current(app) == target && !app.unfinished_account_edit() {
            match format::to_json(&app.s.doc) {
                Ok(bytes) if bytes.len() <= MAX_BYTES => return self.channel.send("recovery-captured", result, Some(bytes.as_bytes())),
                Ok(_) => result["message"] = json!("The AniCraft draft exceeds the recovery size limit."),
                Err(error) => result["message"] = json!(error.to_string()),
            }
        } else {
            result["message"] = json!("The AniCraft project changed before draft capture.");
        }
        self.channel.send("recovery-captured", result, None)
    }

    fn operation(&self, app: &mut App, packet: &JsValue) -> Result<(), String> {
        let mut reply = request(packet)?;
        let id = integer(packet, "documentId").ok_or(tr!("Invalid AniCraft operation target."))?;
        reply["documentId"] = json!(id.to_string());
        let result: Result<Value, String> = (|| {
            let command = string(packet, "command", 64).ok_or(tr!("Invalid AniCraft command."))?;
            let path = nullable(packet, "path", 2048)?;
            if id != app.s.document_id() || path.as_deref() != self.path(app) {
                return Err(tr!("The scoped operation does not identify the live AniCraft project.").into());
            }
            let encoded = string(packet, "args", 4096).ok_or(tr!("Invalid AniCraft operation arguments."))?;
            let args: serde_json::Map<String, Value> = serde_json::from_str(&encoded).map_err(|_| tr!("Invalid AniCraft operation arguments."))?;
            match command.as_str() {
                "document.inspect" if args.is_empty() => {
                    let layers: Vec<Value> = app
                        .s
                        .timeline()
                        .layers
                        .iter()
                        .map(|layer| {
                            json!({
                                "id":layer.id.0.to_string(),"name":layer.name,"visible":layer.visible,"locked":layer.locked,
                            })
                        })
                        .collect();
                    Ok(json!({"layers":layers,"frame":app.s.frame,"frames":app.s.frame_count(),"dirty":app.s.is_dirty()}))
                }
                "layer.rename" if args.len() == 2 && !self.pending() && !app.unfinished_account_edit() && !app.modal_open() => {
                    let layer = args
                        .get("layerId")
                        .and_then(Value::as_str)
                        .and_then(|id| id.parse::<u64>().ok().filter(|number| *number > 0 && number.to_string() == id))
                        .ok_or(tr!("Invalid AniCraft layer ID."))?;
                    let name = args
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|name| {
                            name.len() <= 256 && name.chars().count() <= 128 && !name.trim().is_empty() && !name.chars().any(char::is_control)
                        })
                        .ok_or(tr!("Invalid AniCraft layer name."))?;
                    let command = "layer.rename";
                    let selected = app.s.selection.len();
                    let result = app.s.execute(command, &json!({"layer":layer,"name":name})).map_err(|error| error.to_string())?;
                    app.announce(command, &result, selected);
                    Ok(result)
                }
                _ => Err(tr!("The AniCraft operation is unavailable in this session.").into()),
            }
        })();
        match result {
            Ok(value) => reply["result"] = json!(value.to_string()),
            Err(error) => reply["message"] = json!(error),
        }
        self.channel.send("operation-result", reply, None)
    }

    fn session(&mut self, app: &App, packet: &JsValue) -> Result<(), String> {
        let mut reply = request(packet)?;
        let operation = string(packet, "operation", 16).ok_or(tr!("Invalid AniCraft session operation."))?;
        let lease = string(packet, "lease", 64).filter(|lease| !lease.is_empty()).ok_or(tr!("Invalid AniCraft session lease."))?;
        let result = match operation.as_str() {
            "pause" if !self.suspended() => {
                let target = Target::read(packet)?;
                if target == Target::current(app)
                    && get(packet, "dirty").as_bool() == Some(app.s.is_dirty())
                    && get(packet, "unfinishedEdit").as_bool() == Some(app.unfinished_account_edit())
                    && !self.pending()
                {
                    self.lease = Some(lease.clone());
                    Ok(())
                } else {
                    Err(tr!("The AniCraft project changed after close review.").into())
                }
            }
            "release" | "finalize" if self.lease.as_deref() == Some(lease.as_str()) => {
                self.lease = None;
                self.finalized = operation == "finalize";
                Ok(())
            }
            _ => Err(tr!("The AniCraft session lease is unavailable.").into()),
        };
        response(&mut reply, result);
        reply["operation"] = json!(operation);
        reply["lease"] = json!(lease);
        self.channel.send("session-result", reply, None)
    }
}

fn new_project_command(settings: Option<&str>) -> Result<(&'static str, Value), String> {
    let Some(settings) = settings else { return Ok(("file.new", json!({}))) };
    let data: Value = serde_json::from_str(settings).map_err(|_| tr!("Invalid AniCraft project settings."))?;
    let params = data["params"].as_object().ok_or(tr!("Invalid AniCraft project parameters."))?;
    let command = match data["command"].as_str() {
        Some("file.new") if params.keys().all(|key| matches!(key.as_str(), "width" | "height" | "fps")) => "file.new",
        Some("file.new_sample") if params.keys().all(|key| key == "which") => "file.new_sample",
        Some("file.new_from_template") if params.len() == 1 && params.contains_key("template") => "file.new_from_template",
        _ => return Err(tr!("Unsupported AniCraft project settings.").into()),
    };
    Ok((command, Value::Object(params.clone())))
}
