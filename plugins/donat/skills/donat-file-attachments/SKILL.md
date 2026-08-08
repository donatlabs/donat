---
name: donat-file-attachments
description: Use when a record in a donat application needs an avatar, document or image, or when uploads are lost, left unclaimed, or readable more widely than intended.
---

# File attachments

A file is a **uuid column on a table**, declared beside that table's
permissions. There is no upload service, no second permission model and no
cleanup cron to write. No file byte passes through the engine: the browser
talks to the object store over presigned URLs.

## Declare the column

```yaml
# databases/default/tables/public_customer.yaml
table: { name: customer, schema: public }
attachments:
  - column: avatar
    backend: media
    max_bytes: 2097152
    media_types: [image/png, image/jpeg]
    public: true
```

Note what is **not** there: a role list. A customer may upload an avatar
because the table's `update_permissions` let it write `avatar`, and may read
one back because `select_permissions` expose the column. The file inherits the
row's authorization rather than having its own — which is why there is nothing
to keep in sync.

`public: true` is a real grant, and never inferred. A public attachment gets a
stable, unsigned, immutable URL served from `/v1/files/public/…` that a CDN
caches forever; whoever holds the URL sees the file. Write it beside the
permissions, where a reviewer will see it. A private attachment instead gets a
short-lived URL that the database signs while producing the row.

`max_bytes` and `media_types` are enforced at the point the upload URL is
issued.

## Configure the store: `storage.yaml`

```yaml
backends:
  - name: media
    kind: s3
    bucket: petshop-media
    region: us-east-1
    endpoint: http://minio:9000
    path_style: true                    # MinIO by hostname; a real S3 bucket does not
    access_key_id:     { value_from_env: PETSHOP_S3_KEY }
    secret_access_key: { value_from_env: PETSHOP_S3_SECRET }
    public_base_url: http://127.0.0.1:9000/petshop-media

signing:
  secret: { value_from_env: PETSHOP_FILE_SIGNING_SECRET }
  upload_ttl_seconds: 900
  download_ttl_seconds: 300

gc:
  every_days: 1
  pending_ttl_days: 1
  orphan_grace_days: 1

limits:
  pending_uploads_per_session: 10
  uploads_per_minute_per_session: 30

identity:
  session_variable: x-donat-user-id

cors:
  allow_origins: [http://localhost:3000]
```

Credentials and the signing secret are `value_from_env`, never literals.

`limits` are counted by the engine against rows it owns, because a reverse
proxy cannot see them — it has no idea how many unclaimed uploads one shopper
is holding. `cors` is here because a browser uploading to a presigned URL does
so cross-origin, and nothing else in the engine needs it.

Pointing this at a real S3 bucket changes only this file.

## The upload flow

Three calls, and the third is not a formality.

**1. Ask for a URL.**

```graphql
mutation {
  donat_request_file_upload(
    attachment: public_customer_avatar,
    file_name: "me.png",
    media_type: "image/png",
    size: 1234
  ) { id url method headers { name value } expires_at }
}
```

**2. Upload the bytes** to the returned `url`, with the returned `method` and
**every** header returned — the presigned URL binds them.

```sh
curl -X PUT --data-binary @me.png \
  -H 'Content-Type: image/png' -H "Content-Length: $(stat -c%s me.png)" "<url>"
```

**3. Tell the engine the upload finished**, then store the id like any other
column value.

```sh
curl -X POST "<complete_url>"
```

```graphql
mutation {
  update_customer(where: {customer_id: {_eq: "c-1001"}}, _set: {avatar: "<id>"}) {
    affected_rows
  }
}
```

The completion step is where the engine asks the store what it actually holds
and moves the verified object out from under the presigned URL — which cannot
be revoked. Skipping it leaves an unclaimed upload the collector will reclaim.

The update carries a gate in the same statement: an upload that is not this
session's, was already used, expired, or whose bytes never arrived fails the
mutation instead of storing a dangling id.

## Reading it back

The column reads as an object, not a bare id:

```graphql
{ customer { name avatar { id file_name size url } } }
```

For a `public: true` attachment the `url` is stable and immutable, so a
subscription on the customer never sees it change. For a private one it is
short-lived and signed per read.

## Deletion is not file I/O

Clearing the column or deleting the row does **not** delete the object — a
mutation is database work. The background collector reclaims objects nothing
references any more, and uploads nobody ever claimed, on the `gc` schedule.

Do not add a "delete the file" call anywhere. If an object must be gone by a
deadline, that is a `gc` window question.

## Checklist

1. `attachments` entry on the table, with `max_bytes` and `media_types`.
2. `public:` decided deliberately and written down.
3. The role's `update_permissions` include the column — that is the upload
   grant; `select_permissions` include it — that is the read grant.
4. `storage.yaml` backend, signing secret and CORS origins, all from env.
5. `limits` set to something a single session should not exceed.
6. A test that a different session cannot complete this session's upload.

## Files to read

- [`examples/petshop/metadata/storage.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/storage.yaml) — the file above, fully commented
- [`examples/petshop/metadata/databases/default/tables/public_customer.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/databases/default/tables/public_customer.yaml)
- [`examples/petshop/README.md`](https://github.com/donatlabs/donat/blob/main/examples/petshop/README.md), "Attaching a file"
- [`crates/conformance/tests/file_attachments.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/file_attachments.rs)
