# Development notes

Running the collectors CLI locally.

# 1. Run PosgreSQL and apply migrations scripts

```bash
export DB='delegation-strategy'

docker run --name postgresql-${DB} -p 5432:5432 --rm \
  -e POSTGRES_DB=${DB} \
  -e POSTGRES_USER=${DB} \
  -e POSTGRES_PASSWORD=${DB} \
  postgres:17.4 \
  -c max-prepared-transactions=100 \
  -c log-statement=all \
  -c ssl=on \
  -c ssl_cert_file=/etc/ssl/certs/ssl-cert-snakeoil.pem \
  -c ssl_key_file=/etc/ssl/private/ssl-cert-snakeoil.key

export DB='delegation-strategy'
for FILE in ./migrations/*.sql; do
  echo "Migration SQL init execution: $FILE"
  PGPASSWORD=${DB} psql -U ${DB} -d ${DB} \
    -h localhost -p 5432 -f "$FILE"
done

docker cp postgresql-${DB}:/etc/ssl/certs/ssl-cert-snakeoil.pem /tmp/postgres-root-cert.pem
```

The PostgreSQL URL is then `postgresql://delegation-strategy:delegation-strategy@localhost:5432/delegation-strategy`
