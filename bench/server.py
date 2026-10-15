"""The cache server benchmark: `tinymold serve` against bazel-remote.

Both servers are driven through the same client, Python's `http.client`
over Bazel's HTTP cache protocol (`/cas/<sha256>`, `/ac/<sha256>`), which
both serve, on loopback, one keep-alive connection at a time. Measured,
per run and per server, alternating the servers so drift hits both:

- `hit 1 KiB`: round trips for GETs of one small blob, p50 and p99;
- `download 4 MiB`: GETs of one large blob, MB/s;
- `upload 4 MiB`: PUTs of distinct large blobs, MB/s;
- `cold start`: from launch to the first hit on a store holding
  `--entries` action results.

`FindMissingBlobs` needs gRPC, which the standard library does not speak,
so it is not measured here. Run it as `python3 bench/bench.py server
--bazel-remote PATH`, with a native bazel-remote build; without one, the
docker image is used and the result says so, since docker's port
forwarding on macOS adds latency of its own.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import os
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

import bench

TINYMOLD_PORT = 19190
BAZEL_HTTP_PORT = 19191
BAZEL_GRPC_PORT = 19192
SMALL = 1024
LARGE = 4 << 20
LARGE_SAMPLES = 20


def add_parser(sub) -> None:
    parser = sub.add_parser("server", help="tinymold serve against bazel-remote: latency, "
                            "throughput, cold start")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--requests", type=int, default=2000,
                        help="small GETs per run and server")
    parser.add_argument("--entries", type=int, default=20000,
                        help="action results in the store for the cold-start case")
    parser.add_argument("--bazel-remote", help="a native bazel-remote binary; "
                        "without it, the docker image")


def percentile(samples: list[float], fraction: float) -> float:
    """The `fraction` percentile of `samples`, by nearest rank."""
    ordered = sorted(samples)
    rank = max(0, min(len(ordered) - 1, round(fraction * len(ordered)) - 1))
    return ordered[rank]


class Server:
    """One server process (or container), started and stopped on demand."""

    def __init__(self, name: str, port: int, command: list[str], stop: list[str] | None = None):
        self.name, self.port, self.command, self.stop_command = name, port, command, stop
        self.process: subprocess.Popen | None = None

    def start(self) -> float:
        """Starts the server and returns the seconds until it answers."""
        started = time.perf_counter()
        self.process = subprocess.Popen(self.command, stdout=subprocess.DEVNULL,
                                        stderr=subprocess.DEVNULL)
        for _ in range(600):
            try:
                connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=1)
                connection.request("GET", f"/cas/{hashlib.sha256(b'probe').hexdigest()}")
                connection.getresponse().read()
                connection.close()
                return time.perf_counter() - started
            except OSError:
                time.sleep(0.01)
        raise RuntimeError(f"{self.name} did not start")

    def stop(self) -> None:
        if self.stop_command:
            subprocess.run(self.stop_command, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL)
        if self.process is not None:
            self.process.terminate()
            self.process.wait(timeout=60)
            self.process = None


class Client:
    """One keep-alive HTTP/1.1 connection."""

    def __init__(self, port: int):
        self.connection = http.client.HTTPConnection("127.0.0.1", port, timeout=60)

    def put(self, path: str, body: bytes) -> int:
        self.connection.request("PUT", path, body=body,
                                headers={"Content-Length": str(len(body))})
        response = self.connection.getresponse()
        response.read()
        return response.status

    def get(self, path: str) -> bytes:
        self.connection.request("GET", path)
        response = self.connection.getresponse()
        body = response.read()
        if response.status != 200:
            raise RuntimeError(f"GET {path}: {response.status}")
        return body

    def close(self) -> None:
        self.connection.close()


def cas(body: bytes) -> str:
    return f"/cas/{hashlib.sha256(body).hexdigest()}"


def measure(server: Server, requests: int) -> dict[str, list[float]]:
    """One run's samples for every case but cold start."""
    client = Client(server.port)
    small = os.urandom(SMALL)
    assert client.put(cas(small), small) == 200
    latencies = []
    for _ in range(requests):
        started = time.perf_counter()
        client.get(cas(small))
        latencies.append(time.perf_counter() - started)

    large = os.urandom(LARGE)
    assert client.put(cas(large), large) == 200
    downloads = []
    for _ in range(LARGE_SAMPLES):
        started = time.perf_counter()
        client.get(cas(large))
        downloads.append(LARGE / (time.perf_counter() - started) / 1e6)

    bodies = [os.urandom(LARGE) for _ in range(LARGE_SAMPLES)]
    uploads = []
    for body in bodies:
        started = time.perf_counter()
        assert client.put(cas(body), body) == 200
        uploads.append(LARGE / (time.perf_counter() - started) / 1e6)
    client.close()
    return {
        "hit 1 KiB p50 (ms)": [percentile(latencies, 0.5) * 1000],
        "hit 1 KiB p99 (ms)": [percentile(latencies, 0.99) * 1000],
        "download 4 MiB (MB/s)": [statistics.median(downloads)],
        "upload 4 MiB (MB/s)": [statistics.median(uploads)],
    }


def fill(server: Server, entries: int) -> str:
    """Stores `entries` small action results; returns the last one's path."""
    client = Client(server.port)
    path = ""
    for index in range(entries):
        key = hashlib.sha256(f"action {index}".encode()).hexdigest()
        path = f"/ac/{key}"
        # A valid ActionResult with no blob references: one `stdout_raw`
        # field (5) of 64 bytes, so bazel-remote's validation accepts it.
        assert client.put(path, b"\x2a\x40" + f"{index:064d}".encode()) == 200
    client.close()
    return path


def binary_version(path: str) -> str:
    """A Go binary's main module version, as `go version -m` reports it."""
    if shutil.which("go") is None:
        return "unknown version"
    report = subprocess.run(["go", "version", "-m", path], capture_output=True, text=True)
    for line in report.stdout.splitlines():
        fields = line.split()
        if fields[:1] == ["mod"] and len(fields) >= 3:
            return f"{fields[1]} {fields[2]}"
    return "unknown version"


def servers(args: argparse.Namespace, work: Path) -> tuple[list[Server], dict[str, str]]:
    tinymold = bench.tinymold_binary()
    tinymold_dir = work / "tinymold"
    found = [Server("tinymold serve", TINYMOLD_PORT,
                    [str(tinymold), "--store", str(work / "unused-wrapper-store"), "serve",
                     "--dir", str(tinymold_dir), "--listen", f"127.0.0.1:{TINYMOLD_PORT}",
                     "--max-size", "20G"])]
    described = {"tinymold serve": f"release build, {tinymold.name}, uncompressed store"}
    bazel_dir = work / "bazel-remote"
    if args.bazel_remote:
        version = binary_version(args.bazel_remote)
        found.append(Server("bazel-remote", BAZEL_HTTP_PORT,
                            [args.bazel_remote, "--dir", str(bazel_dir), "--max_size", "20",
                             "--http_address", f"127.0.0.1:{BAZEL_HTTP_PORT}",
                             "--grpc_address", f"127.0.0.1:{BAZEL_GRPC_PORT}"]))
        described["bazel-remote"] = f"native, {version}, default zstd storage"
    elif shutil.which("docker"):
        name = "tinymold-bench-server"
        found.append(Server("bazel-remote", BAZEL_HTTP_PORT,
                            ["docker", "run", "--rm", "--name", name,
                             "-p", f"{BAZEL_HTTP_PORT}:8080", "-v", f"{bazel_dir}:/data",
                             bench.BAZEL_REMOTE_IMAGE, "--dir=/data", "--max_size=20"],
                            stop=["docker", "rm", "-f", name]))
        described["bazel-remote"] = f"docker, {bench.BAZEL_REMOTE_IMAGE}, default zstd storage"
    return found, described


def bench_server(args: argparse.Namespace) -> dict:
    work = bench.WORK / "server"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    candidates, described = servers(args, work)

    samples: dict[tuple[str, str], list[float]] = {}
    for run in range(args.runs):
        # Alternate which server goes first, so drift hits both alike.
        order = candidates if run % 2 == 0 else list(reversed(candidates))
        for server in order:
            server.start()
            try:
                for case, values in measure(server, args.requests).items():
                    samples.setdefault((server.name, case), []).extend(values)
            finally:
                server.stop()
            print(f"run {run + 1}/{args.runs}: {server.name} measured", file=sys.stderr)

    for server in candidates:
        server.start()
        try:
            last = fill(server, args.entries)
        finally:
            server.stop()
        for _ in range(args.runs):
            started = time.perf_counter()
            server.start()
            try:
                client = Client(server.port)
                client.get(last)
                client.close()
            finally:
                server.stop()
            samples.setdefault((server.name, "cold start (s)"), []).append(
                time.perf_counter() - started)

    results = [{"server": name, "case": case,
                "median": round(statistics.median(values), 4),
                "samples": [round(value, 4) for value in values]}
               for (name, case), values in samples.items()]
    shutil.rmtree(work, ignore_errors=True)
    return {"benchmark": "server", "servers": described, "requests": args.requests,
            "entries": args.entries, "not_measured": ["FindMissingBlobs (gRPC)"],
            "results": results,
            "cargo_settings": "tinymold release build; servers on loopback, one client "
                              "connection, HTTP/1.1 keep-alive"}
