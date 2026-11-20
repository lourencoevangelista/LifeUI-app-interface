import { defineConfig } from "drizzle-kit";

// Secrets come from ../.env (git-ignored), edited by hand; never type them into chat or commit them.
// Node 24+ can load a .env file natively. Missing file is fine (variables may already be set).
try {
  process.loadEnvFile("../.env");
} catch {
  /* no .env file */
}

// `generate` needs no database. To APPLY migrations use `npm run migrate` (scripts/migrate.mjs), NOT
// `drizzle-kit migrate`: the latter rejects Supabase's certificate chain (SELF_SIGNED_CERT_IN_CHAIN) and
// cannot take a CA file, while migrate.mjs verifies the connection against db/certs/supabase-prod-ca-2021.crt.
//
// `migrate` needs DATABASE_URL_MIGRATE = Supabase SESSION-mode pooler URL:
//   postgresql://postgres.PROJECT_REF:PASTE_PASSWORD_HERE@aws-0-REGION.pooler.supabase.com:5432/postgres
// Why not the others:
//  - the direct connection (db.<ref>.supabase.co) is IPv6-only on the free plan, so it fails from
//    GitHub Actions and from IPv4-only home networks;
//  - the transaction pooler (port 6543) does not support prepared statements or session state,
//    which migrations need.
// The app itself (api/db_ping.rs) uses the transaction pooler through DATABASE_URL.
export default defineConfig({
  dialect: "postgresql",
  schema: "./src/schema.ts",
  out: "./migrations",
  schemaFilter: ["lifeui"],
  dbCredentials: { url: process.env.DATABASE_URL_MIGRATE ?? "" },
  strict: true,
  verbose: true,
});
