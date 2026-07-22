use std::path::PathBuf;

use anyhow::{Context as _, Result};
use collections::HashMap;
use editor::Editor;
use gpui::{Entity, FocusHandle, Focusable, ScrollHandle, Task, prelude::*};
use language::Buffer;
use project::{Project, agent_server_store::AgentId};
use settings::{
    AllAgentServersSettings, CustomAgentServerSettings, SettingsContent, SettingsStore,
};
use ui::{
    AiSettingItem, AiSettingItemSource, AiSettingItemStatus, ButtonStyle, LabelSize, Tooltip,
    prelude::*,
};
use util::ResultExt as _;

struct QuickInstallAgent {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    icon: IconName,
}

const QUICK_INSTALL_AGENTS: &[QuickInstallAgent] = &[
    QuickInstallAgent {
        id: "codex-acp",
        name: "Codex",
        description: "OpenAI Codex through the ACP Registry.",
        icon: IconName::AiOpenAi,
    },
    QuickInstallAgent {
        id: "claude-acp",
        name: "Claude Code",
        description: "Claude Code through the ACP Registry.",
        icon: IconName::AiClaude,
    },
    QuickInstallAgent {
        id: "opencode",
        name: "OpenCode",
        description: "OpenCode with its native ACP server.",
        icon: IconName::AiOpenCode,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegistryInstallStatus {
    NotInstalled,
    InstalledRegistry,
    InstalledCustom,
}

struct KeyValueRow {
    key: Entity<Editor>,
    value: Entity<Editor>,
}

struct RemoteAgentForm {
    original_id: Option<AgentId>,
    name: Entity<Editor>,
    command: Entity<Editor>,
    args: Entity<Editor>,
    env: Vec<KeyValueRow>,
    default_mode: Option<String>,
    default_model: Option<String>,
    favorite_models: Vec<String>,
    default_config_options: HashMap<String, String>,
    favorite_config_option_values: HashMap<String, Vec<String>>,
    error: Option<SharedString>,
}

impl RemoteAgentForm {
    fn new(
        existing: Option<(AgentId, CustomAgentServerSettings)>,
        window: &mut Window,
        cx: &mut Context<RemoteAgentConfiguration>,
    ) -> Self {
        let original_id = existing.as_ref().map(|(id, _)| id.clone());
        let name = original_id.as_ref().map(|id| id.0.to_string());

        let mut command = None;
        let mut args = None;
        let mut env = Vec::new();
        let mut default_mode = None;
        let mut default_model = None;
        let mut favorite_models = Vec::new();
        let mut default_config_options = HashMap::default();
        let mut favorite_config_option_values = HashMap::default();

        if let Some((
            _,
            CustomAgentServerSettings::Custom {
                path,
                args: existing_args,
                env: existing_env,
                default_mode: existing_default_mode,
                default_model: existing_default_model,
                favorite_models: existing_favorite_models,
                default_config_options: existing_default_config_options,
                favorite_config_option_values: existing_favorite_config_option_values,
            },
        )) = existing
        {
            command = Some(path.to_string_lossy().to_string());
            if !existing_args.is_empty() {
                args = Some(existing_args.join(" "));
            }

            let mut pairs = existing_env.into_iter().collect::<Vec<_>>();
            pairs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            env.extend(
                pairs
                    .into_iter()
                    .map(|(key, value)| new_key_value_row(Some(&key), Some(&value), window, cx)),
            );
            default_mode = existing_default_mode;
            default_model = existing_default_model;
            favorite_models = existing_favorite_models;
            default_config_options = existing_default_config_options;
            favorite_config_option_values = existing_favorite_config_option_values;
        }

        Self {
            original_id,
            name: new_input("my-agent", name.as_deref(), window, cx),
            command: new_input("/path/to/agent", command.as_deref(), window, cx),
            args: new_input("--flag value", args.as_deref(), window, cx),
            env,
            default_mode,
            default_model,
            favorite_models,
            default_config_options,
            favorite_config_option_values,
            error: None,
        }
    }
}

struct RemoteAgentFormValues {
    original_id: Option<AgentId>,
    name: String,
    command: String,
    args: String,
    env: Vec<(String, String)>,
    default_mode: Option<String>,
    default_model: Option<String>,
    favorite_models: Vec<String>,
    default_config_options: HashMap<String, String>,
    favorite_config_option_values: HashMap<String, Vec<String>>,
}

pub struct RemoteAgentConfiguration {
    project: Entity<Project>,
    buffer: Option<Entity<Buffer>>,
    agents: AllAgentServersSettings,
    form: Option<RemoteAgentForm>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    loading: bool,
    saving: bool,
    error: Option<SharedString>,
    _load_task: Task<()>,
    save_task: Option<Task<()>>,
}

impl RemoteAgentConfiguration {
    pub fn new(project: Entity<Project>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let open_task = project.update(cx, |project, cx| project.open_server_settings(cx));
        let load_task = cx.spawn_in(window, async move |this, cx| {
            let result = open_task.await;
            this.update_in(cx, |this, _window, cx| {
                this.loading = false;
                match result {
                    Ok(buffer) => match read_agent_servers(&buffer.read(cx).text(), cx) {
                        Ok(agents) => {
                            this.buffer = Some(buffer);
                            this.agents = agents;
                            this.error = None;
                        }
                        Err(error) => this.error = Some(error.to_string().into()),
                    },
                    Err(error) => {
                        this.error =
                            Some(format!("Failed to open remote settings: {error:#}").into());
                    }
                }
                cx.notify();
            })
            .log_err();
        });

        Self {
            project,
            buffer: None,
            agents: AllAgentServersSettings::default(),
            form: None,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            loading: true,
            saving: false,
            error: None,
            _load_task: load_task,
            save_task: None,
        }
    }

    fn open_new_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form = Some(RemoteAgentForm::new(None, window, cx));
        cx.notify();
    }

    fn open_edit_form(&mut self, id: AgentId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(settings) = self.agents.get(id.0.as_ref()).cloned() else {
            self.error = Some(format!("Agent \"{}\" no longer exists.", id.0).into());
            cx.notify();
            return;
        };
        if !matches!(settings, CustomAgentServerSettings::Custom { .. }) {
            self.error = Some("Registry Agents cannot be edited from this page.".into());
            cx.notify();
            return;
        }

        self.form = Some(RemoteAgentForm::new(Some((id, settings)), window, cx));
        cx.notify();
    }

    fn cancel_form(&mut self, cx: &mut Context<Self>) {
        if !self.saving {
            self.form = None;
            cx.notify();
        }
    }

    fn save_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }

        let built = self
            .form
            .as_ref()
            .context("No remote Agent form is open")
            .and_then(|form| build_settings_from_form(form, cx).map_err(anyhow::Error::msg));
        let (id, original_id, content) = match built {
            Ok(built) => built,
            Err(error) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(error.to_string().into());
                }
                cx.notify();
                return;
            }
        };

        if original_id
            .as_ref()
            .is_none_or(|original_id| original_id.0 != id.0)
            && self.agents.contains_key(id.0.as_ref())
        {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(format!("An Agent named \"{}\" already exists.", id.0).into());
            }
            cx.notify();
            return;
        }

        match self.update_remote_settings(
            move |settings| upsert_custom_agent(settings, original_id, id, content),
            cx,
        ) {
            Ok(buffer) => self.persist_buffer(buffer, true, window, cx),
            Err(error) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(error.to_string().into());
                }
                cx.notify();
            }
        }
    }

    fn remove_agent(&mut self, id: AgentId, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }

        match self.update_remote_settings(move |settings| remove_custom_agent(settings, &id), cx) {
            Ok(buffer) => self.persist_buffer(buffer, false, window, cx),
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
            }
        }
    }

    fn install_registry_agent(
        &mut self,
        id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving || self.registry_install_status(id) != RegistryInstallStatus::NotInstalled {
            return;
        }

        match self.update_remote_settings(
            move |settings| insert_registry_agent_setting(settings, id),
            cx,
        ) {
            Ok(buffer) => self.persist_buffer(buffer, false, window, cx),
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
            }
        }
    }

    fn remove_registry_agent(&mut self, id: AgentId, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }

        match self.update_remote_settings(
            move |settings| remove_registry_agent_setting(settings, &id),
            cx,
        ) {
            Ok(buffer) => self.persist_buffer(buffer, false, window, cx),
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
            }
        }
    }

    fn registry_install_status(&self, id: &str) -> RegistryInstallStatus {
        match self.agents.get(id) {
            Some(CustomAgentServerSettings::Registry { .. }) => {
                RegistryInstallStatus::InstalledRegistry
            }
            Some(CustomAgentServerSettings::Custom { .. }) => {
                RegistryInstallStatus::InstalledCustom
            }
            None => RegistryInstallStatus::NotInstalled,
        }
    }

    fn update_remote_settings(
        &mut self,
        update: impl FnOnce(&mut SettingsContent),
        cx: &mut Context<Self>,
    ) -> Result<Entity<Buffer>> {
        let buffer = self
            .buffer
            .clone()
            .context("Remote server settings are not loaded")?;
        let current_text = buffer.read(cx).text();
        let new_text = cx
            .global::<SettingsStore>()
            .new_text_for_update(current_text, update)?;
        let agents = read_agent_servers(&new_text, cx)?;

        buffer.update(cx, |buffer, cx| {
            let len = buffer.len();
            buffer.edit([(0..len, new_text)], None, cx);
        });
        self.agents = agents;
        self.error = None;
        Ok(buffer)
    }

    fn persist_buffer(
        &mut self,
        buffer: Entity<Buffer>,
        close_form_on_success: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saving = true;
        let save = self
            .project
            .update(cx, |project, cx| project.save_buffer(buffer, cx));
        self.save_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            this.update_in(cx, |this, _window, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        if close_form_on_success {
                            this.form = None;
                        }
                        this.error = None;
                    }
                    Err(error) => {
                        let message = format!("Failed to save remote settings: {error:#}");
                        if let Some(form) = this.form.as_mut() {
                            form.error = Some(message.into());
                        } else {
                            this.error = Some(message.into());
                        }
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new("Remote ACP Agents"))
                    .child(
                        Label::new("Agents saved in this SSH host's server settings.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .when(self.form.is_none() && !self.loading, |this| {
                this.child(
                    Button::new("add-remote-agent", "Add Custom Agent")
                        .style(ButtonStyle::Outlined)
                        .label_size(LabelSize::Small)
                        .start_icon(
                            Icon::new(IconName::Plus)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .disabled(self.buffer.is_none() || self.saving)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_new_form(window, cx);
                        })),
                )
            })
    }

    fn render_registry_quick_install(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_2()
            .child(Label::new("Install from ACP Registry"))
            .child(
                Label::new(
                    "The SSH host resolves compatibility, downloads, authenticates, and runs the Agent.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .children(QUICK_INSTALL_AGENTS.iter().map(|agent| {
                let status = self.registry_install_status(agent.id);
                let action = match status {
                    RegistryInstallStatus::NotInstalled => Button::new(
                        format!("install-remote-registry-agent-{}", agent.id),
                        "Install",
                    )
                    .style(ButtonStyle::Filled)
                    .label_size(LabelSize::Small)
                    .disabled(self.buffer.is_none() || self.saving)
                    .on_click(cx.listener({
                        let id = agent.id;
                        move |this, _, window, cx| {
                            this.install_registry_agent(id, window, cx);
                        }
                    })),
                    RegistryInstallStatus::InstalledRegistry => Button::new(
                        format!("remove-remote-registry-agent-{}", agent.id),
                        "Remove",
                    )
                    .style(ButtonStyle::Outlined)
                    .label_size(LabelSize::Small)
                    .disabled(self.saving)
                    .on_click(cx.listener({
                        let id = AgentId(agent.id.into());
                        move |this, _, window, cx| {
                            this.remove_registry_agent(id.clone(), window, cx);
                        }
                    })),
                    RegistryInstallStatus::InstalledCustom => Button::new(
                        format!("custom-remote-registry-agent-{}", agent.id),
                        "Custom",
                    )
                    .style(ButtonStyle::Outlined)
                    .label_size(LabelSize::Small)
                    .disabled(true),
                };

                h_flex()
                    .w_full()
                    .min_w_0()
                    .p_3()
                    .gap_3()
                    .justify_between()
                    .border_1()
                    .border_color(cx.theme().colors().border_variant)
                    .rounded_sm()
                    .child(
                        h_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_2()
                            .child(
                                Icon::new(agent.icon)
                                    .size(IconSize::Medium)
                                    .color(Color::Muted),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(Label::new(agent.name))
                                    .child(
                                        Label::new(agent.description)
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .child(
                                        Label::new(format!("Registry ID: {}", agent.id))
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    ),
                            ),
                    )
                    .child(action)
            }))
            .into_any_element()
    }

    fn render_agents(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut agents = self
            .agents
            .iter()
            .map(|(id, settings)| (AgentId(id.clone().into()), settings.clone()))
            .collect::<Vec<_>>();
        agents.sort_unstable_by(|a, b| a.0.0.to_lowercase().cmp(&b.0.0.to_lowercase()));

        if agents.is_empty() {
            return v_flex()
                .w_full()
                .p_4()
                .items_center()
                .gap_1()
                .border_1()
                .border_dashed()
                .border_color(cx.theme().colors().border)
                .rounded_sm()
                .child(Label::new("No remote ACP Agents configured."))
                .child(
                    Label::new("Install a Registry Agent or add a custom Agent.")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element();
        }

        v_flex()
            .w_full()
            .gap_1()
            .children(agents.into_iter().map(|(id, settings)| {
                let is_custom = matches!(settings, CustomAgentServerSettings::Custom { .. });
                let source = if is_custom {
                    AiSettingItemSource::Custom
                } else {
                    AiSettingItemSource::Registry
                };
                let id_string = id.0.clone();
                let configure_button = is_custom.then(|| {
                    IconButton::new(
                        format!("configure-remote-agent-{}", id_string),
                        IconName::Settings,
                    )
                    .icon_color(Color::Muted)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Configure Agent"))
                    .disabled(self.saving)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            this.open_edit_form(id.clone(), window, cx);
                        }
                    }))
                });
                let remove_button = Some({
                    IconButton::new(
                        format!("remove-remote-agent-{}", id_string),
                        IconName::Trash,
                    )
                    .icon_color(Color::Muted)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text(if is_custom {
                        "Remove Custom Agent"
                    } else {
                        "Remove Registry Agent"
                    }))
                    .disabled(self.saving)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            if is_custom {
                                this.remove_agent(id.clone(), window, cx);
                            } else {
                                this.remove_registry_agent(id.clone(), window, cx);
                            }
                        }
                    }))
                });

                AiSettingItem::new(
                    id_string.clone(),
                    id_string,
                    AiSettingItemStatus::Stopped,
                    source,
                )
                .icon(
                    Icon::new(IconName::Sparkle)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .when_some(configure_button, |this, button| this.action(button))
                .when_some(remove_button, |this, button| this.action(button))
                .into_any_element()
            }))
            .into_any_element()
    }

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.form.as_ref() else {
            return div().into_any_element();
        };

        v_flex()
            .w_full()
            .gap_4()
            .child(render_field(
                "Agent Name",
                "Required. A unique name for this SSH host.",
                &form.name,
                cx,
            ))
            .child(render_field(
                "Command",
                "Required. Use an executable path available on the SSH host.",
                &form.command,
                cx,
            ))
            .child(render_field(
                "Arguments",
                "Optional space-separated arguments passed to the command.",
                &form.args,
                cx,
            ))
            .child(self.render_environment_variables(&form.env, cx))
            .when_some(form.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .child(
                            Icon::new(IconName::XCircle)
                                .size(IconSize::Small)
                                .color(Color::Error),
                        )
                        .child(Label::new(error).size(LabelSize::Small).color(Color::Error)),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("remote-agent-form-cancel", "Cancel")
                            .style(ButtonStyle::Subtle)
                            .disabled(self.saving)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.cancel_form(cx);
                            })),
                    )
                    .child(
                        Button::new(
                            "remote-agent-form-save",
                            if self.saving { "Saving..." } else { "Save" },
                        )
                        .style(ButtonStyle::Filled)
                        .disabled(self.saving)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.save_form(window, cx);
                        })),
                    ),
            )
            .into_any_element()
    }

    fn render_environment_variables(
        &self,
        rows: &[KeyValueRow],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_2()
            .child(Label::new("Environment Variables"))
            .child(
                Label::new("Optional variables provided only to the remote Agent process.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .children(rows.iter().enumerate().map(|(index, row)| {
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(input_box(&row.key, cx))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(input_box(&row.value, cx))
                            .child(
                                IconButton::new(
                                    ("remove-remote-agent-env", index),
                                    IconName::Close,
                                )
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Muted)
                                .tooltip(Tooltip::text("Remove"))
                                .disabled(self.saving)
                                .on_click(cx.listener(
                                    move |this, _, _window, cx| {
                                        if let Some(form) = this.form.as_mut()
                                            && index < form.env.len()
                                        {
                                            form.env.remove(index);
                                            cx.notify();
                                        }
                                    },
                                )),
                            ),
                    )
            }))
            .child(
                Button::new("add-remote-agent-env", "Add Variable")
                    .style(ButtonStyle::Outlined)
                    .label_size(LabelSize::Small)
                    .start_icon(
                        Icon::new(IconName::Plus)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .disabled(self.saving)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let row = new_key_value_row(None, None, window, cx);
                        let focus_handle = row.key.focus_handle(cx);
                        if let Some(form) = this.form.as_mut() {
                            form.env.push(row);
                            focus_handle.focus(window, cx);
                            cx.notify();
                        }
                    })),
            )
    }
}

impl Focusable for RemoteAgentConfiguration {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RemoteAgentConfiguration {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("remote-agent-configuration")
            .size_full()
            .track_focus(&self.focus_handle)
            .track_scroll(&self.scroll_handle)
            .overflow_y_scroll()
            .p_4()
            .pb_8()
            .gap_4()
            .bg(cx.theme().colors().panel_background)
            .child(self.render_header(cx))
            .when(self.loading, |this| {
                this.child(Label::new("Loading SSH host settings...").color(Color::Muted))
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .child(
                            Icon::new(IconName::XCircle)
                                .size(IconSize::Small)
                                .color(Color::Error),
                        )
                        .child(Label::new(error).size(LabelSize::Small).color(Color::Error)),
                )
            })
            .when(!self.loading && self.form.is_none(), |this| {
                this.child(self.render_registry_quick_install(cx))
                    .child(Label::new("Configured Agents"))
                    .child(self.render_agents(cx))
            })
            .when(self.form.is_some(), |this| this.child(self.render_form(cx)))
    }
}

fn new_input(
    placeholder: &str,
    initial: Option<&str>,
    window: &mut Window,
    cx: &mut Context<RemoteAgentConfiguration>,
) -> Entity<Editor> {
    let placeholder = placeholder.to_string();
    let initial = initial.map(ToOwned::to_owned);
    cx.new(|cx| {
        let mut editor = Editor::single_line(window, cx);
        editor.set_placeholder_text(&placeholder, window, cx);
        if let Some(initial) = initial {
            editor.set_text(initial, window, cx);
        }
        editor
    })
}

fn new_key_value_row(
    key: Option<&str>,
    value: Option<&str>,
    window: &mut Window,
    cx: &mut Context<RemoteAgentConfiguration>,
) -> KeyValueRow {
    KeyValueRow {
        key: new_input("Key", key, window, cx),
        value: new_input("Value", value, window, cx),
    }
}

fn render_field(
    title: &'static str,
    description: &'static str,
    editor: &Entity<Editor>,
    cx: &mut Context<RemoteAgentConfiguration>,
) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_1()
        .child(Label::new(title))
        .child(
            Label::new(description)
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .child(input_box(editor, cx))
}

fn input_box(
    editor: &Entity<Editor>,
    cx: &mut Context<RemoteAgentConfiguration>,
) -> impl IntoElement {
    let colors = cx.theme().colors();
    h_flex()
        .w_full()
        .min_w_0()
        .h_8()
        .py_1()
        .px_2()
        .rounded_md()
        .border_1()
        .border_color(colors.border)
        .bg(colors.editor_background)
        .child(editor.clone())
}

fn read_agent_servers(text: &str, cx: &App) -> Result<AllAgentServersSettings> {
    let mut agents = AllAgentServersSettings::default();
    cx.global::<SettingsStore>()
        .edits_for_update(text, |settings| {
            agents = settings.agent_servers.clone().unwrap_or_default();
        })?;
    Ok(agents)
}

fn build_settings_from_form(
    form: &RemoteAgentForm,
    cx: &App,
) -> Result<(AgentId, Option<AgentId>, CustomAgentServerSettings), SharedString> {
    build_settings_from_values(RemoteAgentFormValues {
        original_id: form.original_id.clone(),
        name: form.name.read(cx).text(cx),
        command: form.command.read(cx).text(cx),
        args: form.args.read(cx).text(cx),
        env: form
            .env
            .iter()
            .map(|row| (row.key.read(cx).text(cx), row.value.read(cx).text(cx)))
            .collect(),
        default_mode: form.default_mode.clone(),
        default_model: form.default_model.clone(),
        favorite_models: form.favorite_models.clone(),
        default_config_options: form.default_config_options.clone(),
        favorite_config_option_values: form.favorite_config_option_values.clone(),
    })
}

fn build_settings_from_values(
    values: RemoteAgentFormValues,
) -> Result<(AgentId, Option<AgentId>, CustomAgentServerSettings), SharedString> {
    let name = values.name.trim();
    if name.is_empty() {
        return Err("Agent name is required.".into());
    }

    let command = values.command.trim();
    if command.is_empty() {
        return Err("Command is required.".into());
    }

    let mut env = HashMap::default();
    for (key, value) in values.env {
        let key = key.trim().to_string();
        if key.is_empty() {
            continue;
        }
        if env.insert(key.clone(), value).is_some() {
            return Err(format!("Duplicate environment variable \"{key}\".").into());
        }
    }

    Ok((
        AgentId(name.to_string().into()),
        values.original_id,
        CustomAgentServerSettings::Custom {
            path: PathBuf::from(command),
            args: values
                .args
                .split_whitespace()
                .map(ToOwned::to_owned)
                .collect(),
            env,
            default_mode: values.default_mode,
            default_model: values.default_model,
            favorite_models: values.favorite_models,
            default_config_options: values.default_config_options,
            favorite_config_option_values: values.favorite_config_option_values,
        },
    ))
}

fn upsert_custom_agent(
    settings: &mut SettingsContent,
    original_id: Option<AgentId>,
    id: AgentId,
    content: CustomAgentServerSettings,
) {
    let agents = settings.agent_servers.get_or_insert_default();
    if let Some(original_id) = original_id
        && original_id.0 != id.0
    {
        agents.remove(original_id.0.as_ref());
    }
    agents.insert(id.0.to_string(), content);
}

fn remove_custom_agent(settings: &mut SettingsContent, id: &AgentId) {
    let Some(agents) = settings.agent_servers.as_mut() else {
        return;
    };
    if agents
        .get(id.0.as_ref())
        .is_some_and(|settings| matches!(settings, CustomAgentServerSettings::Custom { .. }))
    {
        agents.remove(id.0.as_ref());
    }
}

fn insert_registry_agent_setting(settings: &mut SettingsContent, id: &str) {
    settings
        .agent_servers
        .get_or_insert_default()
        .entry(id.to_string())
        .or_insert_with(|| CustomAgentServerSettings::Registry {
            env: HashMap::default(),
            default_mode: None,
            default_model: None,
            favorite_models: Vec::new(),
            default_config_options: HashMap::default(),
            favorite_config_option_values: HashMap::default(),
        });
}

fn remove_registry_agent_setting(settings: &mut SettingsContent, id: &AgentId) {
    let Some(agents) = settings.agent_servers.as_mut() else {
        return;
    };
    if agents
        .get(id.0.as_ref())
        .is_some_and(|settings| matches!(settings, CustomAgentServerSettings::Registry { .. }))
    {
        agents.remove(id.0.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(command: &str) -> CustomAgentServerSettings {
        CustomAgentServerSettings::Custom {
            path: PathBuf::from(command),
            args: Vec::new(),
            env: HashMap::default(),
            default_mode: Some("plan".to_string()),
            default_model: Some("model".to_string()),
            favorite_models: vec!["model".to_string()],
            default_config_options: HashMap::default(),
            favorite_config_option_values: HashMap::default(),
        }
    }

    #[test]
    fn builds_remote_custom_agent_and_preserves_advanced_fields() {
        let mut default_config_options = HashMap::default();
        default_config_options.insert("mode".to_string(), "safe".to_string());
        let values = RemoteAgentFormValues {
            original_id: Some(AgentId("old".into())),
            name: " remote-codex ".to_string(),
            command: " /opt/codex-acp ".to_string(),
            args: "--profile remote".to_string(),
            env: vec![("CODEX_HOME".to_string(), "/srv/codex".to_string())],
            default_mode: Some("plan".to_string()),
            default_model: Some("model".to_string()),
            favorite_models: vec!["model".to_string()],
            default_config_options: default_config_options.clone(),
            favorite_config_option_values: HashMap::default(),
        };

        let (id, original_id, settings) = build_settings_from_values(values).unwrap();
        assert_eq!(id.0.as_ref(), "remote-codex");
        assert_eq!(original_id.unwrap().0.as_ref(), "old");
        let CustomAgentServerSettings::Custom {
            path,
            args,
            env,
            default_mode,
            default_model,
            favorite_models,
            default_config_options: actual_default_config_options,
            ..
        } = settings
        else {
            panic!("expected custom Agent settings");
        };
        assert_eq!(path, PathBuf::from("/opt/codex-acp"));
        assert_eq!(args, ["--profile", "remote"]);
        assert_eq!(env.get("CODEX_HOME").unwrap(), "/srv/codex");
        assert_eq!(default_mode.as_deref(), Some("plan"));
        assert_eq!(default_model.as_deref(), Some("model"));
        assert_eq!(favorite_models, ["model"]);
        assert_eq!(actual_default_config_options, default_config_options);
    }

    #[test]
    fn rejects_duplicate_environment_variables() {
        let result = build_settings_from_values(RemoteAgentFormValues {
            original_id: None,
            name: "agent".to_string(),
            command: "/opt/agent".to_string(),
            args: String::new(),
            env: vec![
                ("TOKEN".to_string(), "one".to_string()),
                (" TOKEN ".to_string(), "two".to_string()),
            ],
            default_mode: None,
            default_model: None,
            favorite_models: Vec::new(),
            default_config_options: HashMap::default(),
            favorite_config_option_values: HashMap::default(),
        });

        assert_eq!(
            result.unwrap_err().as_ref(),
            "Duplicate environment variable \"TOKEN\"."
        );
    }

    #[test]
    fn rename_and_remove_only_touch_custom_agents() {
        let mut settings = SettingsContent::default();
        settings
            .agent_servers
            .get_or_insert_default()
            .insert("old".to_string(), custom("/old"));
        settings.agent_servers.as_mut().unwrap().insert(
            "registry".to_string(),
            CustomAgentServerSettings::Registry {
                env: HashMap::default(),
                default_mode: None,
                default_model: None,
                favorite_models: Vec::new(),
                default_config_options: HashMap::default(),
                favorite_config_option_values: HashMap::default(),
            },
        );

        upsert_custom_agent(
            &mut settings,
            Some(AgentId("old".into())),
            AgentId("new".into()),
            custom("/new"),
        );
        assert!(!settings.agent_servers.as_ref().unwrap().contains_key("old"));
        assert!(settings.agent_servers.as_ref().unwrap().contains_key("new"));

        remove_custom_agent(&mut settings, &AgentId("registry".into()));
        assert!(
            settings
                .agent_servers
                .as_ref()
                .unwrap()
                .contains_key("registry")
        );
        remove_custom_agent(&mut settings, &AgentId("new".into()));
        assert!(!settings.agent_servers.as_ref().unwrap().contains_key("new"));
    }

    #[test]
    fn registry_install_and_remove_preserve_custom_and_registry_settings() {
        let mut settings = SettingsContent::default();
        settings
            .agent_servers
            .get_or_insert_default()
            .insert("codex-acp".to_string(), custom("/opt/custom-codex"));

        let mut registry_env = HashMap::default();
        registry_env.insert("REMOTE_ONLY".to_string(), "1".to_string());
        settings.agent_servers.as_mut().unwrap().insert(
            "claude-acp".to_string(),
            CustomAgentServerSettings::Registry {
                env: registry_env.clone(),
                default_mode: Some("plan".to_string()),
                default_model: None,
                favorite_models: Vec::new(),
                default_config_options: HashMap::default(),
                favorite_config_option_values: HashMap::default(),
            },
        );

        insert_registry_agent_setting(&mut settings, "codex-acp");
        insert_registry_agent_setting(&mut settings, "claude-acp");
        insert_registry_agent_setting(&mut settings, "opencode");

        let agents = settings.agent_servers.as_ref().unwrap();
        assert!(matches!(
            agents.get("codex-acp"),
            Some(CustomAgentServerSettings::Custom { path, .. })
                if path == &PathBuf::from("/opt/custom-codex")
        ));
        assert!(matches!(
            agents.get("claude-acp"),
            Some(CustomAgentServerSettings::Registry { env, default_mode, .. })
                if env == &registry_env && default_mode.as_deref() == Some("plan")
        ));
        assert!(matches!(
            agents.get("opencode"),
            Some(CustomAgentServerSettings::Registry { .. })
        ));
        let serialized = serde_json::to_value(agents.get("opencode").unwrap()).unwrap();
        assert_eq!(serialized["type"], "registry");

        remove_registry_agent_setting(&mut settings, &AgentId("codex-acp".into()));
        remove_registry_agent_setting(&mut settings, &AgentId("opencode".into()));
        let agents = settings.agent_servers.as_ref().unwrap();
        assert!(agents.contains_key("codex-acp"));
        assert!(!agents.contains_key("opencode"));
    }
}
