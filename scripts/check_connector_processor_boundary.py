#!/usr/bin/env python3

import ast
from dataclasses import dataclass
import os
from pathlib import Path
import re
import shlex
import sys
import tempfile
import tomllib


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


def macro_invocation_ranges(source: str) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    pattern = re.compile(
        r"\b(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*"
        r"[A-Za-z_][A-Za-z0-9_]*\s*!\s*([\(\{\[])"
    )
    closing = {"(": ")", "{": "}", "[": "]"}
    for match in pattern.finditer(source):
        opening = match.start(1)
        stack: list[str] = []
        for cursor in range(opening, len(source)):
            character = source[cursor]
            if character in closing:
                stack.append(closing[character])
            elif stack and character == stack[-1]:
                stack.pop()
                if not stack:
                    ranges.append((match.start(), cursor + 1))
                    break
    return ranges


def rust_attribute_ranges(source: str) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    offset = 0
    while offset < len(source):
        if source.startswith("#![", offset):
            opening = offset + 2
        elif source.startswith("#[", offset):
            opening = offset + 1
        else:
            offset += 1
            continue
        depth = 0
        cursor = opening
        while cursor < len(source):
            if source[cursor] == "[":
                depth += 1
            elif source[cursor] == "]":
                depth -= 1
                if depth == 0:
                    ranges.append((offset, cursor + 1))
                    offset = cursor
                    break
            cursor += 1
        offset += 1
    return ranges


def normalized_lint_text(value: str) -> str:
    return re.sub(r"\s+", "", value).replace("-", "_")


def flag_tokens(value: object) -> list[str]:
    if isinstance(value, str):
        try:
            return shlex.split(value)
        except ValueError:
            return value.split()
    if isinstance(value, list):
        tokens: list[str] = []
        for item in value:
            tokens.extend(flag_tokens(item))
        return tokens
    return []


def flags_suppress_large_error(tokens: list[str]) -> bool:
    target = "clippy::result_large_err"
    index = 0
    while index < len(tokens):
        token = tokens[index].replace("-", "_")
        if token in ("_A", "__allow"):
            if index + 1 < len(tokens):
                lint = tokens[index + 1].replace("-", "_")
                if lint == target:
                    return True
                index += 1
        elif token.startswith("_A") and token[2:].lstrip("=") == target:
            return True
        elif token.startswith("__allow=") and token.split("=", 1)[1] == target:
            return True
        elif token in ("__cap_lints",):
            if index + 1 < len(tokens) and tokens[index + 1] in ("allow", "warn"):
                return True
            index += 1
        elif token.startswith("__cap_lints="):
            if token.split("=", 1)[1] in ("allow", "warn"):
                return True
        index += 1
    return False


def recursive_values_for_key(value: object, expected: str) -> list[object]:
    values: list[object] = []
    if isinstance(value, dict):
        for key, child in value.items():
            if key.replace("-", "_") == expected:
                values.append(child)
            values.extend(recursive_values_for_key(child, expected))
    elif isinstance(value, list):
        for child in value:
            values.extend(recursive_values_for_key(child, expected))
    return values


def yaml_without_comments(source: str) -> list[str]:
    lines: list[str] = []
    for line in source.splitlines():
        single = False
        double = False
        escaped = False
        end = len(line)
        for index, character in enumerate(line):
            if escaped:
                escaped = False
                continue
            if double and character == "\\":
                escaped = True
                continue
            if character == "'" and not double:
                single = not single
                continue
            if character == '"' and not single:
                double = not double
                continue
            if (
                character == "#"
                and not single
                and not double
                and (index == 0 or line[index - 1].isspace())
            ):
                end = index
                break
        lines.append(line[:end].rstrip())
    return lines


def yaml_scalar(
    lines: list[str],
    index: int,
    indentation: int,
    value: str,
) -> tuple[str, int]:
    if value.startswith(("|", ">")):
        block: list[str] = []
        index += 1
        while index < len(lines):
            line = lines[index]
            if not line.strip():
                block.append("")
                index += 1
                continue
            line_indentation = len(line) - len(line.lstrip())
            if line_indentation <= indentation:
                break
            block.append(line.strip())
            index += 1
        return "\n".join(block), index
    if len(value) >= 2 and value[0] == value[-1] == '"':
        try:
            decoded = ast.literal_eval(value)
        except (SyntaxError, ValueError):
            decoded = value
        return (decoded if isinstance(decoded, str) else value), index + 1
    if len(value) >= 2 and value[0] == value[-1] == "'":
        return value[1:-1].replace("''", "'"), index + 1
    return value, index + 1


def workflow_run_commands(source: str) -> list[str]:
    lines = yaml_without_comments(source)
    commands: list[str] = []
    index = 0
    run_pattern = re.compile(r"^(?P<indent>\s*)(?:-\s*)?run\s*:\s*(?P<value>.*)$")
    while index < len(lines):
        match = run_pattern.match(lines[index])
        if not match:
            index += 1
            continue
        indentation = len(match.group("indent"))
        value = match.group("value").strip()
        scalar, index = yaml_scalar(lines, index, indentation, value)
        commands.append(scalar)
    return commands


def workflow_rustflags(source: str) -> list[str]:
    lines = yaml_without_comments(source)
    values: list[str] = []
    env_pattern = re.compile(r"^(?P<indent>\s*)(?:-\s*)?env\s*:\s*$")
    rustflags_pattern = re.compile(
        r"^(?P<indent>\s*)(?:RUSTFLAGS|\"RUSTFLAGS\"|'RUSTFLAGS')"
        r"\s*:\s*(?P<value>.*)$"
    )
    index = 0
    while index < len(lines):
        env_match = env_pattern.match(lines[index])
        if not env_match:
            index += 1
            continue
        env_indentation = len(env_match.group("indent"))
        index += 1
        entry_indentation: int | None = None
        while index < len(lines):
            line = lines[index]
            if not line.strip():
                index += 1
                continue
            line_indentation = len(line) - len(line.lstrip())
            if line_indentation <= env_indentation:
                break
            if entry_indentation is None:
                entry_indentation = line_indentation
            if line_indentation != entry_indentation:
                index += 1
                continue
            rustflags_match = rustflags_pattern.match(line)
            if not rustflags_match:
                index += 1
                continue
            value = rustflags_match.group("value").strip()
            scalar, index = yaml_scalar(
                lines,
                index,
                len(rustflags_match.group("indent")),
                value,
            )
            values.append(scalar)
    return values


def lint_suppression(relative: str, source: str, code: str) -> Finding | None:
    message = "clippy::result_large_err must remain denied without suppression"
    if relative.endswith(".rs"):
        if not relative.startswith("crates/connector-abi/src/"):
            return None
        for start, end in rust_attribute_ranges(code):
            attribute = normalized_lint_text(code[start:end])
            if (
                "clippy::result_large_err" in attribute
                and re.search(r"(?:allow|expect)\(", attribute)
            ):
                return Finding(start, "large-error-lint-suppression", message)
        return None

    if relative.endswith(".toml") or relative == ".cargo/config":
        try:
            document = tomllib.loads(source)
        except tomllib.TOMLDecodeError:
            return None
        if relative.startswith(".cargo/"):
            for value in recursive_values_for_key(document, "rustflags"):
                if flags_suppress_large_error(flag_tokens(value)):
                    return Finding(0, "large-error-lint-suppression", message)
        else:
            clippy = document.get("lints", {}).get("clippy", {})
            for key, value in clippy.items():
                if key.replace("-", "_") != "result_large_err":
                    continue
                level = value.get("level") if isinstance(value, dict) else value
                if level in ("allow", "warn"):
                    return Finding(0, "large-error-lint-suppression", message)
        return None

    for command in workflow_run_commands(source):
        if flags_suppress_large_error(flag_tokens(command)):
            return Finding(0, "large-error-lint-suppression", message)
    for rustflags in workflow_rustflags(source):
        if flags_suppress_large_error(flag_tokens(rustflags)):
            return Finding(0, "large-error-lint-suppression", message)
    return None


def static_literal_indirection(
    relative: str,
    code: str,
) -> Finding | None:
    if starts_with(relative, STATIC_LITERAL_ROOTS) or starts_with(
        relative, ABI_TEST_ROOTS
    ):
        return None

    macro_ranges = macro_invocation_ranges(code)
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
            return Finding(
                type_alias.start(),
                "static-literal-type-alias",
                f"{type_name}::literal cannot be reached through a type alias outside STATIC_LITERAL_ROOTS",
            )

        for start, end in macro_ranges:
            invocation = code[start:end]
            if re.search(rf"\b{type_name}\b", invocation) and re.search(
                r"\bliteral\b", invocation
            ):
                return Finding(
                    start,
                    "static-literal-wrapper",
                    f"{type_name}::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
                )

        token_macro = re.search(r"\bmacro_rules\s*!", code)
        generic_token_literal = re.search(
            r"\$\s*[A-Za-z_][A-Za-z0-9_]*\s*>\s*::\s*literal\s*\(",
            code,
        )
        token_literal = (
            generic_token_literal and re.search(rf"\b{type_name}\b", code)
        ) or re.search(rf"\b{type_name}\b\s*::\s*literal\b", code)
        token_macro = token_macro if token_literal else None
        if token_macro:
            return Finding(
                token_macro.start(),
                "static-literal-wrapper",
                f"{type_name}::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
            )

        call = re.search(
            rf"(?:<\s*)?\b{type_name}\b(?:\s*>)?\s*::\s*literal\s*\(",
            code,
        )
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


def approved_host_trait_indirection(
    relative: str,
    offset: int,
    test_ranges: list[tuple[int, int]],
) -> bool:
    if starts_with(
        relative,
        HOST_IMPL_ROOTS + PROCESSOR_TEST_ROOTS + SERVER_TEST_ROOTS,
    ):
        return True
    return (
        starts_with(
            relative,
            (
                "crates/connector-abi/src/",
                "crates/connector-processors/src/",
                "crates/server/src/",
            ),
        )
        and in_ranges(offset, test_ranges)
    )


def host_trait_indirection(
    relative: str,
    code: str,
    test_ranges: list[tuple[int, int]],
) -> Finding | None:
    macro_ranges = macro_invocation_ranges(code)
    for trait_name in ("ConnectorIo", "ProcessorControl"):
        reexport = re.search(
            rf"\bpub\s+use\b[^;]*\b{trait_name}\b[^;]*;",
            code,
        )
        if reexport and not (
            relative == "crates/connector-abi/src/lib.rs"
            and not re.search(rf"\b{trait_name}\s+as\s+", reexport.group())
        ):
            if not approved_host_trait_indirection(
                relative, reexport.start(), test_ranges
            ):
                return Finding(
                    reexport.start(),
                    "host-trait-reexport",
                    f"{trait_name} cannot be re-exported outside approved host implementation roots",
                )

        alias = re.search(
            rf"\buse\b[^;]*\b{trait_name}\s+as\s+"
            r"[A-Za-z_][A-Za-z0-9_]*[^;]*;",
            code,
        )
        if alias and not approved_host_trait_indirection(
            relative, alias.start(), test_ranges
        ):
            return Finding(
                alias.start(),
                "host-trait-alias",
                f"{trait_name} cannot be aliased outside approved host implementation roots",
            )

        type_alias = re.search(
            rf"\btype\s+[A-Za-z_][A-Za-z0-9_]*[^=;]*=\s*"
            rf"(?:dyn\s+)?(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*{trait_name}\s*;",
            code,
        )
        if type_alias and not approved_host_trait_indirection(
            relative, type_alias.start(), test_ranges
        ):
            return Finding(
                type_alias.start(),
                "host-trait-type-alias",
                f"{trait_name} cannot be reached through a type alias outside approved host implementation roots",
            )

        for start, end in macro_ranges:
            if (
                re.search(rf"\b{trait_name}\b", code[start:end])
                and not approved_host_trait_indirection(
                    relative, start, test_ranges
                )
            ):
                return Finding(
                    start,
                    "host-trait-implementation",
                    "host traits can only be implemented in approved host or test roots",
                )

        token_macro = (
            re.search(r"\bmacro_rules\s*!", code)
            if re.search(
                r"\bimpl\s+\$\s*[A-Za-z_][A-Za-z0-9_]*\s+for\b",
                code,
            )
            and re.search(rf"\b{trait_name}\b", code)
            else None
        )
        if token_macro and not approved_host_trait_indirection(
            relative, token_macro.start(), test_ranges
        ):
            return Finding(
                token_macro.start(),
                "host-trait-implementation",
                "host traits can only be implemented in approved host or test roots",
            )
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

    host_indirection = host_trait_indirection(relative, code, test_ranges)
    if host_indirection:
        return [host_indirection]

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
            "crates/server/src/connectors/forbidden_static_error_standalone_alias.rs",
            "use donat_connector_abi::StaticErrorCode as ErrorCode;",
            "static-literal-alias: StaticErrorCode::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_standalone_alias.rs",
            "use donat_connector_abi::StaticSafeMessage as SafeMessage;",
            "static-literal-alias: StaticSafeMessage::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_standalone_reexport.rs",
            "pub use donat_connector_abi::StaticErrorCode;",
            "static-literal-reexport: StaticErrorCode::literal cannot be re-exported outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_standalone_reexport.rs",
            "pub use donat_connector_abi::StaticSafeMessage;",
            "static-literal-reexport: StaticSafeMessage::literal cannot be re-exported outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_standalone_type_alias.rs",
            "type ErrorCode = donat_connector_abi::StaticErrorCode;",
            "static-literal-type-alias: StaticErrorCode::literal cannot be reached through a type alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_standalone_type_alias.rs",
            "type SafeMessage = donat_connector_abi::StaticSafeMessage;",
            "static-literal-type-alias: StaticSafeMessage::literal cannot be reached through a type alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_qualified.rs",
            'const CODE: StaticErrorCode = <StaticErrorCode>::literal("connector_failed");',
            "static-literal-producer: static failure literals are restricted to approved roots",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_qualified.rs",
            'const MESSAGE: StaticSafeMessage = <StaticSafeMessage>::literal("connector failed");',
            "static-literal-producer: static failure literals are restricted to approved roots",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_token_macro.rs",
            "macro_rules! make_literal {\n"
            "    ($kind:ty, $value:expr) => { <$kind>::literal($value) };\n"
            "}\n"
            'const CODE: StaticErrorCode = make_literal!(StaticErrorCode, "connector_failed");',
            "static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_token_macro.rs",
            "macro_rules! make_literal {\n"
            "    ($kind:ty, $value:expr) => { <$kind>::literal($value) };\n"
            "}\n"
            'const MESSAGE: StaticSafeMessage = make_literal!(StaticSafeMessage, "connector failed");',
            "static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_constructor_token.rs",
            "macro_rules! invoke {\n"
            "    ($constructor:path, $value:expr) => { $constructor($value) };\n"
            "}\n"
            'const CODE: StaticErrorCode = invoke!(StaticErrorCode::literal, "connector_failed");',
            "static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_constructor_token.rs",
            "macro_rules! invoke {\n"
            "    ($constructor:path, $value:expr) => { $constructor($value) };\n"
            "}\n"
            'const MESSAGE: StaticSafeMessage = invoke!(StaticSafeMessage::literal, "connector failed");',
            "static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/macro-support/src/static_literal.rs",
            "#[macro_export]\n"
            "macro_rules! construct_static {\n"
            "    ($kind:ty, $method:ident, $value:expr) => {\n"
            "        <$kind>::$method($value)\n"
            "    };\n"
            "}",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_cross_file_tokens.rs",
            "use macro_support::construct_static;\n"
            "const CODE: donat_connector_abi::StaticErrorCode = construct_static!(\n"
            "    donat_connector_abi::StaticErrorCode,\n"
            "    literal,\n"
            '    "connector_failed",\n'
            ");",
            "static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_cross_file_tokens.rs",
            "use macro_support::construct_static;\n"
            "const MESSAGE: donat_connector_abi::StaticSafeMessage = construct_static!(\n"
            "    donat_connector_abi::StaticSafeMessage,\n"
            "    literal,\n"
            '    "connector failed",\n'
            ");",
            "static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/connector-processors/src/allowed_static_cross_file_tokens.rs",
            "use macro_support::construct_static;\n"
            "const CODE: donat_connector_abi::StaticErrorCode = construct_static!(\n"
            "    donat_connector_abi::StaticErrorCode,\n"
            "    literal,\n"
            '    "connector_failed",\n'
            ");",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/macro_token_decoy.rs",
            "// construct_static!(StaticErrorCode, literal, value);\n"
            'const TEXT: &str = "implement_host!(ConnectorIo, Bad)";\n'
            "/* construct_static!(StaticSafeMessage, literal, value); */",
            None,
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
            "crates/connector-processors/src/connector_io_alias_impl.rs",
            "use donat_connector_abi::ConnectorIo as Io;\nimpl Io for Bad {}",
            "host-trait-alias: ConnectorIo cannot be aliased outside approved host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/processor_control_alias_impl.rs",
            "use donat_connector_abi::ProcessorControl as Control;\nimpl Control for Bad {}",
            "host-trait-alias: ProcessorControl cannot be aliased outside approved host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/connector_io_reexport.rs",
            "pub use donat_connector_abi::ConnectorIo;",
            "host-trait-reexport: ConnectorIo cannot be re-exported outside approved host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/processor_control_reexport.rs",
            "pub use donat_connector_abi::ProcessorControl;",
            "host-trait-reexport: ProcessorControl cannot be re-exported outside approved host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/connector_io_type_alias.rs",
            "type Io = dyn donat_connector_abi::ConnectorIo;",
            "host-trait-type-alias: ConnectorIo cannot be reached through a type alias outside approved host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/processor_control_type_alias.rs",
            "type Control = dyn donat_connector_abi::ProcessorControl;",
            "host-trait-type-alias: ProcessorControl cannot be reached through a type alias outside approved host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/connector_io_token_macro.rs",
            "macro_rules! implement_host {\n"
            "    ($host_trait:path) => { impl $host_trait for Bad {} };\n"
            "}\n"
            "implement_host!(ConnectorIo);",
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            "crates/connector-processors/src/processor_control_token_macro.rs",
            "macro_rules! implement_host {\n"
            "    ($host_trait:path) => { impl $host_trait for Bad {} };\n"
            "}\n"
            "implement_host!(ProcessorControl);",
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            "crates/macro-support/src/host_impl.rs",
            "#[macro_export]\n"
            "macro_rules! implement_host {\n"
            "    ($host_trait:ident, $host:ty) => {\n"
            "        impl donat_connector_abi::$host_trait for $host {}\n"
            "    };\n"
            "}",
            None,
        ),
        Fixture(
            "crates/connector-processors/src/connector_io_cross_file_token.rs",
            "use macro_support::implement_host;\n"
            "implement_host!(donat_connector_abi::ConnectorIo, Bad);",
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            "crates/connector-processors/src/processor_control_cross_file_token.rs",
            "use macro_support::implement_host;\n"
            "implement_host!(donat_connector_abi::ProcessorControl, Bad);",
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            "crates/server/src/connectors/allowed_host_cross_file_token.rs",
            "use macro_support::implement_host;\n"
            "implement_host!(donat_connector_abi::ConnectorIo, ServerIo);",
            None,
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
            "crates/connector-abi/src/inner_allow_large_error.rs",
            "#![allow(clippy::result_large_err)]\nfn bad() {}",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            "crates/connector-abi/src/inner_expect_large_error.rs",
            "#![expect(clippy::result_large_err)]\nfn bad() {}",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            "crates/connector-abi/src/multi_allow_large_error.rs",
            "#[allow(dead_code, clippy::result_large_err, unused_variables)]\nfn bad() {}",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            "crates/connector-abi/src/multi_expect_large_error.rs",
            "#[expect(dead_code, clippy::result_large_err)]\nfn bad() {}",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            "crates/connector-abi/src/cfg_attr_allow_large_error.rs",
            "#![cfg_attr(test, allow(dead_code, clippy::result_large_err))]\nfn bad() {}",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            "crates/connector-abi/Cargo.toml",
            '[lints.clippy]\nresult-large-err = "allow"\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/config.toml",
            '[build]\nrustflags = ["-A", "clippy::result_large_err"]\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/string-rustflags.toml",
            '[build]\nrustflags = "-A clippy::result_large_err"\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/cap-lints.toml",
            '[build]\nrustflags = ["--cap-lints", "warn"]\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/config",
            '[build]\nrustflags = "-Aclippy::result-large-err"\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/quoted-decoy.toml",
            '[package]\ndescription = "rustflags = [\'-A\', \'clippy::result_large_err\']"\n',
            None,
        ),
        Fixture(
            ".cargo/inline-comment-decoy.toml",
            '[build]\nrustflags = ["-D", "clippy::result_large_err"] # rustflags = ["-A", "clippy::result_large_err"]\n',
            None,
        ),
        Fixture(
            ".github/workflows/allow-large-error.yml",
            "run: cargo clippy -- -A clippy::result_large_err",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/multiline-cap-lints.yml",
            "steps:\n"
            "  - run: |\n"
            "      cargo clippy --\n"
            "        --cap-lints\n"
            "        allow\n"
            "        -D clippy::result_large_err\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/long-allow-large-error.yml",
            "steps:\n"
            "  - run: cargo clippy -- --allow=clippy::result_large_err\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/quoted-run-allow-large-error.yml",
            'steps:\n  - run: "cargo clippy -- -A clippy::result_large_err"\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/workflow-env-allow-large-error.yml",
            'env:\n  RUSTFLAGS: "-A clippy::result_large_err"\n'
            "jobs:\n  test:\n    runs-on: ubuntu-latest\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/job-env-cap-lints.yml",
            "jobs:\n"
            "  test:\n"
            "    env:\n"
            "      RUSTFLAGS: |-\n"
            "        --cap-lints\n"
            "        warn\n"
            "    runs-on: ubuntu-latest\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/step-env-allow-large-error.yml",
            "jobs:\n"
            "  test:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - env:\n"
            "          RUSTFLAGS: >-\n"
            "            --allow\n"
            "            clippy::result-large-err\n"
            "        run: cargo clippy\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/env-quoted-decoy.yml",
            "env:\n"
            '  DESCRIPTION: \'RUSTFLAGS: "-A clippy::result_large_err"\'\n'
            "jobs: {}\n",
            None,
        ),
        Fixture(
            ".github/workflows/env-inline-comment-decoy.yml",
            "env:\n"
            '  RUSTFLAGS: "-D clippy::result_large_err" # -A clippy::result_large_err\n'
            "jobs: {}\n",
            None,
        ),
        Fixture(
            ".github/workflows/env-multiline-decoy.yml",
            "env:\n"
            "  DESCRIPTION: |-\n"
            '    RUSTFLAGS: "-A clippy::result_large_err"\n'
            "jobs: {}\n",
            None,
        ),
        Fixture(
            ".github/workflows/inline-comment-decoy.yml",
            "steps:\n"
            "  - run: cargo clippy -- -D clippy::result_large_err # -A clippy::result_large_err\n",
            None,
        ),
        Fixture(
            ".github/workflows/quoted-decoy.yml",
            'name: "-A clippy::result_large_err"\nsteps: []\n',
            None,
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
        Path(".cargo/config"),
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
