//! Tenant-facing API. Every route here is called by the QwenPaw plugin
//! (browser → plugin route → backend), so `X-Tenant-Id` is always present.
//!
//! `ApiError`/`ApiResult` live in `crate::error` and the request shapes in
//! `crate::dto`: this module is one of four that returns them, not their owner.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tracing::{debug, error, info, warn};

use crate::auth::TenantId;
use crate::db;
use crate::dto::{default_visibility, ListQuery, LookupQuery};
use crate::error::{ApiError, ApiResult};
use crate::http;
use crate::limits::{self, FileData, MANIFEST};
use crate::market;
use crate::slug::{clean_publisher_name, derive_slug};
use crate::state::Shared;

/// GET /api/v1/limits — the plugin mirrors these instead of hard-coding them.
pub async fn get_limits(State(app): State<Shared>) -> Json<Value> {
    let l = &app.cfg.limits;
    Json(json!({
        "max_bundle_bytes": l.max_bundle_bytes,
        "max_files": l.max_files,
        "max_file_bytes": l.max_file_bytes,
        "manifest": MANIFEST,
    }))
}

/// GET /api/v1/skills — market listing: public + own private, active only.
pub async fn list_skills(
    State(app): State<Shared>,
    tenant: TenantId,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let tenant = tenant.0;
    db::offload(&app, move |app| {
        let db = app.db();
        market::list_page(app, &db, &market::SKILL, &tenant, &q, &app.cfg.public_url)
    })
    .await
    .map(Json)
}

/// GET /api/v1/skills/mine — everything the tenant owns, including withdrawn.
pub async fn list_mine(State(app): State<Shared>, tenant: TenantId) -> ApiResult<Json<Value>> {
    let tenant = tenant.0;
    db::offload(&app, move |app| {
        let db = app.db();
        market::list_owned(&db, &market::SKILL, &tenant, &app.cfg.public_url)
    })
    .await
    .map(Json)
}

/// GET /api/v1/skills/{slug}/status — drives the "已下架" badge on installs.
pub async fn skill_status(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    let tenant = tenant.0;
    db::offload(&app, move |app| {
        let db = app.db();
        market::status(&db, &market::SKILL, &tenant, &slug)
    })
    .await
    .map(Json)
}

#[derive(Debug, Deserialize)]
pub struct PublishRequest {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub version: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    /// Optional publisher name, resolved by the plugin from the tenant's own
    /// console login. Cosmetic: it labels the listing, it grants nothing.
    #[serde(default)]
    pub publisher_name: Option<String>,
    pub files: BTreeMap<String, FileData>,
}

#[derive(Debug, Deserialize)]
pub struct SlugQuery {
    #[serde(default)]
    pub name: String,
}

/// GET /api/v1/slug?name=... — the identifier a publish would derive.
///
/// The publish page calls this to prefill 标识符 (Chinese names included), so the
/// derivation lives in exactly one place: here. `slug` is `null` when the name
/// yields nothing usable; the page then leaves the field empty.
pub async fn preview_slug(Query(query): Query<SlugQuery>) -> Json<Value> {
    Json(json!({ "name": query.name, "slug": derive_slug(&query.name) }))
}

/// GET /api/v1/skills/lookup?names=a,b — display name -> market slug + state.
///
/// The host stores hub installs under the *display* name and its skill list
/// does not expose where a skill came from, so the plugin needs this to map an
/// installed skill back to our slug. Unlike the market listing this includes
/// withdrawn and taken-down skills: that is the whole point.
pub async fn lookup_skills(
    State(app): State<Shared>,
    tenant: TenantId,
    Query(query): Query<LookupQuery>,
) -> ApiResult<Json<Value>> {
    let tenant = tenant.0;
    db::offload(&app, move |app| {
        let db = app.db();
        market::lookup(
            &db,
            &market::SKILL,
            &tenant,
            &query.names,
            &app.cfg.public_url,
        )
    })
    .await
    .map(Json)
}

/// POST /api/v1/skills — publish an immutable version.
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
    let publisher = clean_publisher_name(body.publisher_name.as_deref());
    market::require_fields(&body.version, &body.visibility)?;
    let slug = market::required_slug(
        &market::SKILL,
        app,
        &tenant.0,
        &body.name,
        body.slug.as_deref(),
    )?;

    let checked = match limits::check_bundle(&body.name, &body.files, &app.cfg.limits) {
        Ok(checked) => checked,
        Err(reject) => {
            warn!(
                tenant = %tenant.0,
                slug = %slug,
                version = %body.version,
                code = reject.code,
                reason = %reject.message,
                "publish rejected by gate"
            );
            return Err(reject.into());
        }
    };

    // Serialise before opening the transaction: a bundle that cannot be
    // written must abort the publish, not leave a row whose contents are an
    // empty string (versions are immutable, so that row would be permanent).
    let bundle_json = serde_json::to_string(&checked.files).map_err(|err| {
        error!(error = %err, "bundle serialization failed");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "技能内容无法序列化",
        )
    })?;

    let db = app.db();
    let actor = tenant.actor();
    market::record_tenant(&db, &tenant.0, publisher.as_deref())?;

    // An existing row means either our own entry or someone else's slug: the
    // guard is the same one withdraw/restore use, so a publish cannot slip past
    // a takedown or steal another team's identifier.
    let previous = match market::listing(&db, &market::SKILL, &slug)? {
        Some(claim) => {
            claim.guard(
                &tenant.0,
                &format!("标识符 '{slug}' 已被其他团队占用"),
                "该技能已被管理员下架，不能再发布",
            )?;
            claim.latest_version
        }
        None => None,
    };

    let file_count = checked.files.len();
    let detail = format!(
        "{file_count} files, {} bytes, visibility={}",
        checked.size_bytes, body.visibility
    );
    let advances = market::store(
        &db,
        &market::SKILL,
        market::Publish {
            slug: &slug,
            tenant: &tenant.0,
            version: &body.version,
            visibility: &body.visibility,
            name: &checked.name,
            description: None,
            manifest_text: checked
                .files
                .get(limits::MANIFEST)
                .and_then(|file| file.as_text()),
            file_count,
            skill_count: None,
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
        files = file_count,
        bytes = checked.size_bytes,
        sha256 = %checked.sha256,
        latest = advances.advanced,
        "published"
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "slug": slug,
            "version": body.version,
            "sha256": checked.sha256,
            "size_bytes": checked.size_bytes,
            "file_count": file_count,
            "latest_version": advances.latest_version,
            "install_url": (market::SKILL.bundle_url)(&app.cfg.public_url, &slug, &body.version),
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct WithdrawRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// POST /api/v1/skills/validate — dry run of a publish.
///
/// The publish form calls this on the archive the user just picked, so every
/// problem (bundle gate, slug, version collision, ownership) shows up as a list
/// before they hit 发布 instead of as one 4xx after it. Read-only: no rows are
/// written, nothing is audited, downloads do not move.
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
    let slug = market::resolve_slug(
        &market::SKILL,
        app,
        &tenant.0,
        &body.name,
        body.slug.as_deref(),
        &mut problems,
    );

    let (_checked, rejects) = limits::review_bundle(&body.name, &body.files, &app.cfg.limits);
    problems.extend(rejects.into_iter().map(market::Problem::from));

    // Slate-level checks: only meaningful once the slug is a real string, and
    // the same rules the publish path applies.
    let (existing, next_version) = match slug.as_deref().filter(|slug| limits::valid_slug(slug)) {
        Some(slug) => {
            let db = app.db();
            market::inspect(
                &db,
                &market::SKILL,
                &tenant.0,
                slug,
                &body.version,
                &mut problems,
            )?
        }
        None => (None, None),
    };

    // A manifest that parses is worth showing: the page previews the name and
    // description the host will display after install.
    let manifest = body
        .files
        .get(MANIFEST)
        .and_then(|content| content.as_text())
        .and_then(|text| limits::manifest_meta(text).ok())
        .map(|(name, description)| json!({ "name": name, "description": description }));

    debug!(
        tenant = %tenant.0,
        slug = slug.as_deref().unwrap_or("-"),
        files = body.files.len(),
        problems = problems.len(),
        "validate"
    );
    Ok(market::Answer {
        cfg: &app.cfg,
        name: body.name.clone(),
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

/// POST /api/v1/skills/{slug}/withdraw — owner may pull its own skill.
pub async fn withdraw(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
    body: Option<Json<WithdrawRequest>>,
) -> ApiResult<Json<Value>> {
    let reason = body.and_then(|Json(b)| b.reason);
    db::offload(&app, move |app| {
        let db = app.db();
        let claim = market::listing(&db, &market::SKILL, &slug)?
            .ok_or_else(|| ApiError::not_found("技能不存在"))?;
        claim.guard(
            &tenant.0,
            "只有发布者所在团队才能下架",
            "该条目已被管理员下架",
        )?;
        let change = market::Change::Withdraw { reason };
        market::change_state(&db, &market::SKILL, &slug, change, &tenant.actor())
    })
    .await
    .map(Json)
}

/// POST /api/v1/skills/{slug}/restore — reversible unless an admin took it down.
pub async fn restore(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    db::offload(&app, move |app| {
        let db = app.db();
        let claim = market::listing(&db, &market::SKILL, &slug)?
            .ok_or_else(|| ApiError::not_found("技能不存在"))?;
        claim.guard(
            &tenant.0,
            "只有发布者所在团队才能重新上架",
            "该技能由管理员下架，不能自行恢复上架",
        )?;
        market::change_state(
            &db,
            &market::SKILL,
            &slug,
            market::Change::Restore,
            &tenant.actor(),
        )
    })
    .await
    .map(Json)
}

/// GET /skills/{slug}/{version} (alias of the bundle route) — this is the URL
/// the QwenPaw `MarketSpec.fetch_bundle` is handed as `bundle_url`.
pub async fn fetch_bundle(
    State(app): State<Shared>,
    tenant: TenantId,
    Path((slug, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    db::offload(&app, move |app| {
        let db = app.db();
        let payload = market::bundle(app, &db, &market::SKILL, &tenant.0, &slug, &version)?;
        // A revalidation is not an install: the counter is the only number a
        // publisher sees, so only a real transfer moves it.
        if !http::is_revalidated(&headers, &payload.sha256) {
            app.pending().add_download(market::SKILL.table, &slug);
        }
        // A skill payload carries no description: the manifest is the contract.
        let body = json!({
            "name": payload.name,
            "version": payload.version,
            "files": payload.files,
            "source_url": payload.source_url,
        });
        http::immutable_json(
            &body,
            &payload.sha256,
            payload.visibility == "public",
            &headers,
        )
    })
    .await
}

// ------------------------------------------------------------------ helpers ---
