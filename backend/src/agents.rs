//! Agent market: the same publish/list/withdraw/takedown shape as skills, but
//! for agent bundles (`manifest.json` + persona markdown + bundled skills).
//!
//! Security hardening and per-bundle quotas are deliberately deferred for the
//! MVP (see docs/AGENT_MARKET.md §9). What remains here is the structural
//! validation needed for a bundle to be installable at all: manifest shape,
//! path rules, declared-vs-present files, and each skill's `SKILL.md`
//! frontmatter (the host refuses installs without it).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use tracing::{debug, error, info, warn};

use crate::slug::clean_publisher_name;
use crate::dto::{default_visibility, fifty, ListQuery, LookupQuery};
use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::auth::{Admin, TenantId};
use crate::limits::{self, FileData, Reject};
use crate::market;
use crate::state::Shared;

pub const AGENT_MANIFEST: &str = "manifest.json";

/// Root markdown files an agent bundle may carry. Everything else must live
/// under `skills/<name>/`.
pub const AGENT_MD_FILES: [&str; 5] = [
    "AGENTS.md",
    "SOUL.md",
    "PROFILE.md",
    "HEARTBEAT.md",
    "BOOTSTRAP.md",
];

const THINKING_LEVELS: [&str; 9] = [
    "inherit", "off", "minimal", "low", "medium", "high", "xhigh", "max", "budget",
];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    #[serde(default)]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub thinking_budget: Option<u64>,
    #[serde(default)]
    pub plan_enabled: bool,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentManifest {
    pub schema: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub profile: AgentProfile,
    #[serde(default)]
    pub md_files: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
}

fn default_language() -> String {
    "zh".to_string()
}

/// Normalised agent package that passed structural validation.
#[derive(Debug, Clone)]
pub struct CheckedAgentBundle {
    pub manifest: AgentManifest,
    pub files: BTreeMap<String, FileData>,
    pub sha256: String,
    pub size_bytes: usize,
    pub file_count: usize,
    pub skill_count: usize,
}

/// Structural gate. No size/file-count limits yet: the MVP only rejects
/// bundles that cannot be installed or could write outside a skill directory.
pub fn check_agent_bundle(
    files: &BTreeMap<String, FileData>,
) -> Result<CheckedAgentBundle, Reject> {
    if files.is_empty() {
        return Err(Reject::new("empty_bundle", "压缩包里没有任何文件"));
    }
    let manifest_file = files.get(AGENT_MANIFEST).ok_or_else(|| {
        Reject::new(
            "missing_manifest",
            format!("压缩包根目录必须包含 {AGENT_MANIFEST}"),
        )
    })?;
    let manifest_text = manifest_file.as_text().ok_or_else(|| {
        Reject::new(
            "invalid_manifest",
            format!("{AGENT_MANIFEST} 必须是 UTF-8 文本"),
        )
    })?;
    let manifest: AgentManifest = serde_json::from_str(manifest_text).map_err(|err| {
        Reject::new(
            "invalid_manifest",
            format!("{AGENT_MANIFEST} 不是合法的 JSON：{err}"),
        )
    })?;

    if manifest.schema != 1 {
        return Err(Reject::new(
            "unsupported_schema",
            format!("不支持的 manifest schema {}，应为 1", manifest.schema),
        ));
    }
    if manifest.name.trim().is_empty() {
        return Err(Reject::new("invalid_name", "智能体名称不能为空"));
    }
    if manifest.language.trim().is_empty() {
        return Err(Reject::new("invalid_language", "智能体语言不能为空"));
    }
    validate_profile(&manifest.profile)?;

    if manifest.md_files.is_empty() && manifest.skills.is_empty() {
        return Err(Reject::new(
            "empty_agent",
            "智能体包至少需要一个 markdown 文件或一个内置技能",
        ));
    }

    let mut md_set: BTreeSet<&str> = BTreeSet::new();
    for md in &manifest.md_files {
        if !AGENT_MD_FILES.contains(&md.as_str()) {
            return Err(Reject::new(
                "invalid_md_file",
                format!("{md}：根目录只允许 {}", AGENT_MD_FILES.join(", ")),
            ));
        }
        if !md_set.insert(md.as_str()) {
            return Err(Reject::new("duplicate_md_file", format!("{md}：重复列出")));
        }
        if !files.contains_key(md) {
            return Err(Reject::new(
                "missing_md_file",
                format!("{md}：在 md_files 中声明，但包里没有该文件"),
            ));
        }
    }

    let mut skill_set: BTreeSet<&str> = BTreeSet::new();
    for name in &manifest.skills {
        if !valid_skill_name(name) {
            return Err(Reject::new(
                "invalid_skill_name",
                format!("{name}：技能名不能为空、不超过 128 字节，且不含路径分隔符或控制字符"),
            ));
        }
        if !skill_set.insert(name.as_str()) {
            return Err(Reject::new("duplicate_skill", format!("{name}：重复列出")));
        }
        let skill_manifest = format!("skills/{name}/SKILL.md");
        let content = files
            .get(&skill_manifest)
            .ok_or_else(|| {
                Reject::new(
                    "missing_skill_manifest",
                    format!("{skill_manifest}：声明的技能缺少 SKILL.md"),
                )
            })?
            .as_text()
            .ok_or_else(|| {
                Reject::new(
                    "invalid_skill_manifest",
                    format!("{skill_manifest}：SKILL.md 必须是 UTF-8 文本"),
                )
            })?;
        limits::manifest_meta(content)?;
    }

    let mut size_bytes = 0usize;
    for path in files.keys() {
        limits::check_path(path)?;
        if path == AGENT_MANIFEST {
            continue;
        }
        if !path.contains('/') {
            if !md_set.contains(path.as_str()) {
                return Err(Reject::new(
                    "unexpected_root_file",
                    format!("{path}：根目录只允许 {AGENT_MANIFEST} 和已声明的 md_files"),
                ));
            }
            continue;
        }
        let mut parts = path.split('/');
        let root = parts.next().unwrap_or_default();
        let skill = parts.next().unwrap_or_default();
        if root != "skills" {
            return Err(Reject::new(
                "unexpected_path",
                format!("{path}：非 markdown 文件必须放在 skills/<name>/ 下"),
            ));
        }
        if !skill_set.contains(skill) {
            return Err(Reject::new(
                "undeclared_skill",
                format!("{path}：技能 '{skill}' 未在 manifest.skills 中声明"),
            ));
        }
        if parts.next().is_none() {
            return Err(Reject::new(
                "invalid_path",
                format!("{path}：技能下的条目必须是技能目录里的文件"),
            ));
        }
    }
    size_bytes += files.values().map(FileData::len).sum::<usize>();

    Ok(CheckedAgentBundle {
        sha256: limits::digest(files),
        size_bytes,
        file_count: files.len(),
        skill_count: manifest.skills.len(),
        manifest,
        files: files.clone(),
    })
}

fn validate_profile(profile: &AgentProfile) -> Result<(), Reject> {
    if let Some(level) = profile.thinking_level.as_deref() {
        if !THINKING_LEVELS.contains(&level) {
            return Err(Reject::new(
                "invalid_profile",
                format!("未知的 thinking_level '{level}'"),
            ));
        }
    }
    let budget_ok = match (profile.thinking_level.as_deref(), profile.thinking_budget) {
        (Some("budget"), Some(budget)) => budget > 0,
        (Some("budget"), None) => false,
        (Some(_), None) => true,
        (None, None) => true,
        _ => false,
    };
    if !budget_ok {
        return Err(Reject::new(
            "invalid_profile",
            "thinking_budget 必须是正整数，且仅当 thinking_level=budget 时可用",
        ));
    }
    for tool in &profile.disabled_tools {
        let valid = !tool.is_empty()
            && tool.len() <= 64
            && tool
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            return Err(Reject::new(
                "invalid_profile",
                format!("disabled_tools 中有不合法的工具名 '{tool}'"),
            ));
        }
    }
    Ok(())
}

/// Mirror of the host's `normalize_skill_dir_name`: any non-empty directory
/// name without path separators or control characters is installable. Do not
/// impose a lowercase-ASCII style here — existing agents legitimately carry
/// names such as `QA_source_index`, and the host preserves them.
fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && name != "."
        && name != ".."
        && name.len() <= 128
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(|c| c.is_control())
}

// ---------------------------------------------------------------- listings ---

/// GET /api/v1/agents
pub async fn list_agents(
    State(app): State<Shared>,
    tenant: TenantId,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let tenant = tenant.0;
    db::offload(&app, move |app| {
        let db = app.db();
        market::list_page(&db, &market::AGENT, &tenant, &q, &app.cfg.public_url)
    })
    .await
    .map(Json)
}

/// GET /api/v1/agents/mine
pub async fn list_mine(State(app): State<Shared>, tenant: TenantId) -> ApiResult<Json<Value>> {
    let tenant = tenant.0;
    db::offload(&app, move |app| {
        let db = app.db();
        market::list_owned(&db, &market::AGENT, &tenant, &app.cfg.public_url)
    })
    .await
    .map(Json)
}

/// GET /api/v1/agents/lookup?names=a,b — display name -> market slug + state.
pub async fn lookup_agents(
    State(app): State<Shared>,
    tenant: TenantId,
    Query(query): Query<LookupQuery>,
) -> ApiResult<Json<Value>> {
    let tenant = tenant.0;
    db::offload(&app, move |app| {
        let db = app.db();
        market::lookup(&db, &market::AGENT, &tenant, &query.names, &app.cfg.public_url)
    })
    .await
    .map(Json)
}

/// POST /api/v1/agents/validate — dry run of a publish, nothing is written.
///
/// The agent plane takes its name from the bundle's own manifest, so the gate
/// runs first: without a readable manifest there is no name to derive a slug
/// from, and "your manifest is broken" is the only useful answer.
pub async fn validate(
    State(app): State<Shared>,
    tenant: TenantId,
    Json(body): Json<PublishRequest>,
) -> ApiResult<Json<Value>> {
    db::offload(&app, move |app| validate_sync(app, &tenant, body))
        .await
        .map(Json)
}

fn validate_sync(app: &Shared, tenant: &TenantId, body: PublishRequest) -> ApiResult<Value> {
    let mut problems: Vec<market::Problem> = Vec::new();
    market::check_fields(&body.version, &body.visibility, &mut problems);

    let (name, manifest) = match check_agent_bundle(&body.files) {
        Ok(checked) => {
            let manifest = checked.manifest;
            let preview = json!({
                "name": manifest.name.trim(),
                "description": manifest.description.trim(),
                "language": manifest.language,
                "skills": manifest.skills,
            });
            (manifest.name.clone(), Some(preview))
        }
        Err(reject) => {
            problems.push(reject.into());
            (String::new(), None)
        }
    };

    let slug = if name.is_empty() {
        None
    } else {
        market::resolve_slug(
            &market::AGENT,
            app,
            &tenant.0,
            &name,
            body.slug.as_deref(),
            &mut problems,
        )
    };

    let (existing, next_version) = match slug.as_deref().filter(|slug| limits::valid_slug(slug)) {
        Some(slug) => {
            let db = app.db();
            market::inspect(&db, &market::AGENT, &tenant.0, slug, &body.version, &mut problems)?
        }
        None => (None, None),
    };

    debug!(
        tenant = %tenant.0,
        slug = slug.as_deref().unwrap_or("-"),
        files = body.files.len(),
        problems = problems.len(),
        "agent validate"
    );
    Ok(market::Answer {
        cfg: &app.cfg,
        name,
        version: body.version.clone(),
        visibility: body.visibility.clone(),
        slug,
        files: &body.files,
        manifest,
        existing,
        next_version,
        problems,
    }
    .to_json())
}

/// GET /api/v1/agents/{slug}/status
pub async fn agent_status(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    let tenant = tenant.0;
    db::offload(&app, move |app| {
        let db = app.db();
        market::status(&db, &market::AGENT, &tenant, &slug)
    })
    .await
    .map(Json)
}

// ---------------------------------------------------------------- publish ---

#[derive(Debug, Deserialize)]
pub struct PublishRequest {
    #[serde(default)]
    pub slug: Option<String>,
    pub version: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    #[serde(default)]
    pub publisher_name: Option<String>,
    pub files: BTreeMap<String, FileData>,
}

/// POST /api/v1/agents — publish an immutable agent version.
pub async fn publish(
    State(app): State<Shared>,
    tenant: TenantId,
    Json(body): Json<PublishRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    db::offload(&app, move |app| publish_sync(app, &tenant, body)).await
}

fn publish_sync(
    app: &Shared,
    tenant: &TenantId,
    body: PublishRequest,
) -> ApiResult<(StatusCode, Json<Value>)> {
    market::require_fields(&body.version, &body.visibility)?;

    let checked = match check_agent_bundle(&body.files) {
        Ok(checked) => checked,
        Err(reject) => {
            warn!(
                tenant = %tenant.0,
                version = %body.version,
                code = reject.code,
                reason = %reject.message,
                "agent publish rejected by gate"
            );
            return Err(reject.into());
        }
    };

    let publisher = clean_publisher_name(body.publisher_name.as_deref());
    let name = checked.manifest.name.trim().to_string();
    let description = checked.manifest.description.trim().to_string();
    // The name lives in the bundle, so the gate runs before the slug can be
    // derived from it.
    let slug = market::required_slug(
        &market::AGENT,
        app,
        &tenant.0,
        &name,
        body.slug.as_deref(),
    )?;

    let db = app.db();
    let actor = tenant.actor();
    market::record_tenant(&db, &tenant.0, publisher.as_deref())?;

    let previous = match market::listing(&db, &market::AGENT, &slug)? {
        Some(claim) => {
            claim.guard(
                &tenant.0,
                &format!("标识符 '{slug}' 已被其他团队占用"),
                "该智能体已被管理员下架，不能再发布",
            )?;
            claim.latest_version
        }
        None => None,
    };

    // Serialise before opening the transaction: a bundle that cannot be
    // written must abort the publish, not leave a row whose contents are an
    // empty string (versions are immutable, so that row would be permanent).
    let bundle_json = serde_json::to_string(&checked.files).map_err(|err| {
        error!(error = %err, "bundle serialization failed");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "智能体内容无法序列化",
        )
    })?;

    let detail = format!(
        "{} files, {} bytes, {} skills, visibility={}",
        checked.file_count, checked.size_bytes, checked.skill_count, body.visibility
    );
    let advances = market::store(
        &db,
        &market::AGENT,
        market::Publish {
            slug: &slug,
            tenant: &tenant.0,
            version: &body.version,
            visibility: &body.visibility,
            name: &name,
            description: Some(&description),
            file_count: checked.file_count,
            skill_count: Some(checked.skill_count as i64),
            size_bytes: checked.size_bytes,
            sha256: &checked.sha256,
            bundle_json,
            previous,
        },
        &detail,
        &actor,
    )?;

    info!(
        tenant = %tenant.0,
        slug = %slug,
        version = %body.version,
        visibility = %body.visibility,
        publisher = publisher.as_deref().unwrap_or("-"),
        files = checked.file_count,
        bytes = checked.size_bytes,
        skills = checked.skill_count,
        latest = advances.advanced,
        "agent published"
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "slug": slug,
            "version": body.version,
            "sha256": checked.sha256,
            "size_bytes": checked.size_bytes,
            "file_count": checked.file_count,
            "skill_count": checked.skill_count,
            "latest_version": advances.latest_version,
            "bundle_url": (market::AGENT.bundle_url)(&app.cfg.public_url, &slug, &body.version),
        })),
    ))
}

// -------------------------------------------------------------- withdraw ---

#[derive(Debug, Deserialize)]
pub struct WithdrawRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn withdraw(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
    body: Option<Json<WithdrawRequest>>,
) -> ApiResult<Json<Value>> {
    let reason = body.and_then(|Json(b)| b.reason);
    db::offload(&app, move |app| {
        let db = app.db();
        let claim = market::listing(&db, &market::AGENT, &slug)?
            .ok_or_else(|| ApiError::not_found("智能体不存在"))?;
        claim.guard(
            &tenant.0,
            "只有发布者所在团队才能下架",
            "该条目已被管理员下架",
        )?;
        let change = market::Change::Withdraw { reason };
        market::change_state(&db, &market::AGENT, &slug, change, &tenant.actor())
    })
    .await
    .map(Json)
}

pub async fn restore(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    db::offload(&app, move |app| {
        let db = app.db();
        let claim = market::listing(&db, &market::AGENT, &slug)?
            .ok_or_else(|| ApiError::not_found("智能体不存在"))?;
        claim.guard(
            &tenant.0,
            "只有发布者所在团队才能重新上架",
            "该智能体由管理员下架，不能自行恢复上架",
        )?;
        market::change_state(&db, &market::AGENT, &slug, market::Change::Restore, &tenant.actor())
    })
    .await
    .map(Json)
}

// ----------------------------------------------------------------- bundle ---

/// GET /api/v1/agents/{slug}/{version}/bundle
pub async fn fetch_bundle(
    State(app): State<Shared>,
    tenant: TenantId,
    Path((slug, version)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    db::offload(&app, move |app| {
        let db = app.db();
        let payload = market::bundle(&db, &market::AGENT, &app.cfg, &tenant.0, &slug, &version)?;
        Ok(json!({
            "name": payload.name,
            "description": payload.description,
            "version": payload.version,
            "files": payload.files,
            "source_url": payload.source_url,
        }))
    })
    .await
    .map(Json)
}

// ------------------------------------------------------------------ admin ---

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "fifty")]
    pub limit: i64,
}

/// GET /admin/api/agents
pub async fn admin_list(
    State(app): State<Shared>,
    _admin: Admin,
    Query(query): Query<AdminListQuery>,
) -> ApiResult<Json<Value>> {
    let limit = query.limit.clamp(1, 200);
    db::offload(&app, move |app| {
        let db = app.db();
        market::list_admin(&db, &market::AGENT, &query.q, limit, &app.cfg.public_url)
    })
    .await
    .map(Json)
}

#[derive(Debug, Deserialize)]
pub struct TakedownRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// POST /admin/api/agents/{slug}/takedown
pub async fn admin_takedown(
    State(app): State<Shared>,
    admin: Admin,
    Path(slug): Path<String>,
    body: Option<Json<TakedownRequest>>,
) -> ApiResult<Json<Value>> {
    let reason = body
        .and_then(|Json(b)| b.reason)
        .filter(|r| !r.trim().is_empty());
    db::offload(&app, move |app| {
        let db = app.db();
        let change = market::Change::Takedown { reason };
        market::change_state(&db, &market::AGENT, &slug, change, &admin.actor())
    })
    .await
    .map(Json)
}

/// POST /admin/api/agents/{slug}/restore
pub async fn admin_restore(
    State(app): State<Shared>,
    admin: Admin,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    db::offload(&app, move |app| {
        let db = app.db();
        market::change_state(&db, &market::AGENT, &slug, market::Change::Lift, &admin.actor())
    })
    .await
    .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(manifest: Value, extra: &[(&str, &str)]) -> BTreeMap<String, FileData> {
        let mut files = BTreeMap::new();
        files.insert(
            AGENT_MANIFEST.to_string(),
            FileData::from(manifest.to_string()),
        );
        for (path, content) in extra {
            files.insert((*path).to_string(), FileData::from(*content));
        }
        files
    }

    fn manifest() -> Value {
        json!({
            "schema": 1,
            "name": "QA Agent",
            "description": "demo",
            "language": "zh",
            "profile": { "thinking_level": "medium", "plan_enabled": false,
                         "disabled_tools": ["browser"] },
            "md_files": ["SOUL.md"],
            "skills": ["demo-skill"]
        })
    }

    const SKILL: &str = "---\nname: demo-skill\ndescription: a demo\n---\n";

    #[test]
    fn accepts_a_well_formed_bundle() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("skills/demo-skill/SKILL.md", SKILL),
                ("skills/demo-skill/scripts/run.py", "print(1)\n"),
            ],
        );
        let checked = check_agent_bundle(&files).expect("valid bundle");
        assert_eq!(checked.manifest.name, "QA Agent");
        assert_eq!(checked.skill_count, 1);
        assert_eq!(checked.file_count, 4);
    }

    #[test]
    fn rejects_missing_manifest() {
        let files = BTreeMap::from([("SOUL.md".to_string(), FileData::from("# soul\n"))]);
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "missing_manifest"
        );
    }

    #[test]
    fn rejects_undeclared_skill_files() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("skills/demo-skill/SKILL.md", SKILL),
                ("skills/other/run.py", "print(1)\n"),
            ],
        );
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "undeclared_skill"
        );
    }

    #[test]
    fn rejects_unexpected_root_files() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("MEMORY.md", "private\n"),
                ("skills/demo-skill/SKILL.md", SKILL),
            ],
        );
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "unexpected_root_file"
        );
    }

    #[test]
    fn rejects_missing_skill_frontmatter() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("skills/demo-skill/SKILL.md", "# no frontmatter\n"),
            ],
        );
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "invalid_manifest"
        );
    }

    #[test]
    fn rejects_path_traversal() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("skills/demo-skill/SKILL.md", SKILL),
                ("skills/demo-skill/../escape.py", "print(1)\n"),
            ],
        );
        assert_eq!(check_agent_bundle(&files).unwrap_err().code, "traversal");
    }

    #[test]
    fn rejects_bad_thinking_budget() {
        let mut manifest = manifest();
        manifest["profile"] = json!({ "thinking_level": "medium", "thinking_budget": 100 });
        let files = bundle(manifest, &[("SOUL.md", "# soul\n")]);
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "invalid_profile"
        );
    }
}
