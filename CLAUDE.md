# delegation-strategy-2

## Migrations

`store migrate` runs every pending file in `migrations/` in one transaction
(`store/src/migrations.rs`). If one file fails, every file of that run rolls
back.

- NEVER write a top-level `BEGIN`, `COMMIT`, `END`, `ROLLBACK` or `ABORT`
  in a migration file. A `COMMIT` or `END` fails the run and rolls it back.
  A `ROLLBACK` or `ABORT` fails the run, but the statements after it in that
  file stay committed. A `BEGIN ... END;` inside a `DO` block is allowed.
- NEVER use `CREATE INDEX CONCURRENTLY` or `VACUUM`. They cannot run in a
  transaction, so the run fails.
- After `ALTER TYPE ... ADD VALUE`, the new value is not usable until the run
  commits. Do not use it in a later file of the same release.
- Name a file `NNNN-what-it-does.sql` with the next free number. NEVER edit a
  file after it ran in an environment. Add a new file instead.
- The run holds all its locks until the last file commits. Keep a long
  backfill out of a release that also locks a table the API reads.
