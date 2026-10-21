# delegation-strategy-2

## Migrations

`store migrate` runs every pending file in `migrations/` in one transaction
(`store/src/migrations.rs`). If one file fails, every file of that run rolls
back.

- NEVER write a top-level `BEGIN;`, `COMMIT`, `ROLLBACK`, `ABORT` or
  `START TRANSACTION` in a migration file. It ends the run's transaction,
  and the files after it run without one. A `BEGIN ... END;` inside a `DO`
  block is not transaction control and is allowed.
  `no_file_controls_the_transaction` checks every file.
- NEVER use `CREATE INDEX CONCURRENTLY` or `VACUUM`. They cannot run in a
  transaction, so the run fails.
- After `ALTER TYPE ... ADD VALUE`, the new value is not usable until the run
  commits. Do not use it in a later file of the same release.
- Name a file `NNNN-what-it-does.sql` with the next free number. NEVER edit a
  file after it ran in an environment. Add a new file instead.
- The run holds all its locks until the last file commits. Keep a long
  backfill out of a release that also locks a table the API reads.
