use serde::{Deserialize, Serialize};

/// 标签同步数据
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagsSyncData {
    pub tags: Vec<TagSyncEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagSyncEntry {
    pub name: String,
    pub sort_order: i64,
    pub created_at: String,
    /// 该标签关联的条目，使用 content_hash 跨设备匹配。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub associations: Vec<TagItemSyncEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagItemSyncEntry {
    pub content_hash: String,
    pub sort_order: i64,
}

impl TagsSyncData {
    pub fn association_count(&self) -> usize {
        self.tags.iter().map(|tag| tag.associations.len()).sum()
    }

    /// 按标签分组读取新版关联数据。
    pub fn iter_associations(&self) -> impl Iterator<Item = (&str, &str, i64)> + '_ {
        self.tags
            .iter()
            .flat_map(|tag| {
                let tag_name = tag.name.as_str();
                tag.associations.iter().map(move |association| {
                    (tag_name, association.content_hash.as_str(), association.sort_order)
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_tags_json_omits_repeated_tag_name() {
        let data = TagsSyncData {
            tags: vec![TagSyncEntry {
                name: "工作".to_string(),
                sort_order: 1,
                created_at: "2026-09-18 00:00:00".to_string(),
                associations: vec![TagItemSyncEntry {
                    content_hash: "hash-1".to_string(),
                    sort_order: 2,
                }],
            }],
        };

        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("\"associations\":[{\"content_hash\":\"hash-1\""));
        assert!(!json.contains("tag_name"));
        assert_eq!(data.association_count(), 1);
    }

    #[test]
    fn legacy_flat_tags_json_is_rejected() {
        let json = r#"{
            "tags":[{"name":"工作","sort_order":1,"created_at":"2026-09-18 00:00:00"}],
            "associations":[{"content_hash":"hash-1","tag_name":"工作","sort_order":2}]
        }"#;
        assert!(serde_json::from_str::<TagsSyncData>(json).is_err());
    }
}
