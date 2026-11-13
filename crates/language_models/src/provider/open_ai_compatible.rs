use anyhow::{Context as _, Result, anyhow, bail};
use credentials_provider::CredentialsProvider;
use futures::{FutureExt, StreamExt, future::BoxFuture};
use gpui::{App, AppContext, AsyncApp, Entity, Task};
use http_client::{CustomHeaders, HttpClient};
use language_model::chat_completion::ChatCompletionEventMapper;
use language_model::{
    AuthenticateError, IconOrSvg, LanguageModel, LanguageModelClient, LanguageModelCompletionError,
    LanguageModelCompletionStream, LanguageModelEffortLevel, LanguageModelId, LanguageModelName,
    LanguageModelProvider, LanguageModelProviderId, LanguageModelProviderName,
    LanguageModelProviderState, LanguageModelRequest, LanguageModelToolChoiceSupport,
    ModelRateLimiters, PromptCompactionStrategy, ProviderErrorCategory, ProviderSettingsView,
    RateLimiter, SubPageProviderSettings, unavailable_error,
};
use open_ai::{
    ResponseStreamEvent,
    responses::{Request as ResponseRequest, StreamEvent as ResponsesStreamEvent, stream_response},
    stream_completion,
};
use settings::Settings;
use std::sync::Arc;
use std::time::Duration;
use ui::IconName;

use crate::provider::api_compatible::{
    ApiCompatibleProviderConfigurationView, ApiCompatibleProviderSettings,
    ApiCompatibleProviderState,
};
use crate::provider::open_ai::{OpenAiResponseEventMapper, into_open_ai, into_open_ai_response};
pub use settings::OpenAiCompatibleAvailableModel as AvailableModel;
pub use settings::OpenAiCompatibleModelCapabilities as ModelCapabilities;

const API_KEY_PLACEHOLDER: &str = "000000000000000000000000000000000000000000000000000";
const CREDENTIALS_HELPER_TIMEOUT: Duration = Duration::from_secs(60);

/// Builds the wire request for a single attempt. Shared so a retried attempt can
/// rebuild the request, which is not cloneable.
type SharedRequestBuilder<Req> =
    Arc<dyn Fn() -> Result<Req, LanguageModelCompletionError> + Send + Sync>;

/// Runs a credentials helper command and returns the API key it prints.
type RunCredentialsHelper = fn(&str) -> BoxFuture<'static, Result<String>>;

#[derive(Default, Clone, Debug, PartialEq)]
pub struct OpenAiCompatibleSettings {
    pub api_url: String,
    pub available_models: Vec<AvailableModel>,
    pub custom_headers: CustomHeaders,
    pub credentials_helper: Option<String>,
}

impl ApiCompatibleProviderSettings for OpenAiCompatibleSettings {
    fn api_url(&self) -> &str {
        &self.api_url
    }
}

pub type State = ApiCompatibleProviderState<OpenAiCompatibleSettings>;

pub struct OpenAiCompatibleLanguageModelProvider {
    id: LanguageModelProviderId,
    name: LanguageModelProviderName,
    http_client: Arc<dyn HttpClient>,
    state: Entity<State>,
    request_limiters: ModelRateLimiters,
    /// Serializes credentials helper runs so concurrent requests that fail
    /// with an expired API key share a single refresh.
    refresh_lock: Arc<smol::lock::Mutex<()>>,
}

impl OpenAiCompatibleLanguageModelProvider {
    pub fn new(
        id: Arc<str>,
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        let state = State::new(
            id.clone(),
            credentials_provider,
            |id, cx| {
                crate::AllLanguageModelSettings::get_global(cx)
                    .openai_compatible
                    .get(id)
            },
            cx,
        );

        Self {
            id: id.clone().into(),
            name: id.into(),
            http_client,
            state,
            request_limiters: ModelRateLimiters::default(),
            refresh_lock: Arc::new(smol::lock::Mutex::new(())),
        }
    }

    /// Every model this provider offers, in settings order.
    fn available_models<'a>(&self, cx: &'a App) -> &'a [AvailableModel] {
        &self.state.read(cx).settings.available_models
    }

    fn language_model(&self, model: &AvailableModel) -> LanguageModel {
        LanguageModel {
            supports_tools: model.capabilities.tools,
            supports_images: model.capabilities.images,
            tool_choice_support: LanguageModelToolChoiceSupport {
                auto: model.capabilities.tools,
                any: model.capabilities.tools,
                none: true,
            },
            prompt_compaction_strategy: PromptCompactionStrategy::RebuildPrompt,
            supports_streaming_tools: true,
            supports_thinking: default_thinking_reasoning_effort(model).is_some(),
            supported_effort_levels: supported_thinking_effort_levels(model).into(),
            supports_split_token_display: true,
            max_output_tokens: model.max_output_tokens,
            ..LanguageModel::new(
                LanguageModelId::from(model.name.clone()),
                LanguageModelName::from(
                    model
                        .display_name
                        .clone()
                        .unwrap_or_else(|| model.name.clone()),
                ),
                self.id.clone(),
                self.name.clone(),
                format!("openai/{}", model.name),
                model.max_tokens,
            )
        }
    }

    /// The current configuration of `model`, if this provider still offers it.
    fn config(
        &self,
        model: &LanguageModel,
        cx: &App,
    ) -> Result<AvailableModel, LanguageModelCompletionError> {
        self.available_models(cx)
            .iter()
            .find(|available| available.name == model.id.0.as_ref())
            .cloned()
            .ok_or_else(|| unavailable_error(model))
    }

    fn stream_chat_completion(
        &self,
        request_limiter: &RateLimiter,
        build_request: SharedRequestBuilder<open_ai::Request>,
        cx: &AsyncApp,
    ) -> BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<
                'static,
                Result<ResponseStreamEvent, LanguageModelCompletionError>,
            >,
            LanguageModelCompletionError,
        >,
    > {
        let (api_url, extra_headers) = self.state.read_with(cx, |state, _cx| {
            (
                state.settings.api_url.clone(),
                state.settings.custom_headers.clone(),
            )
        });
        let send = {
            let http_client = self.http_client.clone();
            let provider = self.name.clone();
            let request_limiter = request_limiter.clone();
            move |api_key: Option<Arc<str>>| {
                send_chat_completion(
                    http_client.clone(),
                    provider.clone(),
                    request_limiter.clone(),
                    api_key,
                    api_url.clone(),
                    extra_headers.clone(),
                    build_request.clone(),
                )
            }
        };

        send_with_credentials_refresh(self.state.clone(), self.refresh_lock.clone(), send, cx)
    }

    fn stream_response(
        &self,
        request_limiter: &RateLimiter,
        build_request: SharedRequestBuilder<ResponseRequest>,
        cx: &AsyncApp,
    ) -> BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<
                'static,
                Result<ResponsesStreamEvent, open_ai::RequestError>,
            >,
            LanguageModelCompletionError,
        >,
    > {
        let (api_url, extra_headers) = self.state.read_with(cx, |state, _cx| {
            (
                state.settings.api_url.clone(),
                state.settings.custom_headers.clone(),
            )
        });
        let send = {
            let http_client = self.http_client.clone();
            let provider = self.name.clone();
            let request_limiter = request_limiter.clone();
            move |api_key: Option<Arc<str>>| {
                send_response(
                    http_client.clone(),
                    provider.clone(),
                    request_limiter.clone(),
                    api_key,
                    api_url.clone(),
                    extra_headers.clone(),
                    build_request.clone(),
                )
            }
        };

        send_with_credentials_refresh(self.state.clone(), self.refresh_lock.clone(), send, cx)
    }
}

impl LanguageModelProviderState for OpenAiCompatibleLanguageModelProvider {
    type ObservableEntity = State;

    fn observable_entity(&self) -> Option<Entity<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for OpenAiCompatibleLanguageModelProvider {
    fn id(&self) -> LanguageModelProviderId {
        self.id.clone()
    }

    fn name(&self) -> LanguageModelProviderName {
        self.name.clone()
    }

    fn icon(&self) -> IconOrSvg {
        IconOrSvg::Icon(IconName::AiOpenAiCompat)
    }

    fn default_model(&self, cx: &App) -> Option<LanguageModel> {
        self.available_models(cx)
            .first()
            .map(|model| self.language_model(model))
    }

    fn default_fast_model(&self, _cx: &App) -> Option<LanguageModel> {
        None
    }

    fn provided_models(&self, cx: &App) -> Vec<LanguageModel> {
        self.available_models(cx)
            .iter()
            .map(|model| self.language_model(model))
            .collect()
    }

    fn is_authenticated(&self, cx: &App) -> bool {
        self.state.read(cx).is_authenticated()
    }

    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        self.state.update(cx, |state, cx| state.authenticate(cx))
    }

    fn settings_view(&self, _cx: &mut App) -> Option<ProviderSettingsView> {
        let state = self.state.clone();
        Some(ProviderSettingsView::SubPage(SubPageProviderSettings::new(
            move |window, cx| {
                cx.new(|cx| {
                    ApiCompatibleProviderConfigurationView::new(
                        state.clone(),
                        "OpenAI",
                        API_KEY_PLACEHOLDER,
                        window,
                        cx,
                    )
                })
                .into()
            },
        )))
    }

    fn set_api_key(&self, api_key: Option<String>, cx: &mut App) -> Task<Result<()>> {
        self.state
            .update(cx, |state, cx| state.set_api_key(api_key, cx))
    }
}

impl LanguageModelClient for OpenAiCompatibleLanguageModelProvider {
    fn stream_completion(
        &self,
        model: &LanguageModel,
        mut request: LanguageModelRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<'static, Result<LanguageModelCompletionStream, LanguageModelCompletionError>>
    {
        let config = match cx.update(|cx| self.config(model, cx)) {
            Ok(config) => config,
            Err(error) => return async move { Err(error) }.boxed(),
        };
        let request_limiter = self.request_limiters.for_model(&model.id);
        // `speed` can leak in from a parent thread's model; this provider never
        // supports fast mode, and arbitrary compatible endpoints reject `service_tier`.
        request.speed = None;

        if config.capabilities.chat_completions {
            let reasoning_effort = chat_completion_reasoning_effort(&request, &config);
            let model_name = config.name.clone();
            let parallel_tool_calls = config.capabilities.parallel_tool_calls;
            let prompt_cache_key = config.capabilities.prompt_cache_key;
            let max_output_tokens = config.max_output_tokens;
            let max_tokens_parameter = chat_completion_max_tokens_parameter(&config);
            let interleaved_reasoning = config.capabilities.interleaved_reasoning;
            let build_request: SharedRequestBuilder<open_ai::Request> = Arc::new(move || {
                into_open_ai(
                    request.clone(),
                    &model_name,
                    parallel_tool_calls,
                    prompt_cache_key,
                    max_output_tokens,
                    max_tokens_parameter,
                    reasoning_effort,
                    interleaved_reasoning,
                )
                .map_err(Into::into)
            });
            let completions = self.stream_chat_completion(&request_limiter, build_request, cx);
            let executor = cx.background_executor().clone();
            async move {
                let mapper = ChatCompletionEventMapper::new();
                Ok(language_model::stream_in_background(
                    mapper.map_stream(completions.await?).boxed(),
                    executor,
                ))
            }
            .boxed()
        } else {
            disable_response_thinking_for_none_effort(&mut request, &config);
            let model_name = config.name.clone();
            let parallel_tool_calls = config.capabilities.parallel_tool_calls;
            let prompt_cache_key = config.capabilities.prompt_cache_key;
            let max_output_tokens = config.max_output_tokens;
            let default_reasoning_effort = default_thinking_reasoning_effort(&config);
            let supports_none_effort = supports_none_reasoning_effort(&config);
            let compaction_state_owner = self.id.clone();
            let build_request: SharedRequestBuilder<ResponseRequest> = Arc::new(move || {
                into_open_ai_response(
                    request.clone(),
                    &model_name,
                    parallel_tool_calls,
                    prompt_cache_key,
                    max_output_tokens,
                    default_reasoning_effort,
                    supports_none_effort,
                    &compaction_state_owner,
                )
                .map_err(Into::into)
            });
            let completions = self.stream_response(&request_limiter, build_request, cx);
            let compaction_state_owner = self.id.clone();
            let executor = cx.background_executor().clone();
            async move {
                let mapper = OpenAiResponseEventMapper::new(compaction_state_owner);
                Ok(language_model::stream_in_background(
                    mapper.map_stream(completions.await?).boxed(),
                    executor,
                ))
            }
            .boxed()
        }
    }
}

fn default_thinking_reasoning_effort(model: &AvailableModel) -> Option<open_ai::ReasoningEffort> {
    model
        .reasoning_effort
        .filter(|effort| *effort != open_ai::ReasoningEffort::None)
}

fn supported_thinking_effort_levels(model: &AvailableModel) -> Vec<LanguageModelEffortLevel> {
    let Some(default_effort) = default_thinking_reasoning_effort(model) else {
        return Vec::new();
    };

    open_ai::ReasoningEffort::OPENAI_COMPATIBLE_SELECTABLE
        .into_iter()
        .map(|effort| LanguageModelEffortLevel {
            name: effort.label().into(),
            value: effort.value().into(),
            is_default: effort == default_effort,
            supports_disabling_thinking: None,
        })
        .collect()
}

fn selected_thinking_reasoning_effort(
    request: &LanguageModelRequest,
) -> Option<open_ai::ReasoningEffort> {
    request
        .thinking_effort
        .as_deref()
        .and_then(|effort| effort.parse::<open_ai::ReasoningEffort>().ok())
        .filter(|effort| *effort != open_ai::ReasoningEffort::None)
}

fn chat_completion_max_tokens_parameter(
    model: &AvailableModel,
) -> crate::provider::open_ai::ChatCompletionMaxTokensParameter {
    if model.capabilities.max_tokens_parameter {
        crate::provider::open_ai::ChatCompletionMaxTokensParameter::MaxTokens
    } else {
        crate::provider::open_ai::ChatCompletionMaxTokensParameter::MaxCompletionTokens
    }
}

fn supports_none_reasoning_effort(model: &AvailableModel) -> bool {
    model.reasoning_effort.is_some()
}

fn chat_completion_reasoning_effort(
    request: &LanguageModelRequest,
    model: &AvailableModel,
) -> Option<open_ai::ReasoningEffort> {
    if model.reasoning_effort == Some(open_ai::ReasoningEffort::None) {
        return Some(open_ai::ReasoningEffort::None);
    }

    if request.thinking_allowed {
        selected_thinking_reasoning_effort(request)
            .or_else(|| default_thinking_reasoning_effort(model))
    } else if supports_none_reasoning_effort(model) {
        Some(open_ai::ReasoningEffort::None)
    } else {
        None
    }
}

fn disable_response_thinking_for_none_effort(
    request: &mut LanguageModelRequest,
    model: &AvailableModel,
) {
    if model.reasoning_effort == Some(open_ai::ReasoningEffort::None) {
        request.thinking_allowed = false;
        request.thinking_effort = None;
    }
}

fn is_authentication_rejection(error: &LanguageModelCompletionError) -> bool {
    matches!(
        error,
        LanguageModelCompletionError::ProviderRejection {
            category: ProviderErrorCategory::Authentication,
            ..
        }
    )
}

fn parse_helper_output(output: &str) -> Result<String> {
    let key = output.trim();
    if key.is_empty() {
        bail!("credentials helper produced no output");
    }
    if key.lines().count() > 1 {
        bail!("credentials helper produced multiple lines of output");
    }
    Ok(key.to_string())
}

/// Runs the credentials helper command and returns the API key it prints.
///
/// Returns a boxed future so tests can substitute a fake runner; a real helper
/// spawns a process, which cannot run under gpui's deterministic test scheduler.
fn run_credentials_helper(command: &str) -> BoxFuture<'static, Result<String>> {
    let command = command.to_string();
    async move {
        let mut shell_command = if cfg!(windows) {
            let mut command_builder = smol::process::Command::new("cmd");
            command_builder.arg("/C").arg(&command);
            command_builder
        } else {
            let mut command_builder = smol::process::Command::new("sh");
            command_builder.arg("-c").arg(&command);
            command_builder
        };
        let child = shell_command
            .kill_on_drop(true)
            .stdout(smol::process::Stdio::piped())
            .stderr(smol::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn credentials helper command: {command}"))?;

        // On timeout, `or` drops the output future, which drops the child and
        // kills the process because of `kill_on_drop` above. The helper runs real
        // process I/O, so it can never run under gpui's deterministic test
        // scheduler; that is also why it uses smol's timer rather than
        // `BackgroundExecutor::timer`.
        let output = smol::future::or(
            async move { child.output().await.map_err(anyhow::Error::new) },
            async {
                #[allow(clippy::disallowed_methods)]
                smol::Timer::after(CREDENTIALS_HELPER_TIMEOUT).await;
                Err(anyhow!(
                    "credentials helper did not finish within 60 seconds"
                ))
            },
        )
        .await?;

        if !output.status.success() {
            // The helper's stderr may echo secrets, so only the exit status is
            // reported; the output above drains stderr to keep the helper from
            // blocking on a full pipe.
            bail!("credentials helper exited with status {:?}", output.status);
        }

        parse_helper_output(&String::from_utf8_lossy(&output.stdout))
            .context("credentials helper output was not a single API key")
    }
    .boxed()
}

/// Runs the configured credentials helper and stores the API key it prints.
///
/// Single-flight: `refresh_lock` serializes concurrent refreshes for this provider,
/// and if the stored key already differs from `failed_key` (because another request
/// refreshed it first), the helper is not run again and the current key is returned.
async fn refresh_api_key(
    run_helper: RunCredentialsHelper,
    refresh_lock: &smol::lock::Mutex<()>,
    state: Entity<State>,
    failed_key: Option<Arc<str>>,
    credentials_helper: String,
    cx: &mut AsyncApp,
) -> anyhow::Result<Arc<str>> {
    let _refresh_guard = refresh_lock.lock().await;

    let (current_key, is_from_env_var) = state.read_with(cx, |state, _cx| {
        let api_url = state.settings.api_url();
        (
            state.api_key_state.key(api_url),
            state.api_key_state.is_from_env_var(),
        )
    });

    if is_from_env_var {
        bail!(
            "cannot refresh API key: it is set via environment variable, \
             which the credentials helper cannot update"
        );
    }

    if current_key != failed_key {
        if let Some(current_key) = current_key {
            return Ok(current_key);
        }
    }

    let new_key = run_helper(&credentials_helper).await?;
    let refreshed_key = Arc::from(new_key.as_str());
    state
        .update(cx, |state, cx| state.set_api_key(Some(new_key), cx))
        .await?;
    Ok(refreshed_key)
}

/// Sends one chat-completions request with the given API key.
fn send_chat_completion(
    http_client: Arc<dyn HttpClient>,
    provider: LanguageModelProviderName,
    request_limiter: RateLimiter,
    api_key: Option<Arc<str>>,
    api_url: String,
    extra_headers: CustomHeaders,
    build_request: SharedRequestBuilder<open_ai::Request>,
) -> BoxFuture<
    'static,
    Result<
        futures::stream::BoxStream<
            'static,
            Result<ResponseStreamEvent, LanguageModelCompletionError>,
        >,
        LanguageModelCompletionError,
    >,
> {
    let future = request_limiter.stream(async move {
        let Some(api_key) = api_key else {
            return Err(LanguageModelCompletionError::NoApiKey { provider });
        };
        let request = build_request()?;
        let request = stream_completion(
            http_client.as_ref(),
            provider.0.as_str(),
            &api_url,
            &api_key,
            request,
            &extra_headers,
        );
        let response = request.await?;
        Ok(response)
    });

    async move { Ok(future.await?.boxed()) }.boxed()
}

/// Sends one Responses API request with the given API key.
fn send_response(
    http_client: Arc<dyn HttpClient>,
    provider: LanguageModelProviderName,
    request_limiter: RateLimiter,
    api_key: Option<Arc<str>>,
    api_url: String,
    extra_headers: CustomHeaders,
    build_request: SharedRequestBuilder<ResponseRequest>,
) -> BoxFuture<
    'static,
    Result<
        futures::stream::BoxStream<'static, Result<ResponsesStreamEvent, open_ai::RequestError>>,
        LanguageModelCompletionError,
    >,
> {
    let future = request_limiter.stream(async move {
        let Some(api_key) = api_key else {
            return Err(LanguageModelCompletionError::NoApiKey { provider });
        };
        let request = build_request()?;
        let request = stream_response(
            http_client.as_ref(),
            provider.0.as_str(),
            &api_url,
            &api_key,
            request,
            &extra_headers,
        );
        let response = request.await?;
        Ok(response)
    });

    async move { Ok(future.await?.boxed()) }.boxed()
}

/// Awaits `first_attempt`; if it was rejected as unauthenticated and a credentials
/// helper is configured, refreshes the API key and retries once via `retry`.
/// The first attempt's error is surfaced unchanged when no retry happens or the
/// refresh fails.
async fn retry_with_credentials_refresh<S: Send + 'static>(
    first_attempt: BoxFuture<
        'static,
        Result<futures::stream::BoxStream<'static, S>, LanguageModelCompletionError>,
    >,
    retry: impl FnOnce(
        Arc<str>,
    ) -> BoxFuture<
        'static,
        Result<futures::stream::BoxStream<'static, S>, LanguageModelCompletionError>,
    >,
    run_helper: RunCredentialsHelper,
    refresh_lock: Arc<smol::lock::Mutex<()>>,
    state: Entity<State>,
    failed_key: Option<Arc<str>>,
    credentials_helper: Option<String>,
    cx: &mut AsyncApp,
) -> Result<futures::stream::BoxStream<'static, S>, LanguageModelCompletionError> {
    let stream = match first_attempt.await {
        Ok(stream) => stream,
        Err(error) => {
            let Some(credentials_helper) = credentials_helper else {
                return Err(error);
            };
            if !is_authentication_rejection(&error) {
                return Err(error);
            }
            match refresh_api_key(
                run_helper,
                &refresh_lock,
                state,
                failed_key,
                credentials_helper,
                cx,
            )
            .await
            {
                Ok(new_key) => {
                    log::info!("retrying request with API key refreshed by credentials helper");
                    return retry(new_key).await;
                }
                Err(refresh_error) => {
                    log::error!(
                        "failed to refresh API key with credentials helper: {refresh_error}"
                    );
                    return Err(error);
                }
            }
        }
    };
    Ok(stream)
}

/// Sends a request, refreshing the stored API key with the configured
/// credentials helper and retrying once when the request is rejected as
/// unauthenticated. `send` is called once per attempt with the key to use.
fn send_with_credentials_refresh<S: Send + 'static>(
    state: Entity<State>,
    refresh_lock: Arc<smol::lock::Mutex<()>>,
    send: impl Fn(
        Option<Arc<str>>,
    ) -> BoxFuture<
        'static,
        Result<futures::stream::BoxStream<'static, S>, LanguageModelCompletionError>,
    > + 'static,
    cx: &AsyncApp,
) -> BoxFuture<'static, Result<futures::stream::BoxStream<'static, S>, LanguageModelCompletionError>>
{
    let (api_key, credentials_helper) = state.read_with(cx, |state, _cx| {
        let api_url = state.settings.api_url();
        (
            state.api_key_state.key(api_url),
            state.settings.credentials_helper.clone(),
        )
    });

    let first_attempt = send(api_key.clone());
    let cx = cx.clone();
    let task = cx.spawn(async move |cx| {
        let retry = move |new_key: Arc<str>| send(Some(new_key));
        retry_with_credentials_refresh(
            first_attempt,
            retry,
            run_credentials_helper,
            refresh_lock,
            state,
            api_key,
            credentials_helper,
            cx,
        )
        .await
    });

    task.boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    use credentials_provider::test_support::FakeCredentialsProvider;
    use gpui::TestAppContext;
    use http_client::StatusCode;
    use serde_json::json;

    fn available_model(reasoning_effort: Option<open_ai::ReasoningEffort>) -> AvailableModel {
        AvailableModel {
            name: "custom-model".to_string(),
            display_name: None,
            max_tokens: 128_000,
            max_output_tokens: None,
            max_completion_tokens: None,
            reasoning_effort,
            capabilities: ModelCapabilities {
                chat_completions: false,
                ..Default::default()
            },
        }
    }

    #[test]
    fn configured_reasoning_effort_supports_thinking() {
        assert_eq!(
            default_thinking_reasoning_effort(&available_model(Some(
                open_ai::ReasoningEffort::High
            ))),
            Some(open_ai::ReasoningEffort::High)
        );
    }

    #[test]
    fn missing_or_none_reasoning_effort_does_not_support_thinking() {
        assert_eq!(
            default_thinking_reasoning_effort(&available_model(None)),
            None
        );
        assert_eq!(
            default_thinking_reasoning_effort(&available_model(Some(
                open_ai::ReasoningEffort::None
            ))),
            None
        );
    }

    #[test]
    fn supported_thinking_effort_levels_use_configured_effort_as_default() {
        let effort_levels = supported_thinking_effort_levels(&available_model(Some(
            open_ai::ReasoningEffort::High,
        )));
        let values = effort_levels
            .iter()
            .map(|level| level.value.as_ref())
            .collect::<Vec<_>>();

        assert_eq!(values, ["minimal", "low", "medium", "high", "xhigh", "max"]);
        assert_eq!(
            effort_levels
                .iter()
                .find(|level| level.is_default)
                .map(|level| level.value.as_ref()),
            Some("high")
        );
    }

    #[test]
    fn supported_thinking_effort_levels_hide_missing_or_none_effort() {
        assert!(supported_thinking_effort_levels(&available_model(None)).is_empty());
        assert!(
            supported_thinking_effort_levels(&available_model(Some(
                open_ai::ReasoningEffort::None
            )))
            .is_empty()
        );
    }

    #[test]
    fn chat_completion_reasoning_effort_honors_request_and_configured_effort() {
        let model = available_model(Some(open_ai::ReasoningEffort::Medium));
        let mut request = LanguageModelRequest {
            thinking_allowed: true,
            ..Default::default()
        };

        assert_eq!(
            chat_completion_reasoning_effort(&request, &model),
            Some(open_ai::ReasoningEffort::Medium)
        );

        request.thinking_effort = Some("high".to_string());
        assert_eq!(
            chat_completion_reasoning_effort(&request, &model),
            Some(open_ai::ReasoningEffort::High)
        );

        request.thinking_effort = Some("not-supported".to_string());
        assert_eq!(
            chat_completion_reasoning_effort(&request, &model),
            Some(open_ai::ReasoningEffort::Medium)
        );

        request.thinking_allowed = false;
        assert_eq!(
            chat_completion_reasoning_effort(&request, &model),
            Some(open_ai::ReasoningEffort::None)
        );
    }

    #[test]
    fn chat_completion_reasoning_effort_omits_missing_effort() {
        let model = available_model(None);
        let request = LanguageModelRequest {
            thinking_allowed: false,
            ..Default::default()
        };

        assert_eq!(chat_completion_reasoning_effort(&request, &model), None);
    }

    #[test]
    fn chat_completion_reasoning_effort_preserves_explicit_none() {
        let model = available_model(Some(open_ai::ReasoningEffort::None));
        let request = LanguageModelRequest {
            thinking_allowed: true,
            thinking_effort: Some("high".to_string()),
            ..Default::default()
        };

        assert_eq!(
            chat_completion_reasoning_effort(&request, &model),
            Some(open_ai::ReasoningEffort::None)
        );
    }

    #[test]
    fn chat_completion_max_tokens_parameter_defaults_to_max_completion_tokens() {
        let model = available_model(Some(open_ai::ReasoningEffort::Medium));

        assert_eq!(
            chat_completion_max_tokens_parameter(&model),
            crate::provider::open_ai::ChatCompletionMaxTokensParameter::MaxCompletionTokens
        );
    }

    #[test]
    fn chat_completion_max_tokens_parameter_uses_max_tokens_when_configured() {
        let mut model = available_model(Some(open_ai::ReasoningEffort::Medium));
        model.capabilities.max_tokens_parameter = true;

        assert_eq!(
            chat_completion_max_tokens_parameter(&model),
            crate::provider::open_ai::ChatCompletionMaxTokensParameter::MaxTokens
        );
    }

    #[test]
    fn response_request_includes_reasoning_when_effort_is_configured() {
        let model = available_model(Some(open_ai::ReasoningEffort::High));
        let request = LanguageModelRequest {
            thinking_allowed: true,
            ..Default::default()
        };

        let request = into_open_ai_response(
            request,
            &model.name,
            model.capabilities.parallel_tool_calls,
            model.capabilities.prompt_cache_key,
            model.max_output_tokens,
            default_thinking_reasoning_effort(&model),
            supports_none_reasoning_effort(&model),
            &LanguageModelProviderId::new("test-compatible-provider"),
        )
        .unwrap();
        let serialized = serde_json::to_value(request).unwrap();

        assert_eq!(
            serialized["reasoning"],
            json!({ "effort": "high", "summary": "auto" })
        );
        assert_eq!(
            serialized["include"],
            json!(["reasoning.encrypted_content"])
        );
    }

    #[test]
    fn response_request_omits_reasoning_when_effort_is_missing() {
        let model = available_model(None);
        let request = LanguageModelRequest {
            thinking_allowed: true,
            ..Default::default()
        };

        let request = into_open_ai_response(
            request,
            &model.name,
            model.capabilities.parallel_tool_calls,
            model.capabilities.prompt_cache_key,
            model.max_output_tokens,
            default_thinking_reasoning_effort(&model),
            supports_none_reasoning_effort(&model),
            &LanguageModelProviderId::new("test-compatible-provider"),
        )
        .unwrap();
        let serialized = serde_json::to_value(request).unwrap();

        assert_eq!(serialized.get("reasoning"), None);
        assert_eq!(serialized.get("include"), None);
    }

    #[test]
    fn chat_completion_request_includes_selected_reasoning_effort() {
        let mut model = available_model(Some(open_ai::ReasoningEffort::Medium));
        model.capabilities.chat_completions = true;
        let request = LanguageModelRequest {
            thinking_allowed: true,
            thinking_effort: Some("high".to_string()),
            ..Default::default()
        };
        let reasoning_effort = chat_completion_reasoning_effort(&request, &model);

        let request = into_open_ai(
            request,
            &model.name,
            model.capabilities.parallel_tool_calls,
            model.capabilities.prompt_cache_key,
            model.max_output_tokens,
            chat_completion_max_tokens_parameter(&model),
            reasoning_effort,
            model.capabilities.interleaved_reasoning,
        )
        .unwrap();
        let serialized = serde_json::to_value(request).unwrap();

        assert_eq!(serialized["reasoning_effort"], json!("high"));
    }

    #[test]
    fn configured_reasoning_effort_supports_none_reasoning_effort() {
        assert!(supports_none_reasoning_effort(&available_model(Some(
            open_ai::ReasoningEffort::Medium
        ))));
        assert!(supports_none_reasoning_effort(&available_model(Some(
            open_ai::ReasoningEffort::None
        ))));
        assert!(!supports_none_reasoning_effort(&available_model(None)));
    }

    #[test]
    fn response_thinking_effort_preserves_explicit_none() {
        let model = available_model(Some(open_ai::ReasoningEffort::None));
        let mut request = LanguageModelRequest {
            thinking_allowed: true,
            thinking_effort: Some("high".to_string()),
            ..Default::default()
        };

        disable_response_thinking_for_none_effort(&mut request, &model);
        assert!(!request.thinking_allowed);
        assert_eq!(request.thinking_effort, None);
    }

    #[gpui::test]
    async fn no_retry_without_credentials_helper(cx: &mut TestAppContext) {
        let state = test_helper_state(cx);
        let mut async_cx = cx.update(|cx| cx.to_async());
        let first_attempt =
            futures::future::ready(Err::<futures::stream::BoxStream<'static, i32>, _>(
                LanguageModelCompletionError::NoApiKey {
                    provider: LanguageModelProviderName::new("test-provider"),
                },
            ))
            .boxed();
        let retry = |_new_key: Arc<str>| -> BoxFuture<
            'static,
            Result<futures::stream::BoxStream<'static, i32>, LanguageModelCompletionError>,
        > {
            panic!("retry must not run when no credentials helper is configured");
        };

        let result = retry_with_credentials_refresh(
            first_attempt,
            retry,
            helper_must_not_run,
            Arc::new(smol::lock::Mutex::new(())),
            state,
            Some("old-key".into()),
            None,
            &mut async_cx,
        )
        .await;

        assert!(matches!(
            result,
            Err(LanguageModelCompletionError::NoApiKey { .. })
        ));
    }

    #[gpui::test]
    async fn no_retry_for_non_authentication_errors(cx: &mut TestAppContext) {
        let state = test_helper_state(cx);
        let mut async_cx = cx.update(|cx| cx.to_async());
        let first_attempt = futures::future::ready(Err::<
            futures::stream::BoxStream<'static, i32>,
            _,
        >(provider_rejection(
            StatusCode::TOO_MANY_REQUESTS,
            ProviderErrorCategory::RateLimit,
        )))
        .boxed();
        let retry = |_new_key: Arc<str>| -> BoxFuture<
            'static,
            Result<futures::stream::BoxStream<'static, i32>, LanguageModelCompletionError>,
        > {
            panic!("retry must not run for non-authentication errors");
        };

        let result = retry_with_credentials_refresh(
            first_attempt,
            retry,
            helper_must_not_run,
            Arc::new(smol::lock::Mutex::new(())),
            state,
            Some("old-key".into()),
            Some("printf refreshed-key".to_string()),
            &mut async_cx,
        )
        .await;

        assert!(matches!(
            result,
            Err(LanguageModelCompletionError::ProviderRejection { .. })
        ));
    }

    #[gpui::test]
    async fn refresh_api_key_skips_helper_when_key_already_refreshed(cx: &mut TestAppContext) {
        let credentials = Arc::new(FakeCredentialsProvider::default());
        credentials
            .state
            .lock()
            .keys
            .insert("https://example.com/v1".to_string(), b"new-key".to_vec());
        let state = test_helper_state_with(cx, credentials.clone());
        let authenticate = cx.update(|cx| state.update(cx, |state, cx| state.authenticate(cx)));
        cx.run_until_parked();
        authenticate.await.unwrap();
        assert!(cx.update(|cx| state.read(cx).is_authenticated()));
        let mut async_cx = cx.update(|cx| cx.to_async());

        let refreshed_key = refresh_api_key(
            helper_must_not_run,
            &smol::lock::Mutex::new(()),
            state,
            // The request failed with the old key, but another request already
            // refreshed the stored key, so the helper must not run.
            Some("old-key".into()),
            "printf refreshed-again".to_string(),
            &mut async_cx,
        )
        .await
        .unwrap();

        assert_eq!(refreshed_key.as_ref(), "new-key");
        assert_eq!(
            credentials
                .state
                .lock()
                .keys
                .get("https://example.com/v1")
                .map(|key| key.as_slice()),
            Some(&b"new-key"[..])
        );
    }

    #[gpui::test]
    async fn authentication_failure_runs_helper_and_retries_with_new_key(cx: &mut TestAppContext) {
        let credentials = Arc::new(FakeCredentialsProvider::default());
        credentials
            .state
            .lock()
            .keys
            .insert("https://example.com/v1".to_string(), b"old-key".to_vec());
        let state = test_helper_state_with(cx, credentials.clone());
        let authenticate = cx.update(|cx| state.update(cx, |state, cx| state.authenticate(cx)));
        cx.run_until_parked();
        authenticate.await.unwrap();
        let mut async_cx = cx.update(|cx| cx.to_async());

        let first_attempt = futures::future::ready(Err::<
            futures::stream::BoxStream<'static, i32>,
            _,
        >(provider_rejection(
            StatusCode::UNAUTHORIZED,
            ProviderErrorCategory::Authentication,
        )))
        .boxed();
        let retry_keys = Arc::new(std::sync::Mutex::new(Vec::new()));
        let retry_keys_for_closure = retry_keys.clone();
        let retry = move |new_key: Arc<str>| {
            retry_keys_for_closure
                .lock()
                .unwrap()
                .push(new_key.to_string());
            futures::future::ready(Ok(futures::stream::empty::<i32>().boxed())).boxed()
                as BoxFuture<
                    'static,
                    Result<futures::stream::BoxStream<'static, i32>, LanguageModelCompletionError>,
                >
        };

        let result = retry_with_credentials_refresh(
            first_attempt,
            retry,
            helper_returns_new_key,
            Arc::new(smol::lock::Mutex::new(())),
            state,
            Some("old-key".into()),
            Some("unused".to_string()),
            &mut async_cx,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(*retry_keys.lock().unwrap(), ["new-key"]);
        assert_eq!(
            credentials
                .state
                .lock()
                .keys
                .get("https://example.com/v1")
                .map(|key| key.as_slice()),
            Some(&b"new-key"[..])
        );
    }

    fn helper_must_not_run(_command: &str) -> BoxFuture<'static, Result<String>> {
        panic!("credentials helper must not run in this test");
    }

    fn helper_returns_new_key(_command: &str) -> BoxFuture<'static, Result<String>> {
        futures::future::ready(Ok("new-key".to_string())).boxed()
    }

    fn test_helper_state(cx: &mut TestAppContext) -> Entity<State> {
        test_helper_state_with(cx, Arc::new(FakeCredentialsProvider::default()))
    }

    fn test_helper_state_with(
        cx: &mut TestAppContext,
        credentials: Arc<FakeCredentialsProvider>,
    ) -> Entity<State> {
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            State::new(
                "test-provider".into(),
                credentials,
                test_settings_resolver,
                cx,
            )
        })
    }

    fn test_settings_resolver<'a>(
        _id: &'a str,
        _cx: &'a App,
    ) -> Option<&'a OpenAiCompatibleSettings> {
        static SETTINGS: std::sync::LazyLock<OpenAiCompatibleSettings> =
            std::sync::LazyLock::new(|| OpenAiCompatibleSettings {
                api_url: "https://example.com/v1".to_string(),
                available_models: Vec::new(),
                custom_headers: CustomHeaders::default(),
                credentials_helper: None,
            });
        Some(&SETTINGS)
    }

    fn provider_rejection(
        status: StatusCode,
        category: ProviderErrorCategory,
    ) -> LanguageModelCompletionError {
        LanguageModelCompletionError::ProviderRejection {
            provider: LanguageModelProviderName::new("test-provider"),
            status: Some(status),
            code: None,
            message: "test rejection".to_string(),
            retry_after: None,
            category,
        }
    }

    #[test]
    fn authentication_rejections_are_detected() {
        assert!(is_authentication_rejection(&provider_rejection(
            StatusCode::UNAUTHORIZED,
            ProviderErrorCategory::Authentication,
        )));
    }

    #[test]
    fn other_rejections_are_not_treated_as_authentication() {
        assert!(!is_authentication_rejection(&provider_rejection(
            StatusCode::FORBIDDEN,
            ProviderErrorCategory::Permission,
        )));
        assert!(!is_authentication_rejection(&provider_rejection(
            StatusCode::TOO_MANY_REQUESTS,
            ProviderErrorCategory::RateLimit
        ),));
        assert!(!is_authentication_rejection(
            &LanguageModelCompletionError::NoApiKey {
                provider: LanguageModelProviderName::new("test-provider"),
            },
        ));
    }

    #[test]
    fn helper_output_is_trimmed() {
        assert_eq!(
            parse_helper_output("  new-api-key\n").unwrap(),
            "new-api-key"
        );
    }

    #[test]
    fn empty_helper_output_is_rejected() {
        assert!(parse_helper_output("").is_err());
        assert!(parse_helper_output("   \n  ").is_err());
    }

    #[test]
    fn multi_line_helper_output_is_rejected() {
        assert!(parse_helper_output("first-key\nsecond-key").is_err());
    }

    #[test]
    fn credentials_helper_stdout_becomes_api_key() {
        let key = smol::block_on(run_credentials_helper("printf '  new-api-key  '")).unwrap();
        assert_eq!(key, "new-api-key");
    }

    #[test]
    fn credentials_helper_non_zero_exit_is_an_error() {
        let error =
            smol::block_on(run_credentials_helper("printf 'boom' >&2; exit 1")).unwrap_err();
        assert!(
            error.to_string().contains("status"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn credentials_helper_multi_line_stdout_is_an_error() {
        let error =
            smol::block_on(run_credentials_helper("printf 'key-one\\nkey-two\\n'")).unwrap_err();
        assert!(
            error.to_string().contains("single API key"),
            "unexpected error: {error}"
        );
    }
}
