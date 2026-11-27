//! Optional presentation roles, separate from field storage and identity.
use super::{FieldKind, FieldSchema};
use serde::{Deserialize, Serialize};

/// A vertical property list. Each record contributes one named value;
/// optional secondary text (for example an original key) sits below its label.
/// Every visible field must have exactly one role so the view loses no data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PropertyLayout {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary: Option<String>,
}

impl PropertyLayout {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: None,
            secondary: None,
        }
    }

    pub fn label(mut self, field: impl Into<String>) -> Self {
        self.label = Some(field.into());
        self
    }

    pub fn secondary(mut self, field: impl Into<String>) -> Self {
        self.secondary = Some(field.into());
        self
    }

    pub(super) fn validate(&self, fields: &[FieldSchema]) -> Result<(), String> {
        let mut roles = std::collections::HashSet::new();
        for id in std::iter::once(&self.value)
            .chain(self.label.iter())
            .chain(self.secondary.iter())
        {
            let field = fields
                .iter()
                .find(|field| &field.id == id)
                .ok_or_else(|| format!("property layout references unknown field {id}"))?;
            if field.hidden || !roles.insert(id.as_str()) {
                return Err(format!("property field {id} must be visible and used once"));
            }
        }
        if fields
            .iter()
            .any(|field| !field.hidden && !roles.contains(field.id.as_str()))
        {
            return Err("property layout must include every visible field".to_owned());
        }
        Ok(())
    }
}

/// A conversation-timeline presentation for chat/message records.
///
/// Groups records by `thread_id` into conversations and renders each record
/// as a chat bubble with optional read/delivery receipts, interactive polls,
/// emoji reactions, quoted replies, and media previews.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLayout {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outgoing_values: Vec<String>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

impl ChatLayout {
    pub fn new(thread_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            thread_title: None,
            sender: None,
            direction: None,
            outgoing_values: Vec::new(),
            content: content.into(),
            time: None,
            msg_type: None,
            media: None,
            status: None,
            receipts: None,
            poll: None,
            reactions: None,
            reply_to: None,
        }
    }

    pub fn thread_title(mut self, field: impl Into<String>) -> Self {
        self.thread_title = Some(field.into());
        self
    }

    pub fn sender(mut self, field: impl Into<String>) -> Self {
        self.sender = Some(field.into());
        self
    }

    pub fn direction(mut self, field: impl Into<String>) -> Self {
        self.direction = Some(field.into());
        self
    }

    pub fn outgoing_values(mut self, values: Vec<String>) -> Self {
        self.outgoing_values = values;
        self
    }

    pub fn time(mut self, field: impl Into<String>) -> Self {
        self.time = Some(field.into());
        self
    }

    pub fn msg_type(mut self, field: impl Into<String>) -> Self {
        self.msg_type = Some(field.into());
        self
    }

    pub fn media(mut self, field: impl Into<String>) -> Self {
        self.media = Some(field.into());
        self
    }

    pub fn status(mut self, field: impl Into<String>) -> Self {
        self.status = Some(field.into());
        self
    }

    pub fn receipts(mut self, field: impl Into<String>) -> Self {
        self.receipts = Some(field.into());
        self
    }

    pub fn poll(mut self, field: impl Into<String>) -> Self {
        self.poll = Some(field.into());
        self
    }

    pub fn reactions(mut self, field: impl Into<String>) -> Self {
        self.reactions = Some(field.into());
        self
    }

    pub fn reply_to(mut self, field: impl Into<String>) -> Self {
        self.reply_to = Some(field.into());
        self
    }

    pub(super) fn validate(&self, fields: &[FieldSchema]) -> Result<(), String> {
        let find = |id: &str| {
            fields
                .iter()
                .find(|field| field.id == id)
                .ok_or_else(|| format!("chat layout references unknown field {id}"))
        };
        find(&self.thread_id)?;
        find(&self.content)?;
        for id in [
            self.thread_title.as_deref(),
            self.sender.as_deref(),
            self.direction.as_deref(),
            self.msg_type.as_deref(),
            self.media.as_deref(),
            self.status.as_deref(),
            self.reply_to.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            find(id)?;
        }
        if let Some(id) = self.time.as_deref() {
            let field = find(id)?;
            if !matches!(field.kind, FieldKind::Timestamp | FieldKind::Text) {
                return Err(format!("chat time field {id} must be timestamp or text"));
            }
        }
        for (role, id) in [
            ("receipts", self.receipts.as_deref()),
            ("poll", self.poll.as_deref()),
            ("reactions", self.reactions.as_deref()),
        ] {
            if let Some(id) = id {
                let field = find(id)?;
                if field.kind != FieldKind::Json {
                    return Err(format!("chat {role} field {id} must be json"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FieldKind, RecordSchema};

    fn schema() -> RecordSchema {
        RecordSchema::new("test.property", "属性")
            .field(FieldSchema::new("name", "名称", FieldKind::Text))
            .field(FieldSchema::new("value", "值", FieldKind::Text))
            .field(FieldSchema::new("extra", "其他", FieldKind::Json).hidden())
    }

    fn chat_schema() -> RecordSchema {
        RecordSchema::new("test.chat", "消息")
            .field(FieldSchema::new("chat_id", "会话", FieldKind::Text).key())
            .field(FieldSchema::new("chat_name", "名称", FieldKind::Text))
            .field(FieldSchema::new("sender", "发送者", FieldKind::Text))
            .field(FieldSchema::new("direction", "方向", FieldKind::Text))
            .field(FieldSchema::new("content", "内容", FieldKind::Text))
            .field(FieldSchema::new("time", "时间", FieldKind::Timestamp))
            .field(FieldSchema::new("receipts", "回执", FieldKind::Json).hidden())
            .field(FieldSchema::new("poll", "投票", FieldKind::Json).hidden())
            .field(FieldSchema::new("reactions", "回应", FieldKind::Json).hidden())
    }

    #[test]
    fn presentation_roundtrips_and_tables_keep_their_original_serialization() {
        let original = schema();
        let encoded = rmp_serde::to_vec_named(&original).expect("serialize");
        assert!(!encoded
            .windows(b"properties".len())
            .any(|bytes| bytes == b"properties"));
        assert!(!encoded
            .windows(b"chat".len())
            .any(|bytes| bytes == b"chat"));
        let decoded = rmp_serde::from_slice::<RecordSchema>(&encoded).expect("old schema");
        assert!(decoded.properties.is_none());
        assert!(decoded.chat.is_none());
        let properties = original.properties(PropertyLayout::new("value").label("name"));
        properties.validate().expect("valid layout");
        let encoded = rmp_serde::to_vec_named(&properties).expect("serialize");
        assert_eq!(
            rmp_serde::from_slice::<RecordSchema>(&encoded).expect("decode"),
            properties
        );
    }

    #[test]
    fn rejects_missing_hidden_duplicate_and_omitted_fields() {
        for layout in [
            PropertyLayout::new("missing").label("name"),
            PropertyLayout::new("extra").label("name"),
            PropertyLayout::new("value").label("value"),
            PropertyLayout::new("value"),
        ] {
            assert!(schema().properties(layout).validate().is_err());
        }
    }

    #[test]
    fn chat_layout_roundtrips_and_validates_fields() {
        let layout = ChatLayout::new("chat_id", "content")
            .thread_title("chat_name")
            .sender("sender")
            .direction("direction")
            .outgoing_values(vec!["发出".to_owned()])
            .time("time")
            .receipts("receipts")
            .poll("poll")
            .reactions("reactions");
        let built = chat_schema().chat(layout.clone());
        built.validate().expect("valid chat schema");
        let encoded = rmp_serde::to_vec_named(&built).expect("rmp encode");
        assert!(encoded
            .windows(b"threadId".len())
            .any(|bytes| bytes == b"threadId"));
        assert!(encoded
            .windows(b"threadTitle".len())
            .any(|bytes| bytes == b"threadTitle"));
        assert!(encoded
            .windows(b"outgoingValues".len())
            .any(|bytes| bytes == b"outgoingValues"));
        assert_eq!(
            rmp_serde::from_slice::<RecordSchema>(&encoded).expect("rmp decode"),
            built
        );

        assert!(chat_schema()
            .chat(ChatLayout::new("missing", "content"))
            .validate()
            .is_err());
        assert!(chat_schema()
            .chat(ChatLayout::new("chat_id", "content").receipts("sender"))
            .validate()
            .is_err());
        assert!(chat_schema()
            .chat(ChatLayout::new("chat_id", "content").poll("sender"))
            .validate()
            .is_err());
        assert!(chat_schema()
            .chat(ChatLayout::new("chat_id", "content").reactions("sender"))
            .validate()
            .is_err());
        assert!(schema()
            .properties(PropertyLayout::new("value").label("name"))
            .chat(ChatLayout::new("name", "value"))
            .validate()
            .is_err());
    }
}
