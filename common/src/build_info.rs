use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_timestamp: Option<String>,
    pub dirty: bool,
    pub dev: bool,
}

impl BuildInfo {
    pub fn from_parts(
        version: impl Into<String>,
        release_tag: Option<&str>,
        git_sha: Option<&str>,
        build_timestamp: Option<&str>,
        dirty: bool,
    ) -> Self {
        let release_tag = non_empty(release_tag);
        Self {
            version: version.into(),
            dev: release_tag.is_none(),
            release_tag,
            git_sha: non_empty(git_sha),
            build_timestamp: non_empty(build_timestamp),
            dirty,
        }
    }
}

pub fn current_build_info() -> BuildInfo {
    BuildInfo::from_parts(
        env!("CARGO_PKG_VERSION"),
        option_env!("INTERCOM_RELEASE_TAG"),
        option_env!("INTERCOM_GIT_SHA"),
        option_env!("INTERCOM_BUILD_TIMESTAMP"),
        option_env!("INTERCOM_GIT_DIRTY").is_some_and(|value| value == "1"),
    )
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_without_tag_is_dev() {
        let info = BuildInfo::from_parts("2026.5.1", None, Some("abc123"), None, false);

        assert_eq!(info.version, "2026.5.1");
        assert!(info.dev);
        assert_eq!(info.release_tag, None);
        assert_eq!(info.git_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn build_info_with_tag_is_release() {
        let info = BuildInfo::from_parts(
            "2026.5.1",
            Some("v2026.5.1"),
            Some("abc123"),
            Some("2026-05-05T00:00:00Z"),
            true,
        );

        assert!(!info.dev);
        assert!(info.dirty);
        assert_eq!(info.release_tag.as_deref(), Some("v2026.5.1"));
        assert_eq!(
            info.build_timestamp.as_deref(),
            Some("2026-05-05T00:00:00Z")
        );
    }
}
