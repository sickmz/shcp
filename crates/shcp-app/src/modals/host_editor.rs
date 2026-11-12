//! Create / edit an app host (SSH config hosts are read-only and never reach this modal). The
//! address comes first and is often all it takes: the rest has a default and appears when asked for.
use super::key_setup::{expand, ssh_folder};
use super::{ModalLayer, button_row, editor_frame, title};
use crate::modals::confirm::ConfirmModal;
use crate::modals::host_appearance::AppearancePicker;
use crate::state::AppState;
use crate::theme::{ActiveTheme as _, Theme, mono_font, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, MenuAnchor, MenuEntry, OpenMenu, handler};
use crate::widgets::forward_fields::ForwardFields;
use crate::widgets::host_color::color_dot;
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use crate::widgets::tooltip::tooltip;
use anyhow::{Context as _, Result, bail};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    PathPromptOptions, SharedString, Subscription, Task, Window, div, px, relative,
    transparent_black,
};
use shcp_core::{
    CheckOutcome, CheckQuestion, CheckUpdate, ConnectStep, ConnectionCheck, ForwardSpec,
    HostAppearance, HostConfig, HostId, HostSource, NoticeLevel, SshCommand, address_text,
    find_ssh_keys, is_ssh_command, suggested_host_name,
};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Opens a host that was just saved.
pub type ConnectHandler = Rc<dyn Fn(HostId, &mut Window, &mut App)>;

const NAME_PLACEHOLDER: &str = "filled in from the address";
const AUTOMATIC_KEY: &str =
    "Tries your SSH agent, then the usual keys in ~/.ssh. Asks for a password if none works.";
/// Keys listed next to Automatic. A folder with more can still use Browse.
const MAX_KEYS: usize = 5;

/// What a connection check got to, and how the rows of the check show it.
struct Check {
    /// Stops the check when dropped, as the modal closing does.
    connection: ConnectionCheck,
    /// Reaching the server, verifying it, signing in.
    stage: usize,
    details: [Option<String>; 3],
    new_server: Option<NewServer>,
    outcome: Option<CheckOutcome>,
    _updates: Task<()>,
}

struct NewServer {
    host: String,
    key_type: String,
    fingerprint: String,
}

#[derive(Clone, Copy, PartialEq)]
enum RowState {
    Pending,
    Working,
    Done,
    Asks,
    Failed,
}

pub struct HostEditor {
    embedded: bool,
    state: Entity<AppState>,
    modals: Entity<ModalLayer>,
    /// The host being edited; `None` when creating.
    original: Option<HostConfig>,
    /// What it starts from when duplicating, for the settings without a field here.
    template: Option<HostConfig>,
    address: Entity<TextInput>,
    /// How long the address was, to tell a paste from someone typing.
    address_len: usize,
    /// What a pasted command said that a host has no place for.
    ignored: Vec<String>,
    alias: Entity<TextInput>,
    group: Option<String>,
    group_input: Entity<TextInput>,
    typing_group: bool,
    group_anchor: MenuAnchor,
    menu: Option<OpenMenu>,
    /// `None` leaves it to the agent and the usual keys.
    identity: Option<PathBuf>,
    found_keys: Vec<PathBuf>,
    jump: Entity<TextInput>,
    show_jump: bool,
    proxy: Entity<TextInput>,
    show_proxy: bool,
    keepalive: Entity<TextInput>,
    keepalive_count: Entity<TextInput>,
    show_keepalive: bool,
    forward_agent: bool,
    forwards: Vec<Entity<ForwardFields>>,
    appearance: Entity<AppearancePicker>,
    show_appearance: bool,
    check: Option<Check>,
    error: Option<String>,
    on_connect: Option<ConnectHandler>,
    first_focus: FocusHandle,
    _keys: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for HostEditor {}
impl EventEmitter<super::CancelEvent> for HostEditor {}

impl Focusable for HostEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.first_focus.clone()
    }
}

impl HostEditor {
    pub fn embedded(mut self) -> Self {
        self.embedded = true;
        self
    }

    /// A new app host starting as a copy of another, which `~/.ssh/config` hosts can be too.
    pub fn duplicate(
        state: Entity<AppState>,
        modals: Entity<ModalLayer>,
        source: HostConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut copy = source;
        copy.alias = format!("{} copy", copy.alias);
        let mut this = Self::new(state, modals, Some(copy.clone()), window, cx);
        this.original = None;
        this.template = Some(copy);
        this
    }

    pub fn new(
        state: Entity<AppState>,
        modals: Entity<ModalLayer>,
        original: Option<HostConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = original.clone().unwrap_or_default();
        let field = |placeholder: &str, value: &str, cx: &mut Context<Self>| {
            let value = value.to_string();
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                let mut input = TextInput::new(&placeholder, false, cx);
                input.set_text(&value, cx);
                input
            })
        };
        let start = original.as_ref().map_or_else(String::new, |host| {
            address_text(Some(&host.user), &host.hostname, Some(host.port))
        });
        let address_len = start.len();
        let address = cx.new(|cx| {
            let mut input = TextInput::new("user@server:port, or paste a whole command", false, cx)
                .command("ssh");
            input.set_text(&start, cx);
            input
        });
        let alias = field(NAME_PLACEHOLDER, &host.alias, cx);
        let group_input = field("Name of the new group", "", cx);
        let proxy = field(
            "aws ssm start-session --target %h ...",
            host.proxy_command.as_deref().unwrap_or(""),
            cx,
        );
        let jump = field(
            "admin@bastion:2222, or another host's name",
            host.proxy_jump.as_deref().unwrap_or(""),
            cx,
        );
        let keepalive = field(
            "30",
            &host
                .server_alive_interval
                .map(|v| v.to_string())
                .unwrap_or_default(),
            cx,
        );
        let keepalive_count = field("3", &host.server_alive_count_max.to_string(), cx);
        let forwards: Vec<_> = host
            .local_forwards
            .iter()
            .map(|spec| cx.new(|cx| ForwardFields::new(Some(spec), cx)))
            .collect();
        let saved = original
            .as_ref()
            .map(|host| state.read(cx).host_appearance(&host.id))
            .unwrap_or_default();
        let appearance = cx.new(|_| AppearancePicker::new(&saved));
        let group = original
            .as_ref()
            .and_then(|host| state.read(cx).host_group(&host.id))
            .map(str::to_string);
        let default_count = HostConfig::default().server_alive_count_max;
        let mut subscriptions = vec![cx.observe(&appearance, |_, _, cx| cx.notify())];
        for input in [
            &address,
            &alias,
            &group_input,
            &proxy,
            &jump,
            &keepalive,
            &keepalive_count,
        ] {
            subscriptions.push(cx.subscribe_in(input, window, Self::on_input_event));
        }
        for fields in &forwards {
            let inputs = fields.read(cx).inputs().map(Entity::clone);
            for input in &inputs {
                subscriptions.push(cx.subscribe_in(input, window, Self::on_input_event));
            }
        }
        let first_focus = address.focus_handle(cx);
        // A command typed by hand is taken apart on leaving the field: not before, or the caret
        // would jump around while it is still being written.
        subscriptions.push(
            cx.on_focus_out(&first_focus, window, |this, _, window, cx| {
                this.absorb_command(window, cx);
            }),
        );
        let folder = ssh_folder(&state, cx);
        let keys = cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move { find_ssh_keys(&folder) })
                .await;
            this.update(cx, |this, cx| {
                this.found_keys = found;
                cx.notify();
            })
            .ok();
        });
        let show_appearance = saved != HostAppearance::default() && original.is_some();
        Self {
            embedded: false,
            state,
            modals,
            template: None,
            address,
            address_len,
            ignored: Vec::new(),
            alias,
            group,
            group_input,
            typing_group: false,
            group_anchor: MenuAnchor::default(),
            menu: None,
            identity: host.identity_files.first().cloned(),
            found_keys: Vec::new(),
            show_jump: host.proxy_jump.is_some(),
            jump,
            show_proxy: host.proxy_command.is_some(),
            proxy,
            show_keepalive: host.server_alive_interval.is_some()
                || host.server_alive_count_max != default_count,
            keepalive,
            keepalive_count,
            forward_agent: host.forward_agent,
            forwards,
            appearance,
            show_appearance,
            check: None,
            error: None,
            on_connect: None,
            original,
            first_focus,
            _keys: keys,
            _subscriptions: subscriptions,
        }
    }

    /// Offers to connect once saved. Without it the dialog only saves.
    pub fn on_connect(mut self, handler: ConnectHandler) -> Self {
        self.on_connect = Some(handler);
        self
    }

    fn connects(&self) -> bool {
        self.on_connect.is_some() && self.original.is_none()
    }

    fn on_input_event(
        &mut self,
        input: &Entity<TextInput>,
        event: &TextInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TextInputEvent::Submit => self.save(self.connects(), window, cx),
            TextInputEvent::Cancel => cx.emit(super::CancelEvent),
            TextInputEvent::Changed => {
                if input == &self.address {
                    self.on_address_changed(window, cx);
                }
                // What a check found says nothing about other settings.
                if input == &self.address || input == &self.jump || input == &self.proxy {
                    self.check = None;
                }
                self.error = None;
                self.update_name(cx);
                cx.notify();
            }
        }
    }

    fn on_address_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ignored.clear();
        let len = self.address.read(cx).text().len();
        let pasted = len > self.address_len + 1;
        self.address_len = len;
        if pasted {
            self.absorb_command(window, cx);
        }
    }

    /// A whole `ssh` command gives each setting to its own field, leaving the address alone.
    fn absorb_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.address.read(cx).text().to_string();
        if !is_ssh_command(&text) {
            return;
        }
        let Ok(command) = SshCommand::parse(&text) else {
            return;
        };
        if command.host.is_empty() {
            return;
        }
        let set = |input: &Entity<TextInput>, value: &str, cx: &mut Context<Self>| {
            input.update(cx, |input, cx| input.set_text(value, cx));
        };
        let address = command.address();
        self.address_len = address.len();
        set(&self.address, &address, cx);
        if let Some(identity) = &command.identity {
            self.identity = Some(expand(identity, &ssh_folder(&self.state, cx)));
        }
        if let Some(jump) = &command.jump {
            set(&self.jump, jump, cx);
            self.show_jump = true;
        }
        if let Some(proxy) = &command.proxy_command {
            set(&self.proxy, proxy, cx);
            self.show_proxy = true;
        }
        if let Some(keepalive) = command.keepalive {
            set(&self.keepalive, &keepalive.to_string(), cx);
            self.show_keepalive = true;
        }
        if let Some(count) = command.alive_count {
            set(&self.keepalive_count, &count.to_string(), cx);
            self.show_keepalive = true;
        }
        self.forward_agent |= command.forward_agent;
        for spec in &command.forwards {
            self.push_forward(Some(spec), window, cx);
        }
        self.ignored = command.ignored;
        self.check = None;
        self.update_name(cx);
        cx.notify();
    }

    /// What the name is when nobody typed one, shown as its placeholder.
    fn suggested_name(&self, cx: &App) -> Option<String> {
        SshCommand::parse(self.address.read(cx).text())
            .ok()
            .filter(|command| !command.host.is_empty())
            .map(|command| suggested_host_name(&command.host))
    }

    /// The name the host would get now.
    fn name(&self, cx: &App) -> String {
        let typed = self.alias.read(cx).text().trim();
        if typed.is_empty() {
            self.suggested_name(cx).unwrap_or_default()
        } else {
            typed.to_string()
        }
    }

    fn update_name(&mut self, cx: &mut Context<Self>) {
        let placeholder = self
            .suggested_name(cx)
            .unwrap_or_else(|| NAME_PLACEHOLDER.to_string());
        self.alias
            .update(cx, |input, cx| input.set_placeholder(&placeholder, cx));
    }

    /// An app host goes before a `~/.ssh/config` host of the same name, which comes back once the
    /// app host is gone: allowed, but nobody should do it by accident.
    fn replaces_config_host(&self, cx: &App) -> Option<String> {
        let name = self.name(cx);
        takes_place_of_config_host(&self.state.read(cx).hosts, &name)
            .then(|| format!("Replaces {name} from ~/.ssh/config, for as long as this host exists"))
    }

    fn name_error(&self, cx: &App) -> Option<&'static str> {
        let name = self.name(cx);
        if name.contains(['*', '?']) {
            return Some("A name cannot have * or ? in it");
        }
        self.state
            .read(cx)
            .hosts
            .iter()
            .any(|h| {
                h.source == HostSource::App
                    && h.alias == name
                    && self
                        .original
                        .as_ref()
                        .is_none_or(|original| original.id != h.id)
            })
            .then_some("An app host with this name already exists")
    }

    fn push_forward(
        &mut self,
        spec: Option<&ForwardSpec>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ForwardFields> {
        let fields = cx.new(|cx| ForwardFields::new(spec, cx));
        let inputs = fields.read(cx).inputs().map(Entity::clone);
        for input in &inputs {
            self._subscriptions
                .push(cx.subscribe_in(input, window, Self::on_input_event));
        }
        self.forwards.push(fields.clone());
        fields
    }

    fn add_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fields = self.push_forward(None, window, cx);
        fields.read(cx).focus(window, cx);
        cx.notify();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select identity file".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.first()
            {
                let path = path.clone();
                this.update(cx, |this, cx| this.pick_key(Some(path), cx))
                    .ok();
            }
        })
        .detach();
    }

    fn pick_key(&mut self, key: Option<PathBuf>, cx: &mut Context<Self>) {
        self.identity = key;
        self.check = None;
        cx.notify();
    }

    fn base_host(&self) -> HostConfig {
        self.original
            .clone()
            .or_else(|| self.template.clone())
            .unwrap_or_default()
    }

    fn build(&self, cx: &App) -> Result<HostConfig> {
        let mut host = self.base_host();
        self.apply_connection(&mut host, cx)?;
        self.apply_tunnels(&mut host, cx)?;
        host.source = HostSource::App;
        Ok(host)
    }

    /// Everything but the tunnels, which a connection check has no use for.
    fn apply_connection(&self, host: &mut HostConfig, cx: &App) -> Result<()> {
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let target = SshCommand::parse(&text(&self.address)).map_err(anyhow::Error::msg)?;
        if target.host.is_empty() {
            bail!("Enter the server's address");
        }
        host.hostname = target.host;
        host.port = target.port.unwrap_or(22);
        host.user = target.user.unwrap_or_else(|| HostConfig::default().user);
        if host.user.is_empty() {
            bail!("Say who to log in as: user@server");
        }
        host.alias = self.name(cx);
        if let Some(error) = self.name_error(cx) {
            bail!(error);
        }
        let rest: Vec<PathBuf> = host.identity_files.iter().skip(1).cloned().collect();
        host.identity_files = self.identity.iter().cloned().chain(rest).collect();
        let proxy = text(&self.proxy);
        host.proxy_command = (!proxy.is_empty()).then_some(proxy);
        let jump = text(&self.jump);
        host.proxy_jump = (!jump.is_empty()).then_some(jump);
        host.forward_agent = self.forward_agent;
        let keepalive = text(&self.keepalive);
        host.server_alive_interval = if keepalive.is_empty() {
            None
        } else {
            Some(
                keepalive
                    .parse()
                    .context("Keepalive must be a number of seconds")?,
            )
        };
        host.server_alive_count_max = text(&self.keepalive_count)
            .parse()
            .context("Missed replies must be a nonnegative number")?;
        Ok(())
    }

    fn apply_tunnels(&self, host: &mut HostConfig, cx: &App) -> Result<()> {
        host.local_forwards = self
            .forwards
            .iter()
            .map(|fields| fields.read(cx))
            .filter(|fields| !fields.is_blank(cx))
            .map(|fields| fields.spec(cx))
            .collect::<Result<_>>()?;
        Ok(())
    }

    fn start_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.absorb_command(window, cx);
        self.error = None;
        let started = (|| {
            let mut host = self.base_host();
            self.apply_connection(&mut host, cx)?;
            self.state.read(cx).engine.check_host(host)
        })();
        let connection = match started {
            Ok(connection) => connection,
            Err(error) => {
                self.check = None;
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let updates = connection.updates();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                let alive = this.update(cx, |this, cx| this.on_check_update(update, cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        self.check = Some(Check {
            connection,
            stage: 0,
            details: [None, None, None],
            new_server: None,
            outcome: None,
            _updates: task,
        });
        cx.notify();
    }

    fn on_check_update(&mut self, update: CheckUpdate, cx: &mut Context<Self>) {
        let Some(check) = &mut self.check else {
            return;
        };
        match update {
            CheckUpdate::Step { step, hop, detail } => {
                let stage = match (&hop, step) {
                    // The jump hosts are all part of getting there.
                    (Some(_), _) => 0,
                    (None, ConnectStep::Resolving | ConnectStep::Connecting)
                    | (None, ConnectStep::StartingProxy) => 0,
                    (None, ConnectStep::Handshake | ConnectStep::VerifyingHostKey) => 1,
                    (None, ConnectStep::Authenticating | ConnectStep::OpeningForwards) => 2,
                };
                check.stage = check.stage.max(stage);
                check.new_server = None;
                // A later step with nothing to say leaves what the row showed.
                if let Some(detail) = hop.map(|hop| format!("through {hop}")).or(detail) {
                    check.details[stage] = Some(detail);
                }
            }
            CheckUpdate::NewServer {
                host,
                key_type,
                fingerprint_sha256,
            } => {
                check.new_server = Some(NewServer {
                    host,
                    key_type,
                    fingerprint: fingerprint_sha256,
                });
            }
            CheckUpdate::Finished(outcome) => {
                check.new_server = None;
                check.outcome = Some(outcome);
            }
        }
        cx.notify();
    }

    fn trust_server(&mut self, cx: &mut Context<Self>) {
        if let Some(check) = &mut self.check {
            check.new_server = None;
            check.connection.answer_host_key(true);
        }
        cx.notify();
    }

    fn stop_check(&mut self, cx: &mut Context<Self>) {
        self.check = None;
        cx.notify();
    }

    fn save(&mut self, connect: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.absorb_command(window, cx);
        let result = self.build(cx).and_then(|host| {
            let renamed = self
                .original
                .as_ref()
                .map(|o| o.id.clone())
                .filter(|old| old.0 != host.alias);
            let engine = &self.state.read(cx).engine;
            engine.save_host(host.clone())?;
            // Renamed: drop the entry saved under the old alias.
            if let Some(old) = &renamed {
                engine.delete_host(old)?;
            }
            Ok((renamed, HostId(host.alias)))
        });
        match result {
            Ok((renamed, id)) => {
                let appearance = self.appearance.read(cx).appearance();
                let group = self.chosen_group(cx);
                self.state.update(cx, |state, cx| {
                    if let Some(old) = &renamed {
                        state.host_renamed(old, &id, cx);
                    }
                    state.set_host_appearance(&id, appearance, cx);
                    if state.host_group(&id) != group.as_deref() {
                        state.set_host_group(&id, group, cx);
                    }
                    // A connection right away must find the host in the list.
                    state.hosts = state.engine.hosts();
                });
                cx.emit(DismissEvent);
                if connect && let Some(handler) = self.on_connect.clone() {
                    // After the modal gave the focus back, so that the terminal can take it.
                    window.defer(cx, move |window, cx| handler(id, window, cx));
                }
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn chosen_group(&self, cx: &App) -> Option<String> {
        if self.typing_group {
            let typed = self.group_input.read(cx).text().trim();
            (!typed.is_empty()).then(|| typed.to_string())
        } else {
            self.group.clone()
        }
    }

    fn open_group_menu(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let pick = |group: Option<String>| {
            handler(this.clone(), move |view: &mut Self, _, cx| {
                view.group = group.clone();
                cx.notify();
            })
        };
        let mut entries = vec![MenuEntry::item("Ungrouped", pick(None))];
        let mut groups = self.state.read(cx).groups();
        // One typed before, which no host is in yet.
        if let Some(group) = &self.group
            && !groups.contains(group)
        {
            groups.push(group.clone());
        }
        for group in groups {
            entries.push(MenuEntry::item(group.clone(), pick(Some(group))).icon(IconName::Folder));
        }
        entries.push(MenuEntry::Separator);
        entries.push(
            MenuEntry::item(
                "New group",
                handler(this.clone(), |view: &mut Self, window, cx| {
                    view.typing_group = true;
                    view.group_input.read(cx).focus(window);
                    cx.notify();
                }),
            )
            .icon(IconName::FolderPlus),
        );
        self.menu = Some(OpenMenu::below(&self.group_anchor, entries));
        cx.notify();
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.original.as_ref().map(|o| o.id.clone()) else {
            return;
        };
        let state = self.state.clone();
        let name = id.0.clone();
        let editor = cx.weak_entity();
        let embedded = self.embedded;
        let confirm = cx.new(|cx| {
            ConfirmModal::new(
                "Delete host",
                format!("Delete \"{name}\"? This only removes it from shcp."),
                "Delete",
                move |_, cx| {
                    let deleted = state.update(cx, |state, cx| {
                        if let Err(error) = state.engine.delete_host(&id) {
                            state.push_notice(
                                NoticeLevel::Error,
                                format!("Could not delete host: {error:#}"),
                                cx,
                            );
                            return false;
                        }
                        state.hosts = state.engine.hosts();
                        cx.notify();
                        true
                    });
                    if embedded && deleted {
                        editor.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
                    }
                },
                cx,
            )
        });
        self.modals.update(cx, |layer, cx| {
            if embedded {
                layer.show_front(confirm, None, window, cx)
            } else {
                layer.show(confirm, window, cx)
            }
        });
        if !embedded {
            cx.emit(DismissEvent);
        }
    }
}

fn takes_place_of_config_host(hosts: &[HostConfig], name: &str) -> bool {
    hosts
        .iter()
        .any(|host| host.source == HostSource::SshConfig && host.alias == name)
}

/// A label above its control.
fn field(theme: &Theme, label: &'static str, control: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .min_w_0()
        .gap(px(3.))
        .child(small(theme, label))
        .child(control)
}

fn small(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(ui_px(12.))
        .text_color(theme.text_muted)
        .child(text.into())
}

/// A setting that was asked for: its label, its control and the way to take it back.
fn extra_row(
    theme: &Theme,
    label: &'static str,
    control: impl IntoElement,
    remove: Button,
) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .child(div().w(px(96.)).flex_none().child(small(theme, label)))
        .child(div().flex_1().min_w_0().child(control))
        .child(remove)
}

fn remove_button(id: &'static str, label: &'static str) -> Button {
    Button::new(id)
        .keyboard_focus()
        .icon(IconName::Close)
        .tooltip(label)
        .style(ButtonStyle::Ghost)
}

fn key_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// One step of a connection check.
fn check_row(theme: &Theme, state: RowState, label: String, detail: Option<String>) -> gpui::Div {
    let icon = match state {
        RowState::Pending => div()
            .size(px(8.))
            .mx(px(3.))
            .rounded(px(4.))
            .border_1()
            .border_color(theme.text_disabled)
            .into_any_element(),
        RowState::Working => Icon::new(IconName::Spinner)
            .color(theme.text_muted)
            .spin()
            .into_any_element(),
        RowState::Done => Icon::new(IconName::Check)
            .color(theme.success)
            .into_any_element(),
        RowState::Asks => Icon::new(IconName::Info)
            .color(theme.text_muted)
            .into_any_element(),
        RowState::Failed => Icon::new(IconName::Close)
            .color(theme.error)
            .into_any_element(),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .w(px(14.))
                .flex()
                .flex_none()
                .justify_center()
                .child(icon),
        )
        .child(
            div()
                .flex_none()
                .text_color(if state == RowState::Pending {
                    theme.text_disabled
                } else {
                    theme.text
                })
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_right()
                .font_family(mono_font())
                .text_size(ui_px(11.))
                .text_color(theme.text_muted)
                .children(detail),
        )
}

/// What a check that got as far as a secret means for whoever is about to connect.
fn question_text(question: &CheckQuestion) -> String {
    match question {
        CheckQuestion::Passphrase(key) => format!(
            "{} is protected by a passphrase. shcp asks for it when you connect.",
            key_label(key)
        ),
        CheckQuestion::Password(login) => format!(
            "The server wants a password for {login}. shcp asks for it when you connect, and can \
             set up a key from there."
        ),
        CheckQuestion::Answer(prompt) => {
            format!("The server asks \"{}\" when you connect.", prompt.trim())
        }
    }
}

impl HostEditor {
    /// The color of the host, which opens what picks it.
    fn appearance_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (hover, ring) = (theme.hover, theme.text);
        let color = theme.host_color(self.appearance.read(cx).color());
        div()
            .id("host-appearance")
            .size(px(26.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .border_1()
            .border_color(transparent_black())
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .focusable()
            .tab_stop(true)
            .focus(move |style| style.border_color(ring))
            .tooltip(tooltip("Color"))
            .on_key_down(|event, _, cx| {
                // Enter/Space belong to the focused button, whose click fires on key-up.
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                }
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_appearance = !this.show_appearance;
                cx.notify();
            }))
            .child(color_dot(color, true).size(px(10.)))
    }

    /// Reads back what the address will do, which is where a typo shows.
    fn read_back(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let parsed = SshCommand::parse(self.address.read(cx).text());
        let command = match parsed {
            Ok(command) => command,
            Err(error) => {
                return div()
                    .flex_none()
                    .text_size(ui_px(12.))
                    .text_color(theme.error)
                    .child(error)
                    .into_any_element();
            }
        };
        if command.host.is_empty() {
            return small(
                theme,
                "The address is all you need. Everything else has a default.",
            )
            .flex_none()
            .into_any_element();
        }
        let user = command.user.clone().filter(|user| !user.is_empty());
        let user = user.unwrap_or_else(|| HostConfig::default().user);
        let jump = self.jump.read(cx).text().trim().to_string();
        let jump = (!jump.is_empty()).then_some(jump).or(command.jump.clone());
        let plain = |text: String| div().text_color(theme.text_muted).child(text);
        let strong = |text: String| div().text_color(theme.text).child(text);
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .gap(px(4.))
            .text_size(ui_px(12.))
            .child(plain("Connects to".into()))
            .child(strong(command.host.clone()))
            .child(plain("as".into()))
            .child(strong(user))
            .child(plain("on port".into()))
            .child(strong(command.port.unwrap_or(22).to_string()))
            .when_some(jump, |this, jump| {
                this.child(plain("through".into())).child(strong(jump))
            })
            .into_any_element()
    }

    fn sign_in(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let mut keys: Vec<PathBuf> = self.found_keys.iter().take(MAX_KEYS).cloned().collect();
        if let Some(identity) = &self.identity
            && !keys.contains(identity)
        {
            keys.push(identity.clone());
        }
        let selected = self.identity.clone();
        let pills = keys.into_iter().enumerate().map(|(index, key)| {
            let active = selected.as_ref() == Some(&key);
            Button::new(("host-key", index))
                .keyboard_focus()
                .label(key_label(&key))
                .tooltip(SharedString::from(key.display().to_string()))
                .style(ButtonStyle::Ghost)
                .compact()
                .active(active)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pick_key(Some(key.clone()), cx);
                }))
        });
        let row = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .gap(px(4.))
            .child(
                Button::new("host-key-automatic")
                    .keyboard_focus()
                    .label("Automatic")
                    .style(ButtonStyle::Ghost)
                    .compact()
                    .active(selected.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.pick_key(None, cx))),
            )
            .children(pills)
            .child(
                Button::new("host-key-browse")
                    .keyboard_focus()
                    .label("Browse…")
                    .style(ButtonStyle::Ghost)
                    .compact()
                    .on_click(cx.listener(|this, _, _, cx| this.browse(cx))),
            );
        let hint = match &selected {
            None => small(&theme, AUTOMATIC_KEY),
            Some(key) if key.exists() => small(&theme, key.display().to_string()),
            Some(key) => {
                small(&theme, format!("{} does not exist", key.display())).text_color(theme.error)
            }
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(4.))
            .child(small(&theme, "Sign in"))
            .child(row)
            .child(hint.flex_none())
    }

    /// The settings that were asked for, or that this host has.
    fn extra_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let mut rows = Vec::new();
        if self.show_jump {
            rows.push(
                extra_row(
                    &theme,
                    "Jump host",
                    self.jump.clone(),
                    remove_button("rm-jump", "Connect directly").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.jump.update(cx, |input, cx| input.set_text("", cx));
                            this.show_jump = false;
                            this.check = None;
                            cx.notify();
                        },
                    )),
                )
                .into_any_element(),
            );
        }
        for (index, fields) in self.forwards.iter().enumerate() {
            // Without a label: a tunnel needs the room, and says what it is with its kind.
            rows.push(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .child(fields.clone())
                    .child(
                        Button::new(("rm-forward", index))
                            .keyboard_focus()
                            .icon(IconName::Close)
                            .tooltip("Remove this tunnel")
                            .style(ButtonStyle::Ghost)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if index < this.forwards.len() {
                                    this.forwards.remove(index);
                                }
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        if self.show_proxy {
            rows.push(
                extra_row(
                    &theme,
                    "Proxy command",
                    self.proxy.clone(),
                    remove_button("rm-proxy", "No proxy command").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.proxy.update(cx, |input, cx| input.set_text("", cx));
                            this.show_proxy = false;
                            this.check = None;
                            cx.notify();
                        },
                    )),
                )
                .into_any_element(),
            );
        }
        if self.show_keepalive {
            rows.push(
                extra_row(
                    &theme,
                    "Keepalive",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().w(px(64.)).flex_none().child(self.keepalive.clone()))
                        .child(small(&theme, "seconds, give up after").flex_none())
                        .child(
                            div()
                                .w(px(48.))
                                .flex_none()
                                .child(self.keepalive_count.clone()),
                        )
                        .child(small(&theme, "missed replies").flex_none()),
                    remove_button("rm-keepalive", "Use the default").on_click(cx.listener(
                        |this, _, _, cx| {
                            let count = HostConfig::default().server_alive_count_max.to_string();
                            this.keepalive
                                .update(cx, |input, cx| input.set_text("", cx));
                            this.keepalive_count
                                .update(cx, |input, cx| input.set_text(&count, cx));
                            this.show_keepalive = false;
                            cx.notify();
                        },
                    )),
                )
                .into_any_element(),
            );
        }
        if self.forward_agent {
            rows.push(
                extra_row(
                    &theme,
                    "SSH agent",
                    div()
                        .flex()
                        .flex_col()
                        .child("Let this server use my keys")
                        .child(
                            small(
                                &theme,
                                "While connected, whoever is root on the server can log in \
                                 elsewhere as you.",
                            )
                            .text_color(theme.warning),
                        ),
                    remove_button("rm-agent", "Turn off agent forwarding").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.forward_agent = false;
                            cx.notify();
                        },
                    )),
                )
                .into_any_element(),
            );
        }
        if self.show_appearance {
            rows.push(
                div()
                    .flex()
                    .flex_none()
                    .items_start()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(self.appearance.clone()))
                    .child(
                        remove_button("rm-appearance", "Close").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.show_appearance = false;
                                cx.notify();
                            },
                        )),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    /// What can still be added, each with what opening it does.
    fn add_buttons(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let add = |id: &'static str, label: &'static str| {
            Button::new(id)
                .keyboard_focus()
                .icon(IconName::Plus)
                .label(label)
                .style(ButtonStyle::Ghost)
                .compact()
        };
        let mut buttons = Vec::new();
        if !self.show_jump {
            buttons.push(
                add("add-jump", "Jump host")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_jump = true;
                        this.jump.read(cx).focus(window);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        buttons.push(
            add("add-forward", "Tunnel")
                .on_click(cx.listener(|this, _, window, cx| this.add_forward(window, cx)))
                .into_any_element(),
        );
        if !self.show_proxy {
            buttons.push(
                add("add-proxy", "Proxy command")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_proxy = true;
                        this.proxy.read(cx).focus(window);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        if !self.show_keepalive {
            buttons.push(
                add("add-keepalive", "Keepalive")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_keepalive = true;
                        this.keepalive.read(cx).focus(window);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        if !self.forward_agent {
            buttons.push(
                add("add-agent", "Agent forwarding")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.forward_agent = true;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        if !self.show_appearance {
            buttons.push(
                add("add-appearance", "Appearance")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_appearance = true;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        buttons
    }

    fn group_field(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.typing_group {
            return self.group_input.clone().into_any_element();
        }
        let theme = cx.theme();
        let (hover, ring) = (theme.hover, theme.text);
        let label = self
            .group
            .clone()
            .unwrap_or_else(|| "Ungrouped".to_string());
        self.group_anchor
            .wrap(
                div()
                    .id("host-group")
                    .h(px(28.))
                    .w_full()
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.input_bg)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .focusable()
                    .tab_stop(true)
                    .focus(move |style| style.border_color(ring))
                    .on_key_down(|event, _, cx| {
                        // Enter/Space belong to the focused field, whose click fires on key-up.
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            cx.stop_propagation();
                        }
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.open_group_menu(cx)))
                    .child(div().min_w_0().truncate().child(label))
                    .child(Icon::new(IconName::ChevronDown).color(theme.text_muted)),
            )
            .w_full()
            .into_any_element()
    }

    fn check_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let check = self.check.as_ref()?;
        let theme = cx.theme().clone();
        let state_of = |index: usize| match &check.outcome {
            Some(CheckOutcome::SignedIn) => RowState::Done,
            Some(CheckOutcome::WillAsk(_)) if index < 2 => RowState::Done,
            Some(CheckOutcome::WillAsk(_)) => RowState::Asks,
            Some(CheckOutcome::Failed(_)) if index < check.stage => RowState::Done,
            Some(CheckOutcome::Failed(_)) if index == check.stage => RowState::Failed,
            Some(CheckOutcome::Failed(_)) => RowState::Pending,
            None if index < check.stage => RowState::Done,
            None if index == check.stage && check.new_server.is_none() => RowState::Working,
            None if index == check.stage => RowState::Asks,
            None => RowState::Pending,
        };
        let user = SshCommand::parse(self.address.read(cx).text())
            .ok()
            .and_then(|command| command.user)
            .unwrap_or_else(|| HostConfig::default().user);
        let labels = [
            "Reach the server".to_string(),
            "Verify the server".to_string(),
            format!("Sign in as {user}"),
        ];
        let rows = labels.into_iter().enumerate().map(|(index, label)| {
            check_row(&theme, state_of(index), label, check.details[index].clone())
        });
        let note = |text: String| {
            div()
                .flex_none()
                .pl(px(22.))
                .text_size(ui_px(12.))
                .text_color(theme.text_muted)
                .child(text)
        };
        let message = match &check.outcome {
            Some(CheckOutcome::SignedIn) => {
                Some(note("All good. This host is ready.".to_string()).text_color(theme.success))
            }
            Some(CheckOutcome::WillAsk(question)) => Some(note(question_text(question))),
            Some(CheckOutcome::Failed(error)) => {
                let text = match &error.hint {
                    Some(hint) => format!("{} {hint}", error.message),
                    None => error.message.clone(),
                };
                Some(note(text).text_color(theme.error))
            }
            None => None,
        };
        let trust = check.new_server.as_ref().map(|server| {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .pl(px(22.))
                .gap(px(6.))
                .child(small(
                    &theme,
                    format!(
                        "shcp has never talked to {}. If you can, compare its {} fingerprint \
                         with the one the server's owner gave you.",
                        server.host, server.key_type
                    ),
                ))
                .child(
                    div()
                        .font_family(mono_font())
                        .text_size(ui_px(11.))
                        .child(server.fingerprint.clone()),
                )
                .child(
                    div().flex().gap(px(6.)).child(
                        Button::new("check-trust")
                            .keyboard_focus()
                            .label("Trust and go on")
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| this.trust_server(cx))),
                    ),
                )
        });
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(6.))
                .p(px(10.))
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .border_1()
                .border_color(theme.border)
                .bg(theme.input_bg)
                .children(rows)
                .children(trust)
                .children(message)
                .into_any_element(),
        )
    }
}

impl Render for HostEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let editing = self.original.is_some();
        let target = SshCommand::parse(self.address.read(cx).text());
        let name_error = self.name_error(cx);
        let name_warning = self.replaces_config_host(cx);
        let can_save = name_error.is_none()
            && target
                .as_ref()
                .is_ok_and(|command| !command.host.is_empty());
        let check_running = self
            .check
            .as_ref()
            .is_some_and(|check| check.outcome.is_none());
        let check_failed = matches!(
            self.check.as_ref().and_then(|check| check.outcome.as_ref()),
            Some(CheckOutcome::Failed(_))
        );
        let rows = self.extra_rows(cx);
        let add_buttons = self.add_buttons(cx);
        let group_field = self.group_field(cx);
        let sign_in = self.sign_in(cx);
        let check_panel = self.check_panel(cx);
        let read_back = self.read_back(cx);
        let connects = self.connects();
        let check_button = Button::new("check-connection")
            .keyboard_focus()
            .icon(if check_running {
                IconName::Close
            } else {
                IconName::Plug
            })
            .label(if check_running {
                "Stop"
            } else if self.check.is_some() {
                "Check again"
            } else {
                "Check connection"
            })
            .style(ButtonStyle::Ghost)
            .disabled(!check_running && !can_save)
            .on_click(cx.listener(move |this, _, window, cx| {
                if check_running {
                    this.stop_check(cx);
                } else {
                    this.start_check(window, cx);
                }
            }));
        let menu = self.menu.clone().map(|menu| {
            let this = cx.entity().downgrade();
            ContextMenu::new(
                "host-group-menu",
                menu,
                Rc::new(move |_, cx| {
                    this.update(cx, |this, cx| {
                        this.menu = None;
                        cx.notify();
                    })
                    .ok();
                }),
            )
        });
        editor_frame(&theme, 640., self.embedded)
            .when(!self.embedded, |this| {
                this.max_w(relative(0.96)).max_h(relative(0.96))
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .child(self.appearance_button(cx))
                    .child(title(
                        &theme,
                        if editing { "Edit host" } else { "New host" },
                    )),
            )
            .child(
                div()
                    .id("host-editor-fields")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .gap(px(12.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .gap(px(5.))
                            .child(field(&theme, "Address", self.address.clone()))
                            .child(read_back)
                            .when(!self.ignored.is_empty(), |this| {
                                this.child(
                                    small(&theme, format!("Left out: {}", self.ignored.join(", ")))
                                        .flex_none(),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .gap(px(8.))
                                    .child(
                                        field(&theme, "Name", self.alias.clone())
                                            .flex_grow()
                                            .flex_basis(px(0.)),
                                    )
                                    .child(field(&theme, "Group", group_field).w(px(180.))),
                            )
                            .children(name_error.map(|error| {
                                div()
                                    .flex_none()
                                    .text_size(ui_px(12.))
                                    .text_color(theme.error)
                                    .child(error)
                            }))
                            .children(name_warning.map(|warning| {
                                div()
                                    .flex_none()
                                    .text_size(ui_px(12.))
                                    .text_color(theme.warning)
                                    .child(warning)
                            })),
                    )
                    .child(sign_in)
                    .when(!rows.is_empty(), |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_none()
                                .gap(px(8.))
                                .pt(px(12.))
                                .border_t_1()
                                .border_color(theme.border)
                                .children(rows),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .flex_wrap()
                            .gap(px(4.))
                            .ml(px(-1.))
                            .children(add_buttons),
                    )
                    .children(check_panel),
            )
            .children(
                self.error
                    .clone()
                    .map(|e| div().flex_none().text_color(theme.error).child(e)),
            )
            .child(
                button_row()
                    .flex_none()
                    .items_center()
                    .when(editing, |this| {
                        this.child(
                            Button::new("delete")
                                .keyboard_focus()
                                .label("Delete")
                                .style(ButtonStyle::GhostDanger)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.delete(window, cx)),
                                ),
                        )
                    })
                    .child(check_button)
                    .child(div().flex_1())
                    .child(
                        Button::new("cancel")
                            .keyboard_focus()
                            .label("Cancel")
                            .style(ButtonStyle::Ghost)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(super::CancelEvent))),
                    )
                    .child(
                        Button::new("save")
                            .keyboard_focus()
                            .label(if check_failed { "Save anyway" } else { "Save" })
                            .style(if connects {
                                ButtonStyle::Filled
                            } else {
                                ButtonStyle::Primary
                            })
                            .disabled(!can_save)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save(false, window, cx)),
                            ),
                    )
                    .when(connects, |this| {
                        this.child(
                            Button::new("save-connect")
                                .keyboard_focus()
                                .label("Save & connect")
                                .style(ButtonStyle::Primary)
                                .disabled(!can_save)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.save(true, window, cx)),
                                ),
                        )
                    }),
            )
            .children(menu)
    }
}

impl super::ModalView for HostEditor {
    fn draft(&self, cx: &App) -> Vec<String> {
        let mut values: Vec<String> = [
            &self.address,
            &self.alias,
            &self.group_input,
            &self.jump,
            &self.proxy,
            &self.keepalive,
            &self.keepalive_count,
        ]
        .into_iter()
        .map(|input| input.read(cx).text().to_string())
        .collect();
        values.extend([
            self.group.clone().unwrap_or_default(),
            self.identity
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            self.forward_agent.to_string(),
            self.appearance.read(cx).color().slug().to_string(),
        ]);
        for forward in &self.forwards {
            values.extend(forward.read(cx).draft(cx));
        }
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(alias: &str, source: HostSource) -> HostConfig {
        HostConfig {
            alias: alias.into(),
            source,
            ..Default::default()
        }
    }

    #[test]
    fn only_a_config_host_of_the_same_name_gets_replaced() {
        let hosts = [
            host("from-config", HostSource::SshConfig),
            host("from-app", HostSource::App),
        ];
        assert!(takes_place_of_config_host(&hosts, "from-config"));
        assert!(!takes_place_of_config_host(&hosts, "from-app"));
        assert!(!takes_place_of_config_host(&hosts, "new"));
    }
}
