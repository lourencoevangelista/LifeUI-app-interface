"""Throwaway local PostgreSQL clusters for tests, shared by smoke_test.py and backup.py.

Uses the program files of an installed PostgreSQL (initdb / pg_ctl / psql / pg_dump / pg_restore) but never the
installed service or its data: each cluster lives in its own temp folder, trusts local connections, listens on a free
port on 127.0.0.1 only, and is deleted by stop().

Import it from a script run with `python -I` like this (isolated mode drops the script folder from sys.path):
    sys.path.insert(0, str(Path(__file__).resolve().parent)); from localpg import LocalPostgres, find_pg_bin
"""
from __future__ import annotations

import os
import shutil
import socket
import subprocess
import tempfile
from pathlib import Path

PG_ROOT = Path(r"C:\Program Files\PostgreSQL")  # Windows installer layout: <root>/<major>/bin
PG_ROOT_DEBIAN = Path("/usr/lib/postgresql")       # Debian/Ubuntu packages: <root>/<major>/bin
EXE = ".exe" if os.name == "nt" else ""


def pg_tool(bindir: Path, name: str) -> Path:
    """A PostgreSQL program in bindir: `pg_tool(bin, "psql")` is psql.exe on Windows and psql elsewhere."""
    return bindir / (name.removesuffix(".exe") + EXE)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def find_pg_bin() -> Path | None:
    """PG_BIN if set, else the newest installed major version, else whatever initdb is on PATH."""
    if os.environ.get("PG_BIN"):
        return Path(os.environ["PG_BIN"]) if pg_tool(Path(os.environ["PG_BIN"]), "initdb").exists() else None
    root = PG_ROOT if os.name == "nt" else PG_ROOT_DEBIAN
    candidates = sorted(root.glob("*/bin"), key=lambda p: int(p.parent.name) if p.parent.name.isdigit() else 0, reverse=True)
    found = next((p for p in candidates if pg_tool(p, "initdb").exists()), None)
    on_path = shutil.which("initdb")
    return found or (Path(on_path).parent if on_path else None)


class LocalPostgres:
    def __init__(self, bindir: Path, migrations_dir: Path | None = None, database: str = "lifeui_test"):
        self.bin = bindir
        self.migrations_dir = migrations_dir
        self.database = database
        self.dir = Path(tempfile.mkdtemp(prefix="lifeui-pg-"))
        self.data = self.dir / "data"
        self.port = free_port()
        self.started = False

    @property
    def url(self) -> str:
        return f"postgresql://postgres@127.0.0.1:{self.port}/{self.database}?sslmode=disable"

    def run(self, exe: str, *args: str, timeout: int = 300, env: dict | None = None) -> subprocess.CompletedProcess:
        # PGCLIENTENCODING=UTF8 keeps non-ASCII text (accented names, emoji) intact on a Windows console.
        merged = {**os.environ, "PGCLIENTENCODING": "UTF8", **(env or {})}
        return subprocess.run([str(pg_tool(self.bin, exe)), *args], capture_output=True, text=True, encoding="utf-8", errors="replace",
                              timeout=timeout, env=merged)

    def conn_args(self, database: str | None = None) -> list[str]:
        return ["-h", "127.0.0.1", "-p", str(self.port), "-U", "postgres", "-d", database or self.database]

    def psql(self, sql: str, db: str | None = None, role: str = "postgres", check_ok: bool = True) -> subprocess.CompletedProcess:
        args = ["-h", "127.0.0.1", "-p", str(self.port), "-U", role, "-d", db or self.database, "-v", "ON_ERROR_STOP=1", "-At", "-c", sql]
        p = self.run("psql.exe", *args)
        if check_ok and p.returncode != 0:
            raise RuntimeError(f"psql failed: {p.stderr.strip()[:300]}")
        return p

    def start(self) -> None:
        p = self.run("initdb.exe", "-D", str(self.data), "-U", "postgres", "--auth=trust", "-E", "UTF8", "--no-locale")
        if p.returncode != 0:
            raise RuntimeError(f"initdb failed: {p.stderr.strip()[:300]}")
        # The server process inherits pg_ctl's output handles on Windows: capturing them through PIPES makes
        # subprocess wait for the server to exit (hangs forever). Send them to a file instead.
        with open(self.dir / "pg_ctl.out", "wb") as out:
            p = subprocess.run([str(pg_tool(self.bin, "pg_ctl")), "start", "-D", str(self.data), "-o",
                                f"-p {self.port} -c listen_addresses=127.0.0.1" + ("" if os.name == "nt" else f" -c unix_socket_directories={self.dir}"), "-w", "-t", "60", "-l", str(self.dir / "pg.log")],
                               stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT, timeout=120)
        if p.returncode != 0:
            raise RuntimeError(f"pg_ctl start failed: {(self.dir / 'pg_ctl.out').read_text(errors='replace')[-300:]}")
        self.started = True
        self.psql(f"CREATE DATABASE {self.database}", db="postgres")
        self.psql("CREATE ROLE anon NOLOGIN", db="postgres")  # Supabase has this role; used for the lock-down check
        if self.migrations_dir:
            for migration in sorted(self.migrations_dir.glob("*.sql")):
                p = self.run("psql.exe", *self.conn_args(), "-v", "ON_ERROR_STOP=1", "-q", "-f", str(migration))
                if p.returncode != 0:
                    raise RuntimeError(f"migration {migration.name} failed: {p.stderr.strip()[:400]}")

    def stop(self) -> None:
        if self.started:
            self.run("pg_ctl.exe", "stop", "-D", str(self.data), "-m", "immediate", "-w", "-t", "30")
        shutil.rmtree(self.dir, ignore_errors=True)
