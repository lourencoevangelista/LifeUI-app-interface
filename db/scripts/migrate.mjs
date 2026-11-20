// Applies db/migrations/*.sql to the database in DATABASE_URL_MIGRATE (from ../.env or the environment).
// Same migrator as `drizzle-kit migrate` (same bookkeeping table drizzle.__drizzle_migrations), but the
// TLS connection to Supabase is VERIFIED against Supabase's root CA (db/certs/supabase-prod-ca-2021.crt).
// `drizzle-kit migrate` itself cannot do that: it rejects Supabase's certificate chain, and the usual
// workaround (turning certificate checking off) would leave the connection open to impersonation.
// The password is masked in everything this script prints. Run with:  npm run migrate   (in db/)
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import pg from "pg";
import { drizzle } from "drizzle-orm/node-postgres";
import { migrate } from "drizzle-orm/node-postgres/migrator";

const here = path.dirname(fileURLToPath(import.meta.url));
try {
  process.loadEnvFile(path.join(here, "..", "..", ".env"));
} catch {
  /* no .env file: the variable may come from the environment */
}

const raw = process.env.DATABASE_URL_MIGRATE;
if (!raw) {
  console.error("DATABASE_URL_MIGRATE is not set (put it in ../.env; see .env.example).");
  process.exit(1);
}

const url = new URL(raw);
const secrets = [url.password, decodeURIComponent(url.password)].filter(Boolean);
const mask = (text) => secrets.reduce((t, s) => String(t).split(s).join("***"), String(text));

const local = ["localhost", "127.0.0.1", "::1"].includes(url.hostname);
url.searchParams.delete("sslmode"); // pg would otherwise override the ssl option below
const ssl = local
  ? false
  : { ca: readFileSync(path.join(here, "..", "certs", "supabase-prod-ca-2021.crt"), "utf8"), rejectUnauthorized: true };

const pool = new pg.Pool({ connectionString: url.toString(), ssl, max: 1 });
try {
  console.log(`connecting to ${url.hostname}:${url.port || 5432} as ${decodeURIComponent(url.username)} (TLS ${local ? "off, local" : "verified"})`);
  await migrate(drizzle(pool), { migrationsFolder: path.join(here, "..", "migrations") });
  console.log("migrations applied");
} catch (error) {
  console.error("migration failed:", mask(error.message));
  process.exitCode = 1;
} finally {
  await pool.end();
}
