# Development notes

Running the collectors CLI locally.

# 1. Run marinade-directory

The store keeps its documents in a bucket, so a local run needs the emulator
and the directory in front of it.

```bash
export GCS_PORT=4443
export DIRECTORY_PORT=8080
export JWT_SECRET='delegation-strategy-local-secret-at-least-32b'

docker run -d --rm --name fake-gcs --network host \
  fsouza/fake-gcs-server:1.56.1 \
  -backend memory -scheme http -port $GCS_PORT -public-host localhost:$GCS_PORT

curl -sf -X POST "http://localhost:$GCS_PORT/storage/v1/b?project=delegation-strategy" \
  -H 'Content-Type: application/json' \
  -d '{"name":"delegation-strategy","versioning":{"enabled":true}}'

docker run -d --rm --name marinade-directory --network host \
  -e STORAGE_EMULATOR_HOST=localhost:$GCS_PORT \
  -e GCS_BUCKET=delegation-strategy \
  -e JWT_SECRET="$JWT_SECRET" \
  -e PORT=$DIRECTORY_PORT \
  -e METRICS_PORT=0 \
  marinade-directory:test

curl -sf "http://localhost:$DIRECTORY_PORT/ready"
```

Every `/v1` request carries a HS256 token signed with `JWT_SECRET`, whose
`grants` are globs with a leading slash — `validators/**` matches nothing:

```bash
b64() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
HEADER=$(printf '{"alg":"HS256"}' | b64)
CLAIMS=$(printf '{"sub":"local","grants":["/validators/**:rw","/scoring/**:rw"],"exp":%s}' \
  $(($(date +%s) + 86400)) | b64)
export DIRECTORY_URL="http://localhost:$DIRECTORY_PORT"
export DIRECTORY_TOKEN="$HEADER.$CLAIMS.$(printf "$HEADER.$CLAIMS" \
  | openssl dgst -sha256 -hmac "$JWT_SECRET" -binary | b64)"
```

# 2. Run the tests

The tests own their store: each one starts `marinade-directory` on its
in-memory backend with `docker`, mints its own token and stops the container
when it ends — no bucket and no emulator, since the mem backend answers the
same contract. Without `docker` they say why they skipped and pass.

```bash
cargo test --all-features
```
