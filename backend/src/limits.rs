//! The publish-side gate.
//!
//! Per PLAN §1: file *types* are not restricted (no extension allow-list),
//! so what is left is structural — it protects the host from disk bombs and
//! path escapes, and it protects tenants from broken packages. It is NOT a
//! malware filter: the only malware defence lives on the install side
//! (`QWENPAW_SKILL_SCAN_MODE=block` preferred, `warn` accepted; see
//! spikes/s5_scan_mode_gate.py).

use crate::config::Limits;
use base64ct::{Base64, Encoding};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

pub const MANIFEST: &str = "SKILL.md";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Reject {
    pub code: &'static str,
    pub message: String,
}

impl Reject {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// One file in a bundle. Bytes, not text: docx/pptx/images are legitimate
/// skill assets, and the old `{path: text}` map turned every non-UTF-8 file
/// into a publish-time rejection.
///
/// JSON has no byte type, so the wire format is deliberately asymmetric:
/// valid UTF-8 arrives (and leaves) as a plain string, anything else as
/// `{"base64": "..."}`. That keeps every pre-existing client and every row
/// already stored in SQLite readable without a migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileData(Vec<u8>);

impl FileData {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// `Some` only for valid UTF-8. The manifest must be text; the rest may be
    /// either, and a binary that happens to be valid UTF-8 installs byte-for-byte
    /// through the host's text writer anyway.
    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for FileData {
    fn from(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }
}

impl From<String> for FileData {
    fn from(value: String) -> Self {
        Self(value.into_bytes())
    }
}

impl Serialize for FileData {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.as_text() {
            Some(text) => serializer.serialize_str(text),
            None => {
                use serde::ser::SerializeStruct;
                let encoded = Base64::encode_string(&self.0);
                let mut state = serializer.serialize_struct("FileData", 1)?;
                state.serialize_field("base64", &encoded)?;
                state.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for FileData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Text(String),
            Binary { base64: String },
        }
        match Wire::deserialize(deserializer)? {
            Wire::Text(text) => Ok(Self(text.into_bytes())),
            Wire::Binary { base64 } => Base64::decode_vec(&base64)
                .map(Self)
                .map_err(serde::de::Error::custom),
        }
    }
}

/// Normalised package that passed the gate.
#[derive(Debug, Clone)]
pub struct CheckedBundle {
    pub name: String,
    pub files: BTreeMap<String, FileData>,
    pub size_bytes: usize,
    pub sha256: String,
}

pub fn check_bundle(
    name: &str,
    files: &BTreeMap<String, FileData>,
    limits: &Limits,
) -> Result<CheckedBundle, Reject> {
    match review_bundle(name, files, limits) {
        (Some(checked), _) => Ok(checked),
        // Problems are pushed in the order the publish path used to fail in, so
        // the code an API client sees is unchanged.
        (None, problems) => Err(problems
            .into_iter()
            .next()
            .unwrap_or_else(|| Reject::new("invalid_bundle", "压缩包不合法"))),
    }
}

/// Every problem in a bundle, not just the first one.
///
/// Publishing still fails on the first problem ([`check_bundle`]) — nobody needs
/// the whole list after the fact. The publish *form* does: it runs this against
/// the archive the user just picked and points at everything that needs fixing
/// before they spend a round-trip on `POST /skills`.
///
/// The checks are the same ones, in the same order, so a bundle that passes here
/// always passes the real gate.
pub fn review_bundle(
    name: &str,
    files: &BTreeMap<String, FileData>,
    limits: &Limits,
) -> (Option<CheckedBundle>, Vec<Reject>) {
    let mut problems: Vec<Reject> = Vec::new();
    if name.trim().is_empty() {
        problems.push(Reject::new("invalid_name", "技能名称不能为空"));
    }
    if name.chars().count() > 128 {
        problems.push(Reject::new("invalid_name", "技能名称过长"));
    }
    if files.is_empty() {
        problems.push(Reject::new("empty_bundle", "压缩包里没有任何文件"));
    }
    if files.len() > limits.max_files {
        problems.push(Reject::new(
            "too_many_files",
            format!(
                "压缩包内有 {} 个文件，上限为 {}",
                files.len(),
                limits.max_files
            ),
        ));
    }
    if !files.is_empty() && !files.contains_key(MANIFEST) {
        problems.push(Reject::new(
            "missing_manifest",
            format!("压缩包根目录必须包含 {MANIFEST}"),
        ));
    }

    let mut size_bytes = 0usize;
    for (path, content) in files {
        if let Err(reject) = check_path(path) {
            problems.push(reject);
        }
        let n = content.len();
        if n > limits.max_file_bytes {
            problems.push(Reject::new(
                "file_too_large",
                format!(
                    "{path} 有 {n} 字节，单文件上限为 {} 字节",
                    limits.max_file_bytes
                ),
            ));
        }
        size_bytes += n;
    }
    if size_bytes > limits.max_bundle_bytes {
        problems.push(Reject::new(
            "bundle_too_large",
            format!(
                "压缩包共 {size_bytes} 字节，上限为 {} 字节",
                limits.max_bundle_bytes
            ),
        ));
    }
    match files.get(MANIFEST) {
        // The absence is already reported above; nothing to add.
        None => {}
        Some(content) => match content.as_text() {
            None => problems.push(Reject::new(
                "invalid_manifest",
                "SKILL.md 必须是 UTF-8 文本（二进制文件请放在其他文件里）",
            )),
            Some(manifest) if manifest.trim().is_empty() => {
                problems.push(Reject::new("empty_manifest", "SKILL.md 是空的"));
            }
            Some(manifest) => {
                if let Err(reject) = manifest_meta(manifest) {
                    problems.push(reject);
                }
            }
        },
    }

    if problems.is_empty() {
        let checked = CheckedBundle {
            name: name.trim().to_string(),
            files: files.clone(),
            sha256: digest(files),
            size_bytes,
        };
        (Some(checked), problems)
    } else {
        (None, problems)
    }
}

pub(crate) fn check_path(path: &str) -> Result<(), Reject> {
    if path.is_empty() || path.len() > 200 {
        return Err(Reject::new(
            "invalid_path",
            format!("路径不合法：{:?}", path),
        ));
    }
    if path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return Err(Reject::new(
            "absolute_path",
            format!("{path}：不允许使用绝对路径"),
        ));
    }
    if path.contains('\0') || path.chars().any(|c| c.is_control()) {
        return Err(Reject::new(
            "invalid_path",
            format!("{path}：不允许包含控制字符"),
        ));
    }
    if path.contains('\\') {
        return Err(Reject::new(
            "invalid_path",
            format!("{path}：请使用正斜杠 /"),
        ));
    }
    for seg in path.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(Reject::new(
                "traversal",
                format!("{path}：不允许出现空路径段、'.' 或 '..'"),
            ));
        }
    }
    Ok(())
}

/// Mirror of the host's minimum manifest requirement.
///
/// `qwenpaw/agents/skill_system/store.py:validate_skill_content` refuses to
/// import a bundle whose `SKILL.md` frontmatter lacks a non-empty `name` or
/// `description` — verified live: a bundle without `description` publishes
/// fine but fails at install with "SKILL.md must include non-empty frontmatter
/// name and description". So we reject it at publish time instead.
///
/// ponytail: this is a structural pre-check, not a YAML parser. Anything that
/// slips through surfaces at install time, where the host does the real
/// validation and nothing has been distributed yet.
pub(crate) fn manifest_meta(content: &str) -> Result<(String, String), Reject> {
    let mut lines = content.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(Reject::new(
            "invalid_manifest",
            "SKILL.md 必须以 '---' 包裹的 frontmatter 区块开头，并包含 name 和 description",
        ));
    }
    let (mut name, mut description) = (String::new(), String::new());
    let mut closed = false;
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            closed = true;
            break;
        }
        if line.starts_with(char::is_whitespace) {
            continue; // nested value under some other key
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = unquote(value.trim());
            match key.trim() {
                "name" => name = value,
                "description" => description = value,
                _ => {}
            }
        }
    }
    if !closed {
        return Err(Reject::new(
            "invalid_manifest",
            "SKILL.md 的 frontmatter 缺少结尾的 '---'",
        ));
    }
    if name.is_empty() || description.is_empty() {
        return Err(Reject::new(
            "invalid_manifest",
            "SKILL.md 的 frontmatter 必须包含非空的 name 和 description（否则 QwenPaw 会拒绝安装）",
        ));
    }
    Ok((name, description))
}

/// Frontmatter `description`, for the listings' 描述 column (the host shows the
/// same line in its own 已安装 list). `None` when the stored SKILL.md is not
/// what [`check_bundle`] accepted — an unparsable manifest is a display gap,
/// never an error on a read path.
pub fn manifest_description(content: &str) -> Option<String> {
    manifest_meta(content)
        .ok()
        .map(|(_, description)| description)
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].trim().to_string();
        }
    }
    value.to_string()
}

/// Content hash over the path/content pairs (stable: BTreeMap ordering).
pub fn digest(files: &BTreeMap<String, FileData>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for (path, content) in files {
        h.update(path.as_bytes());
        h.update([0u8]);
        h.update(content.as_bytes());
        h.update([0u8]);
    }
    format!("{:x}", h.finalize())
}

/// Upper bound for a slug, in bytes (`[a-z0-9-]` only, so bytes == chars).
/// `derive_slug` truncates to this so a long Chinese name still gets an id.
pub const MAX_SLUG_LEN: usize = 64;

pub fn valid_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    (1..=MAX_SLUG_LEN).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

pub fn valid_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes.first().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            max_bundle_bytes: 2 * 1024 * 1024,
            max_files: 3,
            max_file_bytes: 256 * 1024,
        }
    }

    const GOOD_MANIFEST: &str = "---\nname: demo\ndescription: a demo skill\n---\n\n# Demo\n";

    fn bundle(pairs: &[(&str, &str)]) -> BTreeMap<String, FileData> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), FileData::from(*v)))
            .collect()
    }

    #[test]
    fn accepts_normal_bundle() {
        let files = bundle(&[(MANIFEST, GOOD_MANIFEST), ("scripts/run.py", "print(1)")]);
        let out = check_bundle("Demo", &files, &limits()).unwrap();
        assert_eq!(out.files.len(), 2);
        assert_eq!(out.sha256.len(), 64);
    }

    #[test]
    fn manifest_must_have_name_and_description() {
        for bad in [
            "# no frontmatter\n",
            "---\nname: demo\n---\n# x\n",
            "---\ndescription: only this\n---\n",
            "---\nname: demo\ndescription: \"\"\n---\n",
            "---\nname: demo\ndescription: x\n",
        ] {
            let files = bundle(&[(MANIFEST, bad)]);
            let err = check_bundle("d", &files, &limits()).unwrap_err();
            assert_eq!(err.code, "invalid_manifest", "{bad}");
        }
        // quoted values and trailing metadata are fine
        let ok =
            "---\nname: \"demo\"\ndescription: 'x'\nmetadata:\n  qwenpaw:\n    emoji: x\n---\n";
        let files = bundle(&[(MANIFEST, ok)]);
        assert_eq!(
            check_bundle("Demo", &files, &limits()).unwrap().name,
            "Demo"
        );
    }

    #[test]
    fn requires_manifest() {
        let files = bundle(&[("readme.md", "x")]);
        assert_eq!(
            check_bundle("d", &files, &limits()).unwrap_err().code,
            "missing_manifest"
        );
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        for bad in ["../x.md", "/etc/passwd", "a/../../b", "a//b", "./x"] {
            let files = bundle(&[(MANIFEST, GOOD_MANIFEST), (bad, "x")]);
            let err = check_bundle("d", &files, &limits()).unwrap_err();
            assert!(
                err.code == "traversal" || err.code == "absolute_path",
                "{bad} -> {err:?}"
            );
        }
    }

    #[test]
    fn binary_assets_are_accepted_and_roundtrip_through_json() {
        let raw = vec![0x50, 0x4b, 0x03, 0x04, 0xff, 0x00, 0x81];
        let mut files = bundle(&[(MANIFEST, GOOD_MANIFEST)]);
        files.insert("assets/report.docx".to_string(), FileData::new(raw.clone()));

        let out = check_bundle("Demo", &files, &limits()).unwrap();
        assert_eq!(out.files["assets/report.docx"].as_bytes(), raw.as_slice());

        // The wire format marks only non-UTF-8 payloads; text stays a string.
        let json = serde_json::to_string(&out.files).unwrap();
        assert!(json.contains("\"base64\""), "{json}");
        assert!(json.contains("\"SKILL.md\":\"---"), "{json}");
        let back: BTreeMap<String, FileData> = serde_json::from_str(&json).unwrap();
        assert_eq!(back["assets/report.docx"].as_bytes(), raw.as_slice());
        assert_eq!(back[MANIFEST].as_text(), Some(GOOD_MANIFEST));
    }

    #[test]
    fn legacy_text_only_json_still_decodes() {
        // Rows written before binary support store plain strings.
        let files: BTreeMap<String, FileData> =
            serde_json::from_str(r#"{"SKILL.md":"---\nname: demo\ndescription: x\n---\n"}"#)
                .unwrap();
        assert_eq!(
            files[MANIFEST].as_text(),
            Some("---\nname: demo\ndescription: x\n---\n")
        );
    }

    #[test]
    fn size_limits_apply_to_decoded_bytes_not_base64() {
        let mut files = bundle(&[(MANIFEST, GOOD_MANIFEST)]);
        // base64 is ~4/3 larger; 256 KiB of zeros would be > 341 KiB encoded.
        files.insert(
            "assets/big.bin".to_string(),
            FileData::new(vec![0u8; 256 * 1024 + 1]),
        );
        assert_eq!(
            check_bundle("d", &files, &limits()).unwrap_err().code,
            "file_too_large"
        );
    }

    #[test]
    fn binary_manifest_is_rejected() {
        let mut files = BTreeMap::new();
        files.insert(MANIFEST.to_string(), FileData::new(vec![0xff, 0xfe]));
        assert_eq!(
            check_bundle("d", &files, &limits()).unwrap_err().code,
            "invalid_manifest"
        );
    }

    #[test]
    fn rejects_windows_separators_and_drive_letters() {
        let files = bundle(&[(MANIFEST, GOOD_MANIFEST), ("a\\b.md", "x")]);
        assert_eq!(
            check_bundle("d", &files, &limits()).unwrap_err().code,
            "invalid_path"
        );
        let files = bundle(&[(MANIFEST, GOOD_MANIFEST), ("C:/x.md", "x")]);
        assert_eq!(
            check_bundle("d", &files, &limits()).unwrap_err().code,
            "absolute_path"
        );
    }

    #[test]
    fn accepts_deeply_nested_paths() {
        // Real skills have this shape (spikes: scripts/office/helpers/__init__.py);
        // only the 200-char path cap bounds the depth now.
        let deep = bundle(&[
            (MANIFEST, GOOD_MANIFEST),
            ("scripts/office/helpers/__init__.py", ""),
        ]);
        assert_eq!(check_bundle("d", &deep, &limits()).unwrap().files.len(), 2);
    }

    #[test]
    fn enforces_count_and_sizes() {
        let many = bundle(&[
            (MANIFEST, GOOD_MANIFEST),
            ("a.md", "x"),
            ("b.md", "y"),
            ("c.md", "z"),
        ]);
        assert_eq!(
            check_bundle("d", &many, &limits()).unwrap_err().code,
            "too_many_files"
        );

        let big = bundle(&[(MANIFEST, &"x".repeat(256 * 1024 + 1))]);
        assert_eq!(
            check_bundle("d", &big, &limits()).unwrap_err().code,
            "file_too_large"
        );
    }

    #[test]
    fn slug_and_version_rules() {
        for ok in ["a", "abc-123", "0day", &"a".repeat(64)] {
            assert!(valid_slug(ok), "{ok}");
        }
        for bad in ["", "A", "-a", "a_b", "a/b", &"a".repeat(65)] {
            assert!(!valid_slug(bad), "{bad}");
        }
        for ok in ["1", "1.0.0", "2.2.2.post1", "v1+build"] {
            assert!(valid_version(ok), "{ok}");
        }
        for bad in ["", ".1", "1 2", "1/2", &"1".repeat(33)] {
            assert!(!valid_version(bad), "{bad}");
        }
    }

    #[test]
    fn digest_is_order_independent() {
        let a = bundle(&[("SKILL.md", "#"), ("b.md", "2"), ("a.md", "1")]);
        let b = bundle(&[("a.md", "1"), ("b.md", "2"), ("SKILL.md", "#")]);
        assert_eq!(digest(&a), digest(&b));
        let c = bundle(&[("SKILL.md", "#"), ("b.md", "3"), ("a.md", "1")]);
        assert_ne!(digest(&a), digest(&c));
    }

    /// The publish form needs the whole list, the publish path only the first.
    #[test]
    fn review_reports_every_problem_at_once() {
        let files = bundle(&[
            ("a.md", "no manifest here"),
            ("../escape.md", "x"),
            ("big.bin", &"0".repeat(300 * 1024)),
            ("b.md", "y"),
            ("c.md", "z"),
        ]);
        let (checked, problems) = review_bundle("Demo", &files, &limits());
        assert!(checked.is_none());
        let codes: Vec<&str> = problems.iter().map(|p| p.code).collect();
        // too_many_files (3 files max, 5 sent), missing manifest, traversal,
        // file too large — all of them, not just the first.
        assert!(codes.contains(&"too_many_files"), "{codes:?}");
        assert!(codes.contains(&"missing_manifest"), "{codes:?}");
        assert!(codes.contains(&"traversal"), "{codes:?}");
        assert!(codes.contains(&"file_too_large"), "{codes:?}");
        // And the fail-fast gate still reports the first one only.
        assert_eq!(
            check_bundle("Demo", &files, &limits()).unwrap_err().code,
            "too_many_files"
        );

        let good = bundle(&[(MANIFEST, GOOD_MANIFEST)]);
        let (checked, problems) = review_bundle("Demo", &good, &limits());
        assert!(problems.is_empty());
        assert_eq!(checked.map(|b| b.files.len()), Some(1));
    }
}
