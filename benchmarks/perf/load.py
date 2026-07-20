#!/usr/bin/env python3
"""Small dependency-free HTTP load generator for local Donat investigations.

It deliberately records measurements instead of enforcing thresholds. Each
worker keeps one HTTP/1.1 connection open, validates every GraphQL response,
and writes a self-contained JSON artifact suitable for before/after diffs.
"""

from __future__ import annotations

import argparse
import http.client
import json
import math
import os
import platform
import subprocess
import threading
import time
from pathlib import Path
from urllib.parse import urlsplit


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, math.ceil(fraction * len(ordered)) - 1))
    return ordered[index]


def process_sample(pid: int | None) -> dict[str, float | int] | None:
    if pid is None:
        return None
    try:
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8").split()
        status = Path(f"/proc/{pid}/status").read_text(encoding="utf-8")
        rss_kib = 0
        for line in status.splitlines():
            if line.startswith("VmRSS:"):
                rss_kib = int(line.split()[1])
                break
        return {
            "cpu_ticks": int(stat[13]) + int(stat[14]),
            "rss_kib": rss_kib,
        }
    except (FileNotFoundError, IndexError, ValueError, PermissionError):
        return None


def established_connections(port: int) -> int | None:
    target = f"{port:04X}"
    try:
        count = 0
        for filename in ("/proc/net/tcp", "/proc/net/tcp6"):
            for line in Path(filename).read_text(encoding="utf-8").splitlines()[1:]:
                fields = line.split()
                if fields[1].rsplit(":", 1)[-1] == target and fields[3] == "01":
                    count += 1
        return count
    except (FileNotFoundError, IndexError, PermissionError):
        return None


def git_revision() -> str | None:
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "HEAD"], text=True, stderr=subprocess.DEVNULL
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def run(args: argparse.Namespace) -> dict[str, object]:
    parsed = urlsplit(args.url)
    connection_type = (
        http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
    )
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    path = parsed.path or "/v1/graphql"
    if parsed.query:
        path += "?" + parsed.query
    request_body = json.dumps(
        {"query": args.query, "variables": json.loads(args.variables)},
        separators=(",", ":"),
    ).encode("utf-8")
    headers = {
        "content-type": "application/json",
        "x-donat-role": args.role,
    }

    latencies: list[float] = []
    successful_latencies: list[float] = []
    error_latencies: list[float] = []
    response_bytes = 0
    requests = 0
    errors = 0
    lock = threading.Lock()
    stop_at = time.perf_counter() + args.duration
    process_before = process_sample(args.pid)
    max_rss_kib = process_before["rss_kib"] if process_before else 0
    max_connections = established_connections(args.server_port or port) or 0

    def worker() -> None:
        nonlocal response_bytes, requests, errors
        connection = connection_type(parsed.hostname, port, timeout=args.timeout)
        local_latencies: list[float] = []
        local_successful_latencies: list[float] = []
        local_error_latencies: list[float] = []
        local_bytes = 0
        local_requests = 0
        local_errors = 0
        while time.perf_counter() < stop_at:
            started = time.perf_counter()
            attempt_failed = False
            try:
                connection.request("POST", path, body=request_body, headers=headers)
                response = connection.getresponse()
                payload = response.read()
                local_bytes += len(payload)
                decoded = json.loads(payload)
                if (
                    response.status != 200
                    or not isinstance(decoded, dict)
                    or decoded.get("errors") is not None
                ):
                    attempt_failed = True
            except (OSError, ValueError, json.JSONDecodeError, http.client.HTTPException):
                attempt_failed = True
                connection.close()
                connection = connection_type(parsed.hostname, port, timeout=args.timeout)
            finally:
                elapsed = (time.perf_counter() - started) * 1000.0
                local_latencies.append(elapsed)
                local_requests += 1
                if attempt_failed:
                    local_errors += 1
                    local_error_latencies.append(elapsed)
                else:
                    local_successful_latencies.append(elapsed)
        connection.close()
        with lock:
            latencies.extend(local_latencies)
            successful_latencies.extend(local_successful_latencies)
            error_latencies.extend(local_error_latencies)
            response_bytes += local_bytes
            requests += local_requests
            errors += local_errors

    threads = [threading.Thread(target=worker, daemon=True) for _ in range(args.concurrency)]
    started = time.perf_counter()
    for thread in threads:
        thread.start()
    while any(thread.is_alive() for thread in threads):
        sample = process_sample(args.pid)
        if sample:
            max_rss_kib = max(max_rss_kib, int(sample["rss_kib"]))
        connections = established_connections(args.server_port or port)
        if connections is not None:
            max_connections = max(max_connections, connections)
        time.sleep(0.1)
    for thread in threads:
        thread.join()
    elapsed = time.perf_counter() - started
    process_after = process_sample(args.pid)

    cpu_percent = None
    if process_before and process_after and elapsed > 0:
        ticks = int(process_after["cpu_ticks"]) - int(process_before["cpu_ticks"])
        cpu_percent = ticks / os.sysconf("SC_CLK_TCK") / elapsed * 100.0

    return {
        "revision": git_revision(),
        "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "machine": {
            "hostname": platform.node(),
            "platform": platform.platform(),
            "cpu_count": os.cpu_count(),
        },
        "workload": {
            "url": args.url,
            "backend": args.backend,
            "query": args.query,
            "concurrency": args.concurrency,
            "duration_seconds": args.duration,
        },
        "result": {
            "requests": requests,
            "errors": errors,
            "throughput_rps": requests / elapsed if elapsed else 0,
            "latency_ms": {
                "p50": percentile(latencies, 0.50),
                "p95": percentile(latencies, 0.95),
                "p99": percentile(latencies, 0.99),
                "max": max(latencies) if latencies else None,
            },
            "successful_latency_ms": {
                "p50": percentile(successful_latencies, 0.50),
                "p95": percentile(successful_latencies, 0.95),
                "p99": percentile(successful_latencies, 0.99),
                "max": max(successful_latencies) if successful_latencies else None,
            },
            "error_latency_ms": {
                "p50": percentile(error_latencies, 0.50),
                "p95": percentile(error_latencies, 0.95),
                "p99": percentile(error_latencies, 0.99),
                "max": max(error_latencies) if error_latencies else None,
            },
            "response_bytes": response_bytes,
            "mean_response_bytes": response_bytes / requests if requests else None,
            "server_cpu_percent": cpu_percent,
            "server_max_rss_kib": max_rss_kib or None,
            "server_max_established_connections": max_connections,
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True)
    parser.add_argument("--query", required=True)
    parser.add_argument("--variables", default="{}")
    parser.add_argument("--role", default="user")
    parser.add_argument("--backend", default="unknown")
    parser.add_argument("--concurrency", type=int, default=10)
    parser.add_argument("--duration", type=float, default=60.0)
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--pid", type=int)
    parser.add_argument("--server-port", type=int)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    result = run(args)
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result["result"], indent=2))


if __name__ == "__main__":
    main()
