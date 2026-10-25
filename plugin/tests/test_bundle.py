# -*- coding: utf-8 -*-
"""`extract_zip`: archive bytes -> the backend's {path: text} bundle map."""

from __future__ import annotations

import io
import zipfile

import pytest

from offline_market.bundle import BundleError, extract_zip

MANIFEST = "---\nname: demo\ndescription: demo skill\n---\n\n# Demo\n"


def make_zip(entries: dict[str, str | bytes], compression: int = zipfile.ZIP_DEFLATED) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", compression) as archive:
        for name, data in entries.items():
            archive.writestr(name, data)
    return buffer.getvalue()


def code_of(data: bytes) -> str:
    with pytest.raises(BundleError) as caught:
        extract_zip(data)
    return caught.value.code


def test_flat_archive_becomes_a_bundle():
    files = extract_zip(
        make_zip({"SKILL.md": MANIFEST, "scripts/run.py": "print(1)\n"})
    )
    assert files == {"SKILL.md": MANIFEST, "scripts/run.py": "print(1)\n"}


def test_single_top_level_folder_is_stripped():
    """Zipping the folder itself is the natural thing to do on every OS."""
    files = extract_zip(
        make_zip(
            {
                "my-skill/SKILL.md": MANIFEST,
                "my-skill/scripts/run.py": "print(1)\n",
                "my-skill/docs/README.md": "hi\n",
            }
        )
    )
    assert sorted(files) == ["SKILL.md", "docs/README.md", "scripts/run.py"]


def test_a_stray_root_file_prevents_stripping():
    archive = make_zip({"my-skill/SKILL.md": MANIFEST, "README.md": "hi\n"})
    assert code_of(archive) == "missing_manifest"


def test_os_junk_is_dropped():
    files = extract_zip(
        make_zip(
            {
                "__MACOSX/._SKILL.md": "junk",
                "my-skill/.DS_Store": "junk",
                "my-skill/Thumbs.db": "junk",
                "my-skill/SKILL.md": MANIFEST,
            }
        )
    )
    assert files == {"SKILL.md": MANIFEST}


def test_bom_is_stripped_so_frontmatter_survives():
    files = extract_zip(make_zip({"SKILL.md": ("\ufeff" + MANIFEST).encode("utf-8")}))
    assert files["SKILL.md"].startswith("---")


@pytest.mark.parametrize(
    ("path", "code"),
    [
        ("../evil.md", "traversal"),
        ("a/../../evil.md", "traversal"),
        ("a//b.md", "traversal"),
        ("/etc/passwd", "absolute_path"),
        ("C:/x.md", "absolute_path"),
        ("..\\evil.md", "traversal"),
    ],
)
def test_paths_cannot_escape_or_be_absolute(path, code):
    archive = make_zip({"SKILL.md": MANIFEST, path: "x"})
    assert code_of(archive) == code


def test_backslashes_are_normalised():
    files = extract_zip(make_zip({"SKILL.md": MANIFEST, "scripts\\run.py": "print(1)\n"}))
    assert "scripts/run.py" in files


def test_symlink_entries_are_rejected():
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("SKILL.md", MANIFEST)
        info = zipfile.ZipInfo("link")
        info.external_attr = 0o120777 << 16  # S_IFLNK
        archive.writestr(info, "/etc/passwd")
    assert code_of(buffer.getvalue()) == "symlink_entry"


def test_encrypted_entries_are_rejected():
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("SKILL.md", MANIFEST)
        archive.writestr("secret.md", "x")
    # zipfile cannot *write* encrypted entries, so set the flag in the central
    # directory the way a real encrypted archive would carry it.
    raw = bytearray(buffer.getvalue())
    position = raw.find(b"PK\x01\x02")
    while position >= 0:
        name_len = int.from_bytes(raw[position + 28 : position + 30], "little")
        if raw[position + 46 : position + 46 + name_len] == b"secret.md":
            raw[position + 8] |= 0x1
            break
        position = raw.find(b"PK\x01\x02", position + 4)
    else:
        raise AssertionError("central directory entry not found")
    assert code_of(bytes(raw)) == "encrypted_entry"


def test_duplicate_entries_are_rejected():
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("SKILL.md", MANIFEST)
        archive.writestr("SKILL.md", "# again\n")
    assert code_of(buffer.getvalue()) == "duplicate_entry"


def test_binary_files_are_rejected_with_a_useful_message():
    archive = make_zip({"SKILL.md": MANIFEST, "icon.png": b"\x89PNG\r\n\x1a\n\xff\xfe"})
    with pytest.raises(BundleError) as caught:
        extract_zip(archive)
    assert caught.value.code == "not_text"
    assert "icon.png" in str(caught.value)


def test_missing_or_broken_archives():
    assert code_of(b"") == "empty_upload"
    assert code_of(b"definitely not a zip") == "invalid_zip"
    assert code_of(make_zip({"SKILL.md": MANIFEST})[:-20]) == "invalid_zip"
    assert code_of(make_zip({"nested/dir/README.md": "x"})) == "missing_manifest"


def test_too_many_files_is_stopped_before_reading():
    archive = make_zip({f"f{index}.md": "x" for index in range(1001)})
    assert code_of(archive) == "too_many_files"
