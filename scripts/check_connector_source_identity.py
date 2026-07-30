#!/usr/bin/env python3
"""Verify checked-in Donat-owned connector records against raw Git content."""

from __future__ import annotations

import hashlib
import re
import subprocess
import sys
from pathlib import Path
from typing import NoReturn


WORKSPACE = Path(__file__).resolve().parent.parent
RECORD = (
    WORKSPACE
    / "crates"
    / "connector-catalog"
    / "sources"
    / "records"
    / "donat-owned-http-v1.yaml"
)
EXPECTED = {
    "record_id": "source.donat.http.v1",
    "repository_commit": "29e885a8cbdaca390681b48860db654d4645715d",
    "source_path": "crates/server/src/connectors/http.rs",
    "source_sha256": "8111711926cbd522bc175305225daf31e7c6add4b3499265c45bd16872e265b8",
    "license_path": "LICENSE",
    "license_sha256": "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
    "spdx_id": "Apache-2.0",
    "selected_dual_license_branch": "null",
    "notice_id": "notice.donat.http.v1",
    "required_copyright_lines": "[]",
    "notice_bundle_destination": "THIRD_PARTY_NOTICES.md",
}


def fail(message: str) -> NoReturn:
    print(f"source_record_identity_mismatch: {message}", file=sys.stderr)
    raise SystemExit(1)


def raw_git_content(commit: str, path: str) -> bytes:
    result = subprocess.run(
        ["git", "show", f"{commit}:{path}"],
        cwd=WORKSPACE,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        fail(result.stderr.decode("utf-8", errors="replace").strip())
    return result.stdout


def one(pattern: str, text: str, field: str) -> str:
    matches = re.findall(pattern, text, flags=re.MULTILINE)
    if len(matches) != 1:
        fail(f"{field} must occur exactly once")
    return matches[0]


def main() -> None:
    text = RECORD.read_text(encoding="utf-8")
    record_id = one(r"^record_id:\s*(\S+)\s*$", text, "record_id")
    commit = one(r"^\s{4}repository_commit:\s*([0-9a-f]+)\s*$", text, "repository_commit")
    file_matches = re.findall(
        r"^\s{6}- path:\s*(\S+)\s*$\n^\s{8}sha256:\s*([0-9a-f]+)\s*$",
        text,
        flags=re.MULTILINE,
    )
    if len(file_matches) != 1:
        fail("Donat-owned source file identity must occur exactly once")
    source_path, source_hash = file_matches[0]
    license_paths = re.findall(r"^\s+license_file_path:\s*(\S+)\s*$", text, re.MULTILINE)
    license_hashes = re.findall(
        r"^\s+license_file_sha256:\s*([0-9a-f]+)\s*$", text, re.MULTILINE
    )

    actual = {
        "record_id": record_id,
        "repository_commit": commit,
        "source_path": source_path,
        "source_sha256": source_hash,
        "spdx_id": one(r"^\s{4}spdx_id:\s*(\S+)\s*$", text, "spdx_id"),
        "selected_dual_license_branch": one(
            r"^\s{4}selected_dual_license_branch:\s*(\S+)\s*$",
            text,
            "selected_dual_license_branch",
        ),
        "notice_id": one(
            r"^notice:\s*$\n^\s{2}id:\s*(\S+)\s*$", text, "notice_id"
        ),
        "required_copyright_lines": one(
            r"^\s{2}required_copyright_lines:\s*(\[[^\r\n]*\])\s*$",
            text,
            "required_copyright_lines",
        ),
        "notice_bundle_destination": one(
            r"^\s{2}notice_bundle_destination:\s*(\S+)\s*$",
            text,
            "notice_bundle_destination",
        ),
    }
    for field, expected in EXPECTED.items():
        if field in actual and actual[field] != expected:
            fail(f"{field} is {actual[field]!r}, expected {expected!r}")
    if license_paths != [EXPECTED["license_path"], EXPECTED["license_path"]]:
        fail("license path must match the source decision and notice")
    if license_hashes != [EXPECTED["license_sha256"], EXPECTED["license_sha256"]]:
        fail("license hash must match the source decision and notice")

    for field in ("repository_commit", "source_sha256", "license_sha256"):
        value = EXPECTED[field]
        if len(set(value)) == 1:
            fail(f"{field} is a placeholder")

    source_content = raw_git_content(commit, source_path)
    license_content = raw_git_content(commit, EXPECTED["license_path"])
    if hashlib.sha256(source_content).hexdigest() != source_hash:
        fail("source file SHA-256 does not match raw Git content")
    if hashlib.sha256(license_content).hexdigest() != EXPECTED["license_sha256"]:
        fail("license SHA-256 does not match raw Git content")

    print(
        "verified connector source identity: "
        f"{record_id} @ {commit} ({source_path}, {EXPECTED['license_path']})"
    )


if __name__ == "__main__":
    main()
