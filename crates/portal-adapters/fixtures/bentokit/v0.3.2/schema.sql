-- Minimal column-compatible fixture from Core's ConversationStore/ProjectStore.
-- Synthetic data is inserted by bentokit_read.rs; no user transcripts are used.
CREATE TABLE conversations (
    id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, title TEXT, contact_id TEXT,
    channel_id TEXT, project_id TEXT, owner_id TEXT, created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL, metadata TEXT, is_deleted INTEGER DEFAULT 0
);
CREATE TABLE messages (
    id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL, role TEXT NOT NULL,
    content TEXT NOT NULL, timestamp TEXT NOT NULL, metadata TEXT,
    is_deleted INTEGER DEFAULT 0, seq INTEGER
);
CREATE INDEX idx_messages_conv_ts ON messages(conversation_id, timestamp DESC);
