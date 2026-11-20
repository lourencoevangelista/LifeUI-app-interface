// READ-ONLY: prints the row count of every table in schema `lifeui` (no row contents).
// Same verified-TLS connection and password masking as migrate.mjs. Run with:  node scripts/counts.mjs   (in db/)
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import pg from "pg";

const here = path.dirname(fileURLToPath(import.meta.url));
try {
  process.loadEnvFile(path.join(here, "..", "..", ".env"));
} catch {
  /* no .env file: the variable may come from the environment */
}

const raw = process.env.DATABASE_URL_MIGRATE;
if (!raw) {
  console.error("DATABASE_URL_MIGRATE is not set.");
  process.exit(1);
}
const url = new URL(raw);
const secrets = [url.password, decodeURIComponent(url.password)].filter(Boolean);
const mask = (text) => secrets.reduce((t, s) => String(t).split(s).join("***"), String(text));
const local = ["localhost", "127.0.0.1", "::1"].includes(url.hostname);
url.searchParams.delete("sslmode");
const ssl = local
  ? false
  : { ca: readFileSync(path.join(here, "..", "certs", "supabase-prod-ca-2021.crt"), "utf8"), rejectUnauthorized: true };

const pool = new pg.Pool({ connectionString: url.toString(), ssl, max: 1 });
try {
  const client = await pool.connect();
  try {
    await client.query("BEGIN READ ONLY");
    const tables = await client.query("SELECT tablename FROM pg_tables WHERE schemaname = 'lifeui' ORDER BY tablename");
    for (const { tablename } of tables.rows) {
      const r = await client.query(`SELECT count(*)::int AS n FROM lifeui."${tablename}"`);
      console.log(`${tablename.padEnd(22)} ${r.rows[0].n}`);
    }
    const kinds = await client.query("SELECT origin, count(*)::int AS n FROM lifeui.entities GROUP BY origin ORDER BY origin");
    console.log("entities by origin:", JSON.stringify(kinds.rows));
    await client.query("ROLLBACK");
  } finally {
    client.release();
  }
} catch (error) {
  console.error("count failed:", mask(error.message));
  process.exitCode = 1;
} finally {
  await pool.end();
}
