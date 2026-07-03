use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use lim_core::error::{LimError, Result};
use lim_core::id::ArtifactId;

/// A stored artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Unique artifact ID.
    pub id: ArtifactId,
    /// Kind of artifact (e.g. "scene_draft", "review_report", "chapter").
    pub kind: String,
    /// The artifact content.
    pub content: serde_json::Value,
    /// When the artifact was created (Unix timestamp as string).
    pub created_at: String,
    /// Parent artifact (for versioning/lineage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ArtifactId>,
    /// Arbitrary metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// File-system backed artifact store.
///
/// Artifacts are stored as JSON files at `{base_dir}/{id}.json`.
pub struct ArtifactStore {
    base_dir: Utf8PathBuf,
}

impl ArtifactStore {
    /// Create a new [`ArtifactStore`] rooted at `base_dir`.
    pub fn new(base_dir: Utf8PathBuf) -> Self {
        Self { base_dir }
    }

    /// Store an artifact to disk.
    pub async fn store(&self, artifact: &Artifact) -> Result<()> {
        // Ensure base directory exists
        tokio::fs::create_dir_all(self.base_dir.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "artifact_store",
                format!("failed to create artifact directory: {e}"),
                e,
            )
        })?;

        let path = self.base_dir.join(format!("{}.json", artifact.id));
        let json = serde_json::to_string_pretty(artifact).map_err(|e| {
            LimError::tool_execution(
                "artifact_store",
                format!("failed to serialize artifact: {e}"),
                e,
            )
        })?;

        tokio::fs::write(path.as_std_path(), json.as_bytes()).await.map_err(|e| {
            LimError::tool_execution("artifact_store", format!("failed to write artifact: {e}"), e)
        })?;

        Ok(())
    }

    /// Load an artifact from disk.
    pub async fn load(&self, id: ArtifactId) -> Result<Artifact> {
        let path = self.base_dir.join(format!("{id}.json"));
        let content = tokio::fs::read_to_string(path.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "artifact_store",
                format!("failed to read artifact `{id}`: {e}"),
                e,
            )
        })?;

        serde_json::from_str(&content).map_err(|e| {
            LimError::tool_execution(
                "artifact_store",
                format!("failed to parse artifact `{id}`: {e}"),
                e,
            )
        })
    }

    /// List all stored artifacts.
    pub async fn list(&self) -> Result<Vec<Artifact>> {
        let mut artifacts = Vec::new();

        let mut entries = tokio::fs::read_dir(self.base_dir.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "artifact_store",
                format!("failed to read artifact directory: {e}"),
                e,
            )
        })?;

        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            LimError::tool_execution("artifact_store", format!("failed to list artifacts: {e}"), e)
        })? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
                    LimError::tool_execution(
                        "artifact_store",
                        format!("failed to read artifact file: {e}"),
                        e,
                    )
                })?;
                if let Ok(artifact) = serde_json::from_str::<Artifact>(&content) {
                    artifacts.push(artifact);
                }
            }
        }

        artifacts.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(artifacts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    #[tokio::test]
    async fn store_and_load_artifact() {
        let dir = std::env::temp_dir().join("lim-test-artifacts");
        let _ = std::fs::remove_dir_all(&dir);
        let base = Utf8PathBuf::try_from(dir.clone()).unwrap();
        let store = ArtifactStore::new(base);

        let artifact = Artifact {
            id: ArtifactId::new(),
            kind: "test".to_string(),
            content: serde_json::json!({"text": "hello world"}),
            created_at: "1000".to_string(),
            parent: None,
            metadata: serde_json::json!({}),
        };

        store.store(&artifact).await.unwrap();
        let loaded = store.load(artifact.id).await.unwrap();
        assert_eq!(loaded.kind, "test");
        assert_eq!(loaded.content["text"], "hello world");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn list_artifacts() {
        let dir = std::env::temp_dir().join("lim-test-artifacts-list");
        let _ = std::fs::remove_dir_all(&dir);
        let base = Utf8PathBuf::try_from(dir.clone()).unwrap();
        let store = ArtifactStore::new(base);

        for i in 0..3 {
            let artifact = Artifact {
                id: ArtifactId::new(),
                kind: format!("type_{i}"),
                content: serde_json::json!({"index": i}),
                created_at: format!("{}", 1000 + i),
                parent: None,
                metadata: serde_json::json!({}),
            };
            store.store(&artifact).await.unwrap();
        }

        let artifacts = store.list().await.unwrap();
        assert_eq!(artifacts.len(), 3);
        // Sorted by created_at
        assert_eq!(artifacts[0].content["index"], 0);
        assert_eq!(artifacts[2].content["index"], 2);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn artifact_with_parent() {
        let dir = std::env::temp_dir().join("lim-test-artifacts-parent");
        let _ = std::fs::remove_dir_all(&dir);
        let base = Utf8PathBuf::try_from(dir.clone()).unwrap();
        let store = ArtifactStore::new(base);

        let parent_id = ArtifactId::new();
        let child = Artifact {
            id: ArtifactId::new(),
            kind: "scene_draft_v2".to_string(),
            content: serde_json::json!("improved draft"),
            created_at: "2000".to_string(),
            parent: Some(parent_id),
            metadata: serde_json::json!({"attempt": 2}),
        };

        store.store(&child).await.unwrap();
        let loaded = store.load(child.id).await.unwrap();
        assert_eq!(loaded.parent, Some(parent_id));
        assert_eq!(loaded.metadata["attempt"], 2);

        let _ = std::fs::remove_dir_all(dir);
    }
}
