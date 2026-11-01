//! Cross-plane contract tests: the skill market and the agent market are the
//! same market with a different object type, so almost every behaviour must be
//! identical. These tests run one set of assertions over both planes and are
//! the safety net for merging the two near-duplicate implementations
//! (`src/api.rs` / `src/agents.rs`): after the merge, the same suite must stay
//! green, and any behaviour that only held on one plane shows up here.

mod common;

use axum::http::StatusCode;
use base64ct::{Base64, Encoding};
use common::{agent_files, field_names, find, skill_files, slugs, Harness};
use serde_json::{json, Value};

const PUBLIC_URL: &str = "http://market.test";

#[derive(Clone, Copy, Debug)]
enum Kind {
    Skill,
    Agent,
}

impl Kind {
    const ALL: [Kind; 2] = [Kind::Skill, Kind::Agent];

    fn table(self) -> &'static str {
        match self {
            Kind::Skill => "skills",
            Kind::Agent => "agents",
        }
    }

    fn list(self) -> &'static str {
        match self {
            Kind::Skill => "/api/v1/skills",
            Kind::Agent => "/api/v1/agents",
        }
    }

    fn mine(self) -> String {
        format!("{}/mine", self.list())
    }

    fn action(self, slug: &str, action: &str) -> String {
        format!("{}/{slug}/{action}", self.list())
    }

    fn bundle(self, slug: &str, version: &str) -> String {
        format!("{}/{slug}/{version}/bundle", self.list())
    }

    /// The `source_url` the host records for an install, and the URL the
    /// listing advertises — the two must be the same address.
    fn install_url(self, slug: &str, version: &str) -> String {
        match self {
            // A skill is served through the short alias the host fetches.
            Kind::Skill => format!("{PUBLIC_URL}/skills/{slug}/{version}"),
            Kind::Agent => format!("{PUBLIC_URL}/api/v1/agents/{slug}/{version}/bundle"),
        }
    }

    /// Files a plain publish accepts on this plane.
    fn files(self) -> Value {
        match self {
            Kind::Skill => skill_files(),
            Kind::Agent => agent_files("QA Agent", "面向配置问答的智能体"),
        }
    }

    async fn publish(
        self,
        h: &Harness,
        tenant: &str,
        slug: &str,
        version: &str,
        visibility: &str,
    ) -> (StatusCode, Value) {
        match self {
            Kind::Skill => h.publish_skill(tenant, slug, version, visibility).await,
            Kind::Agent => h.publish_agent(tenant, slug, version, visibility).await,
        }
    }

    async fn publish_files(
        self,
        h: &Harness,
        tenant: &str,
        slug: &str,
        version: &str,
        files: Value,
    ) -> (StatusCode, Value) {
        match self {
            Kind::Skill => {
                h.publish_skill_with(
                    tenant,
                    json!({
                        "name": format!("Skill {slug}"),
                        "slug": slug,
                        "version": version,
                        "visibility": "public",
                        "files": files
                    }),
                )
                .await
            }
            Kind::Agent => {
                h.publish_agent_with(
                    tenant,
                    json!({
                        "slug": slug,
                        "version": version,
                        "visibility": "public",
                        "publisher_name": "张三",
                        "files": files
                    }),
                )
                .await
            }
        }
    }

    /// Publishes under a display name of our choosing: the lookup endpoint is
    /// keyed on the display name, so the test has to control it.
    async fn publish_named(
        self,
        h: &Harness,
        tenant: &str,
        slug: &str,
        version: &str,
        name: &str,
        visibility: &str,
    ) -> (StatusCode, Value) {
        match self {
            Kind::Skill => {
                h.publish_skill_with(
                    tenant,
                    json!({
                        "name": name,
                        "slug": slug,
                        "version": version,
                        "visibility": visibility,
                        "files": { "SKILL.md": format!(
                            "---\nname: {name}\ndescription: {name} 的描述\n---\n\n# {name}\n"
                        )}
                    }),
                )
                .await
            }
            Kind::Agent => {
                h.publish_agent_with(
                    tenant,
                    json!({
                        "slug": slug,
                        "version": version,
                        "visibility": visibility,
                        "files": agent_files(name, &format!("{name} 的描述"))
                    }),
                )
                .await
            }
        }
    }

    /// The field carrying the fetchable URL of the latest version.
    fn bundle_field(self) -> &'static str {
        match self {
            Kind::Skill => "install_url",
            Kind::Agent => "bundle_url",
        }
    }

    async fn admin(&self, h: &Harness, slug: &str, action: &str) -> (StatusCode, Value) {
        let cookie = h.login().await;
        let (status, body, _) = h
            .call(
                "POST",
                &format!("/admin/api/{}/{slug}/{action}", self.table()),
                None,
                Some(json!({ "reason": "policy" })),
                Some(&cookie),
            )
            .await;
        (status, body)
    }
}

/// Fields every listing item carries, on both planes.
const SHARED_FIELDS: [&str; 15] = [
    "created_at",
    "description",
    "download_count",
    "latest_version",
    "name",
    "owner_name",
    "owner_tenant",
    "size_bytes",
    "slug",
    "takedown_at",
    "takedown_reason",
    "updated_at",
    "versions",
    "visibility",
    "withdrawn_at",
];

/// The only fields a plane is allowed to add on top of the shared set. This is
/// the whole intentional difference between a skill row and an agent row.
fn plane_only_fields(kind: Kind) -> &'static [&'static str] {
    match kind {
        // A skill installs from a plain bundle URL ...
        Kind::Skill => &["install_url"],
        // ... an agent ships skill_count in its listing and a bundle endpoint.
        Kind::Agent => &["bundle_url", "skill_count"],
    }
}

fn expected_fields(kind: Kind) -> Vec<String> {
    let mut fields: Vec<String> = SHARED_FIELDS.iter().map(|f| f.to_string()).collect();
    fields.extend(plane_only_fields(kind).iter().map(|f| f.to_string()));
    fields.sort();
    fields
}

// ---------------------------------------------------------------------------
// listing shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn listing_payload_has_a_fixed_shape() {
    let mut seen = Vec::new();
    for kind in Kind::ALL {
        let h = Harness::new();
        let (status, body) = kind.publish(&h, "tenant-a", "alpha", "1.0.0", "public").await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");

        let (status, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {listing}");
        assert_eq!(
            field_names(&listing),
            ["items", "limit", "page", "sort", "total"],
            "{kind:?} envelope"
        );
        assert_eq!(listing["page"], 1, "{kind:?} default page");
        assert_eq!(listing["limit"], 20, "{kind:?} default limit");

        let item = find(&listing, "alpha");
        assert_eq!(field_names(item), expected_fields(kind), "{kind:?} item");

        // Values that the merged implementation rewrites along with the LIST
        // query are pinned here so a refactor cannot quietly change them.
        assert_eq!(item["slug"], "alpha");
        assert_eq!(item["owner_tenant"], "tenant-a");
        assert_eq!(item["visibility"], "public");
        assert_eq!(item["latest_version"], "1.0.0");
        assert_eq!(item["versions"], 1);
        assert!(item["size_bytes"].as_i64().unwrap() > 0);
        assert_eq!(item["download_count"], 0);
        assert!(item["withdrawn_at"].is_null());
        assert!(item["takedown_at"].is_null());
        assert!(item["created_at"].as_i64().unwrap() > 0);
        assert!(item["updated_at"].as_i64().unwrap() > 0);

        seen.push(field_names(item));
    }

    // The delta between the two planes, stated once: a field added to one side
    // of a future refactor must be added to this list or it fails here.
    let skills_only: Vec<&String> = seen[0].iter().filter(|f| !seen[1].contains(f)).collect();
    let agents_only: Vec<&String> = seen[1].iter().filter(|f| !seen[0].contains(f)).collect();
    assert_eq!(skills_only, ["install_url"]);
    assert_eq!(agents_only, ["bundle_url", "skill_count"]);
}

/// The search box is text, not a pattern: `%` and `_` have to match themselves
/// instead of quietly turning every query into "match everything".
#[tokio::test]
async fn search_treats_wildcards_as_text() {
    for kind in Kind::ALL {
        let h = Harness::new();
        let (status, body) = kind.publish(&h, "tenant-a", "alpha", "1.0.0", "public").await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");

        for wildcard in ["%25", "_", "%25%25"] {
            let uri = format!("{}?q={wildcard}", kind.list());
            let (status, listing, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
            assert_eq!(status, StatusCode::OK, "{kind:?}: {listing}");
            assert_eq!(
                listing["total"], 0,
                "{kind:?} `{uri}` matched everything: {listing}"
            );
        }

        let uri = format!("{}?q=alp", kind.list());
        let (status, listing, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {listing}");
        assert_eq!(listing["total"], 1, "{kind:?} plain search broke: {listing}");
    }
}

#[tokio::test]
async fn pagination_counts_every_row_but_returns_one_page() {
    for kind in Kind::ALL {
        let h = Harness::new();
        for slug in ["alpha", "beta", "gamma"] {
            let (status, body) = kind.publish(&h, "tenant-a", slug, "1.0.0", "public").await;
            assert_eq!(status, StatusCode::CREATED, "{kind:?} {slug}: {body}");
        }

        let uri = format!("{}?limit=2&page=1", kind.list());
        let (status, page, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        assert_eq!(page["total"], 3, "{kind:?} total counts every row");
        assert_eq!(page["items"].as_array().unwrap().len(), 2, "{kind:?}");
        assert_eq!((page["page"].as_i64(), page["limit"].as_i64()), (Some(1), Some(2)));

        let uri = format!("{}?limit=2&page=2", kind.list());
        let (_, page, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
        assert_eq!(page["total"], 3, "{kind:?}");
        assert_eq!(page["items"].as_array().unwrap().len(), 1, "{kind:?}");

        // Past the end: an empty page, not an error and not a full table.
        let uri = format!("{}?limit=2&page=9", kind.list());
        let (status, page, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        assert_eq!(page["total"], 3, "{kind:?}");
        assert_eq!(page["items"].as_array().unwrap().len(), 0, "{kind:?}");

        // `limit` is clamped, never trusted as-is.
        let uri = format!("{}?limit=5000", kind.list());
        let (_, page, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
        assert_eq!(page["limit"], 50, "{kind:?} limit clamp");
        assert_eq!(page["total"], 3, "{kind:?}");

        // A page number that cannot become an offset is the caller's mistake:
        // say so, instead of panicking or quietly serving page one.
        for page in ["0", "-3", "9223372036854775807"] {
            let uri = format!("{}?page={page}", kind.list());
            let (status, body, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{kind:?} page={page}: {body}");
            assert_eq!(body["error"]["code"], "invalid_page", "{kind:?} page={page}");
        }
        // Not a number at all: rejected before any query runs.
        let uri = format!("{}?page=later", kind.list());
        let (status, _, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{kind:?}");
    }
}

#[tokio::test]
async fn listing_is_newest_first_and_sort_keys_are_whitelisted() {
    for kind in Kind::ALL {
        let h = Harness::new();
        for slug in ["alpha", "beta", "gamma"] {
            kind.publish(&h, "tenant-a", slug, "1.0.0", "public").await;
        }
        // `created_at` is whole seconds, so the fixture sets it explicitly
        // instead of sleeping.
        for (slug, at) in [("alpha", 1_000), ("beta", 2_000), ("gamma", 3_000)] {
            h.exec(&format!(
                "UPDATE {} SET created_at = {at} WHERE slug = '{slug}'",
                kind.table()
            ));
        }

        let (_, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        assert_eq!(slugs(&listing), ["gamma", "beta", "alpha"], "{kind:?}");
        assert_eq!(listing["sort"], "created", "{kind:?} default sort");

        let uri = format!("{}?sort=downloads", kind.list());
        let (_, listing, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
        assert_eq!(listing["sort"], "downloads", "{kind:?}");

        // Anything not on the whitelist falls back to the default and is
        // echoed back normalised — never interpolated into the SQL.
        for sort in ["DROP%20TABLE", "", "created_at%20DESC"] {
            let uri = format!("{}?sort={sort}", kind.list());
            let (status, listing, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
            assert_eq!(status, StatusCode::OK, "{kind:?} sort={sort}");
            assert_eq!(listing["sort"], "created", "{kind:?} sort={sort}");
            assert_eq!(listing["total"], 3, "{kind:?} sort={sort}");
        }
    }
}

#[tokio::test]
async fn mine_shows_withdrawn_but_never_other_tenants() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "kept", "1.0.0", "public").await;
        kind.publish(&h, "tenant-a", "gone", "1.0.0", "public").await;
        kind.publish(&h, "tenant-b", "foreign", "1.0.0", "public").await;

        let (status, body, _) = h
            .call(
                "POST",
                &kind.action("gone", "withdraw"),
                Some("tenant-a"),
                Some(json!({ "reason": "retired" })),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");

        let (status, mine, _) = h
            .call("GET", &kind.mine(), Some("tenant-a"), None, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {mine}");
        assert_eq!(field_names(&mine), ["items"], "{kind:?} mine envelope");
        let mut owned = slugs(&mine);
        owned.sort();
        assert_eq!(owned, ["gone", "kept"], "{kind:?} mine contents");

        // The withdrawn entry stays visible to its owner, with the flag set.
        let gone = find(&mine, "gone");
        assert!(gone["withdrawn_at"].as_i64().unwrap() > 0, "{kind:?}");

        // ... and is absent from the market listing another tenant sees.
        let (_, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        let mut market = slugs(&listing);
        market.sort();
        assert_eq!(market, ["foreign", "kept"], "{kind:?} market listing");
    }
}

// ---------------------------------------------------------------------------
// download
// ---------------------------------------------------------------------------

#[tokio::test]
async fn download_roundtrips_every_file_byte_for_byte() {
    for kind in Kind::ALL {
        let h = Harness::new();
        let binary = [0x00u8, 0x01, 0x80, 0xff];
        let mut files = kind.files();
        let binary_path = match kind {
            Kind::Skill => "assets/logo.bin",
            Kind::Agent => "skills/demo-skill/logo.bin",
        };
        files[binary_path] = json!({ "base64": Base64::encode_string(&binary) });

        let (status, body) = kind
            .publish_files(&h, "tenant-a", "alpha", "1.0.0", files.clone())
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");

        let uri = kind.bundle("alpha", "latest");
        let (status, bytes) = h.download(&uri, Some("tenant-b")).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        let bundle: Value = serde_json::from_slice(&bytes).expect("bundle json");

        // Every published file comes back, unchanged: text as text, bytes as
        // base64 (the same wire shape the host decodes).
        assert_eq!(bundle["files"], files, "{kind:?} files");
        assert_eq!(bundle["version"], "1.0.0", "{kind:?}");
        assert_eq!(
            bundle["files"][binary_path]["base64"],
            Base64::encode_string(&binary),
            "{kind:?} {binary_path}"
        );

        // The URL the host records is the URL it just fetched, and it is the
        // same address the listing advertises.
        assert_eq!(
            bundle["source_url"],
            kind.install_url("alpha", "1.0.0"),
            "{kind:?}"
        );

        // A pinned version and `latest` must serve the same bytes.
        let (status, pinned) = h.download(&kind.bundle("alpha", "1.0.0"), Some("tenant-b")).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        assert_eq!(pinned, bytes, "{kind:?} latest == pinned");
    }
}

#[tokio::test]
async fn install_urls_point_at_a_fetchable_bundle() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "alpha", "2.1.3", "public").await;
        let (_, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        let item = find(&listing, "alpha");

        let field = match kind {
            Kind::Skill => "install_url",
            Kind::Agent => "bundle_url",
        };
        let expected = kind.install_url("alpha", "2.1.3");
        assert_eq!(item[field], expected.as_str(), "{kind:?}");

        // Whatever the listing advertises must answer without a rewrite.
        let path = expected.trim_start_matches(PUBLIC_URL);
        let (status, advertised) = h.download(path, Some("tenant-b")).await;
        assert_eq!(status, StatusCode::OK, "{kind:?} advertised url {path}");

        let (_, long_form) = h.download(&kind.bundle("alpha", "2.1.3"), Some("tenant-b")).await;
        assert_eq!(advertised, long_form, "{kind:?} alias == long form");
    }
}

// ---------------------------------------------------------------------------
// error taxonomy
// ---------------------------------------------------------------------------

/// One table for both planes: which failure looks like what. The `fetch_bundle`
/// of a merged implementation has to reproduce every row verbatim — including
/// the deliberate split between "gone" (410, the entry used to exist) and
/// "not found" (404, that version never existed), and the 403 that a private
/// entry answers with.
#[tokio::test]
async fn bundle_failures_have_one_taxonomy() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "open", "1.0.0", "public").await;
        kind.publish(&h, "tenant-a", "secret", "1.0.0", "tenant").await;
        kind.publish(&h, "tenant-a", "retired", "1.0.0", "public").await;
        kind.publish(&h, "tenant-a", "banned", "1.0.0", "public").await;

        let (status, body, _) = h
            .call(
                "POST",
                &kind.action("retired", "withdraw"),
                Some("tenant-a"),
                Some(json!({ "reason": "retired" })),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
        let (status, body) = kind.admin(&h, "banned", "takedown").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");

        struct Case {
            what: &'static str,
            tenant: &'static str,
            slug: &'static str,
            version: &'static str,
            status: StatusCode,
            code: &'static str,
        }
        // StatusCode is a struct with associated constants, so bind them by hand.
        const OK: StatusCode = StatusCode::OK;
        const GONE: StatusCode = StatusCode::GONE;
        const NOT_FOUND: StatusCode = StatusCode::NOT_FOUND;
        const FORBIDDEN: StatusCode = StatusCode::FORBIDDEN;
        let cases = [
            Case {
                what: "public, other tenant",
                tenant: "tenant-b",
                slug: "open",
                version: "latest",
                status: OK,
                code: "",
            },
            Case {
                what: "public, unknown slug",
                tenant: "tenant-b",
                slug: "nope",
                version: "latest",
                status: GONE,
                code: "gone",
            },
            Case {
                what: "public, unknown version",
                tenant: "tenant-b",
                slug: "open",
                version: "9.9.9",
                status: NOT_FOUND,
                code: "not_found",
            },
            Case {
                what: "private, other tenant",
                tenant: "tenant-b",
                slug: "secret",
                version: "latest",
                status: FORBIDDEN,
                code: "forbidden",
            },
            Case {
                what: "private, owner",
                tenant: "tenant-a",
                slug: "secret",
                version: "latest",
                status: OK,
                code: "",
            },
            Case {
                what: "withdrawn, other tenant",
                tenant: "tenant-b",
                slug: "retired",
                version: "latest",
                status: GONE,
                code: "withdrawn",
            },
            Case {
                what: "withdrawn, owner",
                tenant: "tenant-a",
                slug: "retired",
                version: "latest",
                status: OK,
                code: "",
            },
            // A takedown is not reversible by the owner: it outranks ownership.
            Case {
                what: "taken down, owner",
                tenant: "tenant-a",
                slug: "banned",
                version: "latest",
                status: GONE,
                code: "taken_down",
            },
            Case {
                what: "taken down, other tenant",
                tenant: "tenant-b",
                slug: "banned",
                version: "latest",
                status: GONE,
                code: "taken_down",
            },
        ];

        for case in cases {
            let (status, bytes) = h
                .download(&kind.bundle(case.slug, case.version), Some(case.tenant))
                .await;
            assert_eq!(status, case.status, "{kind:?} {}: {}", case.what, String::from_utf8_lossy(&bytes));
            if case.status.is_success() {
                continue;
            }
            let body: Value = serde_json::from_slice(&bytes).expect("error json");
            assert_eq!(
                field_names(&body),
                ["error"],
                "{kind:?} {}: {body}",
                case.what
            );
            assert_eq!(body["error"]["code"], case.code, "{kind:?} {}", case.what);
            assert!(
                body["error"]["message"].as_str().is_some_and(|m| !m.is_empty()),
                "{kind:?} {}: {body}",
                case.what
            );
        }
    }
}

/// The status endpoint is the plugin's "is there an update" probe. It hides
/// entries the caller may not see (404, not 403) and answers a plain state word
/// otherwise — different from the bundle endpoint on purpose, and the same on
/// both planes.
#[tokio::test]
async fn status_endpoint_reports_state_and_hides_foreign_entries() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "open", "1.0.0", "public").await;
        kind.publish(&h, "tenant-a", "secret", "1.0.0", "tenant").await;
        kind.publish(&h, "tenant-a", "retired", "2.0.0", "public").await;
        kind.publish(&h, "tenant-a", "banned", "1.0.0", "public").await;

        let (status, body, _) = h
            .call(
                "POST",
                &kind.action("retired", "withdraw"),
                Some("tenant-a"),
                Some(json!({})),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
        let (status, body) = kind.admin(&h, "banned", "takedown").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");

        let cases = [
            ("open", "tenant-b", StatusCode::OK, "active"),
            ("retired", "tenant-b", StatusCode::OK, "withdrawn"),
            ("banned", "tenant-b", StatusCode::OK, "taken_down"),
            // Not visible to a stranger: no existence leak.
            ("secret", "tenant-b", StatusCode::NOT_FOUND, ""),
            ("nope", "tenant-b", StatusCode::NOT_FOUND, ""),
            // The owner sees its own private entry.
            ("secret", "tenant-a", StatusCode::OK, "active"),
        ];
        for (slug, tenant, expected, state) in cases {
            let (status, body, _) = h
                .call(
                    "GET",
                    &kind.action(slug, "status"),
                    Some(tenant),
                    None,
                    None,
                )
                .await;
            assert_eq!(status, expected, "{kind:?} {slug} for {tenant}: {body}");
            if !expected.is_success() {
                continue;
            }
            assert_eq!(field_names(&body), ["latest_version", "name", "slug", "state", "takedown_reason"], "{kind:?}");
            assert_eq!(body["slug"], slug, "{kind:?}");
            assert_eq!(body["state"], state, "{kind:?} {slug}");
        }
    }
}

#[tokio::test]
async fn visibility_is_a_closed_enum() {
    for kind in Kind::ALL {
        let h = Harness::new();
        let (status, body) = kind
            .publish_files(&h, "tenant-a", "alpha", "1.0.0", kind.files())
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");

        // `private` is not a value of the enum; a publish carrying it fails.
        for bad in ["private", "PUBLIC", ""] {
            let body = match kind {
                Kind::Skill => json!({
                    "name": "x", "slug": "x", "version": "1.0.0",
                    "visibility": bad, "files": skill_files()
                }),
                Kind::Agent => json!({
                    "slug": "x", "version": "1.0.0", "visibility": bad,
                    "files": agent_files("QA Agent", "d")
                }),
            };
            let (status, body) = match kind {
                Kind::Skill => h.publish_skill_with("tenant-a", body).await,
                Kind::Agent => h.publish_agent_with("tenant-a", body).await,
            };
            assert_eq!(status, StatusCode::BAD_REQUEST, "{kind:?} visibility={bad}: {body}");
            assert_eq!(body["error"]["code"], "invalid_visibility", "{kind:?} {bad}");
        }
    }
}

// ---------------------------------------------------------------------------
// lookup and dry run
// ---------------------------------------------------------------------------

/// The host maps an installed item back to the market by display name, so both
/// planes answer the same lookup question with the same shape.
#[tokio::test]
async fn lookup_maps_display_names_for_both_planes() {
    for kind in Kind::ALL {
        let h = Harness::new();
        // Both planes publish under an explicit display name: lookup is keyed
        // on it, and the fixtures would otherwise decide the answer.
        let name = "Alpha Item";
        let (status, body) = kind
            .publish_named(&h, "tenant-a", "alpha", "1.0.0", name, "public")
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");
        // A private entry is only the owner's business.
        let (status, body) = kind
            .publish_named(&h, "tenant-a", "beta", "1.0.0", "Secret", "tenant")
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");

        let uri = format!("{}/lookup?names=Alpha%20Item,Secret,nobody-has-this", kind.list());
        let (status, found, _) = h.call("GET", &uri, Some("tenant-a"), None, None).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {found}");
        assert_eq!(field_names(&found), ["items"], "{kind:?} envelope");
        assert_eq!(slugs(&found), ["alpha", "beta"], "{kind:?}: {found}");

        let alpha = find(&found, "alpha");
        let mut expected = vec![
            "latest_version",
            "name",
            "owner_tenant",
            "slug",
            "state",
            "takedown_reason",
            "visibility",
        ];
        expected.push(kind.bundle_field());
        expected.sort();
        assert_eq!(field_names(alpha), expected, "{kind:?} lookup item");
        assert_eq!(alpha["state"], "active", "{kind:?}");
        assert_eq!(alpha["latest_version"], "1.0.0", "{kind:?}");
        assert_eq!(
            alpha[kind.bundle_field()],
            json!(kind.install_url("alpha", "1.0.0")),
            "{kind:?}: the lookup link must be the same address as the listing"
        );

        // A stranger sees the public one and not the private one.
        let (_, found, _) = h.call("GET", &uri, Some("tenant-b"), None, None).await;
        assert_eq!(slugs(&found), ["alpha"], "{kind:?}");

        // Withdrawn entries still answer: that is why the host asks.
        let (status, _, _) = h
            .call("POST", &kind.action("alpha", "withdraw"), Some("tenant-a"), Some(json!({})), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?} withdraw");
        let uri = format!("{}/lookup?names=Alpha%20Item", kind.list());
        let (_, found, _) = h.call("GET", &uri, Some("tenant-a"), None, None).await;
        assert_eq!(found["items"][0]["state"], "withdrawn", "{kind:?}: {found}");
    }
}

/// A dry run answers with the same envelope on both planes and never writes.
#[tokio::test]
async fn validate_reports_the_same_shape_on_both_planes() {
    for kind in Kind::ALL {
        let h = Harness::new();
        let body = |version: &str, files: Value| match kind {
            Kind::Skill => json!({
                "name": "Demo Skill", "slug": "alpha", "version": version,
                "visibility": "public", "files": files
            }),
            Kind::Agent => json!({
                "slug": "alpha", "version": version,
                "visibility": "public", "files": files
            }),
        };
        let uri = format!("{}/validate", kind.list());

        // A clean bundle: no problems, nothing published.
        let (status, answer, _) = h
            .call("POST", &uri, Some("tenant-a"), Some(body("1.0.0", kind.files())), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {answer}");
        assert_eq!(
            field_names(&answer),
            [
                "existing",
                "file_count",
                "files",
                "limits",
                "manifest",
                "name",
                "next_version",
                "ok",
                "problems",
                "size_bytes",
                "slug",
                "version",
                "visibility",
            ],
            "{kind:?} envelope"
        );
        assert_eq!(answer["ok"], true, "{kind:?}: {answer}");
        assert_eq!(answer["slug"], "alpha", "{kind:?}: {answer}");
        assert!(answer["existing"].is_null(), "{kind:?}: {answer}");
        assert!(answer["manifest"].is_object(), "{kind:?}: {answer}");
        assert_eq!(
            field_names(&answer["limits"]),
            ["max_bundle_bytes", "max_file_bytes", "max_files"],
            "{kind:?}"
        );
        let (_, listing, _) = h.call("GET", kind.list(), Some("tenant-a"), None, None).await;
        assert_eq!(listing["total"], 0, "{kind:?} dry run wrote a row");

        // A broken bundle explains itself, and still writes nothing.
        let (status, answer, _) = h
            .call(
                "POST",
                &uri,
                Some("tenant-a"),
                Some(body("1.0.0", json!({ "notes.txt": "no manifest here" }))),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {answer}");
        assert_eq!(answer["ok"], false, "{kind:?}: {answer}");
        assert!(
            !answer["problems"].as_array().unwrap().is_empty(),
            "{kind:?}: {answer}"
        );
        let (_, listing, _) = h.call("GET", kind.list(), Some("tenant-a"), None, None).await;
        assert_eq!(listing["total"], 0, "{kind:?} rejected bundle was published");

        // Once the slug is real, the same payload reports the collision.
        let (status, _) = kind.publish(&h, "tenant-a", "alpha", "1.0.0", "public").await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}");
        let (status, answer, _) = h
            .call("POST", &uri, Some("tenant-a"), Some(body("1.0.0", kind.files())), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {answer}");
        assert_eq!(answer["ok"], false, "{kind:?}: {answer}");
        assert_eq!(answer["problems"][0]["code"], "version_exists", "{kind:?}: {answer}");
        assert_eq!(answer["next_version"], "1.0.1", "{kind:?}: {answer}");
        assert_eq!(answer["existing"]["mine"], true, "{kind:?}: {answer}");
        assert_eq!(answer["existing"]["state"], "active", "{kind:?}: {answer}");
    }
}
