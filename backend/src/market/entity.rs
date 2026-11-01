//! What actually differs between a skill and an agent: which tables hold it,
//! where its labels live, and how it is linked. Everything else in this module
//! reads these fields instead of branching on the plane.

use rusqlite::Row;
use serde::Serialize;
use serde_json::{json, Value};

use crate::limits;
use crate::state::Shared;

/// Projection for a skill listing row. `description` is not a column on
/// purpose: it lives in the latest version's `SKILL.md`, which is the only
/// source that cannot fall out of sync — and it works for rows published
/// before the column existed. The key needs quoting: `$.SKILL.md` would be
/// read as a nested path.
pub(crate) const SKILL_SELECT: &str = r#"
SELECT s.slug, s.name, s.owner_tenant, t.display_name AS owner_name, s.visibility,
       s.latest_version,
       s.created_at, s.withdrawn_at, s.takedown_at, s.takedown_reason,
       s.download_count,
       COALESCE((SELECT MAX(v.created_at) FROM skill_versions v WHERE v.slug = s.slug),
                s.created_at) AS updated_at,
       (SELECT json_extract(v.bundle_json, '$."SKILL.md"')
          FROM skill_versions v
         WHERE v.slug = s.slug AND v.version = s.latest_version) AS manifest,
       COALESCE((SELECT COUNT(*) FROM skill_versions v WHERE v.slug = s.slug), 0) AS versions,
       COALESCE((SELECT SUM(size_bytes) FROM skill_versions v WHERE v.slug = s.slug), 0) AS size_bytes
FROM skills s
LEFT JOIN tenants t ON t.id = s.owner_tenant
"#;

/// Projection for an agent listing row. An agent carries its own `description`
/// column and counts the skills declared by its latest version.
pub(crate) const AGENT_SELECT: &str = r#"
SELECT a.slug, a.name, a.description, a.owner_tenant, t.display_name AS owner_name,
       a.visibility, a.latest_version, a.created_at, a.withdrawn_at,
       a.takedown_at, a.takedown_reason, a.download_count,
       COALESCE((SELECT MAX(v.created_at) FROM agent_versions v WHERE v.slug = a.slug),
                a.created_at) AS updated_at,
       COALESCE((SELECT COUNT(*) FROM agent_versions v WHERE v.slug = a.slug), 0) AS versions,
       COALESCE((SELECT SUM(size_bytes) FROM agent_versions v WHERE v.slug = a.slug), 0) AS size_bytes,
       COALESCE((SELECT skill_count FROM agent_versions v
                  WHERE v.slug = a.slug AND v.version = a.latest_version), 0) AS skill_count
FROM agents a
LEFT JOIN tenants t ON t.id = a.owner_tenant
"#;

/// Where a plane keeps the labels it serves alongside an entry.
///
/// The agent plane reads them from the version's own manifest so a later
/// publish cannot rewrite the payload of a version that was already handed
/// out; the skill plane keeps its name on the entry row and derives its
/// description from the latest `SKILL.md`.
#[derive(Clone, Copy)]
pub enum Labels {
    /// A plain column on the entry row.
    Column,
    /// The `manifest` column of the projection (latest `SKILL.md`).
    ManifestColumn,
    /// A file inside the version's own bundle.
    File(&'static str),
}

pub struct Entity {
    /// Table holding one row per entry.
    pub table: &'static str,
    /// Table holding the immutable versions of an entry.
    pub version_table: &'static str,
    /// Table alias used by [`Entity::select`].
    pub alias: &'static str,
    /// Audit kind, see [`crate::db`].
    pub kind: &'static str,
    /// Chinese noun for messages ("技能" / "智能体").
    pub label: &'static str,
    /// Full `SELECT … FROM … LEFT JOIN tenants` projection.
    pub select: &'static str,
    /// `SELECT owner_tenant, latest_version, takedown_at` for a write.
    pub select_lifecycle: &'static str,
    /// Upsert of the listing row; only `visibility` is updated on conflict,
    /// because the name belongs to the newest version (see [`store`]).
    pub insert_listing: &'static str,
    /// Insert of the immutable version row.
    pub insert_version: &'static str,
    /// Whitelisted ORDER BY for a listing.
    pub order: fn(&str) -> &'static str,
    /// JSON field carrying the fetchable URL of the latest version.
    pub bundle_field: &'static str,
    /// How that URL is built.
    pub bundle_url: fn(&str, &str, &str) -> String,
    /// Where a listing entry's description comes from.
    pub description_from: Labels,
    /// Where a bundle payload's name/description come from.
    pub bundle_labels: Labels,
    /// How a publish turns a display name into a slug.
    pub slug_from: fn(&Shared, &str, &str) -> Option<String>,
}

pub const SKILL: Entity = Entity {
    table: "skills",
    version_table: "skill_versions",
    alias: "s",
    kind: crate::db::AUDIT_SKILL,
    label: "技能",
    select: SKILL_SELECT,
    select_lifecycle: "SELECT owner_tenant, latest_version, takedown_at FROM skills WHERE slug = ?1",
    insert_listing: "INSERT INTO skills
           (slug, name, owner_tenant, visibility, latest_version, created_at)
         VALUES (:slug, :name, :owner, :visibility, :version, :now)
         ON CONFLICT(slug) DO UPDATE SET visibility = excluded.visibility",
    insert_version: "INSERT OR IGNORE INTO skill_versions
           (slug, version, sha256, size_bytes, file_count, bundle_json, created_at)
         VALUES (:slug, :version, :sha256, :size_bytes, :file_count, :bundle_json, :now)",
    order: skill_order,
    bundle_field: "install_url",
    bundle_url: skill_bundle_url,
    description_from: Labels::ManifestColumn,
    bundle_labels: Labels::Column,
    slug_from: skill_slug_from,
};

pub const AGENT: Entity = Entity {
    table: "agents",
    version_table: "agent_versions",
    alias: "a",
    kind: crate::db::AUDIT_AGENT,
    label: "智能体",
    select: AGENT_SELECT,
    select_lifecycle: "SELECT owner_tenant, latest_version, takedown_at FROM agents WHERE slug = ?1",
    insert_listing: "INSERT INTO agents
           (slug, name, description, owner_tenant, visibility, latest_version, created_at)
         VALUES (:slug, :name, :description, :owner, :visibility, :version, :now)
         ON CONFLICT(slug) DO UPDATE SET visibility = excluded.visibility",
    insert_version: "INSERT OR IGNORE INTO agent_versions
           (slug, version, sha256, size_bytes, file_count, skill_count, bundle_json, created_at)
         VALUES (:slug, :version, :sha256, :size_bytes, :file_count, :skill_count,
                 :bundle_json, :now)",
    order: agent_order,
    bundle_field: "bundle_url",
    bundle_url: agent_bundle_url,
    description_from: Labels::Column,
    bundle_labels: Labels::File(crate::agents::AGENT_MANIFEST),
    slug_from: agent_slug_from,
};

/// A skill's slug is just its pinyin-derived name; there is no history to
/// honour, so the same name always lands on the same slug.
fn skill_slug_from(_app: &Shared, _tenant: &str, name: &str) -> Option<String> {
    crate::slug::derive_slug(name)
}

/// An agent's slug has to stay stable across releases: it reuses the slug this
/// tenant already published under the same name, then falls back to a hashed
/// slug for names with no ASCII form (see [`crate::agents::agent_slug`]).
fn agent_slug_from(app: &Shared, tenant: &str, name: &str) -> Option<String> {
    crate::slug::existing_agent_slug(app, tenant, name)
        .or_else(|| Some(crate::slug::agent_slug(name)))
}

fn skill_bundle_url(public_url: &str, slug: &str, version: &str) -> String {
    format!("{public_url}/skills/{slug}/{version}")
}

fn agent_bundle_url(public_url: &str, slug: &str, version: &str) -> String {
    format!("{public_url}/api/v1/agents/{slug}/{version}/bundle")
}

fn skill_order(sort: &str) -> &'static str {
    match normalize_sort(sort) {
        "downloads" => "s.download_count DESC, s.created_at DESC",
        "updated" => "updated_at DESC, s.created_at DESC",
        _ => "s.created_at DESC",
    }
}

fn agent_order(sort: &str) -> &'static str {
    match normalize_sort(sort) {
        "downloads" => "a.download_count DESC, a.created_at DESC",
        "updated" => "updated_at DESC, a.created_at DESC",
        _ => "a.created_at DESC",
    }
}

/// Whitelisted sort keys; anything else degrades to the historical order.
pub(crate) fn normalize_sort(sort: &str) -> &'static str {
    match sort {
        "downloads" => "downloads",
        "updated" => "updated",
        _ => "created",
    }
}

/// One listing entry, in the shape both planes serve it.
///
/// The 15 shared fields are the contract every listing endpoint shares; the
/// plane adds exactly one link field (`install_url` or `bundle_url`) and, for
/// agents, a skill count. [`Entry::to_json`] is the only place that difference
/// is applied.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_tenant: String,
    pub owner_name: Option<String>,
    pub visibility: String,
    pub latest_version: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub download_count: i64,
    pub withdrawn_at: Option<i64>,
    pub takedown_at: Option<i64>,
    pub takedown_reason: Option<String>,
    pub versions: i64,
    pub size_bytes: i64,
    /// Agent plane only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_count: Option<i64>,
    /// Serialized under [`Entity::bundle_field`], a different key per plane.
    #[serde(skip)]
    pub bundle_url: Option<String>,
}

impl Entry {
    /// The listing item as the API serves it.
    pub fn to_json(&self, entity: &Entity) -> Value {
        let mut fields = match serde_json::to_value(self) {
            Ok(Value::Object(fields)) => fields,
            _ => serde_json::Map::new(),
        };
        fields.insert(
            entity.bundle_field.to_string(),
            json!(self.bundle_url),
        );
        Value::Object(fields)
    }
}

/// Maps a row of [`Entity::select`] (plus the alias columns it joins) to an
/// [`Entry`].
pub(crate) fn row_to_entry(
    row: &Row<'_>,
    entity: &Entity,
    public_url: &str,
) -> rusqlite::Result<Entry> {
    let slug: String = row.get("slug")?;
    let latest: Option<String> = row.get("latest_version")?;
    let bundle_url = latest
        .as_ref()
        .map(|version| (entity.bundle_url)(public_url, &slug, version));
    let description = match entity.description_from {
        Labels::ManifestColumn => row
            .get::<_, Option<String>>("manifest")?
            .as_deref()
            .and_then(limits::manifest_description),
        _ => row.get("description")?,
    };
    // Only the agent projection carries `skill_count`; asking the row for a
    // column it does not have is how we keep this one function for both.
    let skill_count = row.get::<_, i64>("skill_count").ok();
    Ok(Entry {
        slug,
        name: row.get("name")?,
        description,
        owner_tenant: row.get("owner_tenant")?,
        owner_name: row.get("owner_name")?,
        visibility: row.get("visibility")?,
        latest_version: latest,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        download_count: row.get("download_count")?,
        withdrawn_at: row.get("withdrawn_at")?,
        takedown_at: row.get("takedown_at")?,
        takedown_reason: row.get("takedown_reason")?,
        versions: row.get("versions")?,
        size_bytes: row.get("size_bytes")?,
        skill_count,
        bundle_url,
    })
}

