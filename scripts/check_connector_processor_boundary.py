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

UNICODE_VERSION_GAP_IDENTIFIER = "\U00016100"
RUST_PATTERN_WHITESPACE_CASES = (
    ("next_line", "\u0085"),
    ("left_to_right_mark", "\u200e"),
    ("right_to_left_mark", "\u200f"),
    ("line_separator", "\u2028"),
    ("paragraph_separator", "\u2029"),
)
RUST_PATTERN_WHITESPACE = frozenset(
    "\u0009\u000a\u000b\u000c\u000d\u0020"
    "\u0085\u200e\u200f\u2028\u2029"
)


@dataclass(frozen=True)
class Fixture:
    path: str
    source: str
    expected_rule: str | None


@dataclass(frozen=True)
class WorkspaceFixture:
    name: str
    root_manifest: str
    abi_manifest: str
    expected_rule: str | None


@dataclass(frozen=True)
class Finding:
    offset: int
    rule: str
    message: str


@dataclass(frozen=True)
class RustToken:
    value: str
    offset: int
    identifier: bool
    raw: bool = False


@dataclass(frozen=True)
class RustUseLeaf:
    start: int
    path: tuple[str, ...]
    alias: str | None
    public: bool


@dataclass(frozen=True)
class RustUse:
    start: int
    tokens: tuple[RustToken, ...]
    public: bool
    leaves: tuple[RustUseLeaf, ...]
    parsed: bool


@dataclass(frozen=True)
class RustTypeAlias:
    start: int
    right_hand_side: tuple[RustToken, ...]


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

    def scalar_end(offset: int) -> int | None:
        if offset >= len(data):
            return None
        first = data[offset]
        if first < 0x80:
            if first in (10, 13, 39, 92):
                return None
            return offset + 1
        width = (
            2
            if 0xC2 <= first <= 0xDF
            else 3
            if 0xE0 <= first <= 0xEF
            else 4
            if 0xF0 <= first <= 0xF4
            else 0
        )
        if not width or offset + width > len(data):
            return None
        try:
            data[offset : offset + width].decode("utf-8")
        except UnicodeDecodeError:
            return None
        return offset + width

    def escaped_character_end(offset: int, byte: bool) -> int | None:
        if offset >= len(data) or data[offset] != 92:
            return None
        cursor = offset + 1
        if cursor >= len(data):
            return None
        if data[cursor] in b"nrt0\\\\\'\"":
            return cursor + 1
        if data[cursor] == ord("x"):
            end = cursor + 3
            if end > len(data) or any(
                character not in b"0123456789abcdefABCDEF"
                for character in data[cursor + 1 : end]
            ):
                return None
            if not byte and int(data[cursor + 1 : end], 16) > 0x7F:
                return None
            return end
        if byte or data[cursor] != ord("u") or cursor + 1 >= len(data):
            return None
        if data[cursor + 1] != ord("{"):
            return None
        cursor += 2
        digits: list[int] = []
        underscore = False
        while cursor < len(data) and data[cursor] != ord("}"):
            character = data[cursor]
            if character in b"0123456789abcdefABCDEF":
                digits.append(character)
                underscore = False
            elif character == ord("_") and digits and not underscore:
                underscore = True
            else:
                return None
            cursor += 1
        if (
            cursor >= len(data)
            or not digits
            or underscore
            or len(digits) > 6
        ):
            return None
        value = int(bytes(digits), 16)
        if value > 0x10FFFF or 0xD800 <= value <= 0xDFFF:
            return None
        return cursor + 1

    def character_end(offset: int, byte: bool) -> int | None:
        if offset >= len(data):
            return None
        if data[offset] == 92:
            end = escaped_character_end(offset, byte)
        else:
            end = scalar_end(offset)
            if byte and end is not None and data[offset] >= 0x80:
                return None
        if end is not None and end < len(data) and data[end] == 39:
            return end + 1
        return None

    def follows_identifier(offset: int) -> bool:
        if offset == 0:
            return False
        previous = data[offset - 1 : offset]
        return (
            data[offset - 1] >= 0x80
            or b"0" <= previous <= b"9"
            or b"A" <= previous <= b"Z"
            or b"a" <= previous <= b"z"
            or previous == b"_"
        )

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

        if data[index : index + 2] == b"b'" and not follows_identifier(index):
            char_start = index
            character = index + 2
            byte_character = True
        elif (
            data[index : index + 1] == b"'"
            and not follows_identifier(index)
        ):
            char_start = index
            character = index + 1
            byte_character = False
        else:
            character = -1
        if character >= 0:
            end = character_end(character, byte_character)
            if end is not None:
                blank(char_start, end)
                index = end
                continue

        index += 1

    return output.decode("utf-8")


def rust_identifier_atom_character(character: str) -> bool:
    if character in RUST_PATTERN_WHITESPACE:
        return False
    if ord(character) >= 0x80:
        return True
    return (
        "A" <= character <= "Z"
        or "a" <= character <= "z"
        or "0" <= character <= "9"
        or character == "_"
    )


def rust_name(token: RustToken, name: str) -> bool:
    return token.identifier and token.value == name


def rust_keyword(token: RustToken, keyword: str) -> bool:
    return rust_name(token, keyword) and not token.raw


def rust_tokens(source: str) -> list[RustToken]:
    tokens: list[RustToken] = []
    index = 0
    while index < len(source):
        character = source[index]
        if index == 0 and character == "\ufeff":
            index += 1
            continue
        if character in RUST_PATTERN_WHITESPACE:
            index += 1
            continue

        if source.startswith("::", index):
            tokens.append(RustToken("::", index, False))
            index += 2
            continue

        if (
            source.startswith("r#", index)
            and index + 2 < len(source)
            and rust_identifier_atom_character(source[index + 2])
        ):
            end = index + 2
            while end < len(source) and rust_identifier_atom_character(source[end]):
                end += 1
            tokens.append(RustToken(source[index + 2 : end], index, True, True))
            index = end
            continue

        if rust_identifier_atom_character(character):
            end = index + 1
            while end < len(source) and rust_identifier_atom_character(source[end]):
                end += 1
            tokens.append(RustToken(source[index:end], index, True))
            index = end
            continue

        tokens.append(RustToken(character, index, False))
        index += 1
    return tokens


def rust_use_leaves(
    tokens: tuple[RustToken, ...],
    public: bool,
) -> tuple[RustUseLeaf, ...] | None:
    def finish_leaf(
        start: int,
        path: tuple[str, ...],
        cursor: int,
    ) -> tuple[list[RustUseLeaf], int] | None:
        alias: str | None = None
        if cursor < len(tokens) and rust_keyword(tokens[cursor], "as"):
            cursor += 1
            if cursor >= len(tokens):
                return None
            alias_token = tokens[cursor]
            if not (alias_token.identifier or alias_token.value == "_"):
                return None
            alias = alias_token.value
            cursor += 1
        return [RustUseLeaf(start, path, alias, public)], cursor

    def parse_tree(
        cursor: int,
        inherited: tuple[str, ...],
    ) -> tuple[list[RustUseLeaf], int] | None:
        if cursor >= len(tokens):
            return None
        if tokens[cursor].value == "::":
            cursor += 1
            if cursor >= len(tokens):
                return None

        start = tokens[cursor].offset
        path = inherited
        if tokens[cursor].value == "{":
            cursor += 1
            leaves: list[RustUseLeaf] = []
            if cursor < len(tokens) and tokens[cursor].value == "}":
                return leaves, cursor + 1
            while cursor < len(tokens):
                parsed = parse_tree(cursor, path)
                if parsed is None:
                    return None
                branch_leaves, cursor = parsed
                leaves.extend(branch_leaves)
                if cursor >= len(tokens):
                    return None
                if tokens[cursor].value == "}":
                    return leaves, cursor + 1
                if tokens[cursor].value != ",":
                    return None
                cursor += 1
                if cursor < len(tokens) and tokens[cursor].value == "}":
                    return leaves, cursor + 1
            return None

        if tokens[cursor].value == "*":
            return finish_leaf(start, path, cursor + 1)

        while cursor < len(tokens):
            segment = tokens[cursor]
            if not segment.identifier or rust_keyword(segment, "as"):
                return None
            if rust_keyword(segment, "self") and (
                cursor + 1 >= len(tokens)
                or tokens[cursor + 1].value != "::"
            ):
                return finish_leaf(start, path, cursor + 1)
            path += (segment.value,)
            cursor += 1
            if cursor >= len(tokens) or tokens[cursor].value != "::":
                return finish_leaf(start, path, cursor)
            cursor += 1
            if cursor >= len(tokens):
                return None
            if tokens[cursor].value == "{":
                return parse_tree(cursor, path)
            if tokens[cursor].value == "*":
                return finish_leaf(start, path, cursor + 1)
        return None

    parsed = parse_tree(0, ())
    if parsed is None:
        return None
    leaves, cursor = parsed
    if cursor != len(tokens):
        return None
    return tuple(leaves)


def rust_use_statements(tokens: list[RustToken]) -> list[RustUse]:
    statements: list[RustUse] = []
    for index, token in enumerate(tokens):
        if not rust_keyword(token, "use"):
            continue
        end = index + 1
        while end < len(tokens) and tokens[end].value != ";":
            end += 1
        statement_start = index
        cursor = index - 1
        while cursor >= 0 and tokens[cursor].value not in (";", "{", "}"):
            statement_start = cursor
            cursor -= 1
        prefix = tokens[statement_start:index]
        public = any(rust_keyword(item, "pub") for item in prefix)
        statement_tokens = tuple(tokens[index + 1 : end])
        leaves = rust_use_leaves(statement_tokens, public)
        statements.append(
            RustUse(
                tokens[statement_start].offset,
                statement_tokens,
                public,
                leaves if leaves is not None else (),
                leaves is not None,
            )
        )
    return statements


def rust_use_mentions(statement: RustUse, name: str) -> bool:
    if statement.parsed:
        return any(name in leaf.path for leaf in statement.leaves)
    return any(rust_name(token, name) for token in statement.tokens)


def rust_use_aliases(statement: RustUse, name: str) -> bool:
    if statement.parsed:
        return any(
            leaf.alias is not None and name in leaf.path
            for leaf in statement.leaves
        )
    mentioned = rust_use_mentions(statement, name)
    has_alias = any(rust_keyword(token, "as") for token in statement.tokens)
    return mentioned and has_alias


def rust_type_aliases(tokens: list[RustToken]) -> list[RustTypeAlias]:
    aliases: list[RustTypeAlias] = []
    for index, token in enumerate(tokens):
        if not rust_keyword(token, "type"):
            continue
        equals = index + 1
        while equals < len(tokens) and tokens[equals].value not in ("=", ";", "{"):
            equals += 1
        if equals >= len(tokens) or tokens[equals].value != "=":
            continue
        end = equals + 1
        while end < len(tokens) and tokens[end].value != ";":
            end += 1
        aliases.append(
            RustTypeAlias(token.offset, tuple(tokens[equals + 1 : end]))
        )
    return aliases


def rust_tokens_mention(tokens: tuple[RustToken, ...], name: str) -> bool:
    return any(rust_name(token, name) for token in tokens)


def rust_tokens_in_range(
    tokens: list[RustToken],
    start: int,
    end: int,
) -> tuple[RustToken, ...]:
    return tuple(token for token in tokens if start <= token.offset < end)


def rust_exported_item_in_range(
    tokens: list[RustToken],
    start: int,
    end: int,
) -> int | None:
    item_keywords = (
        "const",
        "enum",
        "fn",
        "mod",
        "static",
        "struct",
        "trait",
        "type",
        "use",
    )
    range_tokens = rust_tokens_in_range(tokens, start, end)
    for index, token in enumerate(range_tokens):
        if not rust_keyword(token, "pub"):
            continue
        cursor = index + 1
        if cursor < len(range_tokens) and range_tokens[cursor].value == "(":
            depth = 1
            cursor += 1
            while cursor < len(range_tokens) and depth:
                if range_tokens[cursor].value == "(":
                    depth += 1
                elif range_tokens[cursor].value == ")":
                    depth -= 1
                cursor += 1
            if depth:
                continue
        if (
            cursor < len(range_tokens)
            and any(
                rust_keyword(range_tokens[cursor], keyword)
                for keyword in item_keywords
            )
        ):
            return token.offset
    return None


def rust_keyword_before(
    tokens: list[RustToken],
    offset: int,
    keyword: str,
) -> bool:
    return any(
        token.offset < offset
        and rust_keyword(token, keyword)
        for token in tokens
    )


def rust_named_item_before(
    tokens: list[RustToken],
    offset: int,
    keyword: str,
) -> bool:
    for index, token in enumerate(tokens[:-1]):
        if token.offset >= offset:
            break
        if (
            rust_keyword(token, keyword)
            and tokens[index + 1].identifier
        ):
            return True
    return False


def rust_has_generic_literal_constructor(tokens: list[RustToken]) -> bool:
    for index in range(len(tokens) - 5):
        if (
            tokens[index].value == "$"
            and tokens[index + 1].identifier
            and tokens[index + 2].value == ">"
            and tokens[index + 3].value == "::"
            and rust_name(tokens[index + 4], "literal")
            and tokens[index + 5].value == "("
        ):
            return True
    return False


def rust_path_references(
    tokens: list[RustToken],
    owner: str,
    member: str | None,
) -> list[int]:
    references: list[int] = []
    for index, token in enumerate(tokens):
        if not rust_name(token, owner):
            continue
        cursor = index + 1
        if cursor < len(tokens) and tokens[cursor].value == ">":
            cursor += 1
        if member is None:
            previous_is_path = index > 0 and tokens[index - 1].value == "::"
            next_is_path = cursor < len(tokens) and tokens[cursor].value == "::"
            if previous_is_path or next_is_path:
                references.append(token.offset)
            continue
        if (
            cursor + 1 < len(tokens)
            and tokens[cursor].value == "::"
            and rust_name(tokens[cursor + 1], member)
        ):
            references.append(token.offset)
    return references


def exported_cfg_test_module(tokens: list[RustToken]) -> int | None:
    for index in range(len(tokens) - 9):
        if not (
            tokens[index].value == "#"
            and tokens[index + 1].value == "["
            and rust_name(tokens[index + 2], "cfg")
            and tokens[index + 3].value == "("
            and rust_name(tokens[index + 4], "test")
            and tokens[index + 5].value == ")"
            and tokens[index + 6].value == "]"
        ):
            continue
        cursor = index + 7
        if not rust_keyword(tokens[cursor], "pub"):
            continue
        cursor += 1
        if cursor < len(tokens) and tokens[cursor].value == "(":
            depth = 1
            cursor += 1
            while cursor < len(tokens) and depth:
                if tokens[cursor].value == "(":
                    depth += 1
                elif tokens[cursor].value == ")":
                    depth -= 1
                cursor += 1
        if (
            cursor + 1 < len(tokens)
            and rust_keyword(tokens[cursor], "mod")
            and tokens[cursor + 1].identifier
        ):
            return tokens[index].offset
    return None


def rust_impl_trait_references(
    tokens: list[RustToken],
) -> list[tuple[int, str]]:
    references: list[tuple[int, str]] = []
    host_traits = ("ConnectorIo", "ProcessorControl")
    for index, token in enumerate(tokens):
        if not rust_keyword(token, "impl"):
            continue
        cursor = index + 1
        while (
            cursor < len(tokens)
            and not rust_keyword(tokens[cursor], "for")
            and tokens[cursor].value not in ("{", ";")
        ):
            candidate = tokens[cursor]
            if any(rust_name(candidate, trait) for trait in host_traits):
                lookahead = cursor + 1
                while (
                    lookahead < len(tokens)
                    and not rust_keyword(tokens[lookahead], "for")
                    and tokens[lookahead].value not in ("{", ";")
                ):
                    lookahead += 1
                if lookahead < len(tokens) and rust_keyword(tokens[lookahead], "for"):
                    references.append((candidate.offset, candidate.value))
                break
            cursor += 1
    return references


def private_cfg_test_ranges(tokens: list[RustToken]) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    for index in range(len(tokens) - 9):
        if not (
            tokens[index].value == "#"
            and tokens[index + 1].value == "["
            and rust_name(tokens[index + 2], "cfg")
            and tokens[index + 3].value == "("
            and rust_name(tokens[index + 4], "test")
            and tokens[index + 5].value == ")"
            and tokens[index + 6].value == "]"
            and rust_keyword(tokens[index + 7], "mod")
        ):
            continue
        name = tokens[index + 8]
        opening_index = index + 9
        if not name.identifier or tokens[opening_index].value != "{":
            continue
        depth = 0
        cursor = opening_index
        while cursor < len(tokens):
            if tokens[cursor].value == "{":
                depth += 1
            elif tokens[cursor].value == "}":
                depth -= 1
                if depth == 0:
                    ranges.append(
                        (tokens[index].offset, tokens[cursor].offset + 1)
                    )
                    break
            cursor += 1
    return ranges


def in_ranges(offset: int, ranges: list[tuple[int, int]]) -> bool:
    return any(start <= offset < end for start, end in ranges)


def macro_invocation_ranges(tokens: list[RustToken]) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    closing = {"(": ")", "{": "}", "[": "]"}
    for index, token in enumerate(tokens):
        if token.value != "!" or index == 0 or not tokens[index - 1].identifier:
            continue
        if index + 1 >= len(tokens) or tokens[index + 1].value not in closing:
            continue
        start = index - 1
        while (
            start >= 2
            and tokens[start - 1].value == "::"
            and tokens[start - 2].identifier
        ):
            start -= 2
        opening = index + 1
        stack: list[str] = []
        for cursor in range(opening, len(tokens)):
            character = tokens[cursor].value
            if character in closing:
                stack.append(closing[character])
            elif stack and character == stack[-1]:
                stack.pop()
                if not stack:
                    ranges.append(
                        (tokens[start].offset, tokens[cursor].offset + 1)
                    )
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
    separate_levels = ("_A", "_W", "__allow", "__warn", "__force_warn")
    attached_levels = ("_A", "_W")
    assigned_levels = ("__allow=", "__warn=", "__force_warn=")
    index = 0
    while index < len(tokens):
        token = tokens[index].replace("-", "_")
        if token in separate_levels:
            if index + 1 < len(tokens):
                lint = tokens[index + 1].replace("-", "_")
                if lint == target:
                    return True
                index += 1
        elif any(
            token.startswith(prefix)
            and token[len(prefix) :].lstrip("=") == target
            for prefix in attached_levels
        ):
            return True
        elif any(
            token.startswith(prefix) and token[len(prefix) :] == target
            for prefix in assigned_levels
        ):
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


def yaml_key_indentation(match: re.Match[str]) -> int:
    return len(match.group("indent")) + len(match.groupdict().get("dash") or "")


def yaml_block_scalar_content(lines: list[str]) -> set[int]:
    content: set[int] = set()
    header_pattern = re.compile(
        r"^(?P<indent>\s*)(?P<dash>-\s*)?.+:\s*[|>][1-9+-]*\s*$"
    )
    index = 0
    while index < len(lines):
        if index in content:
            index += 1
            continue
        match = header_pattern.match(lines[index])
        if not match:
            index += 1
            continue
        indentation = yaml_key_indentation(match)
        index += 1
        while index < len(lines):
            line = lines[index]
            if not line.strip():
                content.add(index)
                index += 1
                continue
            line_indentation = len(line) - len(line.lstrip())
            if line_indentation <= indentation:
                break
            content.add(index)
            index += 1
    return content


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
    block_content = yaml_block_scalar_content(lines)
    commands: list[str] = []
    index = 0
    run_pattern = re.compile(
        r"^(?P<indent>\s*)(?P<dash>-\s*)?run\s*:\s*(?P<value>.*)$"
    )
    while index < len(lines):
        if index in block_content:
            index += 1
            continue
        match = run_pattern.match(lines[index])
        if not match:
            index += 1
            continue
        indentation = yaml_key_indentation(match)
        value = match.group("value").strip()
        scalar, index = yaml_scalar(lines, index, indentation, value)
        commands.append(scalar)
    return commands


def workflow_rustflags(source: str) -> list[str]:
    lines = yaml_without_comments(source)
    block_content = yaml_block_scalar_content(lines)
    values: list[str] = []
    env_pattern = re.compile(
        r"^(?P<indent>\s*)(?P<dash>-\s*)?env\s*:\s*$"
    )
    rustflags_pattern = re.compile(
        r"^(?P<indent>\s*)(?:RUSTFLAGS|\"RUSTFLAGS\"|'RUSTFLAGS')"
        r"\s*:\s*(?P<value>.*)$"
    )
    index = 0
    while index < len(lines):
        if index in block_content:
            index += 1
            continue
        env_match = env_pattern.match(lines[index])
        if not env_match:
            index += 1
            continue
        env_indentation = yaml_key_indentation(env_match)
        index += 1
        entry_indentation: int | None = None
        while index < len(lines):
            line = lines[index]
            if index in block_content:
                index += 1
                continue
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
                and re.search(
                    r"(?:allow|expect|warn|force_warn)\(",
                    attribute,
                )
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
    tokens: list[RustToken],
) -> Finding | None:
    if starts_with(relative, STATIC_LITERAL_ROOTS) or starts_with(
        relative, ABI_TEST_ROOTS
    ):
        return None

    use_statements = rust_use_statements(tokens)
    type_aliases = rust_type_aliases(tokens)
    macro_ranges = macro_invocation_ranges(tokens)
    for type_name in ("StaticErrorCode", "StaticSafeMessage"):
        reexport = next(
            (
                statement
                for statement in use_statements
                if statement.public and rust_use_mentions(statement, type_name)
            ),
            None,
        )
        if reexport:
            return Finding(
                reexport.start,
                "static-literal-reexport",
                f"{type_name}::literal cannot be re-exported outside STATIC_LITERAL_ROOTS",
            )

        alias = next(
            (
                statement
                for statement in use_statements
                if rust_use_aliases(statement, type_name)
            ),
            None,
        )
        if alias:
            return Finding(
                alias.start,
                "static-literal-alias",
                f"{type_name}::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
            )

        type_alias = next(
            (
                alias
                for alias in type_aliases
                if rust_tokens_mention(alias.right_hand_side, type_name)
            ),
            None,
        )
        if type_alias:
            return Finding(
                type_alias.start,
                "static-literal-type-alias",
                f"{type_name}::literal cannot be reached through a type alias outside STATIC_LITERAL_ROOTS",
            )

        for start, end in macro_ranges:
            invocation = rust_tokens_in_range(tokens, start, end)
            if rust_tokens_mention(
                invocation,
                type_name,
            ) and rust_tokens_mention(
                invocation,
                "literal",
            ):
                return Finding(
                    start,
                    "static-literal-wrapper",
                    f"{type_name}::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
                )

        if (
            any(
                rust_keyword(token, "macro_rules")
                for token in tokens
            )
            and rust_has_generic_literal_constructor(tokens)
            and any(
                rust_name(token, type_name)
                for token in tokens
            )
        ):
            macro_offset = next(
                token.offset
                for token in tokens
                if rust_keyword(token, "macro_rules")
            )
            return Finding(
                macro_offset,
                "static-literal-wrapper",
                f"{type_name}::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS",
            )

        references = rust_path_references(tokens, type_name, "literal")
        if not references:
            continue
        reference = references[0]
        wrappers = (
            ("macro", rust_keyword_before(tokens, reference, "macro_rules")),
            ("trait", rust_named_item_before(tokens, reference, "trait")),
            ("function", rust_named_item_before(tokens, reference, "fn")),
        )
        for wrapper, marker in wrappers:
            if marker:
                return Finding(
                    reference,
                    "static-literal-wrapper",
                    f"{type_name}::literal cannot be forwarded by a {wrapper} outside STATIC_LITERAL_ROOTS",
                )
        return Finding(
            reference,
            "static-literal-producer",
            "static failure literals are restricted to approved roots",
        )
    return None


def restricted_namespace(
    relative: str,
    tokens: list[RustToken],
    test_ranges: list[tuple[int, int]],
) -> Finding | None:
    namespaces = ("host_construction", "catalog_construction")
    use_statements = rust_use_statements(tokens)
    reexport = next(
        (
            statement
            for statement in use_statements
            if statement.public
            and any(rust_use_mentions(statement, name) for name in namespaces)
        ),
        None,
    )
    if reexport:
        return Finding(
            reexport.start,
            "restricted-namespace-reexport",
            "restricted construction namespaces cannot be re-exported",
        )

    alias = next(
        (
            statement
            for statement in use_statements
            if any(rust_use_aliases(statement, name) for name in namespaces)
        ),
        None,
    )
    if alias:
        return Finding(
            alias.start,
            "restricted-namespace-alias",
            "restricted construction namespaces cannot be aliased",
        )

    if starts_with(relative, ABI_TEST_ROOTS):
        return None

    type_aliases = rust_type_aliases(tokens)
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
        references = rust_path_references(tokens, namespace, None)
        type_alias = next(
            (
                alias
                for alias in type_aliases
                if rust_tokens_mention(alias.right_hand_side, namespace)
            ),
            None,
        )
        if type_alias:
            return Finding(
                type_alias.start,
                "restricted-namespace-wrapper",
                "restricted construction namespaces cannot be named by a type alias outside approved producers",
            )
        for reference in references:
            if starts_with(relative, roots):
                continue
            if (
                relative.startswith("crates/connector-abi/src/")
                and in_ranges(reference, test_ranges)
            ):
                continue
            if (
                rust_named_item_before(tokens, reference, "fn")
                or rust_keyword_before(tokens, reference, "macro_rules")
                or rust_named_item_before(tokens, reference, "trait")
            ):
                return Finding(
                    reference,
                    "restricted-namespace-wrapper",
                    "restricted construction calls cannot be forwarded outside approved producers",
                )
            return Finding(reference, rule, message)
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
                "crates/connector-processors/src/",
                "crates/server/src/",
            ),
        )
        and in_ranges(offset, test_ranges)
    )


def host_trait_indirection(
    relative: str,
    tokens: list[RustToken],
    test_ranges: list[tuple[int, int]],
) -> Finding | None:
    use_statements = rust_use_statements(tokens)
    type_aliases = rust_type_aliases(tokens)
    macro_ranges = macro_invocation_ranges(tokens)
    for trait_name in ("ConnectorIo", "ProcessorControl"):
        reexport = next(
            (
                statement
                for statement in use_statements
                if statement.public and rust_use_mentions(statement, trait_name)
            ),
            None,
        )
        if reexport and not (
            relative == "crates/connector-abi/src/lib.rs"
            and not rust_use_aliases(reexport, trait_name)
        ):
            if not approved_host_trait_indirection(
                relative, reexport.start, test_ranges
            ):
                return Finding(
                    reexport.start,
                    "host-trait-reexport",
                    f"{trait_name} cannot be re-exported outside approved host implementation roots",
                )

        alias = next(
            (
                statement
                for statement in use_statements
                if rust_use_aliases(statement, trait_name)
            ),
            None,
        )
        if alias and not approved_host_trait_indirection(
            relative, alias.start, test_ranges
        ):
            return Finding(
                alias.start,
                "host-trait-alias",
                f"{trait_name} cannot be aliased outside approved host implementation roots",
            )

        type_alias = next(
            (
                alias
                for alias in type_aliases
                if rust_tokens_mention(alias.right_hand_side, trait_name)
            ),
            None,
        )
        if type_alias and not approved_host_trait_indirection(
            relative, type_alias.start, test_ranges
        ):
            return Finding(
                type_alias.start,
                "host-trait-type-alias",
                f"{trait_name} cannot be reached through a type alias outside approved host implementation roots",
            )

        for start, end in macro_ranges:
            invocation = rust_tokens_in_range(tokens, start, end)
            if (
                rust_tokens_mention(invocation, trait_name)
                and not approved_host_trait_indirection(
                    relative, start, test_ranges
                )
            ):
                return Finding(
                    start,
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

    tokens = rust_tokens(code)
    test_ranges = private_cfg_test_ranges(tokens)
    exported_test = exported_cfg_test_module(tokens)
    if exported_test is None:
        for start, end in test_ranges:
            exported_test = rust_exported_item_in_range(tokens, start, end)
            if exported_test is not None:
                break
    if exported_test is not None:
        return [
            Finding(
                exported_test,
                "exported-test-helper",
                "test helpers in production modules must remain private",
            )
        ]

    is_unapproved_test = "/tests/" in relative and not starts_with(
        relative, ABI_TEST_ROOTS + PROCESSOR_TEST_ROOTS + SERVER_TEST_ROOTS
    )
    if is_unapproved_test and (
        rust_path_references(tokens, "host_construction", None)
        or rust_path_references(tokens, "catalog_construction", None)
        or rust_impl_trait_references(tokens)
    ):
        return [
            Finding(
                0,
                "test-path-allowlist",
                "connector construction and host fakes are restricted to approved test roots",
            )
        ]

    static_finding = static_literal_indirection(relative, tokens)
    if static_finding:
        return [static_finding]

    namespace_finding = restricted_namespace(
        relative,
        tokens,
        test_ranges,
    )
    if namespace_finding:
        return [namespace_finding]

    host_indirection = host_trait_indirection(
        relative,
        tokens,
        test_ranges,
    )
    if host_indirection:
        return [host_indirection]

    host_impls = rust_impl_trait_references(tokens)
    for host_impl, _ in host_impls:
        approved_test = starts_with(
            relative, PROCESSOR_TEST_ROOTS + SERVER_TEST_ROOTS
        )
        approved_private_test = (
            starts_with(
                relative,
                (
                    "crates/connector-processors/src/",
                    "crates/server/src/",
                ),
            )
            and in_ranges(host_impl, test_ranges)
        )
        if not (
            starts_with(relative, HOST_IMPL_ROOTS)
            or approved_test
            or approved_private_test
        ):
            return [
                Finding(
                    host_impl,
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
            "crates/server/src/connectors/future_unicode_static_error.rs",
            f"use donat_connector_abi::StaticErrorCode as "
            f"{UNICODE_VERSION_GAP_IDENTIFIER};",
            "static-literal-alias: StaticErrorCode::literal cannot be reached "
            "through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/future_unicode_static_message.rs",
            f"use donat_connector_abi::StaticSafeMessage as "
            f"{UNICODE_VERSION_GAP_IDENTIFIER};",
            "static-literal-alias: StaticSafeMessage::literal cannot be reached "
            "through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/future_unicode_host_namespace.rs",
            f"use donat_connector_abi::host_construction as "
            f"{UNICODE_VERSION_GAP_IDENTIFIER};",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/connector-catalog/src/future_unicode_catalog_namespace.rs",
            f"use donat_connector_abi::catalog_construction as "
            f"{UNICODE_VERSION_GAP_IDENTIFIER};",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/connector-processors/src/future_unicode_connector_io.rs",
            f"use donat_connector_abi::ConnectorIo as "
            f"{UNICODE_VERSION_GAP_IDENTIFIER};",
            "host-trait-alias: ConnectorIo cannot be aliased outside approved "
            "host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/future_unicode_processor_control.rs",
            f"use donat_connector_abi::ProcessorControl as "
            f"{UNICODE_VERSION_GAP_IDENTIFIER};",
            "host-trait-alias: ProcessorControl cannot be aliased outside approved "
            "host implementation roots",
        ),
        Fixture(
            "crates/server/src/connectors/raw_use_field_decoy.rs",
            "struct Decoy { pub r#use: donat_connector_abi::StaticErrorCode }",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/raw_type_binding_decoy.rs",
            "fn decoy() { let r#type = "
            "None::<donat_connector_abi::StaticErrorCode>; }",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/raw_use_alias.rs",
            "use donat_connector_abi::StaticErrorCode as r#use;",
            "static-literal-alias: StaticErrorCode::literal cannot be reached "
            "through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/raw_protected_literal.rs",
            'const CODE: r#StaticErrorCode = '
            'r#StaticErrorCode::r#literal("connector_failed");',
            "static-literal-producer: static failure literals are restricted to "
            "approved roots",
        ),
        Fixture(
            "crates/server/src/connectors/raw_private_test_macro_decoy.rs",
            "#[cfg(test)]\nmod tests { accept_tokens!(r#pub fn); }",
            None,
        ),
        *(
            Fixture(
                f"crates/connector-abi/src/exported_test_{name}.rs",
                "#[cfg(test)]\nmod tests { " + item + " }",
                "exported-test-helper: test helpers in production modules must "
                "remain private",
            )
            for name, item in (
                ("const", "pub const ITEM: u8 = 0;"),
                ("enum", "pub enum Item { Value }"),
                ("fn", "pub fn item() {}"),
                ("mod", "pub mod item {}"),
                ("static", "pub static ITEM: u8 = 0;"),
                ("struct", "pub struct Item;"),
                ("trait", "pub trait Item {}"),
                ("type", "pub type Item = ();"),
                ("use", "pub use crate::Item;"),
                ("restricted", "pub(crate) fn item() {}"),
            )
        ),
        *(
            Fixture(
                f"crates/server/src/connectors/pattern_whitespace_{name}.rs",
                f"use{separator}donat_connector_abi::StaticErrorCode"
                f"{separator}as{separator}{UNICODE_VERSION_GAP_IDENTIFIER};",
                "static-literal-alias: StaticErrorCode::literal cannot be reached "
                "through an alias outside STATIC_LITERAL_ROOTS",
            )
            for name, separator in RUST_PATTERN_WHITESPACE_CASES
        ),
        Fixture(
            "crates/server/src/connectors/leading_bom_alias.rs",
            "\ufeffuse donat_connector_abi::StaticErrorCode as Alias;",
            "static-literal-alias: StaticErrorCode::literal cannot be reached "
            "through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/invalid_unicode_atom_alias.rs",
            "use donat_connector_abi::StaticErrorCode as 💥;",
            "static-literal-alias: StaticErrorCode::literal cannot be reached "
            "through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/unicode_noncode_decoys.rs",
            "// host_construction StaticErrorCode ConnectorIo \U00016100\n"
            "/* outer catalog_construction /* nested StaticSafeMessage */ "
            "ProcessorControl */\n"
            'const NORMAL: &str = "host_construction StaticErrorCode";\n'
            'const RAW: &str = r###"catalog_construction ConnectorIo"###;\n'
            "const CHARACTER: char = '𖄀';\n"
            "fn lifetimes<'host_construction>(value: "
            "&'host_construction str) -> &'host_construction str { value }",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/complete_noncode_literal_decoys.rs",
            'const BYTE: &[u8] = b"host_construction StaticErrorCode ConnectorIo";\n'
            'const C: &core::ffi::CStr = c"host_construction StaticErrorCode ConnectorIo";\n'
            'const RAW_BYTE: &[u8] = br#"host_construction StaticErrorCode ConnectorIo"#;\n'
            'const REVERSED_RAW_BYTE: &[u8] = rb#"host_construction StaticErrorCode ConnectorIo"#;\n'
            'const RAW_C: &core::ffi::CStr = cr#"host_construction StaticErrorCode ConnectorIo"#;\n'
            'const REVERSED_RAW_C: &core::ffi::CStr = rc#"host_construction StaticErrorCode ConnectorIo"#;\n'
            "const BYTE_CHARACTER: u8 = b'h';\n"
            "const BYTE_CHARACTER_ESCAPE: u8 = b'\\\\x68';\n"
            "const CHARACTER: char = 'h';\n"
            "const CHARACTER_ESCAPE: char = '\\\\u{68}';\n"
            "const CHARACTER_SIMPLE_ESCAPE: char = '\\\\n';",
            None,
        ),
        *(
            Fixture(
                (
                    f"crates/server/src/connectors/label_{kind}_{name}.rs"
                    if kind == "static_literal"
                    else f"crates/connector-processors/src/label_{kind}_{name}.rs"
                ),
                source,
                expected,
            )
            for name, separator in RUST_PATTERN_WHITESPACE_CASES
            for kind, source, expected in (
                (
                    "namespace",
                    f"fn{separator}probe(){separator}"
                    "{"
                    f"'scan:{separator}loop{separator}"
                    "{"
                    f"{separator}host_construction::transport_response();"
                    f"{separator}break{separator}'scan;"
                    "}}",
                    "restricted-namespace-wrapper: restricted construction calls "
                    "cannot be forwarded outside approved producers",
                ),
                (
                    "static_literal",
                    f"fn{separator}probe(){separator}"
                    "{"
                    f"'scan:{separator}loop{separator}"
                    "{"
                    f"{separator}StaticErrorCode::literal(\"connector_failed\");"
                    f"{separator}break{separator}'scan;"
                    "}}",
                    "static-literal-wrapper: StaticErrorCode::literal cannot be "
                    "forwarded by a function outside STATIC_LITERAL_ROOTS",
                ),
                (
                    "host_trait",
                    f"fn{separator}probe(){separator}"
                    "{"
                    f"'scan:{separator}loop{separator}"
                    "{"
                    f"{separator}use{separator}donat_connector_abi::"
                    f"ConnectorIo{separator}as{separator}Io;{separator}break"
                    f"{separator}'scan;"
                    "}}",
                    "host-trait-alias: ConnectorIo cannot be aliased outside approved "
                    "host implementation roots",
                ),
            )
        ),
        *(
            Fixture(
                (
                    f"crates/server/src/connectors/lifetime_{kind}_{name}.rs"
                    if kind == "static_literal"
                    else f"crates/connector-processors/src/lifetime_{kind}_{name}.rs"
                ),
                source,
                expected,
            )
            for name, separator in RUST_PATTERN_WHITESPACE_CASES
            for kind, source, expected in (
                (
                    "namespace",
                    f"fn{separator}probe<'scan>(value:{separator}&'scan{separator}str)"
                    f"{separator}"
                    "{"
                    f"{separator}host_construction::transport_response();"
                    f"{separator}let{separator}_='x';"
                    "}",
                    "restricted-namespace-wrapper: restricted construction calls "
                    "cannot be forwarded outside approved producers",
                ),
                (
                    "static_literal",
                    f"fn{separator}probe<'scan>(value:{separator}&'scan{separator}str)"
                    f"{separator}"
                    "{"
                    f"{separator}StaticErrorCode::literal(\"connector_failed\");"
                    f"{separator}let{separator}_='x';"
                    "}",
                    "static-literal-wrapper: StaticErrorCode::literal cannot be "
                    "forwarded by a function outside STATIC_LITERAL_ROOTS",
                ),
                (
                    "host_trait",
                    f"fn{separator}probe<'scan>(value:{separator}&'scan{separator}str)"
                    f"{separator}"
                    "{"
                    f"{separator}use{separator}donat_connector_abi::"
                    f"ConnectorIo{separator}as{separator}Io;{separator}let"
                    f"{separator}_='x';"
                    "}",
                    "host-trait-alias: ConnectorIo cannot be aliased outside approved "
                    "host implementation roots",
                ),
            )
        ),
        Fixture(
            "crates/server/src/connectors/direct_self_prefix_sibling_control.rs",
            "use self::donat_connector_abi::{host_construction, "
            "harmless::Thing as Alias};",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/nested_self_prefix_sibling_control.rs",
            "use self::{donat_connector_abi::{host_construction, "
            "harmless::Thing as Alias}};",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/self_prefix_namespace_alias.rs",
            "use self::donat_connector_abi::host_construction as host_api;",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/self_prefix_static_alias.rs",
            "use self::donat_connector_abi::StaticErrorCode as ErrorCode;",
            "static-literal-alias: StaticErrorCode::literal cannot be reached "
            "through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/connector-processors/src/self_prefix_host_trait_alias.rs",
            "use self::donat_connector_abi::ConnectorIo as Io;",
            "host-trait-alias: ConnectorIo cannot be aliased outside approved "
            "host implementation roots",
        ),
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
            "crates/server/src/connectors/forbidden_static_error_unicode_alias.rs",
            "use donat_connector_abi::{StaticErrorCode as 错误代码};\n"
            'const CODE: 错误代码 = 错误代码::literal("connector_failed");',
            "static-literal-alias: StaticErrorCode::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_raw_alias.rs",
            "use donat_connector_abi::{StaticErrorCode as r#code};\n"
            'const CODE: r#code = r#code::literal("connector_failed");',
            "static-literal-alias: StaticErrorCode::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_unicode_alias.rs",
            "use donat_connector_abi::{StaticSafeMessage as 安全消息};\n"
            'const MESSAGE: 安全消息 = 安全消息::literal("connector failed");',
            "static-literal-alias: StaticSafeMessage::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_raw_alias.rs",
            "use donat_connector_abi::{StaticSafeMessage as r#message};\n"
            'const MESSAGE: r#message = r#message::literal("connector failed");',
            "static-literal-alias: StaticSafeMessage::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/connector-processors/src/allowed_static_error_unicode_alias.rs",
            "use donat_connector_abi::{StaticErrorCode as 错误代码};\n"
            'const CODE: 错误代码 = 错误代码::literal("connector_failed");',
            None,
        ),
        Fixture(
            "crates/connector-processors/src/allowed_static_error_raw_alias.rs",
            "use donat_connector_abi::{StaticErrorCode as r#code};\n"
            'const CODE: r#code = r#code::literal("connector_failed");',
            None,
        ),
        Fixture(
            "crates/connector-processors/src/allowed_static_message_unicode_alias.rs",
            "use donat_connector_abi::{StaticSafeMessage as 安全消息};\n"
            'const MESSAGE: 安全消息 = 安全消息::literal("connector failed");',
            None,
        ),
        Fixture(
            "crates/connector-processors/src/allowed_static_message_raw_alias.rs",
            "use donat_connector_abi::{StaticSafeMessage as r#message};\n"
            'const MESSAGE: r#message = r#message::literal("connector failed");',
            None,
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
            "crates/server/src/connectors/forbidden_static_error_const_function_item.rs",
            "const CODE_LITERAL: fn(&'static str) -> StaticErrorCode = "
            "StaticErrorCode::literal;",
            "static-literal-producer: static failure literals are restricted to approved roots",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_const_function_item.rs",
            "const MESSAGE_LITERAL: fn(&'static str) -> StaticSafeMessage = "
            "StaticSafeMessage::literal;",
            "static-literal-producer: static failure literals are restricted to approved roots",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_static_function_item.rs",
            "static CODE_LITERAL: fn(&'static str) -> StaticErrorCode = "
            "StaticErrorCode::literal;",
            "static-literal-producer: static failure literals are restricted to approved roots",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_static_function_item.rs",
            "static MESSAGE_LITERAL: fn(&'static str) -> StaticSafeMessage = "
            "StaticSafeMessage::literal;",
            "static-literal-producer: static failure literals are restricted to approved roots",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_local_function_item.rs",
            "fn code(value: &'static str) -> StaticErrorCode {\n"
            "    let literal = StaticErrorCode::literal;\n"
            "    literal(value)\n"
            "}",
            "static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a function outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_local_function_item.rs",
            "fn message(value: &'static str) -> StaticSafeMessage {\n"
            "    let literal = StaticSafeMessage::literal;\n"
            "    literal(value)\n"
            "}",
            "static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a function outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_error_cross_file_consumer.rs",
            "use donat_connector_abi::StaticErrorCode;\n"
            "use shared_consumer::consume;\n"
            "const CODE_LITERAL: fn(&'static str) -> StaticErrorCode = "
            "StaticErrorCode::literal;\n"
            "fn build(value: &'static str) { consume(CODE_LITERAL, value); }",
            "static-literal-producer: static failure literals are restricted to approved roots",
        ),
        Fixture(
            "crates/server/src/connectors/forbidden_static_message_cross_file_consumer.rs",
            "use donat_connector_abi::StaticSafeMessage;\n"
            "use shared_consumer::consume;\n"
            "const MESSAGE_LITERAL: fn(&'static str) -> StaticSafeMessage = "
            "StaticSafeMessage::literal;\n"
            "fn build(value: &'static str) { consume(MESSAGE_LITERAL, value); }",
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
            "crates/server/src/connectors/grouped_host_self_alias.rs",
            "use donat_connector_abi::host_construction::{"
            "transport_response, self as host_api};",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/connector-catalog/src/grouped_catalog_self_alias.rs",
            "use donat_connector_abi::catalog_construction::{"
            "static_error_code, self as catalog_api};",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/grouped_host_member_alias.rs",
            "use donat_connector_abi::host_construction::{"
            "authorized_correlations, transport_response as make};",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/connector-catalog/src/nested_catalog_member_alias.rs",
            "use donat_connector_abi::{catalog_construction::{"
            "static_safe_message, static_error_code as make_code}, CapabilityId};",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/unrelated_sibling_alias.rs",
            "use donat_connector_abi::{host_construction, "
            "harmless::{Thing as Alias}};",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/grouped_raw_keyword_alias.rs",
            "use donat_connector_abi::host_construction::{"
            "transport_response, self as r#use};",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/grouped_unaliased_self.rs",
            "use donat_connector_abi::host_construction::{"
            "transport_response, self};",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/leading_global_reexport.rs",
            "pub(crate) use ::donat_connector_abi::StaticErrorCode;",
            "static-literal-reexport: StaticErrorCode::literal cannot be re-exported "
            "outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/restricted_visibility_reexport.rs",
            "pub(in crate) use donat_connector_abi::StaticSafeMessage;",
            "static-literal-reexport: StaticSafeMessage::literal cannot be "
            "re-exported outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/server/src/connectors/nested_host_self_alias.rs",
            "use outer::{donat_connector_abi::{host_construction::{"
            "transport_response, self as host_api,},},};",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/host_as_underscore.rs",
            "use donat_connector_abi::host_construction as _;",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/host_empty_group.rs",
            "use donat_connector_abi::host_construction::{};",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/host_glob.rs",
            "use donat_connector_abi::host_construction::*;",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/grouped_static_types.rs",
            "use donat_connector_abi::{CapabilityId, "
            "StaticErrorCode as ErrorCode, StaticSafeMessage as SafeMessage};",
            "static-literal-alias: StaticErrorCode::literal cannot be reached "
            "through an alias outside STATIC_LITERAL_ROOTS",
        ),
        Fixture(
            "crates/connector-processors/src/grouped_host_traits.rs",
            "use donat_connector_abi::{TypedBindings, "
            "ConnectorIo as Io, ProcessorControl as Control};",
            "host-trait-alias: ConnectorIo cannot be aliased outside approved host "
            "implementation roots",
        ),
        Fixture(
            "crates/server/src/connectors/protected_macro_use_fallback.rs",
            "macro_rules! import_host { ($alias:ident) => { "
            "use donat_connector_abi::host_construction as $alias; }; }",
            "restricted-namespace-alias: restricted construction namespaces "
            "cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/grouped_noncode_decoys.rs",
            "use donat_connector_abi::{host_construction, harmless};\n"
            "// harmless::{Thing as host_construction}\n"
            'const TEXT: &str = r#"{StaticErrorCode as Alias}"#;',
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
            "crates/server/src/connectors/host_unicode_alias.rs",
            "use donat_connector_abi::{host_construction as 主机构造};\n"
            "主机构造::transport_response();",
            "restricted-namespace-alias: restricted construction namespaces cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/host_raw_alias.rs",
            "use donat_connector_abi::{host_construction as r#host};\n"
            "r#host::transport_response();",
            "restricted-namespace-alias: restricted construction namespaces cannot be aliased",
        ),
        Fixture(
            "crates/connector-catalog/src/catalog_unicode_alias.rs",
            "use donat_connector_abi::{catalog_construction as 目录构造};\n"
            "目录构造::static_error_code(value);",
            "restricted-namespace-alias: restricted construction namespaces cannot be aliased",
        ),
        Fixture(
            "crates/connector-catalog/src/catalog_raw_alias.rs",
            "use donat_connector_abi::{catalog_construction as r#catalog};\n"
            "r#catalog::static_error_code(value);",
            "restricted-namespace-alias: restricted construction namespaces cannot be aliased",
        ),
        Fixture(
            "crates/server/src/connectors/allowed_host_unicode_neighbor.rs",
            "let 结果 = host_construction::transport_response();",
            None,
        ),
        Fixture(
            "crates/connector-catalog/src/allowed_catalog_raw_neighbor.rs",
            "let r#result = catalog_construction::static_error_code(value);",
            None,
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
            "crates/connector-processors/src/connector_io_unicode_alias_impl.rs",
            "use donat_connector_abi::{ConnectorIo as 连接器输入输出};\n"
            "impl 连接器输入输出 for Bad {}",
            "host-trait-alias: ConnectorIo cannot be aliased outside approved host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/connector_io_raw_alias_impl.rs",
            "use donat_connector_abi::{ConnectorIo as r#io};\n"
            "impl r#io for Bad {}",
            "host-trait-alias: ConnectorIo cannot be aliased outside approved host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/processor_control_unicode_alias_impl.rs",
            "use donat_connector_abi::{ProcessorControl as 处理器控制};\n"
            "impl 处理器控制 for Bad {}",
            "host-trait-alias: ProcessorControl cannot be aliased outside approved host implementation roots",
        ),
        Fixture(
            "crates/connector-processors/src/processor_control_raw_alias_impl.rs",
            "use donat_connector_abi::{ProcessorControl as r#control};\n"
            "impl r#control for Bad {}",
            "host-trait-alias: ProcessorControl cannot be aliased outside approved host implementation roots",
        ),
        Fixture(
            "crates/server/src/connectors/allowed_connector_io_unicode_alias_impl.rs",
            "use donat_connector_abi::{ConnectorIo as 连接器输入输出};\n"
            "impl 连接器输入输出 for HostIo {}",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/allowed_connector_io_raw_alias_impl.rs",
            "use donat_connector_abi::{ConnectorIo as r#io};\n"
            "impl r#io for HostIo {}",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/allowed_processor_control_unicode_alias_impl.rs",
            "use donat_connector_abi::{ProcessorControl as 处理器控制};\n"
            "impl 处理器控制 for HostControl {}",
            None,
        ),
        Fixture(
            "crates/server/src/connectors/allowed_processor_control_raw_alias_impl.rs",
            "use donat_connector_abi::{ProcessorControl as r#control};\n"
            "impl r#control for HostControl {}",
            None,
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
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            "crates/connector-abi/src/private_test_control_impl.rs",
            "#[cfg(test)]\nmod tests { impl ProcessorControl for FakeControl {} }",
            "host-trait-implementation: host traits can only be implemented in approved host or test roots",
        ),
        Fixture(
            "crates/connector-abi/src/private_test_namespaces.rs",
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn exercise() {\n"
            "        host_construction::authorized_correlations();\n"
            "        catalog_construction::static_error_code(value);\n"
            "    }\n"
            "}",
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
            "crates/connector-abi/src/warn_large_error.rs",
            "#[warn(clippy::result_large_err)]\nfn bad() {}",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            "crates/connector-abi/src/force_warn_large_error.rs",
            "#![force_warn(clippy::result_large_err)]\nfn bad() {}",
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
            ".cargo/short-warn.toml",
            '[build]\nrustflags = ["-W", "clippy::result_large_err"]\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/attached-warn.toml",
            '[build]\nrustflags = "-Wclippy::result-large-err"\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/long-warn.toml",
            '[build]\nrustflags = ["--warn", "clippy::result_large_err"]\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/long-warn-equals.toml",
            '[build]\nrustflags = "--warn=clippy::result-large-err"\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/force-warn.toml",
            '[build]\nrustflags = ["--force-warn", "clippy::result_large_err"]\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/force-warn-equals.toml",
            '[build]\nrustflags = "--force-warn=clippy::result-large-err"\n',
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".cargo/cap-lints-equals.toml",
            '[build]\nrustflags = "--cap-lints=allow"\n',
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
            ".github/workflows/run-short-warn-large-error.yml",
            "steps:\n"
            "  - run: cargo clippy -- -W clippy::result_large_err\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/run-attached-warn-large-error.yml",
            "steps:\n"
            "  - run: cargo clippy -- -Wclippy::result_large_err\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/run-long-warn-large-error.yml",
            "steps:\n"
            "  - run: cargo clippy -- --warn=clippy::result_large_err\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/run-force-warn-large-error.yml",
            "steps:\n"
            "  - run: cargo clippy -- --force-warn clippy::result_large_err\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/run-cap-lints-equals.yml",
            "steps:\n"
            "  - run: cargo clippy -- --cap-lints=warn -D clippy::result_large_err\n",
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
            ".github/workflows/workflow-env-short-warn-large-error.yml",
            'env:\n  RUSTFLAGS: "-W clippy::result_large_err"\n'
            "jobs:\n  test:\n    runs-on: ubuntu-latest\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/job-env-attached-warn-large-error.yml",
            "jobs:\n"
            "  test:\n"
            '    env:\n      RUSTFLAGS: "-Wclippy::result_large_err"\n'
            "    runs-on: ubuntu-latest\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/step-env-long-warn-large-error.yml",
            "jobs:\n"
            "  test:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - env:\n"
            '          RUSTFLAGS: "--warn clippy::result_large_err"\n'
            "        run: cargo clippy\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        Fixture(
            ".github/workflows/step-env-force-warn-large-error.yml",
            "jobs:\n"
            "  test:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - env:\n"
            '          RUSTFLAGS: "--force-warn=clippy::result_large_err"\n'
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
            ".github/workflows/non-env-literal-block-decoy.yml",
            "jobs:\n"
            "  test:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - uses: example/action@v1\n"
            "        with:\n"
            "          content: |-\n"
            "            env:\n"
            '              RUSTFLAGS: "-A clippy::result_large_err"\n',
            None,
        ),
        Fixture(
            ".github/workflows/non-env-folded-block-decoy.yml",
            "jobs:\n"
            "  test:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - uses: example/action@v1\n"
            "        with:\n"
            "          content: >+\n"
            "            env:\n"
            "              RUSTFLAGS: --cap-lints warn\n",
            None,
        ),
        Fixture(
            ".github/workflows/non-run-literal-block-decoy.yml",
            "jobs:\n"
            "  test:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - uses: example/action@v1\n"
            "        with:\n"
            "          content: |-\n"
            "            run: cargo clippy -- -A clippy::result_large_err\n",
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


def workspace_fixtures() -> tuple[WorkspaceFixture, ...]:
    return (
        WorkspaceFixture(
            "inherited-allow",
            "[workspace]\n"
            '[workspace.lints.clippy]\nresult-large-err = "allow"\n',
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n"
            "[lints]\nworkspace = true\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        WorkspaceFixture(
            "inherited-warn",
            "[workspace]\n"
            '[workspace.lints.clippy]\nresult-large-err = "warn"\n',
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n"
            "[lints]\nworkspace = true\n",
            "large-error-lint-suppression: clippy::result_large_err must remain denied without suppression",
        ),
        WorkspaceFixture(
            "inherited-deny-control",
            "[workspace]\n"
            '[workspace.lints.clippy]\nresult-large-err = "deny"\n',
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n"
            "[lints]\nworkspace = true\n",
            None,
        ),
        WorkspaceFixture(
            "not-inherited-control",
            "[workspace]\n"
            '[workspace.lints.clippy]\nresult-large-err = "allow"\n',
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
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
    for fixture in workspace_fixtures():
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            abi_manifest = root / "crates/connector-abi/Cargo.toml"
            abi_manifest.parent.mkdir(parents=True)
            (root / "Cargo.toml").write_text(
                fixture.root_manifest,
                encoding="utf-8",
            )
            abi_manifest.write_text(fixture.abi_manifest, encoding="utf-8")
            diagnostics = scan_workspace(root)
            if fixture.expected_rule is None:
                if diagnostics:
                    failures.append(
                        f"self-test: {fixture.name}: expected no diagnostic, got {diagnostics}"
                    )
                continue
            expected = (
                "connector-boundary: crates/connector-abi/Cargo.toml: "
                f"{fixture.expected_rule}"
            )
            if diagnostics != [expected]:
                failures.append(
                    f"self-test: {fixture.name}: expected [{expected!r}], got {diagnostics}"
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


def workspace_lint_inheritance(root: Path) -> list[str]:
    root_manifest = root / "Cargo.toml"
    abi_manifest = root / "crates/connector-abi/Cargo.toml"
    if not root_manifest.is_file() or not abi_manifest.is_file():
        return []
    try:
        workspace_document = tomllib.loads(
            root_manifest.read_text(encoding="utf-8")
        )
        abi_document = tomllib.loads(abi_manifest.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError:
        return []

    if abi_document.get("lints", {}).get("workspace") is not True:
        return []
    clippy = (
        workspace_document.get("workspace", {})
        .get("lints", {})
        .get("clippy", {})
    )
    for key, value in clippy.items():
        if key.replace("-", "_") != "result_large_err":
            continue
        level = value.get("level") if isinstance(value, dict) else value
        if level in ("allow", "warn"):
            return [
                "connector-boundary: crates/connector-abi/Cargo.toml: "
                "large-error-lint-suppression: "
                "clippy::result_large_err must remain denied without suppression"
            ]
    return []


def scan_workspace(root: Path) -> list[str]:
    diagnostics: list[str] = []
    for path in workspace_files(root):
        relative = path.relative_to(root)
        diagnostics.extend(
            scan_fixture(relative, path.read_text(encoding="utf-8"))
        )
    diagnostics.extend(workspace_lint_inheritance(root))
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
