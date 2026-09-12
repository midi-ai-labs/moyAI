use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::config::{AccessMode, ProviderEndpoint, ProviderProfile, SideChatConfig};
use crate::error::StorageError;
use crate::protocol::{ContentPart, HistoryItemId, HistoryItemPayload, RuntimeEventMsg};
use crate::runtime::{Clock, SystemClock};
use crate::session::{NewSession, SessionId, SessionProviderConnection, SessionStatus};
use crate::system_prompt::normalize_user_configured_system_prompt;

use super::session_repo::{
    AdmittedTurnSnapshot, DurableSessionStopState, SqliteSessionRepository, delete_session_rows,
    insert_session_in_transaction, session_record_from_connection,
    session_stop_state_from_connection,
};

pub const MAX_SIDE_CHAT_DRAFT_BYTES: usize = 1024 * 1024;
pub const MAX_SIDE_CHAT_PROJECTION_MESSAGES: usize = 100;

// V55/V60 require a positive value in the retired column. It is preserved for
// old database compatibility only and is never projected into a request.
const RETIRED_MAX_OUTPUT_TOKENS_PLACEHOLDER: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SideChatId(pub Ulid);

impl SideChatId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for SideChatId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for SideChatId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for SideChatId {
    type Err = ulid::DecodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(value)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideChatContextScope {
    General,
}

impl SideChatContextScope {
    const fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
        }
    }

    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "general" => Ok(Self::General),
            _ => Err(StorageError::Message(format!(
                "side chat binding has invalid context scope `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideChatRouteKind {
    #[default]
    Direct,
    Hub,
}

impl SideChatRouteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Hub => "hub",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideChatProviderTarget {
    #[serde(default)]
    pub route_kind: SideChatRouteKind,
    pub base_url: String,
    pub model: String,
    pub provider_profile: ProviderProfile,
    pub system_prompt: String,
    pub context_window: u32,
    pub request_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub max_retries: u8,
    pub supports_images: bool,
    pub supports_tools: bool,
}

impl TryFrom<&SideChatConfig> for SideChatProviderTarget {
    type Error = StorageError;

    fn try_from(config: &SideChatConfig) -> Result<Self, Self::Error> {
        Self {
            route_kind: SideChatRouteKind::Direct,
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            provider_profile: config.provider_profile,
            system_prompt: config.system_prompt.clone(),
            context_window: config.context_window,
            request_timeout_ms: config.request_timeout_ms,
            connect_timeout_ms: config.connect_timeout_ms,
            max_retries: config.max_retries,
            // Side Chat is deliberately text-only and tool-less. Capability policy is not a
            // configurable provider target field, while old bindings retain their full snapshot.
            supports_images: false,
            supports_tools: false,
        }
        .validate()
    }
}

impl SideChatProviderTarget {
    pub fn validate(mut self) -> Result<Self, StorageError> {
        self.base_url = ProviderEndpoint::parse(&self.base_url)
            .map_err(|error| StorageError::Message(error.to_string()))?
            .as_str()
            .to_string();
        self.model = self.model.trim().to_string();
        if self.model.is_empty() {
            return Err(StorageError::Message(
                "side chat model must not be empty".to_string(),
            ));
        }
        self.system_prompt = normalize_side_chat_system_prompt(&self.system_prompt)?;
        if self.context_window == 0 {
            return Err(StorageError::Message(
                "side chat context limit must be positive".to_string(),
            ));
        }
        if self.request_timeout_ms == 0 || self.request_timeout_ms > i64::MAX as u64 {
            return Err(StorageError::Message(
                "side chat request timeout must fit a positive SQLite integer".to_string(),
            ));
        }
        if self.connect_timeout_ms == 0 || self.connect_timeout_ms > i64::MAX as u64 {
            return Err(StorageError::Message(
                "side chat connect timeout must fit a positive SQLite integer".to_string(),
            ));
        }
        Ok(self)
    }
}

impl Debug for SideChatProviderTarget {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SideChatProviderTarget")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("provider_profile", &self.provider_profile)
            .field("system_prompt_chars", &self.system_prompt.chars().count())
            .field("context_window", &self.context_window)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("supports_images", &self.supports_images)
            .field("supports_tools", &self.supports_tools)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideChatBinding {
    pub route_kind: SideChatRouteKind,
    pub id: SideChatId,
    pub owner_session_id: SessionId,
    pub conversation_session_id: SessionId,
    pub base_url: String,
    pub model: String,
    pub provider_profile: ProviderProfile,
    pub system_prompt: String,
    pub context_window: u32,
    /// Retired V55/V60 compatibility column. Runtime requests ignore it.
    pub max_output_tokens: u32,
    pub request_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub max_retries: u8,
    pub supports_images: bool,
    pub supports_tools: bool,
    /// Retired V55/V60 compatibility column. Runtime requests ignore it.
    pub supports_reasoning: bool,
    pub persisted_draft: String,
    pub draft_revision: u64,
    pub request_generation: u64,
    pub delete_requested_at_ms: Option<i64>,
    pub context_scope: SideChatContextScope,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl SideChatBinding {
    pub fn provider_target(&self) -> SideChatProviderTarget {
        SideChatProviderTarget {
            route_kind: self.route_kind,
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            provider_profile: self.provider_profile,
            system_prompt: self.system_prompt.clone(),
            context_window: self.context_window,
            request_timeout_ms: self.request_timeout_ms,
            connect_timeout_ms: self.connect_timeout_ms,
            max_retries: self.max_retries,
            supports_images: self.supports_images,
            supports_tools: self.supports_tools,
        }
    }
}

impl Debug for SideChatBinding {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SideChatBinding")
            .field("id", &self.id)
            .field("owner_session_id", &self.owner_session_id)
            .field("conversation_session_id", &self.conversation_session_id)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("provider_profile", &self.provider_profile)
            .field("system_prompt_chars", &self.system_prompt.chars().count())
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("supports_images", &self.supports_images)
            .field("supports_tools", &self.supports_tools)
            .field("supports_reasoning", &self.supports_reasoning)
            .field("persisted_draft", &self.persisted_draft)
            .field("draft_revision", &self.draft_revision)
            .field("request_generation", &self.request_generation)
            .field("delete_requested_at_ms", &self.delete_requested_at_ms)
            .field("context_scope", &self.context_scope)
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideChatDraftUpdate {
    pub binding: SideChatBinding,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SideChatRequestClaim {
    pub generation: u64,
    pub binding: SideChatBinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideChatAdmittedRequest {
    pub generation: u64,
    pub binding: SideChatBinding,
    pub admission: AdmittedTurnSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideChatConversationRole {
    User,
    Assistant,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideChatConversationMessage {
    pub id: HistoryItemId,
    pub sequence_no: i64,
    pub role: SideChatConversationRole,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SideChatConversationProjection {
    pub binding: SideChatBinding,
    pub status: SessionStatus,
    pub last_error: Option<String>,
    pub messages: Vec<SideChatConversationMessage>,
}

#[derive(Clone)]
pub struct SqliteSideChatRepository {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteSideChatRepository {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }

    #[cfg(test)]
    pub(crate) fn configure(
        &self,
        owner_session_id: SessionId,
        target: SideChatProviderTarget,
    ) -> Result<SideChatBinding, StorageError> {
        self.write_provider_target_at(owner_session_id, target, SystemClock.now_ms(), true)
    }

    /// Creates a Side Chat from the supplied defaults only when the owner does
    /// not already have one. An existing binding is returned byte-for-byte so
    /// opening the pane cannot silently reconfigure a durable conversation.
    pub fn ensure(
        &self,
        owner_session_id: SessionId,
        target: SideChatProviderTarget,
    ) -> Result<SideChatBinding, StorageError> {
        self.write_provider_target_at(owner_session_id, target, SystemClock.now_ms(), false)
    }

    #[cfg(test)]
    fn configure_at(
        &self,
        owner_session_id: SessionId,
        target: SideChatProviderTarget,
        now_ms: i64,
    ) -> Result<SideChatBinding, StorageError> {
        self.write_provider_target_at(owner_session_id, target, now_ms, true)
    }

    #[cfg(test)]
    fn ensure_at(
        &self,
        owner_session_id: SessionId,
        target: SideChatProviderTarget,
        now_ms: i64,
    ) -> Result<SideChatBinding, StorageError> {
        self.write_provider_target_at(owner_session_id, target, now_ms, false)
    }

    fn write_provider_target_at(
        &self,
        owner_session_id: SessionId,
        target: SideChatProviderTarget,
        now_ms: i64,
        replace_existing: bool,
    ) -> Result<SideChatBinding, StorageError> {
        validate_timestamp(now_ms)?;
        let target = target.validate()?;
        let provider_connection = SessionProviderConnection {
            profile: target.provider_profile,
            api_key_env: None,
            extra_headers: Default::default(),
        };
        // Provider connection snapshots are deliberately not generally serializable: their
        // custom headers may contain credentials. This is the narrow durable-storage projection.
        let provider_connection_json = serde_json::to_string(&serde_json::json!({
            "profile": provider_connection.profile,
            "api_key_env": provider_connection.api_key_env,
            "extra_headers": provider_connection.extra_headers,
        }))?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = binding_for_owner(&transaction, owner_session_id)?;
        let side_chat_id = if let Some(existing) = existing {
            ensure_delete_not_requested(&existing)?;
            if replace_existing {
                ensure_conversation_not_running(&transaction, existing.conversation_session_id)?;
                transaction.execute(
                    "UPDATE sessions
                     SET model_name = ?2,
                         base_url = ?3,
                         provider_connection_json = ?4,
                         updated_at_ms = MAX(updated_at_ms, ?5)
                     WHERE id = ?1",
                    params![
                        existing.conversation_session_id.to_string(),
                        target.model.as_str(),
                        target.base_url.as_str(),
                        provider_connection_json.as_str(),
                        now_ms,
                    ],
                )?;
                let updated = transaction.execute(
                    "UPDATE side_chat_bindings
                     SET base_url = ?3,
                         model = ?4,
                         provider_profile = ?5,
                         system_prompt = ?6,
                         context_window = ?7,
                         request_timeout_ms = ?8,
                         connect_timeout_ms = ?9,
                         max_retries = ?10,
                         supports_images = ?11,
                         supports_tools = ?12,
                         updated_at_ms = MAX(updated_at_ms, ?13)
                     WHERE id = ?1 AND owner_session_id = ?2",
                    params![
                        existing.id.to_string(),
                        owner_session_id.to_string(),
                        target.base_url.as_str(),
                        target.model.as_str(),
                        target.provider_profile.as_str(),
                        target.system_prompt.as_str(),
                        i64::from(target.context_window),
                        target.request_timeout_ms as i64,
                        target.connect_timeout_ms as i64,
                        i64::from(target.max_retries),
                        target.supports_images,
                        target.supports_tools,
                        now_ms,
                    ],
                )?;
                if updated != 1 {
                    return Err(StorageError::Message(format!(
                        "side chat {} changed while its provider was being configured",
                        existing.id
                    )));
                }
            }
            existing.id
        } else {
            let owner = session_record_from_connection(&transaction, owner_session_id)?;
            let owner_is_hidden = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM side_chat_bindings
                     WHERE conversation_session_id = ?1
                 )",
                [owner_session_id.to_string()],
                |row| row.get::<_, bool>(0),
            )?;
            if owner_is_hidden {
                return Err(StorageError::Message(format!(
                    "hidden side chat conversation {owner_session_id} cannot own another side chat"
                )));
            }
            let conversation_session_id = SessionId::new();
            let draft = NewSession {
                project_id: owner.project_id,
                title: "Side chat".to_string(),
                cwd: owner.cwd,
                model: target.model.clone(),
                base_url: target.base_url.clone(),
                access_mode: AccessMode::Default,
                provider_connection: Some(provider_connection.clone()),
            };
            insert_session_in_transaction(&transaction, conversation_session_id, &draft, now_ms)?;
            let side_chat_id = SideChatId::new();
            transaction.execute(
                "INSERT INTO side_chat_bindings (
                     id, owner_session_id, conversation_session_id,
                     base_url, model, provider_profile, system_prompt,
                     context_window, max_output_tokens,
                     request_timeout_ms, connect_timeout_ms, max_retries,
                     supports_images, supports_tools, supports_reasoning,
                     persisted_draft, draft_revision, request_generation,
                     delete_requested_at_ms, context_scope,
                     created_at_ms, updated_at_ms, provider_route_kind
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     ?13, ?14, ?15, '', 0, 0, NULL, ?16, ?17, ?17, ?18
                 )",
                params![
                    side_chat_id.to_string(),
                    owner_session_id.to_string(),
                    conversation_session_id.to_string(),
                    target.base_url.as_str(),
                    target.model.as_str(),
                    target.provider_profile.as_str(),
                    target.system_prompt.as_str(),
                    i64::from(target.context_window),
                    i64::from(RETIRED_MAX_OUTPUT_TOKENS_PLACEHOLDER),
                    target.request_timeout_ms as i64,
                    target.connect_timeout_ms as i64,
                    i64::from(target.max_retries),
                    target.supports_images,
                    target.supports_tools,
                    false,
                    SideChatContextScope::General.as_str(),
                    now_ms,
                    target.route_kind.as_str(),
                ],
            )?;
            side_chat_id
        };
        let binding = binding_for_owner(&transaction, owner_session_id)?.ok_or_else(|| {
            StorageError::Message(format!(
                "side chat {side_chat_id} disappeared while configuring its provider"
            ))
        })?;
        transaction.commit()?;
        Ok(binding)
    }

    pub fn get_by_owner(
        &self,
        owner_session_id: SessionId,
    ) -> Result<Option<SideChatBinding>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        binding_for_owner(&connection, owner_session_id)
    }

    /// Captures the first Direct target for a Hub-origin conversation. The
    /// canonical session/history and existing Side policy remain the same owner.
    pub fn capture_direct_provider(
        &self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
        expected_draft_revision: u64,
        direct: &SideChatConfig,
    ) -> Result<SideChatBinding, StorageError> {
        let direct = SideChatProviderTarget::try_from(direct)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = binding_for_owner(&transaction, owner_session_id)?
            .ok_or_else(|| StorageError::Message("side chat no longer exists".into()))?;
        if existing.id != side_chat_id
            || existing.request_generation != expected_generation
            || existing.draft_revision != expected_draft_revision
            || existing.route_kind != SideChatRouteKind::Hub
        {
            return Err(StorageError::Message(
                "side chat Direct capture target changed".into(),
            ));
        }
        ensure_delete_not_requested(&existing)?;
        ensure_conversation_not_running(&transaction, existing.conversation_session_id)?;
        let now_ms = SystemClock.now_ms();
        let provider_json = serde_json::to_string(&serde_json::json!({
            "profile": direct.provider_profile, "api_key_env": null, "extra_headers": {},
        }))?;
        transaction.execute(
            "UPDATE sessions SET model_name=?2, base_url=?3, provider_connection_json=?4,
             updated_at_ms=MAX(updated_at_ms,?5) WHERE id=?1",
            params![
                existing.conversation_session_id.to_string(),
                direct.model,
                direct.base_url,
                provider_json,
                now_ms
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE side_chat_bindings SET model=?2,base_url=?3,provider_profile=?4,
             provider_route_kind='direct',updated_at_ms=MAX(updated_at_ms,?5)
             WHERE id=?1 AND provider_route_kind='hub' AND request_generation=?6 AND draft_revision=?7",
            params![existing.id.to_string(), direct.model, direct.base_url, direct.provider_profile.as_str(), now_ms,
                sqlite_u64(expected_generation, "side chat generation")?, sqlite_u64(expected_draft_revision, "side chat draft revision")?],
        )?;
        if changed != 1 {
            return Err(StorageError::Message(
                "side chat Direct capture target changed".into(),
            ));
        }
        let binding = binding_for_owner(&transaction, owner_session_id)?
            .ok_or_else(|| StorageError::Message("side chat no longer exists".into()))?;
        transaction.commit()?;
        Ok(binding)
    }

    pub fn get_by_conversation(
        &self,
        conversation_session_id: SessionId,
    ) -> Result<Option<SideChatBinding>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        binding_for_conversation(&connection, conversation_session_id)
    }

    pub fn conversation_projection(
        &self,
        owner_session_id: SessionId,
    ) -> Result<Option<SideChatConversationProjection>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let Some(binding) = binding_for_owner(&connection, owner_session_id)? else {
            return Ok(None);
        };
        let conversation =
            session_record_from_connection(&connection, binding.conversation_session_id)?;
        let messages = conversation_messages(
            &connection,
            binding.conversation_session_id,
            MAX_SIDE_CHAT_PROJECTION_MESSAGES,
        )?;
        let last_error = canonical_terminal_error(
            &connection,
            binding.conversation_session_id,
            conversation.status,
        )?;
        Ok(Some(SideChatConversationProjection {
            binding,
            status: conversation.status,
            last_error,
            messages,
        }))
    }

    pub fn update_draft(
        &self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_revision: u64,
        draft: impl Into<String>,
    ) -> Result<SideChatDraftUpdate, StorageError> {
        self.update_draft_at(
            owner_session_id,
            side_chat_id,
            expected_revision,
            draft.into(),
            SystemClock.now_ms(),
        )
    }

    fn update_draft_at(
        &self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_revision: u64,
        draft: String,
        now_ms: i64,
    ) -> Result<SideChatDraftUpdate, StorageError> {
        validate_timestamp(now_ms)?;
        if draft.len() > MAX_SIDE_CHAT_DRAFT_BYTES {
            return Err(StorageError::Message(format!(
                "side chat draft exceeds the {MAX_SIDE_CHAT_DRAFT_BYTES}-byte limit"
            )));
        }
        let expected_revision = sqlite_u64(expected_revision, "side chat draft revision")?;
        if expected_revision == i64::MAX {
            return Err(StorageError::Message(
                "side chat draft revision is exhausted".to_string(),
            ));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = binding_for_owner(&transaction, owner_session_id)?.ok_or_else(|| {
            StorageError::Message(format!("owner session {owner_session_id} has no side chat"))
        })?;
        if current.id != side_chat_id {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} is not owned by session {owner_session_id}"
            )));
        }
        ensure_delete_not_requested(&current)?;
        if current.draft_revision != expected_revision as u64 {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} draft revision changed: expected {}, current {}",
                expected_revision, current.draft_revision
            )));
        }
        if current.persisted_draft == draft {
            transaction.commit()?;
            return Ok(SideChatDraftUpdate {
                binding: current,
                changed: false,
            });
        }
        let updated = transaction.execute(
            "UPDATE side_chat_bindings
             SET persisted_draft = ?4,
                 draft_revision = draft_revision + 1,
                 updated_at_ms = MAX(updated_at_ms, ?5)
             WHERE owner_session_id = ?1
               AND id = ?2
               AND draft_revision = ?3
               AND delete_requested_at_ms IS NULL",
            params![
                owner_session_id.to_string(),
                side_chat_id.to_string(),
                expected_revision,
                draft,
                now_ms,
            ],
        )?;
        if updated != 1 {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} draft revision changed during update"
            )));
        }
        let binding = binding_for_owner(&transaction, owner_session_id)?.ok_or_else(|| {
            StorageError::Message(format!(
                "side chat {side_chat_id} disappeared after draft update"
            ))
        })?;
        transaction.commit()?;
        Ok(SideChatDraftUpdate {
            binding,
            changed: true,
        })
    }

    pub async fn claim_and_admit_request(
        &self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
        expected_draft_revision: u64,
        expected_provider_target: SideChatProviderTarget,
        turn_id: crate::protocol::TurnId,
        initial_user_turn: &crate::protocol::UserTurn,
    ) -> Result<SideChatAdmittedRequest, StorageError> {
        let expected_generation = sqlite_u64(expected_generation, "side chat request generation")?;
        if expected_generation == i64::MAX {
            return Err(StorageError::Message(
                "side chat request generation is exhausted".to_string(),
            ));
        }
        let expected_draft_revision =
            sqlite_u64(expected_draft_revision, "side chat draft revision")?;
        if expected_draft_revision == i64::MAX {
            return Err(StorageError::Message(
                "side chat draft revision is exhausted".to_string(),
            ));
        }
        let expected_provider_target = expected_provider_target.validate()?;
        let observed = self.get_by_owner(owner_session_id)?.ok_or_else(|| {
            StorageError::Message(format!("owner session {owner_session_id} has no side chat"))
        })?;
        self.claim_and_admit_observed_request(
            observed,
            owner_session_id,
            side_chat_id,
            expected_generation,
            expected_draft_revision,
            expected_provider_target,
            turn_id,
            initial_user_turn,
        )
        .await
    }

    async fn claim_and_admit_observed_request(
        &self,
        observed: SideChatBinding,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: i64,
        expected_draft_revision: i64,
        expected_provider_target: SideChatProviderTarget,
        turn_id: crate::protocol::TurnId,
        initial_user_turn: &crate::protocol::UserTurn,
    ) -> Result<SideChatAdmittedRequest, StorageError> {
        if observed.id != side_chat_id {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} is not owned by session {owner_session_id}"
            )));
        }
        if observed.request_generation != expected_generation as u64 {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} request generation changed: expected {expected_generation}, current {}",
                observed.request_generation
            )));
        }
        if observed.draft_revision != expected_draft_revision as u64 {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} draft revision changed: expected {expected_draft_revision}, current {}",
                observed.draft_revision
            )));
        }
        if observed.provider_target() != expected_provider_target {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} provider target changed before request admission"
            )));
        }
        ensure_delete_not_requested(&observed)?;
        let conversation_session_id = observed.conversation_session_id;
        let now_ms = SystemClock.now_ms();
        validate_timestamp(now_ms)?;
        let session_repository = SqliteSessionRepository::new(self.connection.clone());
        let committed = session_repository
            .admit_session_turn_with_initial_user_turn_and_transaction_commit(
                conversation_session_id,
                turn_id,
                initial_user_turn,
                move |transaction| {
                    advance_request_generation_in_transaction(
                        transaction,
                        owner_session_id,
                        side_chat_id,
                        conversation_session_id,
                        expected_generation,
                        expected_draft_revision,
                        &expected_provider_target,
                        now_ms,
                    )
                },
            )
            .await?;
        let Some((admission, claim)) = committed else {
            return Err(StorageError::Message(format!(
                "side chat conversation {conversation_session_id} could not admit request generation {}",
                expected_generation + 1
            )));
        };
        Ok(SideChatAdmittedRequest {
            generation: claim.generation,
            binding: claim.binding,
            admission,
        })
    }

    pub fn request_delete(
        &self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
    ) -> Result<SideChatBinding, StorageError> {
        self.request_delete_at(
            owner_session_id,
            side_chat_id,
            expected_generation,
            SystemClock.now_ms(),
        )
    }

    fn request_delete_at(
        &self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
        now_ms: i64,
    ) -> Result<SideChatBinding, StorageError> {
        validate_timestamp(now_ms)?;
        let expected_generation = sqlite_u64(expected_generation, "side chat request generation")?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = binding_for_owner(&transaction, owner_session_id)?.ok_or_else(|| {
            StorageError::Message(format!("owner session {owner_session_id} has no side chat"))
        })?;
        if current.id != side_chat_id {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} is not owned by session {owner_session_id}"
            )));
        }
        if current.request_generation != expected_generation as u64 {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} request generation changed: expected {expected_generation}, current {}",
                current.request_generation
            )));
        }
        if current.delete_requested_at_ms.is_some() {
            transaction.commit()?;
            return Ok(current);
        }
        let updated = transaction.execute(
            "UPDATE side_chat_bindings
             SET delete_requested_at_ms = ?4,
                 updated_at_ms = MAX(updated_at_ms, ?4)
             WHERE owner_session_id = ?1
               AND id = ?2
               AND request_generation = ?3
               AND delete_requested_at_ms IS NULL",
            params![
                owner_session_id.to_string(),
                side_chat_id.to_string(),
                expected_generation,
                now_ms,
            ],
        )?;
        if updated != 1 {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} changed while deletion was being requested"
            )));
        }
        let binding = binding_for_owner(&transaction, owner_session_id)?.ok_or_else(|| {
            StorageError::Message(format!(
                "side chat {side_chat_id} disappeared after deletion was requested"
            ))
        })?;
        if binding.delete_requested_at_ms.is_none() {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} did not retain its deletion request"
            )));
        }
        transaction.commit()?;
        Ok(binding)
    }

    pub fn finalize_pending_deletions(&self) -> Result<usize, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending = {
            let sql = format!(
                "{BINDING_SELECT}
                 WHERE delete_requested_at_ms IS NOT NULL
                 ORDER BY delete_requested_at_ms, id"
            );
            let mut statement = transaction.prepare(&sql)?;
            statement
                .query_map([], binding_columns)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(decode_binding)
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut finalized = 0;
        for binding in pending {
            match session_stop_state_from_connection(&transaction, binding.conversation_session_id)?
            {
                Some(DurableSessionStopState::Running(_)) => continue,
                Some(DurableSessionStopState::Idle | DurableSessionStopState::Terminal(_)) => {}
                None => {
                    return Err(StorageError::Message(format!(
                        "side chat {} owns missing conversation {}",
                        binding.id, binding.conversation_session_id
                    )));
                }
            }
            let deleted = transaction.execute(
                "DELETE FROM side_chat_bindings
                 WHERE id = ?1
                   AND owner_session_id = ?2
                   AND conversation_session_id = ?3
                   AND delete_requested_at_ms IS NOT NULL",
                params![
                    binding.id.to_string(),
                    binding.owner_session_id.to_string(),
                    binding.conversation_session_id.to_string(),
                ],
            )?;
            if deleted != 1 {
                return Err(StorageError::Message(format!(
                    "side chat {} changed while pending deletion was being finalized",
                    binding.id
                )));
            }
            delete_session_rows(&transaction, binding.conversation_session_id)?;
            finalized += 1;
        }
        transaction.commit()?;
        Ok(finalized)
    }

    pub fn delete_if_idle(
        &self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let binding = binding_for_owner(&transaction, owner_session_id)?.ok_or_else(|| {
            StorageError::Message(format!("owner session {owner_session_id} has no side chat"))
        })?;
        if binding.id != side_chat_id {
            return Err(StorageError::Message(format!(
                "side chat {side_chat_id} is not owned by session {owner_session_id}"
            )));
        }
        ensure_conversation_not_running(&transaction, binding.conversation_session_id)?;
        transaction.execute(
            "DELETE FROM side_chat_bindings
             WHERE id = ?1 AND owner_session_id = ?2 AND conversation_session_id = ?3",
            params![
                side_chat_id.to_string(),
                owner_session_id.to_string(),
                binding.conversation_session_id.to_string(),
            ],
        )?;
        delete_session_rows(&transaction, binding.conversation_session_id)?;
        transaction.commit()?;
        Ok(())
    }
}

fn ensure_conversation_not_running(
    connection: &Connection,
    conversation_session_id: SessionId,
) -> Result<(), StorageError> {
    match session_stop_state_from_connection(connection, conversation_session_id)? {
        Some(DurableSessionStopState::Running(_)) => Err(StorageError::Message(format!(
            "side chat conversation {conversation_session_id} is running"
        ))),
        Some(DurableSessionStopState::Idle | DurableSessionStopState::Terminal(_)) => Ok(()),
        None => Err(StorageError::Message(format!(
            "side chat conversation {conversation_session_id} does not exist"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn advance_request_generation_in_transaction(
    connection: &Connection,
    owner_session_id: SessionId,
    side_chat_id: SideChatId,
    conversation_session_id: SessionId,
    expected_generation: i64,
    expected_draft_revision: i64,
    expected_provider_target: &SideChatProviderTarget,
    now_ms: i64,
) -> Result<SideChatRequestClaim, StorageError> {
    let current = binding_for_owner(connection, owner_session_id)?.ok_or_else(|| {
        StorageError::Message(format!("owner session {owner_session_id} has no side chat"))
    })?;
    if current.id != side_chat_id || current.conversation_session_id != conversation_session_id {
        return Err(StorageError::Message(format!(
            "side chat {side_chat_id} is not the current binding for owner session {owner_session_id}"
        )));
    }
    if current.request_generation != expected_generation as u64 {
        return Err(StorageError::Message(format!(
            "side chat {side_chat_id} request generation changed: expected {expected_generation}, current {}",
            current.request_generation
        )));
    }
    if current.draft_revision != expected_draft_revision as u64 {
        return Err(StorageError::Message(format!(
            "side chat {side_chat_id} draft revision changed: expected {expected_draft_revision}, current {}",
            current.draft_revision
        )));
    }
    if current.provider_target() != *expected_provider_target {
        return Err(StorageError::Message(format!(
            "side chat {side_chat_id} provider target changed before request admission"
        )));
    }
    ensure_delete_not_requested(&current)?;
    if current.draft_revision == i64::MAX as u64 {
        return Err(StorageError::Message(
            "side chat draft revision is exhausted".to_string(),
        ));
    }
    let updated = connection.execute(
        "UPDATE side_chat_bindings
         SET request_generation = request_generation + 1,
             persisted_draft = '',
             draft_revision = draft_revision + 1,
             updated_at_ms = MAX(updated_at_ms, ?6)
         WHERE owner_session_id = ?1
           AND id = ?2
           AND conversation_session_id = ?3
           AND request_generation = ?4
           AND draft_revision = ?5
           AND delete_requested_at_ms IS NULL",
        params![
            owner_session_id.to_string(),
            side_chat_id.to_string(),
            conversation_session_id.to_string(),
            expected_generation,
            expected_draft_revision,
            now_ms,
        ],
    )?;
    if updated != 1 {
        let observed = binding_for_owner(connection, owner_session_id)?
            .map(|binding| binding.request_generation.to_string())
            .unwrap_or_else(|| "missing".to_string());
        return Err(StorageError::Message(format!(
            "side chat {side_chat_id} request generation changed: expected {expected_generation}, current {observed}"
        )));
    }
    let binding = binding_for_owner(connection, owner_session_id)?.ok_or_else(|| {
        StorageError::Message(format!(
            "side chat {side_chat_id} disappeared after request claim"
        ))
    })?;
    Ok(SideChatRequestClaim {
        generation: binding.request_generation,
        binding,
    })
}

type BindingColumns = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
    bool,
    bool,
    bool,
    String,
    i64,
    i64,
    Option<i64>,
    String,
    i64,
    i64,
    String,
);

fn binding_columns(row: &Row<'_>) -> rusqlite::Result<BindingColumns> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
        row.get(20)?,
        row.get(21)?,
        row.get(22)?,
    ))
}

const BINDING_SELECT: &str = "SELECT id, owner_session_id, conversation_session_id,
            base_url, model, provider_profile, system_prompt,
            context_window, max_output_tokens,
            request_timeout_ms, connect_timeout_ms, max_retries,
            supports_images, supports_tools, supports_reasoning,
            persisted_draft, draft_revision, request_generation,
            delete_requested_at_ms, context_scope,
            created_at_ms, updated_at_ms, provider_route_kind
     FROM side_chat_bindings";

fn binding_for_owner(
    connection: &Connection,
    owner_session_id: SessionId,
) -> Result<Option<SideChatBinding>, StorageError> {
    let sql = format!("{BINDING_SELECT} WHERE owner_session_id = ?1");
    let raw = connection
        .query_row(&sql, [owner_session_id.to_string()], binding_columns)
        .optional()?;
    raw.map(decode_binding).transpose()
}

fn binding_for_conversation(
    connection: &Connection,
    conversation_session_id: SessionId,
) -> Result<Option<SideChatBinding>, StorageError> {
    let sql = format!("{BINDING_SELECT} WHERE conversation_session_id = ?1");
    let raw = connection
        .query_row(&sql, [conversation_session_id.to_string()], binding_columns)
        .optional()?;
    raw.map(decode_binding).transpose()
}

fn decode_binding(raw: BindingColumns) -> Result<SideChatBinding, StorageError> {
    Ok(SideChatBinding {
        route_kind: match raw.22.as_str() {
            "direct" => SideChatRouteKind::Direct,
            "hub" => SideChatRouteKind::Hub,
            _ => {
                return Err(StorageError::Message(
                    "side chat has an invalid route kind".into(),
                ));
            }
        },
        id: raw.0.parse().map_err(|error| {
            StorageError::Message(format!(
                "side chat binding has invalid id `{}`: {error}",
                raw.0
            ))
        })?,
        owner_session_id: parse_session_id(&raw.1, "owner")?,
        conversation_session_id: parse_session_id(&raw.2, "conversation")?,
        base_url: raw.3,
        model: raw.4,
        provider_profile: ProviderProfile::parse(&raw.5).ok_or_else(|| {
            StorageError::Message(format!(
                "side chat binding has invalid provider profile `{}`",
                raw.5
            ))
        })?,
        system_prompt: decode_side_chat_system_prompt(raw.6)?,
        context_window: parse_positive_u32(raw.7, "context window")?,
        max_output_tokens: parse_positive_u32(raw.8, "max output tokens")?,
        request_timeout_ms: parse_positive_u64(raw.9, "request timeout")?,
        connect_timeout_ms: parse_positive_u64(raw.10, "connect timeout")?,
        max_retries: u8::try_from(raw.11).map_err(|_| {
            StorageError::Message(format!(
                "side chat binding has invalid max retries `{}`",
                raw.11
            ))
        })?,
        supports_images: raw.12,
        supports_tools: raw.13,
        supports_reasoning: raw.14,
        persisted_draft: raw.15,
        draft_revision: parse_u64(raw.16, "draft revision")?,
        request_generation: parse_u64(raw.17, "request generation")?,
        delete_requested_at_ms: parse_optional_timestamp(raw.18, "delete request timestamp")?,
        context_scope: SideChatContextScope::parse(&raw.19)?,
        created_at_ms: raw.20,
        updated_at_ms: raw.21,
    })
}

fn normalize_side_chat_system_prompt(value: &str) -> Result<String, StorageError> {
    normalize_user_configured_system_prompt(Some(value))
        .map(Option::unwrap_or_default)
        .map_err(|message| StorageError::Message(format!("side chat system prompt {message}")))
}

fn decode_side_chat_system_prompt(value: String) -> Result<String, StorageError> {
    let normalized = normalize_side_chat_system_prompt(&value)?;
    if normalized != value {
        return Err(StorageError::Message(
            "side chat binding has a non-canonical system prompt boundary".to_string(),
        ));
    }
    Ok(value)
}

fn conversation_messages(
    connection: &Connection,
    conversation_session_id: SessionId,
    limit: usize,
) -> Result<Vec<SideChatConversationMessage>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT history.id, append_order.append_position,
                history.payload_json, history.payload_sha256
         FROM protocol_history_items AS history
         INNER JOIN protocol_item_append_order AS append_order
           ON append_order.session_id = history.session_id
          AND append_order.source_kind = 'history_item'
          AND append_order.source_id = history.id
         WHERE history.session_id = ?1
           AND json_extract(history.payload_json, '$.kind') IN (
               'user_turn', 'assistant_message', 'error'
           )
         ORDER BY append_order.append_position DESC
         LIMIT ?2",
    )?;
    let rows = statement
        .query_map(
            params![conversation_session_id.to_string(), limit as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut messages = Vec::with_capacity(rows.len());
    for (id, sequence_no, payload_json, payload_sha256) in rows.into_iter().rev() {
        let actual_sha256 = format!("{:x}", Sha256::digest(payload_json.as_bytes()));
        if actual_sha256 != payload_sha256 {
            return Err(StorageError::Message(format!(
                "side chat canonical history item {id} has a payload hash mismatch"
            )));
        }
        let payload: HistoryItemPayload = serde_json::from_str(&payload_json)?;
        let projected = match payload {
            HistoryItemPayload::UserTurn { content, .. } => {
                Some((SideChatConversationRole::User, content_parts_text(&content)))
            }
            HistoryItemPayload::AssistantMessage { content, .. } => Some((
                SideChatConversationRole::Assistant,
                content_parts_text(&content),
            )),
            HistoryItemPayload::Error { message } => {
                Some((SideChatConversationRole::Error, message))
            }
            _ => None,
        };
        let Some((role, content)) = projected else {
            continue;
        };
        if content.is_empty() {
            continue;
        }
        messages.push(SideChatConversationMessage {
            id: id.parse().map_err(|error| {
                StorageError::Message(format!(
                    "side chat canonical history has invalid item id `{id}`: {error}"
                ))
            })?,
            sequence_no,
            role,
            content,
        });
    }
    Ok(messages)
}

fn content_parts_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.as_str(),
            ContentPart::Image { .. } => "[image]",
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn canonical_terminal_error(
    connection: &Connection,
    conversation_session_id: SessionId,
    status: SessionStatus,
) -> Result<Option<String>, StorageError> {
    if !matches!(status, SessionStatus::Failed | SessionStatus::Cancelled) {
        return Ok(None);
    }
    let terminal_row = connection
        .query_row(
            "SELECT terminal.turn_id, terminal.msg_json, terminal.payload_sha256
             FROM protocol_runtime_events AS terminal
             INNER JOIN protocol_item_append_order AS append_order
               ON append_order.session_id = terminal.session_id
              AND append_order.source_kind = 'runtime_event'
              AND append_order.source_id = terminal.id
             WHERE terminal.session_id = ?1
               AND json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
             ORDER BY append_order.append_position DESC
             LIMIT 1",
            [conversation_session_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((turn_id, terminal_json, terminal_sha256)) = terminal_row else {
        return Err(StorageError::Message(format!(
            "terminal side chat conversation {conversation_session_id} has no canonical terminal"
        )));
    };
    let actual_sha256 = format!("{:x}", Sha256::digest(terminal_json.as_bytes()));
    if actual_sha256 != terminal_sha256 {
        return Err(StorageError::Message(format!(
            "side chat conversation {conversation_session_id} canonical terminal has a payload hash mismatch"
        )));
    }
    turn_id.parse::<crate::protocol::TurnId>().map_err(|error| {
        StorageError::Message(format!(
            "side chat conversation {conversation_session_id} has invalid terminal turn id `{turn_id}`: {error}"
        ))
    })?;
    let RuntimeEventMsg::TurnTerminal { terminal } =
        serde_json::from_str::<RuntimeEventMsg>(&terminal_json)?
    else {
        return Err(StorageError::Message(format!(
            "side chat conversation {conversation_session_id} terminal discriminator is invalid"
        )));
    };
    if terminal.outcome.session_status() != status {
        return Err(StorageError::Message(format!(
            "side chat conversation {conversation_session_id} status does not match its latest canonical terminal"
        )));
    }
    Ok(Some(terminal.outcome.summary().to_string()))
}

fn validate_timestamp(value: i64) -> Result<(), StorageError> {
    if value < 0 {
        return Err(StorageError::Message(
            "side chat timestamp must not be negative".to_string(),
        ));
    }
    Ok(())
}

fn parse_optional_timestamp(value: Option<i64>, label: &str) -> Result<Option<i64>, StorageError> {
    value
        .map(|value| {
            if value < 0 {
                Err(StorageError::Message(format!(
                    "side chat binding has invalid {label} `{value}`"
                )))
            } else {
                Ok(value)
            }
        })
        .transpose()
}

fn ensure_delete_not_requested(binding: &SideChatBinding) -> Result<(), StorageError> {
    if binding.delete_requested_at_ms.is_some() {
        Err(StorageError::Message(format!(
            "side chat {} is pending deletion",
            binding.id
        )))
    } else {
        Ok(())
    }
}

fn sqlite_u64(value: u64, label: &str) -> Result<i64, StorageError> {
    i64::try_from(value)
        .map_err(|_| StorageError::Message(format!("{label} does not fit a SQLite integer")))
}

fn parse_u64(value: i64, label: &str) -> Result<u64, StorageError> {
    u64::try_from(value)
        .map_err(|_| StorageError::Message(format!("{label} has invalid value `{value}`")))
}

fn parse_positive_u64(value: i64, label: &str) -> Result<u64, StorageError> {
    parse_u64(value, label).and_then(|value| {
        if value == 0 {
            Err(StorageError::Message(format!("{label} must be positive")))
        } else {
            Ok(value)
        }
    })
}

fn parse_positive_u32(value: i64, label: &str) -> Result<u32, StorageError> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| StorageError::Message(format!("{label} has invalid value `{value}`")))
}

fn parse_session_id(value: &str, role: &str) -> Result<SessionId, StorageError> {
    value.parse().map_err(|error| {
        StorageError::Message(format!(
            "side chat binding has invalid {role} session id `{value}`: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier};

    use crate::protocol::{
        ModelResponseId, TurnId, TurnInterruptionCause, TurnTerminalOutcome, UserInputItem,
        UserTurn,
    };
    use crate::session::{DurableTurnTerminal, ProjectId, RunEvent, SessionRepository};
    use crate::storage::session_repo::ModelResponseWrite;
    use crate::storage::{SqliteStore, StoragePaths};

    use super::*;

    fn fixture() -> (SqliteStore, SqliteSideChatRepository, SessionId) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir =
            camino::Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 data dir");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let repo = store.side_chat_repo();
        let owner_session_id = SessionId::new();
        let project_id = ProjectId::new();
        let connection = repo.connection.lock().expect("sqlite mutex");
        connection
            .execute(
                "INSERT INTO projects
                 (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, 'side chat', 'none', 1, 1)",
                params![project_id.to_string(), format!("C:/side-chat/{project_id}"),],
            )
            .expect("project");
        connection
            .execute(
                "INSERT INTO sessions
                 (id, project_id, title, status, cwd_path, model_name, base_url,
                  access_mode, model_parameters_json,
                  created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES (?1, ?2, 'owner', 'idle', 'C:/side-chat', 'main',
                         'http://localhost:1234', 'default', '{}', 1, 1, NULL)",
                params![owner_session_id.to_string(), project_id.to_string()],
            )
            .expect("session");
        drop(connection);
        (store, repo, owner_session_id)
    }

    fn target(model: &str) -> SideChatProviderTarget {
        SideChatProviderTarget {
            route_kind: SideChatRouteKind::Direct,
            base_url: "http://localhost:1234/v1/".to_string(),
            model: model.to_string(),
            provider_profile: ProviderProfile::LmStudioChatCompletions,
            system_prompt: String::new(),
            context_window: 65_536,
            request_timeout_ms: 60_000,
            connect_timeout_ms: 5_000,
            max_retries: 1,
            supports_images: true,
            supports_tools: false,
        }
    }

    #[test]
    fn global_side_chat_config_converts_to_an_independent_provider_target() {
        let config = SideChatConfig {
            base_url: "http://localhost:8080/v1/".to_string(),
            model: "  side-model  ".to_string(),
            system_prompt: "  SIDE_ONLY_MARKER  ".to_string(),
            provider_profile: ProviderProfile::OpenAiCompatible,
            context_window: 32_768,
            request_timeout_ms: 90_000,
            connect_timeout_ms: 7_500,
            max_retries: 3,
        };

        assert_eq!(
            SideChatProviderTarget::try_from(&config).expect("side target"),
            SideChatProviderTarget {
                route_kind: SideChatRouteKind::Direct,
                base_url: "http://localhost:8080/v1".to_string(),
                model: "side-model".to_string(),
                provider_profile: ProviderProfile::OpenAiCompatible,
                system_prompt: "SIDE_ONLY_MARKER".to_string(),
                context_window: 32_768,
                request_timeout_ms: 90_000,
                connect_timeout_ms: 7_500,
                max_retries: 3,
                supports_images: false,
                supports_tools: false,
            }
        );
    }

    #[test]
    fn v65_side_chat_migrates_as_direct_and_reopens_without_reconfiguration() {
        let (store, repo, owner) = fixture();
        let binding = repo
            .ensure(owner, target("legacy-model"))
            .expect("legacy binding");
        repo.update_draft(owner, binding.id, 0, "keep draft")
            .expect("draft");
        let binding = repo.get_by_owner(owner).unwrap().unwrap();
        {
            let connection = repo.connection.lock().unwrap();
            // Remove only the forward addition to reproduce the released V65 shape.
            connection
                .execute_batch(
                    "DROP TRIGGER validate_side_chat_route_kind_before_update;
                ALTER TABLE side_chat_bindings DROP COLUMN provider_route_kind;
                DELETE FROM moyai_schema_migrations WHERE version=66;",
                )
                .expect("V65 fixture");
        }
        store.migrate().expect("forward migration");
        let reopened = SqliteStore::open(store.paths()).expect("reopen");
        reopened.migrate().expect("current schema remains valid");
        assert_eq!(
            reopened.side_chat_repo().get_by_owner(owner).unwrap(),
            Some(binding)
        );
    }

    #[tokio::test]
    async fn hub_side_chat_captures_direct_once_preserving_canonical_history_and_draft() {
        let (store, repo, owner) = fixture();
        let mut hub_target = target("logical-hub-model");
        hub_target.route_kind = SideChatRouteKind::Hub;
        hub_target.base_url = "https://hub.test:9471".into();
        hub_target.system_prompt = "existing side policy".into();
        let binding = repo.ensure(owner, hub_target).expect("Hub origin");
        let turn_id = TurnId::new();
        let admitted = repo
            .claim_and_admit_request(
                owner,
                binding.id,
                0,
                0,
                binding.provider_target(),
                turn_id,
                &user_turn(turn_id, "previous Hub question"),
            )
            .await
            .expect("previous turn");
        let direct = SideChatConfig {
            base_url: "http://direct.test:1234/v1".into(),
            model: "direct-model".into(),
            system_prompt: "must not overwrite policy".into(),
            ..SideChatConfig::default()
        };
        let running = repo.get_by_owner(owner).unwrap().unwrap();
        assert!(
            repo.capture_direct_provider(
                owner,
                binding.id,
                running.request_generation,
                running.draft_revision,
                &direct
            )
            .is_err(),
            "active turn rejects before mutation"
        );
        assert_eq!(repo.get_by_owner(owner).unwrap(), Some(running));
        let terminal = RunEvent::TurnTerminal {
            session_id: binding.conversation_session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                binding.conversation_session_id,
                admitted.admission.admission_id,
                &terminal,
                turn_id,
                None,
                None,
            )
            .await
            .expect("finish old turn");
        let draft = repo.get_by_owner(owner).unwrap().unwrap();
        repo.update_draft(
            owner,
            binding.id,
            draft.draft_revision,
            "unsent next question",
        )
        .expect("retain draft");
        let before = repo.get_by_owner(owner).unwrap().unwrap();
        let previous_messages = repo
            .conversation_projection(owner)
            .unwrap()
            .unwrap()
            .messages;
        let history_payloads = || {
            let connection = repo.connection.lock().unwrap();
            let mut statement = connection.prepare("SELECT payload_json FROM protocol_history_items WHERE session_id=?1 ORDER BY id").unwrap();
            statement
                .query_map([binding.conversation_session_id.to_string()], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let previous_history = history_payloads();
        for (wrong_owner, wrong_id, wrong_generation, wrong_revision) in [
            (
                SessionId::new(),
                binding.id,
                before.request_generation,
                before.draft_revision,
            ),
            (
                owner,
                SideChatId::new(),
                before.request_generation,
                before.draft_revision,
            ),
            (
                owner,
                binding.id,
                before.request_generation + 1,
                before.draft_revision,
            ),
            (
                owner,
                binding.id,
                before.request_generation,
                before.draft_revision + 1,
            ),
        ] {
            assert!(
                repo.capture_direct_provider(
                    wrong_owner,
                    wrong_id,
                    wrong_generation,
                    wrong_revision,
                    &direct
                )
                .is_err()
            );
            assert_eq!(repo.get_by_owner(owner).unwrap(), Some(before.clone()));
        }
        let applied = repo
            .capture_direct_provider(
                owner,
                binding.id,
                before.request_generation,
                before.draft_revision,
                &direct,
            )
            .expect("explicit first Direct target");
        assert_eq!(applied.route_kind, SideChatRouteKind::Direct);
        assert_eq!(applied.id, before.id);
        assert_eq!(
            applied.conversation_session_id,
            before.conversation_session_id
        );
        assert_eq!(applied.model, "direct-model");
        assert_eq!(applied.base_url, "http://direct.test:1234/v1");
        assert_eq!(applied.system_prompt, before.system_prompt);
        assert_eq!(applied.context_window, before.context_window);
        assert_eq!(applied.persisted_draft, before.persisted_draft);
        assert_eq!(applied.draft_revision, before.draft_revision);
        assert_eq!(applied.request_generation, before.request_generation);
        assert_eq!(
            repo.conversation_projection(owner)
                .unwrap()
                .unwrap()
                .messages,
            previous_messages
        );
        assert_eq!(history_payloads(), previous_history);
        assert!(
            repo.capture_direct_provider(
                owner,
                applied.id,
                applied.request_generation,
                applied.draft_revision,
                &direct
            )
            .is_err(),
            "Direct target remains immutable"
        );
        assert_eq!(repo.get_by_owner(owner).unwrap(), Some(applied.clone()));
        let reopened = SqliteStore::open(store.paths()).expect("reopen");
        reopened.migrate().expect("current schema");
        assert_eq!(
            reopened.side_chat_repo().get_by_owner(owner).unwrap(),
            Some(applied)
        );
    }

    #[test]
    fn global_side_chat_prompt_does_not_inherit_the_main_prompt() {
        let mut main = crate::config::ResolvedConfig::default().model;
        main.system_prompt = "MAIN_ONLY_MARKER".to_string();
        let mut side = SideChatConfig::default();
        side.system_prompt.clear();

        let blank = SideChatProviderTarget::try_from(&side).expect("blank side target");
        assert_eq!(blank.system_prompt, "");
        assert_ne!(blank.system_prompt, main.system_prompt);

        side.system_prompt = "  SIDE_ONLY_MARKER  ".to_string();
        let configured = SideChatProviderTarget::try_from(&side).expect("configured side target");
        assert_eq!(configured.system_prompt, "SIDE_ONLY_MARKER");
        assert_ne!(configured.system_prompt, main.system_prompt);
    }

    #[test]
    fn ensure_materializes_once_and_never_reconfigures_an_existing_binding() {
        let (_store, repo, owner) = fixture();
        let mut first_target = target("first-side-model");
        first_target.system_prompt = "FIRST_SIDE_PROMPT".to_string();
        let first = repo
            .ensure_at(owner, first_target, 1_000)
            .expect("materialize first Side Chat");

        let mut later_defaults = target("second-side-model");
        later_defaults.system_prompt = "SECOND_SIDE_PROMPT".to_string();
        let unchanged = repo
            .ensure_at(owner, later_defaults, 2_000)
            .expect("return existing Side Chat");

        assert_eq!(unchanged, first);
        assert_eq!(unchanged.model, "first-side-model");
        assert_eq!(unchanged.system_prompt, "FIRST_SIDE_PROMPT");
        assert_eq!(unchanged.updated_at_ms, 1_000);
    }

    #[test]
    fn system_prompt_validation_trims_outer_boundary_and_counts_unicode_characters() {
        let mut trimmed = target("gemma");
        trimmed.system_prompt = " \n最初\n  次  \t".to_string();
        let trimmed = trimmed.validate().expect("trimmed prompt");
        assert_eq!(trimmed.system_prompt, "最初\n  次");

        let mut accepted = target("gemma");
        accepted.system_prompt =
            "界".repeat(crate::system_prompt::MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS);
        assert_eq!(
            accepted
                .validate()
                .expect("boundary prompt")
                .system_prompt
                .chars()
                .count(),
            crate::system_prompt::MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS
        );

        let mut rejected = target("gemma");
        rejected.system_prompt =
            "界".repeat(crate::system_prompt::MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS + 1);
        let error = rejected
            .validate()
            .expect_err("oversized prompt must reject");
        assert!(error.to_string().contains("at most 16384 characters"));
    }

    #[test]
    fn configured_system_prompt_persists_across_reopen_without_debug_disclosure() {
        let (store, repo, owner) = fixture();
        let mut configured_target = target("gemma");
        configured_target.system_prompt = "  private side instructions  ".to_string();
        let binding = repo
            .configure(owner, configured_target)
            .expect("configure prompt");
        assert_eq!(binding.system_prompt, "private side instructions");
        let debug = format!("{binding:?}");
        assert!(debug.contains("system_prompt_chars: 25"));
        assert!(!debug.contains("private side instructions"));

        let reopened_store = SqliteStore::open(store.paths()).expect("reopen store");
        reopened_store.migrate().expect("reopen current schema");
        let reopened = reopened_store
            .side_chat_repo()
            .get_by_owner(owner)
            .expect("read reopened binding")
            .expect("durable binding");
        assert_eq!(reopened.system_prompt, "private side instructions");
    }

    #[test]
    fn binding_decode_rejects_noncanonical_unicode_prompt_boundary() {
        let (_store, repo, owner) = fixture();
        let binding = repo.configure(owner, target("gemma")).expect("configure");
        let connection = repo.connection.lock().expect("sqlite mutex");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .expect("enable corruption fixture");
        connection
            .execute(
                "UPDATE side_chat_bindings
                 SET system_prompt = ?2, updated_at_ms = updated_at_ms + 1
                 WHERE id = ?1",
                params![binding.id.to_string(), "\u{2003}corrupt boundary"],
            )
            .expect("inject noncanonical prompt");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = OFF")
            .expect("restore constraints");
        drop(connection);

        let error = repo
            .get_by_owner(owner)
            .expect_err("noncanonical prompt must fail closed");
        assert!(error.to_string().contains("non-canonical system prompt"));
    }

    fn user_turn(turn_id: TurnId, text: &str) -> UserTurn {
        UserTurn {
            turn_id,
            items: vec![UserInputItem::Text {
                text: text.to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        }
    }

    async fn terminal_projection(outcome: TurnTerminalOutcome) -> SideChatConversationProjection {
        let (store, repo, owner) = fixture();
        let binding = repo.configure(owner, target("gemma")).expect("configure");
        let turn_id = TurnId::new();
        let user_turn = user_turn(turn_id, "terminal question");
        let expected_provider_target = binding.provider_target();
        let admitted = repo
            .claim_and_admit_request(
                owner,
                binding.id,
                0,
                0,
                expected_provider_target,
                turn_id,
                &user_turn,
            )
            .await
            .expect("claim and admit side chat turn");
        let terminal = RunEvent::TurnTerminal {
            session_id: binding.conversation_session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                binding.conversation_session_id,
                admitted.admission.admission_id,
                &terminal,
                turn_id,
                None,
                None,
            )
            .await
            .expect("terminalize side chat turn");
        repo.conversation_projection(owner)
            .expect("project side chat conversation")
            .expect("side chat projection")
    }

    #[tokio::test]
    async fn hidden_conversation_is_atomic_stable_and_absent_from_normal_discovery() {
        let (store, repo, owner) = fixture();
        let first = repo
            .configure_at(owner, target("gemma-first"), 10)
            .expect("configure");
        assert_ne!(first.owner_session_id, first.conversation_session_id);
        assert_eq!(first.base_url, "http://localhost:1234/v1");
        let mut replacement = target("gemma-second");
        replacement.provider_profile = ProviderProfile::OpenAiResponses;
        let second = repo
            .configure_at(owner, replacement.clone(), 11)
            .expect("reconfigure");
        assert_eq!(second.id, first.id);
        assert_eq!(
            second.conversation_session_id,
            first.conversation_session_id
        );
        assert_eq!(second.provider_target(), replacement.validate().unwrap());

        let sessions = store.session_repo();
        let owner_record = sessions.get_session(owner).await.unwrap();
        assert_eq!(owner_record.provider_connection, None);
        let hidden_record = sessions
            .get_session(second.conversation_session_id)
            .await
            .expect("hidden canonical session");
        assert_eq!(
            second.max_output_tokens,
            RETIRED_MAX_OUTPUT_TOKENS_PLACEHOLDER
        );
        assert!(!second.supports_reasoning);
        assert_eq!(hidden_record.model_parameters.max_output_tokens, None);
        assert_eq!(
            hidden_record.provider_connection,
            Some(SessionProviderConnection {
                profile: ProviderProfile::OpenAiResponses,
                api_key_env: None,
                extra_headers: BTreeMap::new(),
            })
        );
        assert_eq!(
            sessions
                .latest_session(owner_record.project_id)
                .await
                .unwrap()
                .unwrap()
                .id,
            owner
        );
        assert_eq!(
            sessions
                .list_sessions(owner_record.project_id, 20)
                .await
                .unwrap()
                .iter()
                .map(|session| session.id)
                .collect::<Vec<_>>(),
            vec![owner]
        );
        assert_eq!(
            sessions
                .list_recent_sessions(20)
                .await
                .unwrap()
                .iter()
                .map(|session| session.id)
                .collect::<Vec<_>>(),
            vec![owner]
        );
        assert!(
            sessions
                .search_sessions(owner_record.project_id, "gemma-second", 20, true)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            sessions
                .list_sessions_with_projection_state(owner_record.project_id, 20, true)
                .await
                .unwrap()
                .iter()
                .all(|session| session.session.id != second.conversation_session_id)
        );
    }

    #[tokio::test]
    async fn legacy_generation_columns_remain_readable_but_do_not_affect_target_or_admission() {
        let (_store, repo, owner) = fixture();
        let configured = repo.configure(owner, target("gemma")).expect("configure");
        {
            let connection = repo.connection.lock().expect("sqlite mutex");
            connection
                .execute(
                    "UPDATE side_chat_bindings
                     SET max_output_tokens = 65536,
                         supports_reasoning = 1,
                         updated_at_ms = updated_at_ms + 1
                     WHERE owner_session_id = ?1",
                    [owner.to_string()],
                )
                .expect("inject legacy generation columns");
        }

        let legacy = repo
            .get_by_owner(owner)
            .expect("read legacy binding")
            .expect("binding");
        assert_eq!(legacy.max_output_tokens, 65_536);
        assert!(legacy.supports_reasoning);
        assert_eq!(
            legacy.provider_target(),
            target("gemma").validate().expect("canonical target")
        );

        let turn_id = TurnId::new();
        repo.claim_and_admit_request(
            owner,
            configured.id,
            0,
            0,
            legacy.provider_target(),
            turn_id,
            &user_turn(turn_id, "legacy columns must be inert"),
        )
        .await
        .expect("legacy columns must not block admission");
    }

    #[tokio::test]
    async fn canonical_admission_is_the_only_history_and_running_fence_owner() {
        let (store, repo, owner) = fixture();
        let binding = repo.configure(owner, target("gemma")).expect("configure");
        let turn_id = TurnId::new();
        let user_turn = UserTurn {
            turn_id,
            items: vec![UserInputItem::Text {
                text: "side question".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        };
        let expected_provider_target = binding.provider_target();
        let admitted = repo
            .claim_and_admit_request(
                owner,
                binding.id,
                0,
                0,
                expected_provider_target,
                turn_id,
                &user_turn,
            )
            .await
            .expect("claim and admit canonical turn");
        assert_eq!(admitted.generation, 1);
        store
            .session_repo()
            .record_model_response_with_protocol_bundle(
                binding.conversation_session_id,
                admitted.admission.admission_id,
                turn_id,
                ModelResponseWrite {
                    response_id: ModelResponseId::new(),
                    assistant_text: Some("side answer".to_string()),
                    assistant_protocol_sequence_no: None,
                    tool_calls: Vec::new(),
                },
            )
            .await
            .expect("canonical assistant response");
        assert!(repo.configure(owner, target("replacement")).is_err());
        assert!(repo.delete_if_idle(owner, binding.id).is_err());
        let projection = repo
            .conversation_projection(owner)
            .unwrap()
            .expect("conversation projection");
        assert_eq!(projection.status, SessionStatus::Running);
        assert_eq!(projection.binding.request_generation, 1);
        assert_eq!(projection.last_error, None);
        assert_eq!(
            projection
                .messages
                .iter()
                .map(|message| (message.role, message.content.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (SideChatConversationRole::User, "side question"),
                (SideChatConversationRole::Assistant, "side answer"),
            ]
        );
        let connection = repo.connection.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM protocol_history_items
                     WHERE session_id = ?1
                       AND json_extract(payload_json, '$.kind') = 'user_turn'",
                    [binding.conversation_session_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert!(!table_exists_for_test(&connection, "side_chat_messages"));
    }

    #[test]
    fn exact_draft_revision_has_one_cross_store_winner() {
        let (store, repo, owner) = fixture();
        let binding = repo.configure(owner, target("gemma")).expect("configure");
        let second_store = SqliteStore::open(store.paths()).expect("second store");
        second_store.migrate().expect("second migrate");
        let second_repo = second_store.side_chat_repo();
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = barrier.clone();
        let second_barrier = barrier.clone();
        let first_repo = repo.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_repo.update_draft(owner, binding.id, 0, "first")
        });
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_repo.update_draft(owner, binding.id, 0, "second")
        });
        barrier.wait();
        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_err()).count(),
            1
        );
        let current = repo.get_by_owner(owner).unwrap().unwrap();
        assert_eq!(current.draft_revision, 1);
        assert!(matches!(
            current.persisted_draft.as_str(),
            "first" | "second"
        ));
        let unchanged = repo
            .update_draft(owner, current.id, 1, current.persisted_draft.clone())
            .unwrap();
        assert!(!unchanged.changed);
        assert_eq!(unchanged.binding.draft_revision, 1);
    }

    #[tokio::test]
    async fn request_admission_rejects_a_stale_draft_revision_without_clearing_or_history() {
        let (_store, repo, owner) = fixture();
        let binding = repo.configure(owner, target("gemma")).expect("configure");
        let saved = repo
            .update_draft(owner, binding.id, binding.draft_revision, "newer draft")
            .expect("save newer draft")
            .binding;
        let turn_id = TurnId::new();
        let turn = user_turn(turn_id, "stale submitted text");
        let expected_provider_target = binding.provider_target();

        let error = repo
            .claim_and_admit_request(
                owner,
                binding.id,
                binding.request_generation,
                binding.draft_revision,
                expected_provider_target,
                turn_id,
                &turn,
            )
            .await
            .expect_err("stale draft revision must reject admission");

        assert!(error.to_string().contains("draft revision changed"));
        let projection = repo
            .conversation_projection(owner)
            .expect("project side chat")
            .expect("side chat projection");
        assert_eq!(projection.status, SessionStatus::Idle);
        assert_eq!(
            projection.binding.request_generation,
            binding.request_generation
        );
        assert_eq!(projection.binding.persisted_draft, "newer draft");
        assert_eq!(projection.binding.draft_revision, saved.draft_revision);
        assert!(projection.messages.is_empty());
    }

    #[tokio::test]
    async fn request_admission_rejects_a_changed_provider_target_without_creating_evidence() {
        let (store, repo, owner) = fixture();
        let mut original_target = target("gemma");
        original_target.system_prompt = "OLD_SIDE_PROMPT_MARKER".to_string();
        let binding = repo
            .configure(owner, original_target)
            .expect("configure original target");
        let expected_provider_target = binding.provider_target();

        let second_store = SqliteStore::open(store.paths()).expect("second store");
        second_store.migrate().expect("second migrate");
        let second_repo = second_store.side_chat_repo();
        let mut replacement_target = expected_provider_target.clone();
        replacement_target.system_prompt = "NEW_SIDE_PROMPT_MARKER".to_string();
        let reconfigured = second_repo
            .configure(owner, replacement_target)
            .expect("reconfigure during preflight");
        assert_eq!(reconfigured.id, binding.id);
        assert_eq!(reconfigured.request_generation, binding.request_generation);
        assert_eq!(reconfigured.draft_revision, binding.draft_revision);

        let turn_id = TurnId::new();
        let error = repo
            .claim_and_admit_request(
                owner,
                binding.id,
                binding.request_generation,
                binding.draft_revision,
                expected_provider_target,
                turn_id,
                &user_turn(turn_id, "must not use a stale provider target"),
            )
            .await
            .expect_err("changed provider target must reject admission");

        assert!(error.to_string().contains("provider target changed"));
        let projection = repo
            .conversation_projection(owner)
            .expect("project side chat")
            .expect("side chat projection");
        assert_eq!(projection.status, SessionStatus::Idle);
        assert_eq!(
            projection.binding.request_generation,
            binding.request_generation
        );
        assert_eq!(projection.binding.draft_revision, binding.draft_revision);
        assert_eq!(projection.binding.system_prompt, "NEW_SIDE_PROMPT_MARKER");
        assert!(projection.messages.is_empty());
    }

    #[test]
    fn concurrent_draft_save_and_request_admission_have_one_exact_revision_winner() {
        let (store, repo, owner) = fixture();
        let binding = repo.configure(owner, target("gemma")).expect("configure");
        let second_store = SqliteStore::open(store.paths()).expect("second store");
        second_store.migrate().expect("second migrate");
        let claim_repo = second_store.side_chat_repo();
        let barrier = Arc::new(Barrier::new(3));
        let save_barrier = barrier.clone();
        let claim_barrier = barrier.clone();
        let save_repo = repo.clone();
        let side_chat_id = binding.id;
        let expected_provider_target = binding.provider_target();
        let turn_id = TurnId::new();

        let save = std::thread::spawn(move || {
            save_barrier.wait();
            save_repo.update_draft(owner, side_chat_id, 0, "newer autosave")
        });
        let claim = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("claim runtime");
            let turn = user_turn(turn_id, "submitted text");
            claim_barrier.wait();
            runtime.block_on(claim_repo.claim_and_admit_request(
                owner,
                side_chat_id,
                0,
                0,
                expected_provider_target,
                turn_id,
                &turn,
            ))
        });
        barrier.wait();
        let save = save.join().expect("save thread");
        let claim = claim.join().expect("claim thread");

        assert_ne!(save.is_ok(), claim.is_ok(), "exactly one CAS must win");
        let projection = repo
            .conversation_projection(owner)
            .expect("project side chat")
            .expect("side chat projection");
        assert_eq!(projection.binding.draft_revision, 1);
        match (save, claim) {
            (Ok(saved), Err(error)) => {
                assert!(error.to_string().contains("draft revision changed"));
                assert_eq!(projection.status, SessionStatus::Idle);
                assert_eq!(projection.binding.request_generation, 0);
                assert_eq!(projection.binding.persisted_draft, "newer autosave");
                assert_eq!(projection.binding, saved.binding);
                assert!(projection.messages.is_empty());
            }
            (Err(error), Ok(admitted)) => {
                assert!(error.to_string().contains("draft revision changed"));
                assert_eq!(projection.status, SessionStatus::Running);
                assert_eq!(projection.binding.request_generation, 1);
                assert_eq!(projection.binding.persisted_draft, "");
                assert_eq!(projection.binding, admitted.binding);
                assert_eq!(projection.messages.len(), 1);
                assert_eq!(projection.messages[0].content, "submitted text");
            }
            _ => unreachable!("the exact revision fence must produce one winner"),
        }
    }

    #[test]
    fn atomic_claim_and_admission_has_one_cross_store_winner_without_loser_evidence() {
        let (store, repo, owner) = fixture();
        let binding = repo.configure(owner, target("gemma")).expect("configure");
        let saved = repo
            .update_draft(owner, binding.id, 0, "durable draft")
            .expect("save draft")
            .binding;
        let second_store = SqliteStore::open(store.paths()).expect("second store");
        second_store.migrate().expect("second migrate");
        let second_repo = second_store.side_chat_repo();
        let first_observed = repo.get_by_owner(owner).unwrap().unwrap();
        let second_observed = second_repo.get_by_owner(owner).unwrap().unwrap();
        assert_eq!(first_observed, second_observed);
        let first_expected_provider_target = first_observed.provider_target();
        let second_expected_provider_target = second_observed.provider_target();
        let expected_draft_revision = saved.draft_revision as i64;

        let first_turn_id = TurnId::new();
        let second_turn_id = TurnId::new();
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = barrier.clone();
        let second_barrier = barrier.clone();
        let first_repo = repo.clone();
        let first = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("first runtime");
            let turn = user_turn(first_turn_id, "first atomic request");
            first_barrier.wait();
            runtime.block_on(first_repo.claim_and_admit_observed_request(
                first_observed,
                owner,
                binding.id,
                0,
                expected_draft_revision,
                first_expected_provider_target,
                first_turn_id,
                &turn,
            ))
        });
        let second = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("second runtime");
            let turn = user_turn(second_turn_id, "second atomic request");
            second_barrier.wait();
            runtime.block_on(second_repo.claim_and_admit_observed_request(
                second_observed,
                owner,
                binding.id,
                0,
                expected_draft_revision,
                second_expected_provider_target,
                second_turn_id,
                &turn,
            ))
        });
        barrier.wait();
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_ne!(first.is_ok(), second.is_ok());
        let (winner, winner_turn_id, winner_text, loser_turn_id) = match (&first, &second) {
            (Ok(winner), Err(_)) => (
                winner,
                first_turn_id,
                "first atomic request",
                second_turn_id,
            ),
            (Err(_), Ok(winner)) => (
                winner,
                second_turn_id,
                "second atomic request",
                first_turn_id,
            ),
            _ => panic!("exactly one atomic request must win"),
        };
        assert_eq!(winner.generation, 1);
        assert_eq!(winner.binding.request_generation, 1);
        assert_eq!(winner.binding.persisted_draft, "");
        assert_eq!(winner.binding.draft_revision, saved.draft_revision + 1);
        assert!(winner.admission.initial_user_history_item_id.is_some());

        let projection = repo
            .conversation_projection(owner)
            .unwrap()
            .expect("projection");
        assert_eq!(projection.status, SessionStatus::Running);
        assert_eq!(projection.binding.request_generation, 1);
        assert_eq!(projection.binding.persisted_draft, "");
        assert_eq!(
            projection
                .messages
                .iter()
                .map(|message| (message.role, message.content.as_str()))
                .collect::<Vec<_>>(),
            vec![(SideChatConversationRole::User, winner_text)]
        );
        let connection = repo.connection.lock().unwrap();
        let (status, active_turn_id, has_admission) = connection
            .query_row(
                "SELECT status, active_turn_id, active_run_id IS NOT NULL
                 FROM sessions WHERE id = ?1",
                [binding.conversation_session_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(status, "running");
        assert_eq!(active_turn_id, winner_turn_id.to_string());
        assert!(has_admission);
        for table in [
            "protocol_history_items",
            "protocol_runtime_events",
            "protocol_turn_items",
        ] {
            let count = connection
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {table}
                         WHERE session_id = ?1 AND turn_id = ?2"
                    ),
                    params![
                        binding.conversation_session_id.to_string(),
                        loser_turn_id.to_string(),
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            assert_eq!(count, 0, "loser must not append to {table}");
        }
    }

    #[tokio::test]
    async fn atomic_claim_rolls_back_admission_and_draft_clear_on_commit_failure() {
        let (_store, repo, owner) = fixture();
        let binding = repo.configure(owner, target("gemma")).expect("configure");
        let saved = repo
            .update_draft(owner, binding.id, 0, "retain after failed send")
            .expect("save draft")
            .binding;
        {
            let connection = repo.connection.lock().unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER inject_side_chat_claim_failure
                     BEFORE UPDATE OF request_generation ON side_chat_bindings
                     WHEN NEW.request_generation = OLD.request_generation + 1
                     BEGIN
                         SELECT RAISE(ABORT, 'injected atomic claim failure');
                     END;",
                )
                .unwrap();
        }
        let turn_id = TurnId::new();
        let turn = user_turn(turn_id, "must roll back");
        let expected_provider_target = binding.provider_target();
        assert!(
            repo.claim_and_admit_request(
                owner,
                binding.id,
                0,
                saved.draft_revision,
                expected_provider_target,
                turn_id,
                &turn,
            )
            .await
            .is_err()
        );
        {
            let connection = repo.connection.lock().unwrap();
            connection
                .execute_batch("DROP TRIGGER inject_side_chat_claim_failure;")
                .unwrap();
            for table in [
                "protocol_history_items",
                "protocol_runtime_events",
                "protocol_turn_items",
            ] {
                let count = connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
                        [binding.conversation_session_id.to_string()],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap();
                assert_eq!(count, 0, "failed atomic commit must roll back {table}");
            }
        }
        let projection = repo
            .conversation_projection(owner)
            .unwrap()
            .expect("projection");
        assert_eq!(projection.status, SessionStatus::Idle);
        assert_eq!(projection.binding.request_generation, 0);
        assert_eq!(projection.binding.persisted_draft, saved.persisted_draft);
        assert_eq!(projection.binding.draft_revision, saved.draft_revision);
        assert!(projection.messages.is_empty());
    }

    #[tokio::test]
    async fn projection_uses_latest_canonical_terminal_after_active_turn_is_cleared() {
        let completed = terminal_projection(TurnTerminalOutcome::Completed).await;
        assert_eq!(completed.status, SessionStatus::Completed);
        assert_eq!(completed.last_error, None);
        assert_eq!(completed.messages.len(), 1);

        let cancelled = terminal_projection(TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::UserStop,
        })
        .await;
        assert_eq!(cancelled.status, SessionStatus::Cancelled);
        assert_eq!(cancelled.last_error.as_deref(), Some("run stopped by user"));
        assert_eq!(cancelled.messages.len(), 1);

        let failed = terminal_projection(TurnTerminalOutcome::Failed {
            error: "provider request failed".to_string(),
        })
        .await;
        assert_eq!(failed.status, SessionStatus::Failed);
        assert_eq!(
            failed.last_error.as_deref(),
            Some("provider request failed")
        );
        assert_eq!(failed.messages.len(), 1);
    }

    #[tokio::test]
    async fn delete_request_is_exact_idempotent_and_freezes_idle_binding() {
        let (store, repo, owner) = fixture();
        let binding = repo
            .configure_at(owner, target("gemma"), 10)
            .expect("configure");
        let wrong_id_error = repo
            .request_delete_at(owner, SideChatId::new(), 0, 20)
            .expect_err("wrong side chat id must fail");
        assert!(wrong_id_error.to_string().contains("is not owned"));
        let stale_generation_error = repo
            .request_delete_at(owner, binding.id, 1, 20)
            .expect_err("stale generation must fail");
        assert!(
            stale_generation_error
                .to_string()
                .contains("request generation changed")
        );

        let requested = repo
            .request_delete_at(owner, binding.id, 0, 20)
            .expect("request delete");
        assert_eq!(requested.delete_requested_at_ms, Some(20));
        let repeated = repo
            .request_delete_at(owner, binding.id, 0, 30)
            .expect("repeat exact delete request");
        assert_eq!(repeated, requested);

        assert!(
            repo.configure(owner, target("replacement"))
                .expect_err("provider configuration must freeze")
                .to_string()
                .contains("pending deletion")
        );
        assert!(
            repo.update_draft(owner, binding.id, 0, "must not persist")
                .expect_err("draft update must freeze")
                .to_string()
                .contains("pending deletion")
        );
        let blocked_turn_id = TurnId::new();
        let blocked_turn = user_turn(blocked_turn_id, "must not admit");
        let expected_provider_target = binding.provider_target();
        assert!(
            repo.claim_and_admit_request(
                owner,
                binding.id,
                0,
                binding.draft_revision,
                expected_provider_target,
                blocked_turn_id,
                &blocked_turn,
            )
            .await
            .expect_err("request claim must freeze")
            .to_string()
            .contains("pending deletion")
        );
        {
            let connection = repo.connection.lock().unwrap();
            assert!(
                connection
                    .execute(
                        "UPDATE side_chat_bindings
                         SET persisted_draft = 'bypass', draft_revision = draft_revision + 1
                         WHERE id = ?1",
                        [binding.id.to_string()],
                    )
                    .is_err(),
                "V55 must reject direct mutation after the tombstone"
            );
        }

        assert_eq!(repo.finalize_pending_deletions().unwrap(), 1);
        assert_eq!(repo.finalize_pending_deletions().unwrap(), 0);
        assert!(repo.get_by_owner(owner).unwrap().is_none());
        assert!(
            store
                .session_repo()
                .get_session(binding.conversation_session_id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn pending_delete_survives_reopen_and_waits_for_canonical_terminal() {
        let (store, repo, owner) = fixture();
        let binding = repo.configure(owner, target("gemma")).expect("configure");
        let turn_id = TurnId::new();
        let turn = user_turn(turn_id, "running request");
        let expected_provider_target = binding.provider_target();
        let admitted = repo
            .claim_and_admit_request(
                owner,
                binding.id,
                0,
                binding.draft_revision,
                expected_provider_target,
                turn_id,
                &turn,
            )
            .await
            .expect("claim and admit");
        let requested = repo
            .request_delete_at(owner, binding.id, 1, 20)
            .expect("request active delete");
        assert_eq!(requested.delete_requested_at_ms, Some(20));

        let reopened_store = SqliteStore::open(store.paths()).expect("reopen store");
        reopened_store.migrate().expect("reopen current V55");
        let reopened_repo = reopened_store.side_chat_repo();
        assert_eq!(
            reopened_repo
                .get_by_owner(owner)
                .unwrap()
                .expect("durable binding")
                .delete_requested_at_ms,
            Some(20)
        );
        assert_eq!(reopened_repo.finalize_pending_deletions().unwrap(), 0);
        assert!(reopened_repo.get_by_owner(owner).unwrap().is_some());

        let terminal = RunEvent::TurnTerminal {
            session_id: binding.conversation_session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: TurnInterruptionCause::UserStop,
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        };
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                binding.conversation_session_id,
                admitted.admission.admission_id,
                &terminal,
                turn_id,
                None,
                None,
            )
            .await
            .expect("terminalize canonical side chat turn");

        assert_eq!(reopened_repo.finalize_pending_deletions().unwrap(), 1);
        assert_eq!(reopened_repo.finalize_pending_deletions().unwrap(), 0);
        assert!(reopened_repo.get_by_owner(owner).unwrap().is_none());
        assert!(
            reopened_store
                .session_repo()
                .get_session(binding.conversation_session_id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn direct_and_owner_delete_remove_binding_and_hidden_canonical_session_atomically() {
        let (store, repo, owner) = fixture();
        let first = repo.configure(owner, target("gemma")).expect("configure");
        repo.delete_if_idle(owner, first.id).expect("direct close");
        assert!(repo.get_by_owner(owner).unwrap().is_none());
        assert!(
            store
                .session_repo()
                .get_session(first.conversation_session_id)
                .await
                .is_err()
        );

        let second = repo.configure(owner, target("gemma")).expect("recreate");
        store
            .session_repo()
            .delete_session(owner)
            .await
            .expect("owner delete");
        assert!(repo.get_by_owner(owner).unwrap().is_none());
        assert!(
            store
                .session_repo()
                .get_session(second.conversation_session_id)
                .await
                .is_err()
        );
    }

    fn table_exists_for_test(connection: &Connection, name: &str) -> bool {
        connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
                 )",
                [name],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    }
}
