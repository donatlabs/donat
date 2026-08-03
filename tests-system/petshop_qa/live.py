"""The store's fourth door: a subscription held open over a websocket.

A shopper watching their order does not poll. The browser opens one connection,
says what it wants to watch, and the store pushes a frame whenever the answer
changes. This is that connection, as a test holds it — the same legacy
`graphql-ws` protocol the engine speaks: `connection_init` carrying the caller's
headers, `start` naming the query, `data` frames until `stop`.
"""

from __future__ import annotations

import json
import time
from typing import Any, Mapping

import websocket


class LiveTimeout(AssertionError):
    """The store did not push what was expected within the budget."""


class Live:
    """One open subscription connection, for one caller.

    Every frame the store sends is kept, so a test can ask what arrived rather
    than racing the socket. Keepalives are dropped: they are the transport
    talking to itself.
    """

    def __init__(self, base_url: str, headers: Mapping[str, str], *, timeout: float = 15):
        url = base_url.replace("https://", "wss://").replace("http://", "ws://")
        self._timeout = timeout
        self._socket = websocket.create_connection(
            f"{url}/v1/graphql", subprotocols=["graphql-ws"], timeout=timeout
        )
        self._send({"type": "connection_init", "payload": {"headers": dict(headers)}})
        acknowledged = self._read_until(lambda frame: frame.get("type") == "connection_ack")
        assert acknowledged, "the store never acknowledged the connection"

    # -- what a client does ------------------------------------------------

    def watch(self, query: str, variables: Mapping[str, Any] | None = None, *, id: str = "1"):
        self._send(
            {
                "id": id,
                "type": "start",
                "payload": {"query": query, "variables": dict(variables or {})},
            }
        )
        return self

    def stop(self, *, id: str = "1"):
        self._send({"id": id, "type": "stop"})
        return self

    def close(self) -> None:
        try:
            self._socket.close()
        except Exception:  # pragma: no cover - the socket may already be gone
            pass

    def __enter__(self) -> "Live":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    # -- what the test asks ------------------------------------------------

    def next_frame(self, *, timeout: float | None = None) -> dict:
        """The next frame that is not a keepalive."""

        frame = self._read_until(lambda frame: frame.get("type") != "ka", timeout=timeout)
        if frame is None:
            raise LiveTimeout(f"no frame within {timeout or self._timeout:g}s")
        return frame

    def await_data(self, accept, *, timeout: float = 30, description: str = "a matching frame"):
        """Frames until one carries data the test accepts.

        Returns the accepted payload. A subscription is a stream of answers, so
        the first one is usually the state at subscribe time and the
        interesting one comes later.
        """

        deadline = time.monotonic() + timeout
        seen: list[Any] = []
        while time.monotonic() < deadline:
            try:
                frame = self.next_frame(timeout=max(0.5, deadline - time.monotonic()))
            except (LiveTimeout, websocket.WebSocketTimeoutException):
                break
            if frame.get("type") != "data":
                continue
            payload = frame.get("payload", {})
            seen.append(payload)
            if accept(payload):
                return payload
        raise LiveTimeout(f"{description} never arrived; saw: {seen!r}")

    def quiet_for(self, seconds: float) -> list[dict]:
        """Every non-keepalive frame that arrives in the next `seconds`."""

        deadline = time.monotonic() + seconds
        arrived: list[dict] = []
        while time.monotonic() < deadline:
            try:
                arrived.append(self.next_frame(timeout=max(0.2, deadline - time.monotonic())))
            except (LiveTimeout, websocket.WebSocketTimeoutException):
                break
        return arrived

    # -- plumbing ----------------------------------------------------------

    def _send(self, frame: Mapping[str, Any]) -> None:
        self._socket.send(json.dumps(dict(frame)))

    def _read_until(self, accept, *, timeout: float | None = None) -> dict | None:
        deadline = time.monotonic() + (timeout if timeout is not None else self._timeout)
        while time.monotonic() < deadline:
            self._socket.settimeout(max(0.2, deadline - time.monotonic()))
            try:
                raw = self._socket.recv()
            except websocket.WebSocketTimeoutException:
                return None
            except (websocket.WebSocketConnectionClosedException, OSError):
                return None
            if not raw:
                continue
            try:
                frame = json.loads(raw)
            except ValueError:
                continue
            if accept(frame):
                return frame
        return None
