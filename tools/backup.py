#!/usr/bin/env python
"""Encrypted backups for LifeUI. Supabase's free plan has NO backups, and data-private/ exists only on this PC.

    python -I tools/backup.py backup  [--out DIR] [--keep N] [--no-db] [--no-files]
    python -I tools/backup.py verify  FILE.lifeui [--compare-source]   # decrypt + restore into a THROWAWAY local PostgreSQL
    python -I tools/backup.py restore FILE.lifeui --into-url URL --yes   # restore into an EMPTY database you name
    python -I tools/backup.py selftest                                   # full round trip on throwaway databases

What is saved
  - the database: pg_dump (custom format) of schema `lifeui` (+ drizzle's bookkeeping schema if present), over the
    SESSION pooler in DATABASE_URL_MIGRATE with VERIFIED TLS (db/certs/supabase-prod-ca-2021.crt);
  - the files: a zip of data-private/ (owner.ttl, imports, evidence). NOT .env (it holds passwords) and never the passphrase.
Both are encrypted with AES-256-GCM; the key comes from your passphrase through scrypt. File format:
    b"LIFEUIBK1" | log2(n) r p (3 bytes) | salt (16) | nonce (12) | ciphertext+tag. The header is authenticated, so any change is detected.

The passphrase: env BACKUP_PASSPHRASE (also read from .env) or a hidden prompt. At least 16 characters.
WRITE IT DOWN IN A PASSWORD MANAGER. If this PC dies, .env dies with it, and without the passphrase the backups are unreadable.
Secrets (database password, passphrase) are never printed.
"""
from __future__ import annotations

import argparse
import getpass
import hashlib
import io
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import zipfile
from pathlib import Path
from urllib.parse import unquote, urlparse

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cryptography.exceptions import InvalidTag  # noqa: E402
from cryptography.hazmat.primitives.ciphers.aead import AESGCM  # noqa: E402
from cryptography.hazmat.primitives.kdf.scrypt import Scrypt  # noqa: E402
from localpg import LocalPostgres, find_pg_bin, pg_tool  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
CA_FILE = ROOT / "db" / "certs" / "supabase-prod-ca-2021.crt"
DATA_PRIVATE = ROOT / "data-private"
DEFAULT_OUT = ROOT.parent / "LifeUI-backups"  # next to the repo, outside it: never committed or uploaded

MAGIC = b"LIFEUIBK1"
SCRYPT_LOG2_N, SCRYPT_R, SCRYPT_P = 15, 8, 1  # 32 MiB of memory
MAX_LOG2_N = 20  # refuse absurd parameters in a (possibly hostile) file header
MIN_PASSPHRASE_CHARS = 16
EXPECTED_TABLES = {"agent_inbox", "agent_messages", "agent_runs", "agent_settings", "agent_tokens", "audit_log", "auth_throttle",
                   "entities", "entity_links", "events", "news_briefings", "news_items", "owner_credentials", "sessions", "sources"}
NAME_DB = re.compile(r"^lifeui-db-\d{8}-\d{6}\.lifeui$")
NAME_FILES = re.compile(r"^lifeui-files-\d{8}-\d{6}\.lifeui$")


class BackupError(Exception):
    pass


# ----------------------------------------------------------------------------- crypto (pure, tested by selftest)
def _key(passphrase: str, salt: bytes, log2n: int, r: int, p: int) -> bytes:
    return Scrypt(salt=salt, length=32, n=2**log2n, r=r, p=p).derive(passphrase.encode("utf-8"))


def encrypt_bytes(data: bytes, passphrase: str) -> bytes:
    salt, nonce = os.urandom(16), os.urandom(12)
    header = MAGIC + bytes([SCRYPT_LOG2_N, SCRYPT_R, SCRYPT_P]) + salt + nonce
    return header + AESGCM(_key(passphrase, salt, SCRYPT_LOG2_N, SCRYPT_R, SCRYPT_P)).encrypt(nonce, data, header)


def decrypt_bytes(blob: bytes, passphrase: str) -> bytes:
    header_len = len(MAGIC) + 3 + 16 + 12
    if len(blob) < header_len + 16 or not blob.startswith(MAGIC):
        raise BackupError("not a LifeUI backup file (bad header or truncated)")
    log2n, r, p = blob[len(MAGIC)], blob[len(MAGIC) + 1], blob[len(MAGIC) + 2]
    if not (10 <= log2n <= MAX_LOG2_N and 1 <= r <= 16 and 1 <= p <= 4):
        raise BackupError("refusing unreasonable key-derivation parameters in the file header")
    salt = blob[len(MAGIC) + 3 : len(MAGIC) + 19]
    nonce = blob[len(MAGIC) + 19 : header_len]
    try:
        return AESGCM(_key(passphrase, salt, log2n, r, p)).decrypt(nonce, blob[header_len:], blob[:header_len])
    except InvalidTag:
        raise BackupError("wrong passphrase, or the file is damaged or has been tampered with") from None


# ----------------------------------------------------------------------------- environment and connections
def load_env() -> dict[str, str]:
    env: dict[str, str] = {}
    path = ROOT / ".env"
    if path.exists():
        for line in path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                key, value = line.split("=", 1)
                env[key.strip()] = value.strip().strip('"').strip("'")
    return env


def get_passphrase(confirm: bool) -> str:
    value = os.environ.get("BACKUP_PASSPHRASE") or load_env().get("BACKUP_PASSPHRASE")
    if not value:
        if not sys.stdin.isatty():
            raise BackupError("no passphrase: set BACKUP_PASSPHRASE in .env or the environment")
        value = getpass.getpass("Backup passphrase: ")
        if confirm and getpass.getpass("Repeat it: ") != value:
            raise BackupError("the two passphrases differ")
    if confirm and len(value) < MIN_PASSPHRASE_CHARS:
        raise BackupError(f"the passphrase must have at least {MIN_PASSPHRASE_CHARS} characters")
    return value


def conn_from_url(url: str) -> dict:
    u = urlparse(url)
    return {
        "host": u.hostname or "", "port": str(u.port or 5432), "user": unquote(u.username or ""),
        "password": unquote(u.password or ""), "db": (u.path or "/postgres").lstrip("/") or "postgres",
        "local": (u.hostname or "") in ("localhost", "127.0.0.1", "::1"),
    }


def pg_env(conn: dict) -> dict:
    env = {"PGPASSWORD": conn["password"], "PGCONNECT_TIMEOUT": "30", "PGCLIENTENCODING": "UTF8"}
    if conn["local"]:
        env["PGSSLMODE"] = "disable"
    else:
        env.update(PGSSLMODE="verify-full", PGSSLROOTCERT=str(CA_FILE))
    return env


def mask(text: str, conn: dict) -> str:
    return text.replace(conn["password"], "***") if conn.get("password") else text


def pg(bindir: Path, exe: str, conn: dict, *args: str, timeout: int = 900) -> subprocess.CompletedProcess:
    p = subprocess.run(
        [str(pg_tool(bindir, exe)), "-h", conn["host"], "-p", conn["port"], "-U", conn["user"], "-d", conn["db"], *args],
        capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout, env={**os.environ, **pg_env(conn)},
    )
    return p


def scalar(bindir: Path, conn: dict, sql: str) -> str:
    p = pg(bindir, "psql.exe", conn, "-v", "ON_ERROR_STOP=1", "-tAc", sql)
    if p.returncode != 0:
        raise BackupError(f"query failed: {mask(p.stderr.strip(), conn)[:300]}")
    return p.stdout.strip()


# ----------------------------------------------------------------------------- database dump / restore / compare
def dump_database(bindir: Path, conn: dict, out_file: Path) -> list[str]:
    schemas = ["lifeui"]
    if scalar(bindir, conn, "SELECT count(*) FROM pg_namespace WHERE nspname = 'drizzle'") != "0":
        schemas.append("drizzle")  # keeps the migration bookkeeping, so a restore matches the original exactly
    args = ["-Fc", "--no-owner", "--no-privileges", "-f", str(out_file)]
    for schema in schemas:
        args += ["-n", schema]
    p = pg(bindir, "pg_dump.exe", conn, *args)
    if p.returncode != 0 or not out_file.exists() or out_file.stat().st_size == 0:
        raise BackupError(f"pg_dump failed: {mask(p.stderr.strip(), conn)[:400]}")
    return schemas


def restore_dump(bindir: Path, conn: dict, dump_file: Path) -> None:
    p = pg(bindir, "pg_restore.exe", conn, "--no-owner", "--no-privileges", "--exit-on-error", str(dump_file))
    if p.returncode != 0:
        raise BackupError(f"pg_restore failed: {mask(p.stderr.strip(), conn)[:500]}")


def summarize(bindir: Path, conn: dict) -> dict[str, tuple[int, str]]:
    """Per table of schema lifeui: (row count, md5 over all rows as text). Same on source and restored copy if identical."""
    names = scalar(bindir, conn, "SELECT string_agg(table_name, ',' ORDER BY table_name) FROM information_schema.tables "
                                 "WHERE table_schema = 'lifeui' AND table_type = 'BASE TABLE'")
    result: dict[str, tuple[int, str]] = {}
    for name in filter(None, names.split(",")):
        row = scalar(bindir, conn, f"SELECT count(*) || '|' || coalesce(md5(string_agg(t::text, '|' ORDER BY t::text)), '') FROM lifeui.\"{name}\" t")
        count, checksum = row.split("|", 1)
        result[name] = (int(count), checksum)
    return result


# ----------------------------------------------------------------------------- file backup
def zip_directory(directory: Path) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(directory.rglob("*")):
            if path.is_file():
                archive.write(path, path.relative_to(directory).as_posix())
    return buffer.getvalue()


def prune(out_dir: Path, pattern: re.Pattern, keep: int) -> list[str]:
    files = sorted((f for f in out_dir.iterdir() if pattern.match(f.name)), key=lambda f: f.name, reverse=True)
    removed = []
    for old in files[keep:]:
        old.unlink()
        removed.append(old.name)
    return removed


# ----------------------------------------------------------------------------- commands
def cmd_backup(args) -> int:
    passphrase = get_passphrase(confirm=True)
    out_dir = Path(args.out) if args.out else DEFAULT_OUT
    out_dir.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    made = []
    if not args.no_db:
        url = load_env().get("DATABASE_URL_MIGRATE")
        if not url:
            raise BackupError("DATABASE_URL_MIGRATE is missing in .env (the session-pooler string used for migrations)")
        bindir = find_pg_bin()
        if not bindir:
            raise BackupError("no installed PostgreSQL found (pg_dump is needed)")
        conn = conn_from_url(url)
        with tempfile.TemporaryDirectory() as tmp:
            raw = Path(tmp) / "dump.pgdump"
            schemas = dump_database(bindir, conn, raw)
            target = out_dir / f"lifeui-db-{stamp}.lifeui"
            target.write_bytes(encrypt_bytes(raw.read_bytes(), passphrase))
        made.append((target, f"database, schemas {', '.join(schemas)}"))
    if not args.no_files:
        if not DATA_PRIVATE.exists():
            raise BackupError("data-private/ not found")
        target = out_dir / f"lifeui-files-{stamp}.lifeui"
        target.write_bytes(encrypt_bytes(zip_directory(DATA_PRIVATE), passphrase))
        made.append((target, "data-private/ (zip)"))
    for path, what in made:
        print(f"wrote {path}  ({path.stat().st_size / 1024:.0f} KB, {what}, AES-256-GCM)")
    for pattern in (NAME_DB, NAME_FILES):
        for name in prune(out_dir, pattern, args.keep):
            print(f"removed old backup {name} (keeping the newest {args.keep})")
    print("Second location: copy the folder to a place that survives this PC (cloud drive, external disk). "
          "The backups are encrypted; the passphrase must be kept somewhere else.")
    return 0


def cmd_verify(args) -> int:
    passphrase = get_passphrase(confirm=False)
    blob = Path(args.file).read_bytes()
    plain = decrypt_bytes(blob, passphrase)
    print(f"decrypted OK ({len(plain) / 1024:.0f} KB)")
    if plain[:2] == b"PK":  # a files backup
        names = zipfile.ZipFile(io.BytesIO(plain)).namelist()
        print(f"files backup: {len(names)} files, e.g. {names[:3]}")
        return 0
    bindir = find_pg_bin()
    if not bindir:
        raise BackupError("no installed PostgreSQL found")
    cluster = LocalPostgres(bindir, None, "restore_check")
    try:
        cluster.start()
        conn = conn_from_url(cluster.url)
        with tempfile.TemporaryDirectory() as tmp:
            dump = Path(tmp) / "dump.pgdump"
            dump.write_bytes(plain)
            restore_dump(bindir, conn, dump)
        restored = summarize(bindir, conn)
        print("restored into a throwaway database:")
        for name, (count, _) in restored.items():
            print(f"  lifeui.{name:22} {count} row(s)")
        missing = EXPECTED_TABLES - set(restored)
        if missing:
            raise BackupError(f"restored copy lacks tables: {sorted(missing)}")
        if args.compare_source:
            source = summarize(bindir, conn_from_url(load_env()["DATABASE_URL_MIGRATE"]))
            if source != restored:
                diff = sorted(t for t in set(source) | set(restored) if source.get(t) != restored.get(t))
                raise BackupError(f"restored copy differs from the live database in: {diff} (data may have changed since the backup)")
            print("restored copy is IDENTICAL to the live database (row counts and checksums)")
        print("VERIFIED")
    finally:
        cluster.stop()
    return 0


def cmd_restore(args) -> int:
    if not args.yes:
        raise BackupError("restore writes into the database you name: add --yes to confirm (the target must be empty)")
    passphrase = get_passphrase(confirm=False)
    plain = decrypt_bytes(Path(args.file).read_bytes(), passphrase)
    bindir = find_pg_bin()
    if not bindir:
        raise BackupError("no installed PostgreSQL found")
    conn = conn_from_url(args.into_url)
    existing = scalar(bindir, conn, "SELECT count(*) FROM pg_namespace WHERE nspname = 'lifeui'")
    if existing != "0":
        raise BackupError("the target already has a 'lifeui' schema: restore only into an empty database (drop the schema yourself if you really want to replace it)")
    with tempfile.TemporaryDirectory() as tmp:
        dump = Path(tmp) / "dump.pgdump"
        dump.write_bytes(plain)
        restore_dump(bindir, conn, dump)
    print("restored:", {k: v[0] for k, v in summarize(bindir, conn).items()})
    return 0


# ----------------------------------------------------------------------------- self test
SAMPLE_SQL = """
INSERT INTO lifeui.sources (iri, label, kind) VALUES ('https://lifeui.example/id/src-a', 'Source: meeting with Zoë ✓', 'UserStated');
INSERT INTO lifeui.events (iri, class_iri, name, description, occurred_at, occurred_on, started_at, ended_at, status, sensitivity, source_id, confidence, payload)
SELECT 'https://lifeui.example/id/event-1', 'https://lifeui.example/ontology#Milestone', 'Aniversário 🎂 "quoted" \\ backslash', E'line1\\nline2', '2026-05-29T10:00:00+01', NULL, NULL, NULL,
       'Completed', 'Personal', id, 0.85, '{"k": ["ç", "日本語", {"n": 1.5}]}'::jsonb FROM lifeui.sources LIMIT 1;
INSERT INTO lifeui.events (iri, class_iri, name, occurred_on, status) VALUES ('https://lifeui.example/id/event-2', 'https://lifeui.example/ontology#Event', 'Só a data', '2026-11-08', 'Planned');
INSERT INTO lifeui.news_briefings (date, intro, weather, generated_at) VALUES ('2026-11-19', E'3 things\n- Ação ✓', '{"summary": "Faro: 13–18 °C"}'::jsonb, now());
INSERT INTO lifeui.news_items (briefing_id, position, url, title, source, importance, focus_relevance, summary, tags)
SELECT id, 0, 'https://example.com/a', 'Gemini 4 «Neon»', 'DeepMind', 9, 3, 'One. Two.', '["release"]'::jsonb FROM lifeui.news_briefings LIMIT 1;
INSERT INTO lifeui.owner_credentials (id, password_hash) VALUES (1, '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA');
INSERT INTO lifeui.sessions (token_hash, expires_at) VALUES ('abc123', now() + interval '1 day');
INSERT INTO lifeui.auth_throttle (id, failed_count) VALUES (1, 2);
INSERT INTO lifeui.audit_log (action, target, ip, detail) VALUES ('login_failed', NULL, '203.0.113.7', '{"retry_after_secs": 30}'::jsonb);
"""


def cmd_selftest(_args) -> int:
    results: list[tuple[bool, str]] = []

    def check(ok: bool, name: str, detail: str = "") -> None:
        results.append((ok, name))
        print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f": {detail}" if detail else ""))

    def raises(fn) -> bool:
        try:
            fn()
        except BackupError:
            return True
        return False

    secret = "correct horse battery staple 42"
    # --- crypto
    blob = encrypt_bytes(b"hello LifeUI \xc3\xa7", secret)
    check(decrypt_bytes(blob, secret) == b"hello LifeUI \xc3\xa7", "encrypt/decrypt round trip")
    check(encrypt_bytes(b"x", secret) != encrypt_bytes(b"x", secret), "same input encrypts differently every time (fresh salt and nonce)")
    check(raises(lambda: decrypt_bytes(blob, secret + "x")), "wrong passphrase is refused")
    flipped = bytearray(blob)
    flipped[-5] ^= 1
    check(raises(lambda: decrypt_bytes(bytes(flipped), secret)), "a flipped ciphertext byte is detected")
    header_flip = bytearray(blob)
    header_flip[len(MAGIC) + 5] ^= 1  # inside the salt: also authenticated through the key / header
    check(raises(lambda: decrypt_bytes(bytes(header_flip), secret)), "a changed header byte is detected")
    check(raises(lambda: decrypt_bytes(blob[:30], secret)) and raises(lambda: decrypt_bytes(b"not a backup at all, really not", secret)), "truncated or foreign files are refused")
    hostile = bytearray(blob)
    hostile[len(MAGIC)] = 40  # absurd scrypt cost: must be refused before any work is done
    check(raises(lambda: decrypt_bytes(bytes(hostile), secret)), "absurd key-derivation parameters in a header are refused")

    # --- database round trip on two throwaway clusters
    bindir = find_pg_bin()
    if not bindir:
        check(False, "needs an installed PostgreSQL")
        return 1
    source = LocalPostgres(bindir, ROOT / "db" / "migrations")
    target = LocalPostgres(bindir, None, "restored")
    try:
        source.start()
        target.start()
        with tempfile.TemporaryDirectory() as tmp:
            sql_file = Path(tmp) / "sample.sql"
            sql_file.write_text(SAMPLE_SQL, encoding="utf-8")
            p = source.run("psql.exe", *source.conn_args(), "-v", "ON_ERROR_STOP=1", "-q", "-f", str(sql_file))
            check(p.returncode == 0, "sample data (unicode, emoji, quotes, jsonb, timestamps, a Secret pointer) inserted", p.stderr.strip()[:200])
            src_conn, dst_conn = conn_from_url(source.url), conn_from_url(target.url)
            before = summarize(bindir, src_conn)
            check(set(before) == EXPECTED_TABLES and before["events"][0] == 2, f"source has the {len(EXPECTED_TABLES)} tables and the sample rows", str({k: v[0] for k, v in before.items()}))

            raw = Path(tmp) / "dump.pgdump"
            schemas = dump_database(bindir, src_conn, raw)
            check(schemas == ["lifeui"], "dump covers schema lifeui", str(schemas))
            encrypted = encrypt_bytes(raw.read_bytes(), secret)
            check(b"PGDMP" not in encrypted and "Zoë".encode() not in encrypted and b"lifeui" not in encrypted and b"events" not in encrypted,
                  "the encrypted file contains no readable database content")
            check(raises(lambda: decrypt_bytes(encrypted, "another passphrase of 16+")), "the encrypted dump is refused with another passphrase")

            plain = decrypt_bytes(encrypted, secret)
            restored_file = Path(tmp) / "restored.pgdump"
            restored_file.write_bytes(plain)
            restore_dump(bindir, dst_conn, restored_file)
            after = summarize(bindir, dst_conn)
            check(after == before, "restored database equals the original: every table, row count and checksum", str({k: v[0] for k, v in after.items()}))
            check(scalar(bindir, dst_conn, "SELECT count(*) FROM pg_tables WHERE schemaname = 'lifeui' AND NOT rowsecurity") == "0",
                  "row-level security is still on for every restored table")
            check(scalar(bindir, dst_conn, "SELECT count(*) FROM pg_constraint WHERE conname = 'events_has_time_check'") == "1",
                  "the CHECK constraints came back with the data")
            check(scalar(bindir, dst_conn, "SELECT count(*) FROM pg_trigger WHERE tgname IN ('audit_log_append_only', 'audit_log_no_truncate')") == "2",
                  "the audit log's append-only triggers came back with the data")
            check(scalar(bindir, dst_conn, "SELECT name FROM lifeui.events WHERE iri LIKE '%event-1'") == 'Aniversário 🎂 "quoted" \\ backslash',
                  "special characters survived byte for byte")

            # --- restore command safety: refuses a target that already has the schema
            second = Path(tmp) / "again.pgdump"
            second.write_bytes(plain)
            check(scalar(bindir, dst_conn, "SELECT count(*) FROM pg_namespace WHERE nspname = 'lifeui'") == "1", "the target now has a lifeui schema (so a second restore must be refused by cmd_restore)")
    finally:
        source.stop()
        target.stop()

    # --- files round trip and retention
    with tempfile.TemporaryDirectory() as tmp:
        folder = Path(tmp) / "data"
        (folder / "import").mkdir(parents=True)
        (folder / "owner.ttl").write_bytes("# Zoë\n".encode("utf-8"))  # bytes: write_text would turn \n into \r\n on Windows
        (folder / "import" / "work.ttl").write_bytes(b"x" * 5000)
        archive = decrypt_bytes(encrypt_bytes(zip_directory(folder), secret), secret)
        zf = zipfile.ZipFile(io.BytesIO(archive))
        check(sorted(zf.namelist()) == ["import/work.ttl", "owner.ttl"] and zf.read("owner.ttl").decode() == "# Zoë\n", "files backup round trip keeps names and contents")
        out = Path(tmp) / "out"
        out.mkdir()
        for day in range(1, 6):
            (out / f"lifeui-db-2026110{day}-120000.lifeui").write_bytes(b"x")
        (out / "keep-me.txt").write_text("not a backup")
        removed = prune(out, NAME_DB, 2)
        left = sorted(f.name for f in out.iterdir())
        check(len(removed) == 3 and left == ["keep-me.txt", "lifeui-db-20261104-120000.lifeui", "lifeui-db-20261105-120000.lifeui"],
              "retention keeps only the newest N backups and never touches other files", str(left))

    failed = [name for ok, name in results if not ok]
    print(f"\n{len(results) - len(failed)} passed, {len(failed)} failed")
    return 1 if failed else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    b = sub.add_parser("backup")
    b.add_argument("--out", help=f"folder for the backups (default {DEFAULT_OUT})")
    b.add_argument("--keep", type=int, default=30, help="how many of each kind to keep (default 30)")
    b.add_argument("--no-db", action="store_true")
    b.add_argument("--no-files", action="store_true")
    v = sub.add_parser("verify")
    v.add_argument("file")
    v.add_argument("--compare-source", action="store_true", help="also compare with the live database (run right after a backup)")
    r = sub.add_parser("restore")
    r.add_argument("file")
    r.add_argument("--into-url", required=True)
    r.add_argument("--yes", action="store_true")
    sub.add_parser("selftest")
    args = parser.parse_args()
    try:
        return {"backup": cmd_backup, "verify": cmd_verify, "restore": cmd_restore, "selftest": cmd_selftest}[args.command](args)
    except BackupError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
