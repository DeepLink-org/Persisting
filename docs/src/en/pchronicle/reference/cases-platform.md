# pChronicle Directory and platform cases

Platform-oriented Directory setup. The ACL file manages users, datasets
(libraries), and grants; Warehouse listen/Gateway options still come from
`pchronicle serve`.

## P01: Issue a Directory user from an empty config

```bash
pchronicle serve catalog issue \
  --catalog-config ./catalog.toml alice
```

If the file does not exist, the command creates it, writes a user with empty
grants, and prints the secret once on stdout.

## P02: Register a dataset library

```bash
pchronicle serve catalog dataset add \
  --catalog-config ./catalog.toml \
  prod \
  --uri s3://bucket/prod \
  --endpoint http://127.0.0.1:9000 \
  --region us-west-2 \
  --access-key BACKEND_AK \
  --secret-key BACKEND_SK
```

This only registers the URI and backend credentials. It does not create or
delete object-store data. All `s3://` libraries in one file must share the same
endpoint, region, and backend keys.

## P03: Grant libraries to a user

```bash
pchronicle serve catalog grant \
  --catalog-config ./catalog.toml \
  alice prod
```

Expected: a `[[grants]]` entry lists `prod` under that user. v1 grants are
library membership (not `--permission` flags).

## P04: Serve with catalog mounts

```bash
pchronicle serve \
  --catalog-config ./catalog.toml \
  --listen 127.0.0.1:8081
```

Every `[datasets.*]` entry is mounted into Warehouse. Directory ticket routes
remain available for `catalog://` pins. Restart after editing the ACL file.

## P05: Open an authorized dataset via a Directory pin

```bash
pchronicle dataset pin team catalog://127.0.0.1:8081 \
  --ak USER_AK --sk USER_SK
pchronicle query @team/prod \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
```

Expected: an authorized user can query `prod`; unknown datasets fail closed.
Automated runs skip this case by default; set `PCHRONICLE_CASE_MODE=catalog`
after completing P01–P04 against a live Directory.

## P06: Revoke a library grant

```bash
pchronicle serve catalog revoke \
  --catalog-config ./catalog.toml \
  alice prod
```

Expected: later `@team/prod` access is denied for that user.

## P07: RustFS Warehouse regression

Prepare RustFS and set:

```bash
export PCHRONICLE_RUSTFS_ENDPOINT=http://127.0.0.1:9000
export PCHRONICLE_RUSTFS_ACCESS_KEY=rustfsadmin
export PCHRONICLE_RUSTFS_SECRET_KEY=rustfsadmin
export PCHRONICLE_RUSTFS_BUCKET=pchronicle-cases
```

Then run the RustFS regression coverage for Dataset writes, Snapshot discovery
(including `chronicle.manifest` when present), SQL, Explorer, and refresh.
Automated runs skip this case by default; set `PCHRONICLE_CASE_MODE=rustfs`
with a reachable endpoint to execute it.

Platform checks:

- ACL files can be built from empty;
- user, dataset, and grant edits are deterministic;
- backend object-store keys stay in the catalog file / ticket path, not in
  `dataset list` output;
- Warehouse mounts every registered library when serving `--catalog-config`;
- Snapshot refresh does not mutate an in-flight Snapshot;
- RustFS Warehouse behavior matches local Datasets for the covered paths.
