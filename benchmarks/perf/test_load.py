import argparse
import importlib.util
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path


SPEC = importlib.util.spec_from_file_location("donat_perf_load", Path(__file__).with_name("load.py"))
assert SPEC and SPEC.loader
LOAD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LOAD)


class SlowHandler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        content_length = int(self.headers.get("content-length", "0"))
        self.rfile.read(content_length)
        time.sleep(0.05)
        try:
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"data":{"ok":true}}')
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, _format: str, *args: object) -> None:
        pass


class NonObjectHandler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        content_length = int(self.headers.get("content-length", "0"))
        self.rfile.read(content_length)
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(b"[]")

    def log_message(self, _format: str, *args: object) -> None:
        pass


class LoadHarnessTests(unittest.TestCase):
    def test_runner_keeps_database_url_out_of_process_arguments(self) -> None:
        runner = Path(__file__).with_name("run.sh").read_text(encoding="utf-8")

        self.assertIn('DONAT_GRAPHQL_DATABASE_URL="${database_url}"', runner)
        self.assertNotIn('--database-url "${database_url}"', runner)

    def test_timeout_attempts_contribute_to_latency_distribution(self) -> None:
        server = HTTPServer(("127.0.0.1", 0), SlowHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        host, port = server.server_address
        args = argparse.Namespace(
            url=f"http://{host}:{port}/v1/graphql",
            query="{ ok }",
            variables="{}",
            role="user",
            backend="test",
            concurrency=1,
            duration=0.04,
            timeout=0.01,
            pid=None,
            server_port=None,
        )

        try:
            result = LOAD.run(args)["result"]
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=1)

        self.assertGreaterEqual(result["requests"], 1)
        self.assertEqual(result["errors"], result["requests"])
        self.assertIsNotNone(result["latency_ms"]["p99"])
        self.assertIsNotNone(result["error_latency_ms"]["p99"])
        self.assertIsNone(result["successful_latency_ms"]["p99"])

    def test_non_object_response_is_counted_as_an_error(self) -> None:
        server = HTTPServer(("127.0.0.1", 0), NonObjectHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        host, port = server.server_address
        args = argparse.Namespace(
            url=f"http://{host}:{port}/v1/graphql",
            query="{ ok }",
            variables="{}",
            role="user",
            backend="test",
            concurrency=1,
            duration=0.04,
            timeout=1,
            pid=None,
            server_port=None,
        )

        try:
            result = LOAD.run(args)["result"]
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=1)

        self.assertGreaterEqual(result["requests"], 1)
        self.assertEqual(result["errors"], result["requests"])
        self.assertIsNone(result["successful_latency_ms"]["p99"])


if __name__ == "__main__":
    unittest.main()
