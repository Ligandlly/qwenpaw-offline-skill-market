//! How an entry gets its identifier: pinyin for Chinese display names, an
//! ascii-safe fallback, and the publisher alias that rides along with it.

use rusqlite::params;
use pinyin::ToPinyin;
use sha2::{Digest, Sha256};

use crate::limits;
use crate::state::Shared;

/// Identifier a publish derives from a display name: `My Skill!` -> `my-skill`,
/// `周报助手` -> `zhoubaozhushou`.
///
/// Han characters become plain (tone-less) pinyin, syllables joined, so a
/// Chinese name still gets a stable lowercase-ASCII slug instead of forcing the
/// page to invent one. Everything else that is not `[a-z0-9]` separates words,
/// repeated separators collapse, and the result is capped at `MAX_SLUG_LEN`
/// (truncated on a separator) so long names do not fail the publish.
pub(crate) fn derive_slug(name: &str) -> Option<String> {
    let mut slug = String::with_capacity(name.len());
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if let Some(pinyin) = c.to_pinyin() {
            slug.push_str(pinyin.plain());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.len() > limits::MAX_SLUG_LEN {
        slug[..limits::MAX_SLUG_LEN].trim_end_matches('-')
    } else {
        slug
    };
    if slug.is_empty() || !limits::valid_slug(slug) {
        None
    } else {
        Some(slug.to_string())
    }
}

/// Slug for an agent whose display name has no ASCII slug form (e.g. emoji).
/// Stable across versions so republishing reuses the same market entry.
pub(crate) fn agent_slug(name: &str) -> String {
    if let Some(slug) = derive_slug(name) {
        return slug;
    }
    let digest = Sha256::digest(name.trim().as_bytes());
    let suffix: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("agent-{suffix}")
}

/// Slug this tenant already published under the same display name, if any.
///
/// Chinese names used to derive `agent-<sha256>` and now derive pinyin, so
/// without this a republish after the upgrade would create a *second* entry for
/// an agent that is already on the market. Same tenant + same name only, so it
/// cannot reach into someone else's listing.
pub(crate) fn existing_agent_slug(app: &Shared, tenant: &str, name: &str) -> Option<String> {
    let db = app.db();
    db.query_row(
        "SELECT slug FROM agents WHERE owner_tenant = ?1 AND name = ?2
         ORDER BY created_at LIMIT 1",
        params![tenant, name.trim()],
        |row| row.get(0),
    )
    .ok()
}

/// Publisher names are labels, not identities: anything unusable is dropped
/// instead of failing the publish (ownership is the tenant header, always).
pub(crate) fn clean_publisher_name(raw: Option<&str>) -> Option<String> {
    let cleaned: String = raw?.chars().filter(|c| !c.is_control()).take(64).collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_slugs() {
        assert_eq!(derive_slug("My Skill!"), Some("my-skill".to_string()));
        assert_eq!(derive_slug("  My   Skill  "), Some("my-skill".into()));
        assert_eq!(derive_slug("v1.2 tool"), Some("v1-2-tool".into()));
        // Chinese names transliterate to plain pinyin instead of failing.
        assert_eq!(derive_slug("技能"), Some("jineng".into()));
        assert_eq!(derive_slug("周报助手"), Some("zhoubaozhushou".into()));
        assert_eq!(
            derive_slug("周报助手 Pro"),
            Some("zhoubaozhushou-pro".into())
        );
        assert_eq!(derive_slug("技能（测试）"), Some("jineng-ceshi".into()));
        // Nothing usable -> None (publish then demands an explicit slug).
        assert_eq!(derive_slug(""), None);
        assert_eq!(derive_slug("  "), None);
        assert_eq!(derive_slug("!!!"), None);
        // Long names are truncated on a separator, not rejected.
        let long = derive_slug(&"技能".repeat(40)).unwrap();
        assert!(limits::valid_slug(&long), "{long}");
        assert_eq!(long.len(), 64);
    }

    #[test]
    fn cleans_publisher_names_but_never_invents_one() {
        assert_eq!(clean_publisher_name(Some(" admin ")), Some("admin".into()));
        assert_eq!(clean_publisher_name(Some("张三")), Some("张三".into()));
        assert_eq!(
            clean_publisher_name(Some("\u{0}admin\n")),
            Some("admin".into())
        );
        assert_eq!(clean_publisher_name(Some("   ")), None);
        assert_eq!(clean_publisher_name(None), None);
        assert_eq!(
            clean_publisher_name(Some(&"x".repeat(200))).unwrap().len(),
            64
        );
    }
}
