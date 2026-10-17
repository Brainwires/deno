#!/usr/bin/env bash
# Copyright 2018-2026 the Deno authors. MIT license.
#
# The services the npm matrix's database and object-store cases talk to, as
# throwaway Docker containers on fixed loopback ports:
#   Postgres 17 :55432 (postgres/pw), Redis 7 :56379, MongoDB 7 :57017,
#   MinIO :59000 (minioadmin/minioadmin).
# Usage: tests/npm_matrix/services.sh up|down
set -euo pipefail

names=(npm-matrix-postgres npm-matrix-redis npm-matrix-mongo npm-matrix-minio)

case "${1:-}" in
  up)
    docker run -d --rm --name npm-matrix-postgres -p 127.0.0.1:55432:5432 \
      -e POSTGRES_PASSWORD=pw postgres:17-alpine
    docker run -d --rm --name npm-matrix-redis -p 127.0.0.1:56379:6379 \
      redis:7-alpine
    docker run -d --rm --name npm-matrix-mongo -p 127.0.0.1:57017:27017 \
      mongo:7
    # minio/minio is no longer published on Docker Hub; Bitnami's image
    # starts the server itself
    docker run -d --rm --name npm-matrix-minio -p 127.0.0.1:59000:9000 \
      -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
      bitnamilegacy/minio:2025.7.23-debian-12-r5
    ;;
  down)
    for name in "${names[@]}"; do
      docker rm -f "$name" >/dev/null 2>&1 || true
    done
    ;;
  *)
    echo "usage: $0 up|down" >&2
    exit 2
    ;;
esac
