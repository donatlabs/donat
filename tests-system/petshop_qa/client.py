"""The three doors into the store, opened the way a client opens them.

Every surface — GraphQL, the RESTified endpoints, MCP — runs the same
per-role permissions, so the suite talks to all three through one actor and can
compare what each one answers.
"""

from __future__ import annotations

import json as jsonlib
from typing import Any, Iterable, Mapping

import requests

from .auth import issue_token
from .config import MCP_PROTOCOL_VERSION, Config


class Response:
    """One HTTP answer, with the accessors the assertions actually use."""

    def __init__(self, response: requests.Response, *, request_summary: str):
        self.status = response.status_code
        self.headers = response.headers
        self.text = response.text
        self.request_summary = request_summary
        try:
            self.json: Any = response.json()
        except ValueError:
            self.json = None

    # -- GraphQL shape ----------------------------------------------------

    @property
    def errors(self) -> list[dict]:
        if isinstance(self.json, dict) and isinstance(self.json.get("errors"), list):
            return self.json["errors"]
        return []

    @property
    def data(self) -> Any:
        if isinstance(self.json, dict):
            return self.json.get("data")
        return None

    def error_code(self) -> str | None:
        """The Donat error code of the first error, if the answer carries one."""

        for error in self.errors:
            extensions = error.get("extensions") or {}
            code = extensions.get("code")
            if code:
                return code
        return None

    def error_message(self) -> str | None:
        for error in self.errors:
            message = error.get("message")
            if message:
                return message
        return None

    def value(self, path: str, default: Any = None) -> Any:
        """A slash-separated lookup into the JSON body, e.g. `data/cart/0/id`."""

        cursor: Any = self.json
        for segment in path.strip("/").split("/"):
            if isinstance(cursor, Mapping) and segment in cursor:
                cursor = cursor[segment]
            elif isinstance(cursor, list) and segment.lstrip("-").isdigit():
                index = int(segment)
                if -len(cursor) <= index < len(cursor):
                    cursor = cursor[index]
                else:
                    return default
            else:
                return default
        return cursor

    # -- assertions -------------------------------------------------------

    def unwrap(self) -> Any:
        """The `data` of a successful GraphQL answer; fails loudly otherwise."""

        assert self.status == 200, f"{self.request_summary} -> HTTP {self.status}: {self.text}"
        assert not self.errors, f"{self.request_summary} -> errors: {self._pretty()}"
        assert self.data is not None, f"{self.request_summary} -> no data: {self._pretty()}"
        return self.data

    def _pretty(self) -> str:
        if self.json is None:
            return self.text
        return jsonlib.dumps(self.json, indent=2, sort_keys=False)

    def __repr__(self) -> str:  # pragma: no cover - diagnostics only
        return f"<Response {self.status} {self.request_summary}: {self._pretty()}>"


class Actor:
    """One caller with one identity, across all three surfaces."""

    def __init__(
        self,
        config: Config,
        session: requests.Session,
        *,
        role: str | None,
        user_id: str | None = None,
        token: str | None = None,
    ):
        self._config = config
        self._session = session
        self.role = role
        self.user_id = user_id
        self._token = token

    @property
    def label(self) -> str:
        if self.role is None:
            return "anonymous (no token)"
        return f"{self.role}" + (f"/{self.user_id}" if self.user_id else "")

    def _headers(
        self, extra: Mapping[str, str] | None = None, *, with_body: bool = True
    ) -> dict[str, str]:
        # A GET carries no body, and announcing a JSON body it does not have is
        # how a client gets a parse error instead of an answer.
        headers = {"content-type": "application/json"} if with_body else {}
        if self._token:
            headers["authorization"] = f"Bearer {self._token}"
        if self.role:
            # Names which of the token's allowed roles this request runs as.
            headers["x-donat-role"] = self.role
        if extra:
            headers.update(extra)
        return headers

    # -- GraphQL ----------------------------------------------------------

    def graphql(
        self,
        query: str,
        variables: Mapping[str, Any] | None = None,
        *,
        operation_name: str | None = None,
        headers: Mapping[str, str] | None = None,
    ) -> Response:
        body: dict[str, Any] = {"query": query}
        if variables is not None:
            body["variables"] = dict(variables)
        if operation_name is not None:
            body["operationName"] = operation_name
        return self._post("/v1/graphql", body, headers, summary=_first_line(query))

    def query(self, query: str, variables: Mapping[str, Any] | None = None) -> Any:
        """A GraphQL read that is expected to succeed, unwrapped to its data."""

        return self.graphql(query, variables).unwrap()

    # -- RESTified endpoints ----------------------------------------------

    def rest(
        self,
        method: str,
        path: str,
        *,
        json: Any = None,
        params: Mapping[str, Any] | None = None,
        headers: Mapping[str, str] | None = None,
    ) -> Response:
        url = f"{self._config.base_url}/api/rest/{path.lstrip('/')}"
        response = self._session.request(
            method.upper(),
            url,
            json=json,
            params=dict(params) if params else None,
            headers=self._headers(headers, with_body=json is not None),
            timeout=self._config.request_timeout,
        )
        return Response(response, request_summary=f"{method.upper()} {url} as {self.label}")

    # -- MCP ---------------------------------------------------------------

    def mcp(
        self,
        method: str,
        params: Mapping[str, Any] | None = None,
        *,
        request_id: int = 1,
        headers: Mapping[str, str] | None = None,
    ) -> Response:
        body: dict[str, Any] = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            body["params"] = dict(params)
        merged = {
            "MCP-Protocol-Version": MCP_PROTOCOL_VERSION,
            # Streamable HTTP: a client states both shapes it can read.
            "accept": "application/json, text/event-stream",
        }
        if headers:
            merged.update(headers)
        return self._post("/mcp", body, merged, summary=f"mcp {method}")

    def mcp_tool(self, name: str, arguments: Mapping[str, Any]) -> Response:
        return self.mcp("tools/call", {"name": name, "arguments": dict(arguments)})

    # -- plumbing ----------------------------------------------------------

    def _post(
        self,
        path: str,
        body: Mapping[str, Any],
        headers: Mapping[str, str] | None,
        *,
        summary: str,
    ) -> Response:
        url = f"{self._config.base_url}{path}"
        response = self._session.post(
            url,
            json=dict(body),
            headers=self._headers(headers),
            timeout=self._config.request_timeout,
        )
        return Response(response, request_summary=f"{summary} as {self.label}")


class Store:
    """A running Petshop, addressed from outside."""

    def __init__(self, config: Config):
        self.config = config
        self._session = requests.Session()

    def as_role(
        self,
        role: str,
        user_id: str | None = None,
        *,
        allowed_roles: Iterable[str] | None = None,
    ) -> Actor:
        token = issue_token(
            self.config,
            role,
            user_id,
            allowed_roles=list(allowed_roles) if allowed_roles is not None else None,
        )
        return Actor(self.config, self._session, role=role, user_id=user_id, token=token)

    def anonymous(self) -> Actor:
        """No token at all — the engine's unauthorized role answers."""

        return Actor(self.config, self._session, role=None)

    def with_token(self, token: str, role: str | None = None, user_id: str | None = None) -> Actor:
        """An actor carrying a token the test built itself (expired, forged, …)."""

        return Actor(self.config, self._session, role=role, user_id=user_id, token=token)

    def is_up(self) -> bool:
        try:
            response = self._session.post(
                f"{self.config.base_url}/v1/graphql",
                json={"query": "{ __typename }"},
                timeout=self.config.request_timeout,
            )
        except requests.RequestException:
            return False
        return response.status_code < 500


def _first_line(query: str) -> str:
    for line in query.strip().splitlines():
        stripped = line.strip()
        if stripped:
            return stripped
    return "graphql"
