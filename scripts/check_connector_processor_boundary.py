#!/usr/bin/env python3

from dataclasses import dataclass
import os
from pathlib import Path
import re
import sys
import tempfile


HOST_CONSTRUCTION_ROOTS = ("crates/server/src/connectors/",)
CATALOG_CONSTRUCTION_ROOTS = ("crates/connector-catalog/src/",)
STATIC_LITERAL_ROOTS = (
    "crates/connector-abi/src/",
    "crates/connector-processors/src/",
    "crates/connector-catalog/src/generated/",
)
HOST_IMPL_ROOTS = ("crates/server/src/connectors/",)

ABI_TEST_ROOTS = ("crates/connector-abi/tests/",)
PROCESSOR_TEST_ROOTS = ("crates/connector-processors/tests/",)
SERVER_TEST_ROOTS = ("crates/server/tests/",)


@dataclass(frozen=True)
class Fixture:
    path: str
    source: str
    expected_rule: str | None


@dataclass(frozen=True)
class Finding:
    offset: int
    rule: str
    message: str


def scan_fixture(path: Path, source: str) -> list[str]:
    relative = path.as_posix()
    findings = scan_source(relative, source)
    return [
        f"connector-boundary: {relative}: {finding.rule}: {finding.message}"
        for finding in sorted(findings, key=lambda item: (item.rule, item.offset))
    ]


def starts_with(path: str, roots: tuple[str, ...]) -> bool:
    return any(path.startswith(root) for root in roots)


def blank_rust_noncode(source: str) -> str:
    data = source.encode("utf-8")
    output = bytearray(data)

    def blank(start: int, end: int) -> None:
        for offset in range(start, end):
            if output[offset] not in (10, 13):
                output[offset] = 32

    def raw_start(offset: int) -> tuple[int, bytes] | None:
        for prefix in (b"br", b"rb", b"cr", b"rc", b"r"):
            if not data.startswith(prefix, offset):
                continue
            cursor = offset + len(prefix)
            while cursor < len(data) and data[cursor] == 35:
                cursor += 1
            if cursor < len(data) and data[cursor] == 34:
                hashes = data[offset + len(prefix) : cursor]
                return cursor + 1, b'"' + hashes
        return None

    index = 0
    while index < len(data):
        if data.startswith(b"//", index):
            end = data.find(b"\n", index + 2)
            if end < 0:
                end = len(data)
            blank(index, end)
            index = end
            continue

        if data.startswith(b"/*", index):
            depth = 1
            cursor = index + 2
            while cursor < len(data) and depth:
                if data.startswith(b"/*", cursor):
                    depth += 1
                    cursor += 2
                elif data.startswith(b"*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            blank(index, cursor)
            index = cursor
            continue

        raw = raw_start(index)
        if raw is not None:
            content_start, terminator = raw
            end = data.find(terminator, content_start)
            end = len(data) if end < 0 else end + len(terminator)
            blank(index, end)
            index = end
            continue

        string_start = index
        if data[index : index + 1] == b'"':
            cursor = index + 1
        elif data[index : index + 2] in (b'b"', b'c"'):
            cursor = index + 2
        else:
            cursor = -1
        if cursor >= 0:
            escaped = False
            while cursor < len(data):
                byte = data[cursor]
                cursor += 1
                if escaped:
                    escaped = False
                elif byte == 92:
                    escaped = True
                elif byte == 34:
                    break
            blank(string_start, cursor)
            index = cursor
            continue

        char_start = index
        if data[index : index + 2] == b"b'":
            cursor = index + 2
        elif data[index : index + 1] == b"'":
            cursor = index + 1
        else:
            cursor = -1
        if cursor >= 0:
            escaped = False
            closing = -1
            while cursor < len(data) and data[cursor] not in (10, 13, 32, 9):
                byte = data[cursor]
                cursor += 1
                if escaped:
                    escaped = False
                elif byte == 92:
                    escaped = True
                elif byte == 39:
                    closing = cursor
                    break
            if closing >= 0:
                blank(char_start, closing)
                index = closing
                continue

        index += 1

    return output.decode("latin1")


def private_cfg_test_ranges(source: str) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    pattern = re.compile(
        r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*"
        r"mod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{",
        re.MULTILINE,
    )
    for match in pattern.finditer(source):
        opening = source.find("{", match.start(), match.end())
        depth = 0
        cursor = opening
        while cursor < len(source):
            if source[cursor] == "{":
                depth += 1
            elif source[cursor] == "}":
                depth -= 1
                if depth == 0:
                    ranges.append((match.start(), cursor + 1))
                    break
            cursor += 1
    return ranges


def in_ranges(offset: int, ranges: list[tuple[int, int]]) -> bool:
    return any(start <= offset < end for start, end in ranges)


def lint_suppression(relative: str, source: str, code: str) -> Finding | None:
    message = "clippy::result_large_err must remain denied without suppression"
    if relative.endswith(".rs"):
        if not relative.startswith("crates/connector-abi/src/"):
            return None
        match = re.search(
            r"#\s*\[\s*(?:allow|expect)\s*\(\s*"
            r"clippy::result[_-]large[_-]err\s*\)\s*\]",
            code,
        )
        if match:
            return Finding(match.start(), "large-error-lint-suppression", message)
        return None

    uncommented = "\n".join(
        line for line in source.splitlines() if not line.lstrip().startswith("#")
    )
    if relative.endswith(".toml"):
        match = re.search(
            r"result[_-]large[_-]err\s*=\s*[\"']allow[\"']",
            uncommented,
        )
    else:
        match = re.search(
            r"(?:-A|--allow|--cap-lints(?:\s+|=)(?:allow|warn))"
            r"[^\n]*clippy::result[_-]large[_-]err",
            uncommented,
        )
    if match:
        return Finding(match.start(), "large-error-lint-suppression", message)
    return None


def static_literal_indirection(
    relative: str,
    code: str,
) -> Finding | None:
    if starts_with(relative, STATIC_LITERAL_ROOTS) or starts_with(
        relative, ABI_TEST_ROOTS
    ):
        return None

    for type_name in ("StaticErrorCode", "StaticSafeMessage"):
        reexport = re.search(
            rf"\bpub\s+use\b[^;]*\b{type_name}\b[^;]*;",
            code,
        )
        if reexport:
            return Finding(
                reexport.start(),
                "static-literal-reexport",
                f"{type_name}::literal cannot be re-exported outside STATIC_LITERAL_ROOTS",
            )

        alias = re.search(
            rf"\buse\b[^;]*\b{type_name}\s+as\s+"
            r"([A-Za-z_][A-Za-z0-9_]*)[^;]*;",
            code,
        )
        if alias:
            local_name = alias.group(1)
            call = re.search(rf"\b{re.escape(local_name)}\s*::\s*literal\s*\(", code)
            if call:
                return Finding(
                    alias.start(),
                    "static-literal-alias",
                    f"{type_name}::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
                )

        type_alias = re.search(
            rf"\btype\s+([A-Za-z_][A-Za-z0-9_]*)[^=;]*=\s*"
            rf"(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*{type_name}\s*;",
            code,
        )
        if type_alias:
            local_name = type_alias.group(1)
            call = re.search(rf"\b{re.escape(local_name)}\s*::\s*literal\s*\(", code)
            if call:
                return Finding(
                    type_alias.start(),
                    "static-literal-type-alias",
                    f"{type_name}::literal cannot be reached through a type alias outside STATIC_LITERAL_ROOTS",
                )

        call = re.search(rf"\b{type_name}\s*::\s*literal\s*\(", code)
        if not call:
            continue
        prefix = code[: call.start()]
        wrappers = (
            ("macro", re.search(r"\bmacro_rules\s*!", prefix)),
            ("trait", re.search(r"\btrait\s+[A-Za-z_]", prefix)),
            ("function", re.search(r"\bfn\s+[A-Za-z_]", prefix)),
        )
        for wrapper, marker in wrappers:
            if marker:
                return Finding(
                    call.start(),
                    "static-literal-wrapper",
                    f"{type_name}::literal cannot be forwarded by a {wrapper} outside STATIC_LITERAL_ROOTS",
                )
        return Finding(
            call.start(),
            "static-literal-producer",
            "static failure literals are restricted to approved roots",
        )
    return None


def restricted_namespace(relative: str, code: str) -> Finding | None:
    reexport = re.search(
        r"\bpub\s+use\b[^;]*\b(?:host_construction|catalog_construction)\b[^;]*;",
        code,
    )
    if reexport:
        return Finding(
            reexport.start(),
            "restricted-namespace-reexport",
            "restricted construction namespaces cannot be re-exported",
        )

    alias = re.search(
        r"\buse\b[^;]*\b(?:host_construction|catalog_construction)\s+as\s+"
        r"[A-Za-z_][A-Za-z0-9_]*[^;]*;",
        code,
    )
    if alias:
        return Finding(
            alias.start(),
            "restricted-namespace-alias",
            "restricted construction namespaces cannot be aliased",
        )

    if starts_with(relative, ABI_TEST_ROOTS):
        return None

    for namespace, roots, rule, message in (
        (
            "host_construction",
            HOST_CONSTRUCTION_ROOTS,
            "host-construction-producer",
            "host_construction is restricted to server connector producers",
        ),
        (
            "catalog_construction",
            CATALOG_CONSTRUCTION_ROOTS,
            "catalog-construction-producer",
            "catalog_construction is restricted to connector catalog producers",
        ),
    ):
        reference = re.search(rf"\b{namespace}\s*::", code)
        if not reference or starts_with(relative, roots):
            continue

        type_alias = re.search(
            rf"\btype\s+[A-Za-z_][A-Za-z0-9_]*[^=;]*=[^;]*\b{namespace}\s*::",
            code,
        )
        if type_alias:
            return Finding(
                type_alias.start(),
                "restricted-namespace-wrapper",
                "restricted construction namespaces cannot be named by a type alias outside approved producers",
            )
        prefix = code[: reference.start()]
        if (
            re.search(r"\bfn\s+[A-Za-z_]", prefix)
            or re.search(r"\bmacro_rules\s*!", prefix)
            or re.search(r"\btrait\s+[A-Za-z_]", prefix)
        ):
            return Finding(
                reference.start(),
                "restricted-namespace-wrapper",
                "restricted construction calls cannot be forwarded outside approved producers",
            )
        return Finding(reference.start(), rule, message)
    return None


def scan_source(relative: str, source: str) -> list[Finding]:
    code = blank_rust_noncode(source) if relative.endswith(".rs") else source

    suppression = lint_suppression(relative, source, code)
    if suppression:
        return [suppression]
    if not relative.endswith(".rs"):
        return []

    test_ranges = private_cfg_test_ranges(code)
    exported_test = re.search(
        r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*"
        r"pub(?:\s*\([^)]*\))?\s+mod\s+[A-Za-z_][A-Za-z0-9_]*",
        code,
    )
    if exported_test is None:
        public_item = re.compile(
            r"\bpub(?:\s*\([^)]*\))?\s+"
            r"(?:const|enum|fn|mod|static|struct|trait|type|use)\b"
        )
        for start, end in test_ranges:
            match = public_item.search(code, start, end)
            if match:
                exported_test = match
                break
    if exported_test:
        return [
            Finding(
                exported_test.start(),
                "exported-test-helper",
                "test helpers in production modules must remain private",
            )
        ]

    is_unapproved_test = "/tests/" in relative and not starts_with(
        relative, ABI_TEST_ROOTS + PROCESSOR_TEST_ROOTS + SERVER_TEST_ROOTS
    )
    if is_unapproved_test and (
        re.search(r"\b(?:host_construction|catalog_construction)\s*::", code)
        or re.search(
            r"\bimpl\b[^{};]*\b(?:ConnectorIo|ProcessorControl)\b[^{};]*\bfor\b",
            code,
        )
    ):
        return [
            Finding(
                0,
                "test-path-allowlist",
                "connector construction and host fakes are restricted to approved test roots",
            )
        ]

    static_finding = static_literal_indirection(relative, code)
    if static_finding:
        return [static_finding]

    namespace_finding = restricted_namespace(relative, code)
    if namespace_finding:
        return [namespace_finding]

    host_impl = re.search(
        r"\bimpl\b[^{};]*\b(?:ConnectorIo|ProcessorControl)\b[^{};]*\bfor\b",
        code,
    )
    if host_impl:
        approved_test = starts_with(
            relative, PROCESSOR_TEST_ROOTS + SERVER_TEST_ROOTS
        )
        approved_private_test = (
            starts_with(
                relative,
                (
                    "crates/connector-abi/src/",
                    "crates/connector-processors/src/",
                    "crates/server/src/",
                ),
            )
            and in_ranges(host_impl.start(), test_ranges)
        )
        if not (
            starts_with(relative, HOST_IMPL_ROOTS)
            or approved_test
            or approved_private_test
        ):
            return [
                Finding(
                    host_impl.start(),
                    "host-trait-implementation",
                    "host traits can only be implemented in approved host or test roots",
                )
            ]

    if starts_with(relative, ("crates/connector-processors/src/",)):
        leak = re.search(
            r"(?:(?:Box|String|Vec)\s*)?::\s*leak\b",
            code,
        )
        if leak and not in_ranges(leak.start(), test_ranges):
            return [
                Finding(
                    leak.start(),
                    "processor-allocation-leak",
                    "allocation leak APIs are forbidden in processor production code",
                )
            ]

    return []


def fixtures() -> tuple[Fixture, ...]:
    return (
        Fixture(
            "crates/server/src/connectors/executor.rs",
            "host_construction::transport_response();",
            None,
        ),
        Fixture(
            "crates/connector-processors/src/bad.rs",
            "host_construction::transport_response();",
            "host-construction-producer: host_construction is restricted to server connector producers",
        ),
        Fixture(
            "crates/connector-catalog/src/loader.rs",
            "catalog_construction::static_error_code(value);",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/bad.rs",
            "catalog_construction::static_error_code(value);",
            "catalog-construction-producer: catalog_construction is restricted to connector catalog producers",
        ),
        Fixture(
            "crates/connector-abi/src/envelope.rs",
            'StaticErrorCode::literal("connector_failed");',
            None,
        ),
        Fixture(
            "crates/connector-processors/src/error.rs",
            'StaticSafeMessage::literal("connector failed");',
            None,
        ),
        Fixture(
            "crates/connector-catalog/src/generated/errors.rs",
            'StaticErrorCode::literal("connector_failed");',
            None,
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_literal.rs",
            'const CODE: StaticErrorCode = StaticErrorCode::literal("connector_failed");',
            "static-literal-producer: static failure literals are restricted to approved roots",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_alias.rs",
            "use donat_connector_abi::StaticErrorCode as ErrorCode;\n"
            'const CODE: ErrorCode = ErrorCode::literal("connector_failed");',
            "static-literal-alias: StaticErrorCode::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_alias.rs",
            "use donat_connector_abi::StaticSafeMessage as SafeMessage;\n"
            'const MESSAGE: SafeMessage = SafeMessage::literal("connector failed");',
            "static-literal-alias: StaticSafeMessage::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_reexport.rs",
            "pub use donat_connector_abi::StaticErrorCode;\n"
            'const CODE: StaticErrorCode = StaticErrorCode::literal("connector_failed");',
            "static-literal-reexport: StaticErrorCode::literal cannot be re-exported outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_reexport.rs",
            "pub use donat_connector_abi::StaticSafeMessage;\n"
            'const MESSAGE: StaticSafeMessage = StaticSafeMessage::literal("connector failed");',
            "static-literal-reexport: StaticSafeMessage::literal cannot be re-exported outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_type_alias.rs",
            "type ErrorCode = donat_connector_abi::StaticErrorCode;\n"
            'const CODE: ErrorCode = ErrorCode::literal("connector_failed");',
            "static-literal-type-alias: StaticErrorCode::literal cannot be reached through a type alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_type_alias.rs",
            "type SafeMessage = donat_connector_abi::StaticSafeMessage;\n"
            'const MESSAGE: SafeMessage = SafeMessage::literal("connector failed");',
            "static-literal-type-alias: StaticSafeMessage::literal cannot be reached through a type alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_function_wrapper.rs",
            "fn failure_code() -> StaticErrorCode {\n"
            '    StaticErrorCode::literal("connector_failed")\n'
            "}",
            "static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a function outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_function_wrapper.rs",
            "fn failure_message() -> StaticSafeMessage {\n"
            '    StaticSafeMessage::literal("connector failed")\n'
            "}",
            "static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a function outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_macro_wrapper.rs",
            "macro_rules! failure_code { () => {\n"
            '    StaticErrorCode::literal("connector_failed")\n'
            "} }",
            "static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_macro_wrapper.rs",
            "macro_rules! failure_message { () => {\n"
            '    StaticSafeMessage::literal("connector failed")\n'
            "} }",
            "static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_trait_wrapper.rs",
            "trait FailureCode {\n"
            "    fn code() -> StaticErrorCode {\n"
            '        StaticErrorCode::literal("connector_failed")\n'
            "    }\n"
            "}",
            "static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a trait outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_trait_wrapper.rs",
            "trait FailureMessage {\n"
            "    fn message() -> StaticSafeMessage {\n"
            '        StaticSafeMessage::literal("connector failed")\n'
            "    }\n"
            "}",
            "static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a trait outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/comment_decoy.rs",
            "// host_construction::transport_response();\n"
            'const TEXT: &str = "StaticErrorCode::literal(\\\"bad\\\")";\n'
            "/* catalog_construction::static_error_code(value); */",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/private_use.rs",
            "use donat_connector_abi::host_construction;\n"
            "host_construction::transport_response();",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/reexport.rs",
            "pub use donat_connector_abi::host_construction;",
            "restricted-namespace-reexport: restricted construction namespaces cannot be re-exported",
        ),
        Fixture(
            "crates/server/src/connectors/alias.rs",
            "use donat_connector_abi::host_construction as host_api;\n"
            "host_api::transport_response();",
            "restricted-namespace-alias: restricted construction namespaces cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/direct.rs",
            "fn build() { host_construction::transport_response(); }",
            None,
        ),
        Fixture(
            "crates/connector-processors/src/namespace_function.rs",
            "fn build() { host_construction::transport_response(); }",
            "restricted-namespace-wrapper: restricted construction calls cannot be forwarded outside approved producers",
        ),
        Fixture(
            "crates/connector-processors/src/namespace_macro.rs",
            "macro_rules! build { () => { host_construction::transport_response() } }",
            "restricted-namespace-wrapper: restricted construction calls cannot be forwarded outside approved producers",
        ),
        Fixture(
            "crates/connector-processors/src/namespace_type.rs",
            "type HostBuilder = host_construction::ResponseBuilder;",
            "restricted-namespace-wrapper: restricted construction namespaces cannot be named by a type alias outside approved producers",
        ),
        Fixture(
            "crates/connector-processors/src/namespace_trait.rs",
            "trait Build { fn build() { host_construction::transport_response(); } }",
            "restricted-namespace-wrapper: restricted construction calls cannot be forwarded outside approved producers",
        ),
        Fixture(
            "crates/server/src/connectors/io.rs",
            "impl ConnectorIo for HostIo {}",
            None,
        ),
        Fixture(
            "crates/connector-processors/tests/fake_host.rs",
            "impl ProcessorControl for FakeControl {}",
            None,
        ),
        Fixture(
            "crates/server/tests/connector_fake.rs",
            "impl ConnectorIo for FakeIo {}",
            None,
        ),
        Fixture(
            "crates/connector-catalog/src/bad.rs",
            "impl ConnectorIo for CatalogIo {}",
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            "crates/connector-abi/src/bad.rs",
            "impl ProcessorControl for AbiControl {}",
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            "crates/connector-processors/src/bad_impl.rs",
            "impl ConnectorIo for ProcessorIo {}",
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            "crates/connector-processors/src/owned.rs",
            "let value = Box::new(1_u8);",
            None,
        ),
        Fixture(
            "crates/connector-processors/src/core_leak.rs",
            "value::leak();",
            "processor-allocation-leak: allocation leak APIs are forbidden in processor production code",
        ),
        Fixture(
            "crates/connector-processors/src/box_leak.rs",
            "Box::leak(value);",
            "processor-allocation-leak: allocation leak APIs are forbidden in processor production code",
        ),
        Fixture(
            "crates/connector-processors/src/string_leak.rs",
            "String::leak(value);",
            "processor-allocation-leak: allocation leak APIs are forbidden in processor production code",
        ),
        Fixture(
            "crates/connector-processors/src/vec_leak.rs",
            "Vec::leak(value);",
            "processor-allocation-leak: allocation leak APIs are forbidden in processor production code",
        ),
        Fixture(
            "crates/connector-abi/tests/namespace.rs",
            "host_construction::authorized_correlations();\n"
            "catalog_construction::static_error_code(value);",
            None,
        ),
        Fixture(
            "crates/other/tests/fake_host.rs",
            "impl ConnectorIo for FakeIo {}",
            "test-path-allowlist: connector construction and host fakes are restricted to approved test roots",
        ),
        Fixture(
            "crates/connector-abi/src/lib.rs",
            "#[cfg(test)]\nmod tests { struct Helper; }",
            None,
        ),
        Fixture(
            "crates/connector-abi/src/exported_test_helper.rs",
            "#[cfg(test)]\nmod tests { pub struct Helper; }",
            "exported-test-helper: test helpers in production modules must remain private",
        ),
        Fixture(
            "crates/connector-abi/src/private_test_impl.rs",
            "#[cfg(test)]\nmod tests { impl ConnectorIo for FakeIo {} }",
            None,
        ),
        Fixture(
            "crates/connector-processors/src/private_test_impl.rs",
            "#[cfg(test)]\nmod tests { impl ProcessorControl for FakeControl {} }",
            None,
        ),
        Fixture(
            "crates/server/src/private_test_impl.rs",
            "#[cfg(test)]\nmod tests { impl ConnectorIo for FakeIo {} }",
            None,
        ),
        Fixture(
            "crates/connector-abi/tests/forbidden_fake_host.rs",
            "impl ConnectorIo for FakeIo {}",
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            ".github/workflows/strict.yml",
            "run: cargo clippy -- -D warnings -D clippy::result_large_err",
            None,
        ),
        Fixture(
            "crates/connector-abi/src/allow_large_error.rs",
            "#[allow(clippy::result_large_err)]\nfn bad() {}",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            "crates/connector-abi/src/expect_large_error.rs",
            "#[expect(clippy::result_large_err)]\nfn bad() {}",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            "crates/connector-abi/Cargo.toml",
            '[lints.clippy]\nresult-large-err = "allow"\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/allow-large-error.yml",
            "run: cargo clippy -- -A clippy::result_large_err",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
    )


def run_self_test() -> list[str]:
    failures: list[str] = []
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        rows = sorted(fixtures(), key=lambda fixture: fixture.path)
        for fixture in rows:
            path = root / fixture.path
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(fixture.source, encoding="utf-8")

        for fixture in rows:
            relative = Path(fixture.path)
            diagnostics = scan_fixture(
                relative,
                (root / relative).read_text(encoding="utf-8"),
            )
            if fixture.expected_rule is None:
                if diagnostics:
                    failures.append(
                        f"self-test: {fixture.path}: expected no diagnostic, got {diagnostics}"
                    )
                continue
            expected = (
                f"connector-boundary: {fixture.path}: {fixture.expected_rule}"
            )
            if diagnostics != [expected]:
                failures.append(
                    f"self-test: {fixture.path}: expected [{expected!r}], got {diagnostics}"
                )
    return failures


def workspace_files(root: Path) -> list[Path]:
    paths: list[Path] = []
    for directory, names, files in os.walk(root):
        names[:] = sorted(
            name
            for name in names
            if name not in {"target", ".git", ".worktrees"}
        )
        base = Path(directory)
        for name in sorted(files):
            if name.endswith(".rs"):
                paths.append(base / name)

    for relative in (
        Path("crates/connector-abi/Cargo.toml"),
        Path("Cargo.toml"),
        Path(".cargo/config.toml"),
    ):
        candidate = root / relative
        if candidate.is_file():
            paths.append(candidate)
    workflow_root = root / ".github/workflows"
    if workflow_root.is_dir():
        paths.extend(workflow_root.glob("*.yml"))
        paths.extend(workflow_root.glob("*.yaml"))
    return sorted(set(paths), key=lambda path: path.relative_to(root).as_posix())


def scan_workspace(root: Path) -> list[str]:
    diagnostics: list[str] = []
    for path in workspace_files(root):
        relative = path.relative_to(root)
        diagnostics.extend(
            scan_fixture(relative, path.read_text(encoding="utf-8"))
        )
    return sorted(diagnostics)


def main() -> int:
    if sys.argv[1:] not in ([], ["--self-test"]):
        print("usage: check_connector_processor_boundary.py [--self-test]")
        return 2
    failures = run_self_test()
    if not failures and not sys.argv[1:]:
        root = Path(__file__).resolve().parents[1]
        failures.extend(scan_workspace(root))
    for failure in failures:
        print(failure)
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
